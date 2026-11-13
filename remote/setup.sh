#!/usr/bin/env bash
# bash-screen: install the remote tools into this directory, without root.
#
#   root/   Xvnc and openbox, unpacked from the distribution's packages
#   ad/     agent-desktop, from its npm package (npm itself is not needed)
#
# Safe to run again: finished steps are skipped. Things that only an
# administrator can install are reported, with the command to run.
set -u

BASE="$(cd "$(dirname "$0")" && pwd)"
AD_VERSION="1.2.0"
AD_URL="https://registry.npmjs.org/agent-desktop/-/agent-desktop-$AD_VERSION.tgz"
AD_SHA256="9e4b1a69d9d6be487873a1df8db61234a8b7a9c79aeced4e403a8126c0e0bcc0"
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
  # Older keymap compilers (Ubuntu 22.04) only read maps from the system
  # directory, so an unpacked copy does not help them. Try it.
  if command -v xkbcomp >/dev/null 2>&1; then
    probe="$(mktemp -d)"
    keymap='xkb_keymap { xkb_keycodes { include "evdev+aliases(qwerty)" }; xkb_types { include "complete" }; xkb_compat { include "complete" }; xkb_symbols { include "pc+us" }; };'
    if ! printf '%s' "$keymap" | xkbcomp -w 0 "-R$BASE/root/usr/share/X11/xkb" -xkm - "$probe/probe.xkm" >/dev/null 2>&1; then
      say "  MISSING  keyboard maps (this system cannot use an unpacked copy)"
      needs_admin+=(xkb-data)
    fi
    rm -rf "$probe"
  fi
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
else
  # The package carries a ready-made program for each system; take ours out
  # of it. This is all `npm install` would do, without needing Node.
  mkdir -p "$BASE/ad"
  if AD_URL="$AD_URL" AD_SHA256="$AD_SHA256" python3 - "$BASE/ad" <<'PYTHON' && "$AD" version >/dev/null 2>&1
import hashlib, io, os, platform, sys, tarfile, urllib.request

dest = os.path.join(sys.argv[1], "node_modules", "agent-desktop", "bin")
arch = {"x86_64": "x64", "aarch64": "arm64"}.get(platform.machine())
if arch is None:
    sys.exit(f"no agent-desktop program for {platform.machine()}")
data = b""
for attempt in range(3):  # a slow link can drop a 15 MB download part-way
    try:
        data = urllib.request.urlopen(os.environ["AD_URL"], timeout=300).read()
    except Exception as error:
        print(f"  download failed ({error.__class__.__name__}), attempt {attempt + 1} of 3")
        continue
    if hashlib.sha256(data).hexdigest() == os.environ["AD_SHA256"]:
        break
else:
    sys.exit("the agent-desktop package could not be downloaded intact")
package = tarfile.open(fileobj=io.BytesIO(data))
os.makedirs(dest, exist_ok=True)
wanted = {
    f"package/bin/agent-desktop-linux-{arch}": "agent-desktop",
    f"package/bin/agent-desktop-linux-clipboard-owner-linux-{arch}": "agent-desktop-linux-clipboard-owner",
}
for member, name in wanted.items():
    path = os.path.join(dest, name)
    with open(path + ".new", "wb") as out:
        out.write(package.extractfile(member).read())
    os.chmod(path + ".new", 0o755)
    os.replace(path + ".new", path)
links = os.path.join(sys.argv[1], "node_modules", ".bin")
os.makedirs(links, exist_ok=True)
link = os.path.join(links, "agent-desktop")
if os.path.lexists(link):
    os.remove(link)
os.symlink("../agent-desktop/bin/agent-desktop", link)
PYTHON
  then
    say "  installed agent-desktop $AD_VERSION"
  else
    say "  FAILED   agent-desktop"
    problems+=("agent-desktop $AD_VERSION could not be installed; it needs glibc 2.35 or newer (this host has $(ldd --version 2>/dev/null | sed -n '1s/.* //p')) and access to registry.npmjs.org")
  fi
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
