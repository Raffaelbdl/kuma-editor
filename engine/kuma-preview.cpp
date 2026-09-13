// kuma-preview: pull frames from an MLT timeline, stream JPEGs, play audio.
//
// Single pull thread owns the MLT producer. Commands arrive on stdin (reader thread) and
// are applied between frames. Audio drives the clock: the SDL callback drains a ring
// buffer and the pull loop only decodes ahead while the ring has room, which paces video
// to real time without a separate timer.

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
    int rate = 48000;
    int channels = 2;

    bool open() {
        if (SDL_InitSubSystem(SDL_INIT_AUDIO) != 0) return false;
        SDL_AudioSpec want{}, have{};
        want.freq = rate;
        want.format = AUDIO_S16SYS;
        want.channels = (Uint8)channels;
        want.samples = 1024;
        want.callback = &AudioOut::cb;
        want.userdata = this;
        dev_ = SDL_OpenAudioDevice(nullptr, 0, &want, &have, 0);
        if (dev_ == 0) return false;
        rate = have.freq;
        channels = have.channels;
        SDL_PauseAudioDevice(dev_, 0);
        return true;
    }

    // Samples (per channel) consumed since the last reset — the master clock.
    long long consumed() const { return consumed_.load(); }
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
        consumed_ = 0;
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
        if (running_) consumed_ += (long long)(want / channels); // wall-clock paced by the device
    }

    SDL_AudioDeviceID dev_ = 0;
    std::mutex m_;
    std::vector<int16_t> ring_;
    std::atomic<long long> consumed_{0};
    std::atomic<bool> running_{false};
};

// Fallback when there is no audio device: a wall clock expressed in samples.
class WallClock {
public:
    void start() { t0_ = std::chrono::steady_clock::now(); base_ = 0; running_ = true; }
    void stop() { base_ = consumed(); running_ = false; }
    void reset() { base_ = 0; t0_ = std::chrono::steady_clock::now(); }
    long long consumed() const {
        if (!running_) return base_;
        auto dt = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0_).count();
        return base_ + (long long)(dt * rate);
    }
    int rate = 48000;

private:
    std::chrono::steady_clock::time_point t0_;
    long long base_ = 0;
    bool running_ = false;
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
        audio_ok_ = audio_.open();
        if (!audio_ok_) {
            std::fprintf(stderr, "kuma-preview: no audio device (%s); using wall clock\n", SDL_GetError());
        }
        emit("ok ready audio=" + std::string(audio_ok_ ? "sdl" : "wallclock"));

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

            // Playback: decode ahead while the audio ring has less than ~200 ms queued.
            long long ahead = audio_ok_ ? audio_.queued() : 0;
            if (audio_ok_ && ahead > rate_ / 5) {
                std::this_thread::sleep_for(std::chrono::milliseconds(5));
                continue;
            }
            if (!audio_ok_) {
                // Wall clock: only advance when the clock has reached the next frame.
                long long due = (long long)((double)(pos_ - play_start_frame_) * rate_ / fps_);
                if (clock_.consumed() < due) {
                    std::this_thread::sleep_for(std::chrono::milliseconds(2));
                    continue;
                }
            }
            if (pos_ >= length_) {
                pause();
                emit("ok end");
                continue;
            }
            bool late = audio_ok_ && ahead < rate_ / 20; // < 50 ms buffered: skip JPEG to catch up
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
            emit("pos " + std::to_string(pos_));
        } else if (c.op == "speed") {
            speed_ = std::max(0.1, std::min(4.0, std::atof(c.arg.c_str())));
            emit("ok speed");
        } else {
            emit("err unknown command " + c.op);
        }
    }

    void load(const std::string &path) {
        bool was_playing = playing_;
        pause();
        auto profile = std::make_unique<Mlt::Profile>();
        auto prod = std::make_unique<Mlt::Producer>(*profile, "xml", path.c_str());
        if (!prod->is_valid()) {
            emit("err cannot load " + path);
            return;
        }
        prod->set("consumer.progressive", 1);
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
        clock_.reset();
        clock_.rate = rate_;
        play_start_frame_ = pos_;
        playing_ = true;
        audio_.set_running(true);
        clock_.start();
        emit("ok play");
    }

    void pause() {
        if (!playing_) return;
        playing_ = false;
        audio_.set_running(false);
        clock_.stop();
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
            if (img && w > 0 && h > 0) {
                unsigned char *out = nullptr;
                unsigned long size = 0;
                if (tjCompress2(jpeg_, img, w, 0, h, TJPF_RGB, &out, &size, TJSAMP_420, 80, TJFLAG_FASTDCT) == 0) {
                    std::string header = "frame " + std::to_string(frame_index) + " " + std::to_string(size) + "\n";
                    std::lock_guard<std::mutex> l(out_m_);
                    std::fwrite(header.data(), 1, header.size(), stdout);
                    std::fwrite(out, 1, size, stdout);
                    std::fflush(stdout);
                    tjFree(out);
                }
            }
        } else {
            emit("pos " + std::to_string(frame_index));
        }

        if (with_audio && audio_ok_) {
            mlt_audio_format afmt = mlt_audio_s16;
            int freq = rate_, channels = audio_.channels;
            int samples = mlt_audio_calculate_frame_samples(fps_, freq, frame_index);
            int16_t *pcm = static_cast<int16_t *>(frame->get_audio(afmt, freq, channels, samples));
            if (pcm && samples > 0) {
                if (channels == audio_.channels && freq == rate_) {
                    audio_.push(pcm, samples);
                } else {
                    // Unexpected layout: feed silence to keep the clock honest.
                    std::vector<int16_t> silence((size_t)samples * audio_.channels, 0);
                    audio_.push(silence.data(), samples);
                }
            }
        }
    }

    void emit(const std::string &line) {
        std::lock_guard<std::mutex> l(out_m_);
        std::fwrite(line.data(), 1, line.size(), stdout);
        std::fputc('\n', stdout);
        std::fflush(stdout);
    }

    std::unique_ptr<Mlt::Profile> profile_;
    std::unique_ptr<Mlt::Producer> producer_;
    tjhandle jpeg_ = nullptr;
    AudioOut audio_;
    WallClock clock_;
    bool audio_ok_ = false;
    std::mutex out_m_;

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
    double speed_ = 1.0;
    int rate_ = 48000;
};

} // namespace

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;
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
