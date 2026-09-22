// kuma-preview: pull frames from MLT timelines, stream JPEGs, play audio.
//
// The picture and the sound are two graphs, compiled from one document by
// `kuma_mlt::Streams`, each pulled by its own thread, so an edit to one reloads only that
// one and a slow picture never starves the sound. The sound is the clock: its thread keeps
// the SDL ring a few frames full, and the picture's thread shows whatever frame the samples
// actually played have reached, dropping frames when it falls behind. Commands arrive on
// stdin (reader thread) and are applied on the main thread. Without an audio device SDL's
// dummy driver paces the callback instead, so there is always a clock.

#include <Mlt.h>
#include <SDL.h>
#include <turbojpeg.h>

#include <algorithm>
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

    // Samples (per channel) handed to the device since the last reset: the clock.
    long long consumed() const { return consumed_; }

    // Dropped unless `gen` is the one the ring was last reset for, so sound pulled for a
    // playback that has since stopped never reaches the next one.
    void push(const int16_t *data, int samples, unsigned gen) {
        std::lock_guard<std::mutex> l(m_);
        if (gen != gen_) return;
        ring_.insert(ring_.end(), data, data + (size_t)samples * channels);
    }
    void push_silence(int samples, unsigned gen) {
        std::lock_guard<std::mutex> l(m_);
        if (gen != gen_) return;
        ring_.insert(ring_.end(), (size_t)samples * channels, 0);
    }
    void reset(unsigned gen) {
        std::lock_guard<std::mutex> l(m_);
        ring_.clear();
        consumed_ = 0;
        gen_ = gen;
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
            consumed_ += (long long)(have / channels);
        }
        if (have < want) std::memset(out + have, 0, (want - have) * sizeof(int16_t));
    }

    SDL_AudioDeviceID dev_ = 0;
    std::mutex m_;
    std::vector<int16_t> ring_;
    unsigned gen_ = 0;
    std::atomic<long long> consumed_{0};
    std::atomic<bool> running_{false};
};

// ----------------------------------------------------------------- graphs ----------

// One compiled timeline. The profile is declared first so it outlives the producer.
struct Graph {
    std::unique_ptr<Mlt::Profile> profile;
    std::unique_ptr<Mlt::Producer> producer;
    int length = 0;
    double fps = 30.0;
    int width = 0, height = 0;

    static std::unique_ptr<Graph> open(const std::string &path) {
        auto g = std::make_unique<Graph>();
        g->profile = std::make_unique<Mlt::Profile>();
        g->producer = std::make_unique<Mlt::Producer>(*g->profile, "xml", path.c_str());
        if (!g->producer->is_valid()) return nullptr;
        g->fps = g->profile->fps();
        g->width = g->profile->width();
        g->height = g->profile->height();
        g->length = g->producer->get_length();
        if (g->length <= 0) g->length = g->producer->get_playtime();
        return g;
    }
};

// A graph and the lock its thread holds while pulling from it, so a load can swap it
// between two frames without stopping playback.
struct Slot {
    std::mutex m;
    std::unique_ptr<Graph> g;
};

// ----------------------------------------------------------------- engine ----------

struct Command {
    std::string op;
    std::string arg;
    unsigned gen = 0; // `end` only: the playback that ended
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
        std::thread sound([this] { sound_loop(); });
        std::thread picture([this] { picture_loop(); });
        emit("ok ready");

        while (!quit_) {
            std::unique_lock<std::mutex> l(qm_);
            qcv_.wait(l, [this] { return !queue_.empty(); });
            Command c = std::move(queue_.front());
            queue_.pop_front();
            l.unlock();
            apply(c);
        }
        sound.join();
        picture.join();
        audio_.set_running(false);
        audio_.close();
        // MLT objects must die before the factory does.
        video_.g.reset();
        sound_.g.reset();
        Mlt::Factory::close();
    }

private:
    void apply(const Command &c) {
        if (c.op == "quit") {
            wake([this] { quit_ = true; });
        } else if (c.op == "load-video") {
            load(c.arg, video_, "video");
        } else if (c.op == "load-audio") {
            load(c.arg, sound_, "audio");
        } else if (c.op == "seek") {
            pause();
            pos_ = std::max(0, std::min(length_ - 1, std::atoi(c.arg.c_str())));
            show(pos_);
        } else if (c.op == "play") {
            if (length_ == 0) { emit("err no timeline loaded"); return; }
            if (pos_ >= length_ - 1) pos_ = 0;
            play();
        } else if (c.op == "pause") {
            pause();
        } else if (c.op == "end") {
            // From the sound thread: the playback it was pulling for ran out and has been heard.
            if (playing_ && epoch_ == c.gen) {
                pause();
                pos_ = std::max(0, length_ - 1);
                emit("ok end");
            }
        } else {
            emit("err unknown command " + c.op);
        }
    }

    // Replace one graph. Validated first, so a bad file leaves what is loaded, and
    // playing, untouched; a good one is swapped in between two frames without pausing.
    void load(const std::string &path, Slot &slot, const char *name) {
        auto g = Graph::open(path);
        if (!g) { emit("err cannot load " + path); return; }
        emit(std::string("ok loaded ") + name + " frames=" + std::to_string(g->length) + " fps=" +
             std::to_string(g->fps) + " size=" + std::to_string(g->width) + "x" + std::to_string(g->height));
        fps_ = g->fps;
        (&slot == &video_ ? video_len_ : sound_len_) = g->length;
        length_ = std::max(video_len_.load(), sound_len_.load());
        {
            // The old graph is destroyed below, after its lock is released.
            std::lock_guard<std::mutex> l(slot.m);
            std::swap(slot.g, g);
        }
        if (!playing_) {
            pos_ = std::max(0, std::min(length_ - 1, pos_.load()));
            if (&slot == &video_) show(pos_);
        }
    }

    void play() {
        if (playing_) return;
        shown_ = pos_.load();
        unsigned e = ++epoch_;
        audio_.reset(e);
        audio_.set_running(true);
        wake([this] { playing_ = true; });
        emit("ok play");
    }

    void pause() {
        if (!playing_) return;
        playing_ = false;
        unsigned e = ++epoch_;
        audio_.set_running(false);
        audio_.reset(e);
        pos_ = shown_.load();
    }

    // Change what the workers wait on under their lock, so none misses the change.
    template <class F> void wake(F change) {
        {
            std::lock_guard<std::mutex> l(wm_);
            change();
        }
        wcv_.notify_all();
    }

    // Block while paused; false once quitting.
    bool await_playback() {
        std::unique_lock<std::mutex> l(wm_);
        wcv_.wait(l, [this] { return playing_ || quit_; });
        return !quit_;
    }

    static void sleep_ms(double ms) {
        std::this_thread::sleep_for(std::chrono::microseconds((long long)(std::clamp(ms, 1.0, 50.0) * 1000)));
    }

    // The sound thread: keep about three frames of sound queued, pulled from the audio
    // graph, or silence when there is none so the clock still runs.
    void sound_loop() {
        unsigned seen = 0;
        int apos = 0;
        bool ended = false;
        while (await_playback()) {
            unsigned e = epoch_;
            if (e != seen) { seen = e; apos = pos_; ended = false; }
            double fps = fps_;
            long long target = std::max<long long>((long long)(3.0 * rate_ / fps), 2048);
            long long queued = audio_.queued();
            if (queued > target) { sleep_ms((queued - target) * 1000.0 / rate_); continue; }
            if (apos >= length_) {
                // Let the last frames' sound play out, then say so once.
                if (!ended && queued == 0) { ended = true; push(Command{"end", "", e}); }
                sleep_ms(5);
                continue;
            }
            int samples = mlt_audio_calculate_frame_samples(fps, rate_, apos);
            if (!pull_sound(apos, samples, e)) audio_.push_silence(samples, e);
            apos++;
        }
    }

    // Queue one frame of the audio graph's sound; false when there is none to queue.
    bool pull_sound(int frame_index, int samples, unsigned epoch) {
        std::lock_guard<std::mutex> l(sound_.m);
        if (!sound_.g) return false;
        sound_.g->producer->seek(frame_index);
        std::unique_ptr<Mlt::Frame> frame(sound_.g->producer->get_frame());
        if (!frame || !frame->is_valid()) return false;
        mlt_audio_format afmt = mlt_audio_s16;
        int freq = rate_, channels = audio_.channels;
        int16_t *pcm = static_cast<int16_t *>(frame->get_audio(afmt, freq, channels, samples));
        if (!pcm || samples <= 0 || channels != audio_.channels || freq != rate_) return false;
        audio_.push(pcm, samples, epoch);
        return true;
    }

    // The picture thread: show the frame the sound has reached. Ahead of it, wait until
    // it is due; behind it, skip to it.
    void picture_loop() {
        unsigned seen = 0;
        int vpos = 0, start = 0;
        while (await_playback()) {
            unsigned e = epoch_;
            if (e != seen) { seen = e; vpos = start = pos_; }
            double fps = fps_;
            int heard = start + (int)((double)audio_.consumed() * fps / rate_);
            if (vpos >= length_) { sleep_ms(5); continue; }
            if (vpos > heard) { sleep_ms((vpos - heard) * 1000.0 / fps); continue; }
            vpos = std::min(heard, length_ - 1);
            if (picture(vpos, e)) shown_ = vpos;
            vpos++;
        }
    }

    // Show one frame now, from the main thread.
    void show(int frame_index) { picture(frame_index, 0); }

    // Whether a frame rendered for `epoch` may still be shown: always from the main
    // thread (0), and from the picture thread only while that playback is the current one,
    // so a frame finished after a pause or a seek never lands after the seek's.
    bool current(unsigned epoch) const { return epoch == 0 || (playing_ && epoch_ == epoch); }

    // Render one frame of the video graph and write it as a JPEG.
    bool picture(int frame_index, unsigned epoch) {
        std::lock_guard<std::mutex> l(video_.m);
        if (!video_.g || !current(epoch)) return false;
        Graph &g = *video_.g;
        g.producer->seek(frame_index);
        std::unique_ptr<Mlt::Frame> frame(g.producer->get_frame());
        if (!frame || !frame->is_valid()) {
            emit("err no frame at " + std::to_string(frame_index));
            return false;
        }
        frame->set("consumer.rescale", "bilinear");
        frame->set("consumer.deinterlace", 1);
        mlt_image_format fmt = mlt_image_rgb;
        int w = g.width, h = g.height;
        uint8_t *img = frame->get_image(fmt, w, h);
        unsigned char *out = nullptr;
        unsigned long size = 0;
        // 4:4:4 keeps text and thin overlay strokes crisp; localhost bandwidth is not a concern.
        bool ok = img && w > 0 && h > 0 && fmt == mlt_image_rgb &&
                  tjCompress2(jpeg_, img, w, 0, h, TJPF_RGB, &out, &size, TJSAMP_444, 88, TJFLAG_FASTDCT) == 0;
        if (!ok) {
            tjFree(out);
            emit("err no image at " + std::to_string(frame_index));
            return false;
        }
        {
            std::lock_guard<std::mutex> o(out_);
            if (current(epoch)) {
                std::string header = "frame " + std::to_string(frame_index) + " " + std::to_string(size) + "\n";
                std::fwrite(header.data(), 1, header.size(), stdout);
                std::fwrite(out, 1, size, stdout);
                std::fflush(stdout);
            }
        }
        tjFree(out);
        return true;
    }

    void emit(const std::string &line) {
        std::lock_guard<std::mutex> o(out_);
        std::fwrite(line.data(), 1, line.size(), stdout);
        std::fputc('\n', stdout);
        std::fflush(stdout);
    }

    Slot video_, sound_;
    // Only `picture` uses the compressor, under the video lock.
    tjhandle jpeg_ = nullptr;
    AudioOut audio_;
    std::mutex out_; // stdout: lines and frames are written whole

    std::mutex qm_;
    std::condition_variable qcv_;
    std::deque<Command> queue_;
    std::mutex wm_; // what the workers wait on: `playing_` and `quit_`
    std::condition_variable wcv_;

    // Written by the main thread; the two workers read them.
    std::atomic<bool> quit_{false};
    std::atomic<bool> playing_{false};
    std::atomic<unsigned> epoch_{0}; // bumped by every play and pause
    std::atomic<int> pos_{0};        // the paused position, and where playback starts
    std::atomic<int> shown_{0};      // the last frame the picture thread showed
    std::atomic<int> video_len_{0}, sound_len_{0}, length_{0};
    std::atomic<double> fps_{30.0};
    static constexpr int rate_ = AudioOut::rate;
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
