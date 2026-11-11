#!/bin/sh
# Install step of the herdr plugin: put the bash-screen binary in bin/ and
# link it onto PATH.
#
# The binary for this version is downloaded from the GitHub release and
# checked against the release's checksums. If there is none for this system,
# or BASH_SCREEN_BUILD_FROM_SOURCE is set, it is built with cargo instead.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
repo="TolemyW/bash-screen"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' herdr-plugin.toml | head -1)"

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) os=unknown ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=arm64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) arch=unknown ;;
esac
asset="bash-screen-$os-$arch"
base="https://github.com/$repo/releases/download/v$version"
fetch=curl

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

download() {
  [ "$os" != unknown ] && [ "$arch" != unknown ] || return 1
  command -v "$fetch" >/dev/null 2>&1 || return 1
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  "$fetch" -fsSL --retry 2 -o "$tmp/$asset" "$base/$asset" || return 1
  "$fetch" -fsSL --retry 2 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || return 1
  expected="$(awk -v name="$asset" '$2 == name {print $1}' "$tmp/SHA256SUMS")"
  if [ -z "$expected" ] || [ "$expected" != "$(sha256 "$tmp/$asset")" ]; then
    echo "bash-screen: checksum of $asset does not match; not using it" >&2
    return 1
  fi
  chmod +x "$tmp/$asset"
  "$tmp/$asset" --version >/dev/null 2>&1 || return 1
  mkdir -p bin
  rm -f bin/bash-screen
  mv "$tmp/$asset" bin/bash-screen
}

build() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "bash-screen: no prebuilt binary for $os-$arch at v$version, and cargo is not installed." >&2
    echo "bash-screen: install Rust from https://rustup.rs and run the install again." >&2
    return 1
  fi
  cargo build --release
  mkdir -p bin
  rm -f bin/bash-screen
  cp target/release/bash-screen bin/bash-screen
}

if [ -z "${BASH_SCREEN_BUILD_FROM_SOURCE:-}" ] && download; then
  echo "bash-screen: installed prebuilt $asset v$version"
else
  build
  echo "bash-screen: built v$version from source"
fi

# Not fatal: the plugin still works, the command just is not on PATH yet.
sh scripts/link-cli.sh || true
