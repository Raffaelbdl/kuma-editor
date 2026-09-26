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

- The MCP server is a separate process from the daemon. After kuma is rebuilt, every tool
  result starts with a note saying the MCP server is the old build, whose tools lack what
  the rebuild added. Say so and ask for the MCP server to be restarted. Don't work around
  it silently.
- A font an entry names that the daemon cannot find renders as a fallback. `validate_project`
  and a render's `warnings` list each one; `health`'s `blueprint_dirs` say which libraries
  the daemon reads. `kuma library` installs the starter fonts into the user library.

## The checklist

Before the final render:
```
validate_project                                  # no issues, no font warnings
python3 assets/diff.py … > patches/diff.json      # 0 ops
measure_audio                                     # ≈ -16…-17 LUFS, peak ≤ -1.5 dBTP, only the silences you meant
sample_frames times=[…] tile=true                 # every cut ±0.3 s, each key scene, one sheet
python3 <skill>/scripts/reading_audit.py          # no scene badly short, few long holds
```

After it:
```
render result `warnings`                          # none
ffprobe -v error -show_entries format=duration -of csv=p=0 out.mp4
ffmpeg -i out.mp4 -vn -af ebur128=peak=true -f null - 2>&1 | tail -12        # integrated, true peak
python3 <skill>/scripts/contact_sheet.py out.mp4 sheet.png 3.5 41.7 42.3 …   # the same moments, from the file
```

Then **look at the sheet**, and keep the previous render under another name before
rendering to the same path again (`…-v1.mp4`), so the two can be compared.
