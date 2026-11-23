#!/usr/bin/env bash
# Check the remote side on this machine, as an unprivileged user would get it:
# install the tools into a fresh directory, start a session, take a
# screenshot, stop. Run from the repository root. Needs nothing beyond what
# docs/reference.md asks an administrator for: no Python.
# Usage: tests/remote-setup.sh <the bs-helper program built for this machine> [directory]
set -euo pipefail

helper="$1"
dir="${2:-$HOME/bash-screen-test}"
rm -rf "$dir"
mkdir -p "$dir"
cp remote/session.sh remote/setup.sh "$helper" "$dir/"
[ "$(basename "$helper")" = bs-helper ] || mv "$dir/$(basename "$helper")" "$dir/bs-helper"
chmod 700 "$dir"/*
# Packages downloaded by an earlier run can be reused, to save the downloads.
if [ -n "${BASH_SCREEN_TEST_PACKAGES:-}" ]; then
  mkdir -p "$dir/pkg"
  cp "$BASH_SCREEN_TEST_PACKAGES"/*.deb "$dir/pkg/"
fi

bash "$dir/setup.sh"

call() { # request JSON -> response JSON; fails unless the response says ok
  local reply
  reply="$(printf '%s' "$1" | "$dir/bs-helper")"
  printf '%s\n' "$reply" | cut -c1-300
  case "$reply" in '{"ok":true'*) ;; *) return 1 ;; esac
}

# On failure show the session's logs before stopping it removes them.
trap 'tail -n 30 "$dir"/run/*.log 2>/dev/null || true; printf "%s" "{\"cmd\":\"stop\"}" | "$dir/bs-helper" >/dev/null 2>&1 || true' EXIT

echo "== start"
call '{"cmd":"start","geometry":"1280x800"}'
echo "== status"
call '{"cmd":"status"}'
echo "== screenshot"
reply="$(printf '%s' '{"cmd":"look","scale":0.25}' | "$dir/bs-helper")"
case "$reply" in '{"ok":true'*) ;; *) echo "$reply" | cut -c1-300; exit 1 ;; esac
picture="$(printf '%s' "$reply" | sed -n 's/.*"image_webp":"\([^"]*\)".*/\1/p')"
[ "$(printf '%s' "$picture" | base64 -d | head -c 4)" = RIFF ]
echo "screenshot ok, $(printf '%s' "$picture" | base64 -d | wc -c) bytes"
echo "== observe"
call '{"cmd":"observe","geometry":"1280x800"}'
echo "== stop"
call '{"cmd":"stop"}'
trap - EXIT
echo "remote side works"
