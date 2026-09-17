#!/usr/bin/env bash
# Rebuild every artifact kuma runs from, in dependency order, then run `kuma doctor`.
#
# Three of them are not in git — the `kuma` binary, `ui/dist` and `app/dist` — so a pull
# updates the sources and leaves them behind, and a stale one fails far from its cause.
#
# Two pieces are optional and may fail without failing the rebuild: the preview engine
# (it needs system libraries; without it the preview falls back to single frames) and
# whisper.cpp for subtitles (a clone, a build and a model download; without it Transcribe
# says what is missing).

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
root=$(pwd)

bold=""; dim=""; red=""; off=""
if [ -t 1 ]; then bold=$'\033[1m'; dim=$'\033[2m'; red=$'\033[31m'; off=$'\033[0m'; fi
step() { printf '\n%s==> %s%s\n' "$bold" "$1" "$off"; }
note() { printf '%s    %s%s\n' "$dim" "$1" "$off"; }

engine_note="built"
whisper_note="built"

step "node modules (hyperframes, gsap)"
npm install --no-audit --no-fund

step "kuma binary (cargo build --release)"
cargo build --release

step "editor UI (ui/dist — served by the daemon)"
npm --prefix ui install --no-audit --no-fund
npm --prefix ui run build

step "desktop shell (app/dist)"
npm --prefix app install --no-audit --no-fund
npm --prefix app run build

step "preview engine (optional)"
if ! command -v cmake >/dev/null 2>&1; then
  engine_note="skipped — cmake is not installed"
  note "$engine_note"
elif cmake -S engine -B engine/build >/dev/null && cmake --build engine/build -j; then
  note "engine/build/kuma-preview"
else
  engine_note="FAILED — see above; playback falls back to single frames"
  printf '%s    %s%s\n' "$red" "$engine_note" "$off"
fi

step "whisper.cpp for subtitles (optional)"
# Cloned next to the other build trees and kept out of git; the daemon and `kuma doctor`
# look for its whisper-cli here before PATH. The model goes to the user library, where
# `kuma library` puts templates, so it is shared across checkouts. WHISPER_MODEL picks
# the size (tiny, base, small, medium, large-v3; add `.en` for English-only).
models="${XDG_DATA_HOME:-$HOME/.local/share}/kuma/models"
model="${WHISPER_MODEL:-base}"
if ! command -v cmake >/dev/null 2>&1; then
  whisper_note="skipped — cmake is not installed"
  note "$whisper_note"
else
  [ -d whisper.cpp ] || git clone --depth 1 https://github.com/ggml-org/whisper.cpp whisper.cpp || true
  if [ -d whisper.cpp ] && cmake -S whisper.cpp -B whisper.cpp/build -DCMAKE_BUILD_TYPE=Release -DWHISPER_BUILD_TESTS=OFF >/dev/null && cmake --build whisper.cpp/build -j --target whisper-cli; then
    note "whisper.cpp/build/bin/whisper-cli"
    if ls "$models"/ggml-*.bin >/dev/null 2>&1; then
      note "model: $(ls "$models"/ggml-*.bin | head -1)"
    else
      mkdir -p "$models"
      if curl -fL --retry 3 -o "$models/ggml-$model.bin.part" "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-$model.bin"; then
        mv "$models/ggml-$model.bin.part" "$models/ggml-$model.bin"
        note "model: $models/ggml-$model.bin"
      else
        rm -f "$models/ggml-$model.bin.part"
        whisper_note="built, but the model download failed — put a ggml-*.bin in $models or set KUMA_WHISPER_MODEL"
        printf '%s    %s%s\n' "$red" "$whisper_note" "$off"
      fi
    fi
  else
    whisper_note="FAILED — see above; Transcribe will say what is missing"
    printf '%s    %s%s\n' "$red" "$whisper_note" "$off"
  fi
fi

step "kuma doctor"
"$root/target/release/kuma" doctor || true

printf '\n%sRebuilt.%s binary, ui/dist, app/dist. Preview engine: %s. whisper.cpp: %s\n' "$bold" "$off" "$engine_note" "$whisper_note"
printf 'Start the app with %snpm run app%s.\n' "$bold" "$off"
