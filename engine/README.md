# kuma-preview — real-time preview engine

A ~350-line C++ sidecar that links `libmlt++` and pulls composited frames from the
compiled timeline (`.kuma/preview.mlt`, at the preview scale the UI picks, ½ by default)
so the editor can scrub and play the *same graph* that `melt` renders. It is the only
component that touches MLT in-process; everything else in kuma is Rust talking to it
over a pipe.

```
cmake -S engine -B engine/build && cmake --build engine/build -j
```

Dependencies (Debian/Ubuntu): `libmlt++-dev libmlt-dev libsdl2-dev libturbojpeg0-dev cmake`.

## Protocol (stdin → engine, engine → stdout)

```
→ load <path.mlt>       replace the timeline (keeps position)
→ seek <frame>          show that frame (pauses)
→ play | pause          playback paced by the audio device
→ quit

← ok <what> | err <message>
← pos <frame>                                  position after a pause, or a frame whose JPEG was skipped
← frame <frame> <bytes>\n<bytes of JPEG>      one composited frame, JPEG, profile size
```

Audio goes to the default SDL2 device. The pull loop decodes the next frame only while
the audio ring holds less than about three frames of sound, so playback runs at the
device's rate and the picture leads the sound by at most that much; a `load` that fails
leaves the current timeline playing. When no device exists (`SDL_AUDIODRIVER=dummy` or
headless CI) a wall clock stands in.

MLT's Qt module (qtblend, qtcrop) needs an X display, so the daemon starts the engine
under `xvfb-run -a` when `DISPLAY` is unset.
