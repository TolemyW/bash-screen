#!/bin/sh
# Copies the bash-screen mod into the Herdr app's folder. The app picks it up at once.
# Run it again after an update. It needs the bash-screen CLI on PATH to show anything.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
home=${HERDR_APP_HOME:-$HOME/.config/herdr-app}

if [ ! -d "$home/mods" ]; then
  echo "No Herdr app folder at $home (start the app once, or set HERDR_APP_HOME)." >&2
  exit 1
fi

for name in bash-screen bash-screen-frame; do
  mkdir -p "$home/mods/$name"
  cp "$here/mods/$name/main.js" "$here/mods/$name/mod.json" "$home/mods/$name/"
done

command -v bash-screen >/dev/null 2>&1 || echo "Note: the bash-screen CLI is not on PATH yet; the pane stays empty until it is." >&2

echo "Installed. Open it beside the current pane with:"
echo "  herdr-app do pane.split '{\"direction\":\"right\",\"mod\":\"bash-screen\"}'"
