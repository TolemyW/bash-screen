#!/usr/bin/env bash
# End-to-end check of the CLI against this machine as the host ("local"):
# install, two separate sessions, JSON output, stop. Needs a Debian or Ubuntu
# machine with the packages the README lists.
# Usage: tests/local-e2e.sh <path to the bash-screen binary>
set -euo pipefail

bs="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
work="$(mktemp -d)"
export BASH_SCREEN_CONFIG="$work/config.toml"
export BASH_SCREEN_NO_PREVIEW=1
dir="$HOME/bash-screen-e2e"
rm -rf "$dir"

cleanup() {
  "$bs" stop >/dev/null 2>&1 || true
  "$bs" --session second stop >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

echo "== init"
"$bs" init local --remote-dir "~/bash-screen-e2e"

echo "== a second session on the same host"
"$bs" --session second start
"$bs" sessions | tee "$work/sessions.txt"
grep -q '^main  running' "$work/sessions.txt"
grep -q '^second  running' "$work/sessions.txt"
first="$("$bs" --json status | python3 -c 'import json,sys; print(json.load(sys.stdin)["display"])')"
second="$("$bs" --json --session second status | python3 -c 'import json,sys; print(json.load(sys.stdin)["display"])')"
echo "displays: $first $second"
[ "$first" != "$second" ]

echo "== screenshot of each"
"$bs" look --scale 0.25 -o "$work/one.png"
BASH_SCREEN_SESSION=second "$bs" look --scale 0.25 -o "$work/two.png"
test -s "$work/one.png" && test -s "$work/two.png"

echo "== stopping one leaves the other"
"$bs" --session second stop
"$bs" sessions | tee "$work/sessions.txt"
grep -q '^main  running' "$work/sessions.txt"
grep -q '^second  stopped' "$work/sessions.txt"
"$bs" stop
echo "local host works"
