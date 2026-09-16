#!/usr/bin/env bash
# Rebuild every artifact kuma runs from, in dependency order, then run `kuma doctor`.
#
# Three of them are not in git — the `kuma` binary, `ui/dist` and `app/dist` — so a pull
# updates the sources and leaves them behind, and a stale one fails far from its cause.
#
# The preview engine is the one optional piece (it needs system libraries, and without it
# the preview falls back to single frames), so it may fail without failing the rebuild.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
root=$(pwd)

bold=""; dim=""; red=""; off=""
if [ -t 1 ]; then bold=$'\033[1m'; dim=$'\033[2m'; red=$'\033[31m'; off=$'\033[0m'; fi
step() { printf '\n%s==> %s%s\n' "$bold" "$1" "$off"; }
note() { printf '%s    %s%s\n' "$dim" "$1" "$off"; }

engine_note="built"

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

step "kuma doctor"
"$root/target/release/kuma" doctor || true

printf '\n%sRebuilt.%s binary, ui/dist, app/dist. Preview engine: %s\n' "$bold" "$off" "$engine_note"
printf 'Start the app with %snpm run app%s.\n' "$bold" "$off"
