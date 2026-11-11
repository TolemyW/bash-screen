#!/usr/bin/env bash
# bash-screen: install the remote tools into this directory, without root.
#
#   root/   Xvnc and openbox, unpacked from the distribution's packages
#   ad/     agent-desktop, from npm
#
# Safe to run again: finished steps are skipped. Things that only an
# administrator can install are reported, with the command to run.
set -u

BASE="$(cd "$(dirname "$0")" && pwd)"
AD_VERSION="1.2.0"
# The programs we need. Their library dependencies are worked out below.
PROGRAMS="tigervnc-standalone-server openbox"

problems=()
say() { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

# --- what must already be on the host ----------------------------------------
step "Checking the host"
needs_admin=()
check() { # command, package
  if command -v "$1" >/dev/null 2>&1; then say "  ok       $1"; else say "  MISSING  $1"; needs_admin+=("$2"); fi
}
check apt-get apt
check dpkg dpkg
check python3 python3
check npm npm
check dbus-daemon dbus
check xauth xauth
check mcookie util-linux
check xkbcomp x11-xkb-utils
if [ -x /usr/libexec/at-spi-bus-launcher ]; then say "  ok       at-spi bus"; else say "  MISSING  at-spi bus"; needs_admin+=(at-spi2-core); fi
if python3 -c 'import PIL' >/dev/null 2>&1; then say "  ok       python Pillow"; else say "  MISSING  python Pillow"; needs_admin+=(python3-pil); fi

if ! command -v apt-get >/dev/null 2>&1 || ! command -v dpkg >/dev/null 2>&1; then
  say ""
  say "This host is not Debian or Ubuntu; setup cannot unpack packages here."
  exit 1
fi

# --- Xvnc and openbox ----------------------------------------------------------
step "Unpacking Xvnc and openbox into $BASE/root"
mkdir -p "$BASE/pkg" "$BASE/root"

installed() { dpkg -s "$1" >/dev/null 2>&1; }

# Library packages that `package` depends on directly and the host lacks.
missing_libraries() { # package...
  for package in "$@"; do
    apt-cache depends "$package" 2>/dev/null | awk '/^ +(Pre)?Depends:/ {print $2}' | tr -d '<>'
  done | grep -E '^lib' | sort -u \
    | while read -r dependency; do installed "$dependency" || printf '%s
' "$dependency"; done
}

failed_packages=""
unpack() { # package
  local package="$1" deb
  if [ -f "$BASE/pkg/$package.unpacked" ]; then say "  ok       $package"; return 0; fi
  deb="$(ls "$BASE/pkg/${package}"_*.deb 2>/dev/null | head -1)"
  if [ -z "$deb" ]; then
    say "  download $package"
    (cd "$BASE/pkg" && apt-get download "$package" >/dev/null 2>&1)
    deb="$(ls "$BASE/pkg/${package}"_*.deb 2>/dev/null | head -1)"
  fi
  if [ -z "$deb" ] || ! dpkg -x "$deb" "$BASE/root" 2>/dev/null; then
    say "  skipped  $package (could not download or unpack)"
    failed_packages="$failed_packages $package"
    return 1
  fi
  touch "$BASE/pkg/$package.unpacked"
  say "  unpacked $package"
}

# Names of the shared libraries the two programs still cannot find.
unresolved_libraries() {
  local libdir program
  libdir="$(ls -d "$BASE"/root/usr/lib/*-linux-gnu 2>/dev/null | head -1)"
  for program in Xtigervnc openbox; do
    [ -x "$BASE/root/usr/bin/$program" ] || { printf '%s
' "$program"; continue; }
    LD_LIBRARY_PATH="$libdir" ldd "$BASE/root/usr/bin/$program" 2>/dev/null | awk '/not found/ {print $1}'
  done | sort -u | tr '
' ' '
}

for package in $PROGRAMS; do
  unpack "$package"
done
# Add libraries one dependency level at a time, and stop as soon as both
# programs can load everything they need. Whether they run is what counts,
# not whether every package in the dependency tree could be fetched.
level="$PROGRAMS"
for _ in 1 2 3 4 5 6 7 8; do
  [ -z "$(unresolved_libraries)" ] && break
  next="$(missing_libraries $level)"
  [ -z "$next" ] && break
  for package in $next; do
    unpack "$package"
  done
  level="$next"
done

# The X server cannot start without keyboard maps; a host with no desktop
# installed has none.
if [ ! -d /usr/share/X11/xkb ]; then
  unpack xkb-data || problems+=("keyboard maps (package xkb-data) could not be fetched")
fi

unresolved="$(unresolved_libraries)"
if [ -n "$unresolved" ]; then
  problems+=("Xvnc or openbox cannot find: $unresolved")
  [ -n "$failed_packages" ] && problems+=("these packages could not be fetched:$failed_packages")
else
  say "  ok       Xvnc and openbox run"
fi

# --- agent-desktop -------------------------------------------------------------
step "Installing agent-desktop $AD_VERSION into $BASE/ad"
AD="$BASE/ad/node_modules/.bin/agent-desktop"
have="$("$AD" version 2>/dev/null | sed -n 's/.*"version":"\([0-9.]*\)"}}.*/\1/p')"
if [ "$have" = "$AD_VERSION" ]; then
  say "  ok       agent-desktop $have"
elif command -v npm >/dev/null 2>&1; then
  mkdir -p "$BASE/ad"
  if (cd "$BASE/ad" && npm install --no-audit --no-fund "agent-desktop@$AD_VERSION" >/dev/null 2>&1) && "$AD" version >/dev/null 2>&1; then
    say "  installed agent-desktop $AD_VERSION"
  else
    say "  FAILED   agent-desktop"
    problems+=("npm could not install agent-desktop@$AD_VERSION")
  fi
else
  problems+=("npm is needed to install agent-desktop")
fi

# --- summary -------------------------------------------------------------------
step "Result"
if [ "${#needs_admin[@]}" -gt 0 ]; then
  say "An administrator needs to install these packages:"
  say "  sudo apt-get install ${needs_admin[*]}"
fi
for problem in ${problems[@]+"${problems[@]}"}; do
  say "Problem: $problem"
done
if [ "${#needs_admin[@]}" -eq 0 ] && [ "${#problems[@]}" -eq 0 ]; then
  say "Ready. Start a session with: bash-screen start"
  exit 0
fi
exit 1
