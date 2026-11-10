#!/bin/sh
# Put the bash-screen command on PATH by linking it into ~/.local/bin.
# Run by herdr at server start and by the "link-cli" plugin action, so the
# link keeps pointing at the current installation after plugin updates.
set -eu

root="${HERDR_PLUGIN_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
binary="$root/bin/bash-screen"
target="$HOME/.local/bin/bash-screen"

if [ ! -x "$binary" ]; then
  echo "bash-screen: $binary is missing; build the plugin first" >&2
  exit 1
fi
# Never replace something that is not our own link.
if [ -e "$target" ] && [ ! -L "$target" ]; then
  echo "bash-screen: $target exists and is not a link; leaving it alone" >&2
  exit 1
fi
mkdir -p "$HOME/.local/bin"
ln -sfn "$binary" "$target"
echo "bash-screen: linked $target -> $binary"
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) echo "bash-screen: add $HOME/.local/bin to PATH to use the command" >&2 ;;
esac
