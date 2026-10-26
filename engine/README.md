# kuma-preview — real-time preview engine

A ~450-line C++ sidecar that links `libmlt++` and pulls composited frames from the
compiled timeline so the editor can scrub and play the *same graph* that `melt` renders.
The daemon compiles the timeline twice, at the preview scale the UI picks (½ by default):
`.kuma/preview-video.mlt` holds only the picture and `.kuma/preview-audio.mlt` only the
sound (`kuma_mlt::Streams`), and each is pulled by its own thread. It is the only
component that touches MLT in-process; everything else in kuma is Rust talking to it over
a pipe.

```
cmake -S engine -B engine/build && cmake --build engine/build -j
```

Dependencies (Debian/Ubuntu): `libmlt++-dev libmlt-dev libsdl2-dev libturbojpeg0-dev cmake`.

## Protocol (stdin → engine, engine → stdout)

```
→ load-video <path.mlt>  replace the picture's graph (keeps position and playback)
→ load-audio <path.mlt>  replace the sound's graph (keeps position and playback)
→ seek <frame>           show that frame (pauses); not shown if a later seek is already queued
→ play | pause           playback paced by the audio device
→ quit

← ok <what> | err <message>
← ok play                 playback started
← ok waiting <why>        a play held until the first timeline is open, then `ok play`
← err no timeline loaded  a play refused: nothing to play, nothing loading
← frame <frame> <bytes>\n<bytes of JPEG>      one composited frame, JPEG, profile size
```

Audio goes to the default SDL2 device, opened at 48 kHz stereo (the engine mixes at that
rate whatever the project's `sample_rate`). The sound is the clock. Its thread keeps
about three frames of sound in the ring, from the audio graph, or silence when there is
none, so the clock always runs. The picture's thread shows the frame the samples played so
far have reached: ahead of it, it waits; behind it, it skips to it, so a slow picture drops
frames and never slows the sound. A load swaps its graph between two frames without
pausing, and one that fails leaves the current graph playing. Graphs open on a loader
thread — every producer opens its file, which is seconds on a long timeline — so a seek
or play sent just after a load is answered at once on the graph already held; loads of
one graph queued behind each other open only the newest. A frame the picture's
thread finishes after a pause or seek is dropped, so the seek's frame is always the last.
When no device can be opened (no sound server in CI) the engine reopens with SDL's
`dummy` driver, which still paces the callback in real time. The daemon forwards `frame`
lines, `ok end`, what became of a play and the engine's errors to every preview socket.
`crates/kuma-daemon/tests/engine.rs` drives the protocol with real media.

MLT's Qt module (qtblend, qtcrop) needs an X display, so the daemon starts the engine
under `xvfb-run -a` when `DISPLAY` is unset.
