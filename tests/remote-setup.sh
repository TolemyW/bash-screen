#!/usr/bin/env bash
# Check the remote side on this machine, as an unprivileged user would get it:
# install the tools into a fresh directory, start a session, take a
# screenshot, stop. Run from the repository root.
set -euo pipefail

dir="${1:-$HOME/bash-screen-test}"
rm -rf "$dir"
mkdir -p "$dir"
cp remote/bs_remote.py remote/session.sh remote/setup.sh "$dir/"
chmod 700 "$dir"/*

bash "$dir/setup.sh"

call() { # request JSON -> response JSON; fails unless the response says ok
  local reply
  reply="$(printf '%s' "$1" | python3 "$dir/bs_remote.py")"
  printf '%s
' "$reply" | cut -c1-300
  printf '%s' "$reply" | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("ok") else 1)'
}

trap 'printf "%s" "{\"cmd\":\"stop\"}" | python3 "$dir/bs_remote.py" >/dev/null 2>&1 || true; tail -n 30 "$dir"/run/*.log 2>/dev/null || true' EXIT

echo "== start"
call '{"cmd":"start","geometry":"1280x800"}'
echo "== status"
call '{"cmd":"status"}'
echo "== screenshot"
reply="$(printf '%s' '{"cmd":"look","scale":0.25}' | python3 "$dir/bs_remote.py")"
printf '%s' "$reply" | python3 -c '
import base64, json, sys
reply = json.load(sys.stdin)
assert reply.get("ok"), reply
picture = base64.b64decode(reply["image_webp"])
assert picture[:4] == b"RIFF" and len(picture) > 100, len(picture)
print("screenshot ok,", len(picture), "bytes")
'
echo "== observe"
call '{"cmd":"observe","geometry":"1280x800"}'
echo "== stop"
call '{"cmd":"stop"}'
trap - EXIT
echo "remote side works"
