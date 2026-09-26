# Rendering and reviewing

## What things cost

- **Bakes.** A changed overlay bakes its whole clip before a sample or render shows it.
  Several bake side by side, but a change to `_vendor/`, to a font file, or to a component
  used everywhere re-bakes everything: ~400 overlays took about 20 minutes. Unchanged
  overlays come from the cache.
- **Sampling the master bakes every changed overlay in it**, whatever time you ask for.
  While iterating, sample the overlay entry or its chapter sequence over a short range.
- **Encoding.**
  - `draft`: x264 ultrafast, CRF 26. For review.
  - `youtube-1080p`: x264 slow, CRF 18. The final.
  - `youtube-nvenc`: NVENC p6, CQ 21, when `list_presets` says it is `available`. Much
    faster than x264 slow, for about a third more bytes at matching quality.
  - `threads`, `quality` and `speed` override the preset for one render.
- Renders queue one at a time. Editing the project while one encodes is safe: it compiled
  its graph when it started. Overwriting a media file it reads is not.

## Versions and environment

- The MCP server is a separate process from the daemon. After kuma is rebuilt, the daemon
  may offer options (render `speed`, new presets) that the MCP's tool schema does not have
  yet. Say so and ask for the MCP server to be restarted. Don't work around it silently.
- A daemon whose library lacks fonts reports `font_missing` in `check_overlay`, and the
  Latin faces then render as fallbacks. Run `kuma library` once, or start it with
  `KUMA_LIBRARY=<kuma-editor>/library`.

## The checklist

Before the final render:
```
validate_project                                  # no issues
python3 assets/diff.py … > patches/diff.json      # 0 ops
python3 <skill>/scripts/fontcheck.py              # nothing reported
python3 <skill>/scripts/mixsim.py --lufs          # ≈ -16…-17 LUFS, peak ≤ -1.5 dBFS
python3 <skill>/scripts/reading_audit.py          # no scene badly short, few long holds
```

After it:
```
ffprobe -v error -show_entries format=duration -of csv=p=0 out.mp4
ffmpeg -i out.mp4 -vn -af ebur128=peak=true -f null - 2>&1 | tail -12        # integrated, true peak
for t in 0 2 4 6; do ffmpeg -ss $t -t 2 -i out.mp4 -vn -af volumedetect -f null - 2>&1 | grep mean_volume; done
python3 <skill>/scripts/contact_sheet.py out.mp4 sheet.png 3.5 41.7 42.3 …   # every cut ±0.3 s, each key scene
```

Then **look at the sheet**, and keep the previous render under another name before
rendering to the same path again (`…-v1.mp4`), so the two can be compared.
