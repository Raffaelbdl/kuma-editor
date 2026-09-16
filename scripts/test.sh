#!/usr/bin/env bash
# Run every suite in the project and report what happened in all of them.
#
# Three, because three languages: the Rust model, compiler and daemon; the desktop
# shell's own decisions; and the editor, which needs a real browser to say anything about
# compositing. They are run whatever the others do — a failure in one is not a reason to
# leave you guessing about the rest — and the summary distinguishes a suite that failed
# from one that skipped, because several of them skip on purpose when a tool is absent
# (`melt`, `ffmpeg`, the preview engine, a browser) and a silent skip reads like a pass.

set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT

bold=""; dim=""; red=""; green=""; yellow=""; off=""
if [ -t 1 ]; then bold=$'\033[1m'; dim=$'\033[2m'; red=$'\033[31m'; green=$'\033[32m'; yellow=$'\033[33m'; off=$'\033[0m'; fi
step() { printf '\n%s==> %s%s\n' "$bold" "$1" "$off"; }

names=(); states=(); counts=()
run() { # run <name> <logfile> <command...>
  local name=$1 log=$2; shift 2
  step "$name"
  if "$@" 2>&1 | tee "$log"; then state=ok; else state=FAILED; fi
  local count; count=$(summarize "$log")
  # A suite that ran nothing is not a suite that passed, and the difference is the whole
  # reason several of these skip rather than fail when a tool is missing.
  if [ "$state" = ok ] && [[ "$count" == 0\ passed* ]]; then state=skipped; fi
  names+=("$name"); states+=("$state"); counts+=("$count")
}

# Pull the totals out of whichever runner produced the log.
summarize() {
  local log=$1
  python3 - "$log" <<'PY'
import re, sys
text = open(sys.argv[1], errors="replace").read()
passed = failed = skipped = 0
for p, f, i in re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", text):
    passed += int(p); failed += int(f); skipped += int(i)
for label, n in re.findall(r"^# (pass|fail|skipped) (\d+)$", text, re.M):
    n = int(n)
    if label == "pass": passed += n
    elif label == "fail": failed += n
    else: skipped += n
# Suites that stood down rather than ran: worth saying so, not worth counting as passes.
skips = sorted(set(re.findall(r"skipping: ([^\n']+)", text) + re.findall(r"^(.*); skipping$", text, re.M)))
out = f"{passed} passed"
if failed: out += f", {failed} failed"
if skipped: out += f", {skipped} skipped"
if skips: out += " — " + "; ".join(s.strip() for s in skips[:3])
print(out)
PY
}

run "Rust: model, compiler, daemon"   "$out/rust.log"  cargo test
run "Desktop shell (app/)"            "$out/app.log"   npm --prefix app test
run "Editor UI (ui/)"                 "$out/ui.log"    npm --prefix ui test

step "Summary"
failed=0
for i in "${!names[@]}"; do
  case "${states[$i]}" in
    ok) colour=$green ;;
    skipped) colour=$yellow ;;
    *) colour=$red; failed=1 ;;
  esac
  printf '  %s%-8s%s %-32s %s%s%s\n' "$colour" "${states[$i]}" "$off" "${names[$i]}" "$dim" "${counts[$i]}" "$off"
done

if [ "$failed" = 0 ]; then
  printf '\n%sNothing failed.%s A suite marked skipped ran nothing — the note beside it says why.\n' "$green" "$off"
else
  printf '\n%sSomething failed.%s Scroll up: every suite ran, so more than one may be at fault.\n' "$red" "$off"
fi
printf '%sOne at a time:%s cargo test · npm run app:test · npm run ui:test\n' "$dim" "$off"
exit "$failed"
