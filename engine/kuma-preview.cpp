// kuma-preview: pull frames from an MLT timeline, stream JPEGs, play audio.
//
// Single pull thread owns the MLT producer and stdout. Commands arrive on stdin (reader
// thread) and are applied between frames. Audio paces playback: the SDL callback drains
// a ring buffer and the pull loop decodes the next frame only while the ring holds less
// than a few frames of sound, so video runs at real time and leads audio by at most
// that much. Without an audio device SDL's dummy driver paces the callback instead.

#include <Mlt.h>
#include <SDL.h>
#include <turbojpeg.h>

#include <atomic>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <deque>
#include <iostream>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

namespace {

// ---------------------------------------------------------------- audio clock ----

class AudioOut {
public:
    static constexpr int rate = 48000;
    static constexpr int channels = 2;

    // Opens the default device; without one (CI) SDL's dummy driver still paces
    // callbacks in real time, so playback is always audio-paced.
    bool open() {
        if (SDL_InitSubSystem(SDL_INIT_AUDIO) != 0 || !open_device()) {
            SDL_QuitSubSystem(SDL_INIT_AUDIO);
            SDL_setenv("SDL_AUDIODRIVER", "dummy", 1);
            if (SDL_InitSubSystem(SDL_INIT_AUDIO) != 0 || !open_device()) return false;
        }
        SDL_PauseAudioDevice(dev_, 0);
        return true;
    }

private:
    bool open_device() {
        SDL_AudioSpec want{}, have{};
        want.freq = rate;
        want.format = AUDIO_S16SYS;
        want.channels = (Uint8)channels;
        want.samples = 1024;
        want.callback = &AudioOut::cb;
        want.userdata = this;
        dev_ = SDL_OpenAudioDevice(nullptr, 0, &want, &have, 0); // allowed_changes = 0: have == want
        return dev_ != 0;
    }

public:
    // Samples (per channel) waiting to be played.
    long long queued() {
        std::lock_guard<std::mutex> l(m_);
        return (long long)ring_.size() / channels;
    }

    void push(const int16_t *data, int samples) {
        std::lock_guard<std::mutex> l(m_);
        ring_.insert(ring_.end(), data, data + (size_t)samples * channels);
    }
    void reset() {
        std::lock_guard<std::mutex> l(m_);
        ring_.clear();
    }
    void set_running(bool on) { running_ = on; }
    void close() {
        if (dev_) {
            SDL_CloseAudioDevice(dev_);
            dev_ = 0;
        }
    }

private:
    static void cb(void *ud, Uint8 *stream, int len) { static_cast<AudioOut *>(ud)->fill(stream, len); }

    void fill(Uint8 *stream, int len) {
        int16_t *out = reinterpret_cast<int16_t *>(stream);
        size_t want = (size_t)len / sizeof(int16_t);
        std::lock_guard<std::mutex> l(m_);
        size_t have = running_ ? std::min(want, ring_.size()) : 0;
        if (have) {
            std::memcpy(out, ring_.data(), have * sizeof(int16_t));
            ring_.erase(ring_.begin(), ring_.begin() + (long)have);
        }
        if (have < want) std::memset(out + have, 0, (want - have) * sizeof(int16_t));
    }

    SDL_AudioDeviceID dev_ = 0;
    std::mutex m_;
    std::vector<int16_t> ring_;
    std::atomic<bool> running_{false};
};

// ----------------------------------------------------------------- engine ----------

struct Command {
    std::string op;
    std::string arg;
};

class Engine {
public:
    Engine() : jpeg_(tjInitCompress()) {}
    ~Engine() {
        if (jpeg_) tjDestroy(jpeg_);
    }

    void push(Command c) {
        {
            std::lock_guard<std::mutex> l(qm_);
            queue_.push_back(std::move(c));
        }
        qcv_.notify_one();
    }

    void run() {
        Mlt::Factory::init();
        if (!audio_.open()) {
            std::fprintf(stderr, "kuma-preview: cannot open an audio device: %s\n", SDL_GetError());
            return;
        }
        emit("ok ready");

        while (!quit_) {
            // Apply pending commands.
            {
                std::unique_lock<std::mutex> l(qm_);
                if (!playing_ && queue_.empty()) qcv_.wait(l);
                while (!queue_.empty()) {
                    Command c = std::move(queue_.front());
                    queue_.pop_front();
                    l.unlock();
                    apply(c);
                    l.lock();
                }
            }
            if (quit_) break;
            if (!playing_ || !producer_) continue;

            // Playback: decode ahead while the audio ring holds less than ~3 frames, so
            // the picture never leads the sound by more than that.
            long long ahead = audio_.queued();
            long long target = (long long)(3.0 * rate_ / fps_);
            if (ahead > target) {
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
                continue;
            }
            if (pos_ >= length_) {
                // Let the queued sound of the last frames play out before stopping.
                if (audio_.queued() > 0) {
                    std::this_thread::sleep_for(std::chrono::milliseconds(5));
                    continue;
                }
                pause();
                emit("ok end");
                continue;
            }
            // < 1 frame buffered: skip the JPEG to catch up — except right after play(),
            // when the ring is empty by construction and the picture must still update.
            bool late = pos_ > play_start_frame_ + 3 && ahead < target / 3;
            pull(pos_, /*with_audio=*/true, /*emit_image=*/!late || (pos_ % 3 == 0));
            pos_++;
        }
        audio_.set_running(false);
        audio_.close();
        // MLT objects must die before the factory does.
        producer_.reset();
        profile_.reset();
        Mlt::Factory::close();
    }

private:
    void apply(const Command &c) {
        if (c.op == "quit") {
            quit_ = true;
        } else if (c.op == "load") {
            load(c.arg);
        } else if (c.op == "seek") {
            pause();
            pos_ = std::max(0, std::min(length_ - 1, std::atoi(c.arg.c_str())));
            if (producer_) pull(pos_, false, true);
        } else if (c.op == "play") {
            if (!producer_) { emit("err no timeline loaded"); return; }
            if (pos_ >= length_ - 1) pos_ = 0;
            play();
        } else if (c.op == "pause") {
            pause();
        } else {
            emit("err unknown command " + c.op);
        }
    }

    void load(const std::string &path) {
        // Validate first: a bad file leaves the current timeline (and playback) untouched.
        auto profile = std::make_unique<Mlt::Profile>();
        auto prod = std::make_unique<Mlt::Producer>(*profile, "xml", path.c_str());
        if (!prod->is_valid()) {
            emit("err cannot load " + path);
            return;
        }
        bool was_playing = playing_;
        pause();
        producer_ = std::move(prod);
        profile_ = std::move(profile);
        fps_ = profile_->fps();
        width_ = profile_->width();
        height_ = profile_->height();
        length_ = producer_->get_length();
        if (length_ <= 0) length_ = producer_->get_playtime();
        pos_ = std::max(0, std::min(length_ - 1, pos_));
        emit("ok loaded frames=" + std::to_string(length_) + " fps=" + std::to_string(fps_) + " size=" +
             std::to_string(width_) + "x" + std::to_string(height_));
        pull(pos_, false, true);
        if (was_playing) play();
    }

    void play() {
        if (playing_) return;
        audio_.reset();
        play_start_frame_ = pos_;
        playing_ = true;
        audio_.set_running(true);
        emit("ok play");
    }

    void pause() {
        if (!playing_) return;
        playing_ = false;
        audio_.set_running(false);
        audio_.reset();
    }

    // Decode one frame: optionally queue its audio, optionally emit its image.
    void pull(int frame_index, bool with_audio, bool emit_image) {
        producer_->seek(frame_index);
        std::unique_ptr<Mlt::Frame> frame(producer_->get_frame());
        if (!frame || !frame->is_valid()) {
            emit("err no frame at " + std::to_string(frame_index));
            return;
        }
        frame->set("consumer.rescale", "bilinear");
        frame->set("consumer.deinterlace", 1);

        if (emit_image) {
            mlt_image_format fmt = mlt_image_rgb;
            int w = width_, h = height_;
            uint8_t *img = frame->get_image(fmt, w, h);
            unsigned char *out = nullptr;
            unsigned long size = 0;
            // 4:4:4 keeps text and thin overlay strokes crisp; localhost bandwidth is not a concern.
            if (img && w > 0 && h > 0 && fmt == mlt_image_rgb &&
                tjCompress2(jpeg_, img, w, 0, h, TJPF_RGB, &out, &size, TJSAMP_444, 88, TJFLAG_FASTDCT) == 0) {
                std::string header = "frame " + std::to_string(frame_index) + " " + std::to_string(size) + "\n";
                std::fwrite(header.data(), 1, header.size(), stdout);
                std::fwrite(out, 1, size, stdout);
                std::fflush(stdout);
            } else {
                emit("err no image at " + std::to_string(frame_index));
            }
            tjFree(out);
        }

        if (with_audio) {
            mlt_audio_format afmt = mlt_audio_s16;
            int freq = rate_, channels = audio_.channels;
            int samples = mlt_audio_calculate_frame_samples(fps_, freq, frame_index);
            int16_t *pcm = static_cast<int16_t *>(frame->get_audio(afmt, freq, channels, samples));
            if (pcm && samples > 0 && channels == audio_.channels && freq == rate_) audio_.push(pcm, samples);
        }
    }

    // stdout is written only from the engine thread, so no lock is needed.
    void emit(const std::string &line) {
        std::fwrite(line.data(), 1, line.size(), stdout);
        std::fputc('\n', stdout);
        std::fflush(stdout);
    }

    std::unique_ptr<Mlt::Profile> profile_;
    std::unique_ptr<Mlt::Producer> producer_;
    tjhandle jpeg_ = nullptr;
    AudioOut audio_;

    std::mutex qm_;
    std::condition_variable qcv_;
    std::deque<Command> queue_;

    std::atomic<bool> quit_{false};
    bool playing_ = false;
    int pos_ = 0;
    int play_start_frame_ = 0;
    int length_ = 0;
    int width_ = 0, height_ = 0;
    double fps_ = 30.0;
    int rate_ = 48000;
};

} // namespace

int main() {
    setvbuf(stdout, nullptr, _IOFBF, 1 << 20);
    Engine engine;
    std::thread reader([&] {
        std::string line;
        while (std::getline(std::cin, line)) {
            if (line.empty()) continue;
            auto sp = line.find(' ');
            Command c{line.substr(0, sp), sp == std::string::npos ? "" : line.substr(sp + 1)};
            bool q = c.op == "quit";
            engine.push(std::move(c));
            if (q) break;
        }
        engine.push(Command{"quit", ""});
    });
    engine.run();
    reader.join();
    return 0;
}
