#!/usr/bin/env bash
# bash-screen: start/stop one isolated GUI session on the remote host.
# Everything lives under $BASE; nothing touches the user's real desktop session.
#
#   session.sh start [WIDTHxHEIGHT]   start Xvnc + session bus + window manager
#   session.sh env                    print the env file path (source it to run commands)
#   session.sh status                 show processes and sockets
#   session.sh stop                   kill everything this script started
set -u

BASE="${BSCREEN_BASE:-$(cd "$(dirname "$0")" && pwd)}"
RUN="$BASE/run"
XVNC="$BASE/root/usr/bin/Xtigervnc"
# Libraries unpacked next to the programs by setup.sh.
LIBDIR="$(ls -d "$BASE"/root/usr/lib/*-linux-gnu 2>/dev/null | head -1)"
AD="$BASE/ad/node_modules/.bin/agent-desktop"
ENVFILE="$RUN/env.sh"

# Window manager: BSCREEN_WM=openbox (default, unpacked under $BASE/root) or xfwm4.
# BSCREEN_MAXIMIZE=1 makes openbox open every normal window maximized and undecorated.
start_wm() {
  local wm="${BSCREEN_WM:-openbox}"
  if [ "$wm" = "xfwm4" ]; then
    setsid nohup xfwm4 --compositor=off >"$RUN/wm.log" 2>&1 &
    echo $! > "$RUN/wm.pid"
    return
  fi
  local cfg="$RUN/config"
  mkdir -p "$cfg/openbox"
  if [ "${BSCREEN_MAXIMIZE:-0}" = "1" ]; then
    # Insert a catch-all rule into the stock config's <applications> section.
    sed 's|</applications>|<application class="*" type="normal"><decor>no</decor><maximized>yes</maximized><focus>yes</focus></application></applications>|' \
      "$BASE/root/etc/xdg/openbox/rc.xml" > "$cfg/openbox/rc.xml"
  else
    cp "$BASE/root/etc/xdg/openbox/rc.xml" "$cfg/openbox/rc.xml"
  fi
  LD_LIBRARY_PATH="$LIBDIR" \
  XDG_DATA_DIRS="$BASE/root/usr/share:/usr/local/share:/usr/share" \
    setsid nohup "$BASE/root/usr/bin/openbox" --config-file "$cfg/openbox/rc.xml" >"$RUN/wm.log" 2>&1 &
  echo $! > "$RUN/wm.pid"
}

start() {
  local geometry="${1:-1280x800}"
  if [ -f "$ENVFILE" ]; then echo "session already started; run: $0 stop" >&2; exit 1; fi
  mkdir -p "$RUN" && chmod 700 "$RUN"

  # Private X authority so only this user can connect to the display.
  export XAUTHORITY="$RUN/Xauthority"
  : > "$XAUTHORITY" && chmod 600 "$XAUTHORITY"

  # Let the server pick a free display number and report it back.
  local fdfile="$RUN/displayfd"
  : > "$fdfile"
  LD_LIBRARY_PATH="$LIBDIR" setsid nohup "$XVNC" -displayfd 3 \
      -geometry "$geometry" -depth 24 \
      -rfbunixpath "$RUN/vnc.sock" -rfbunixmode 0600 -rfbport -1 \
      -SecurityTypes None -AcceptSetDesktopSize=0 \
      -auth "$XAUTHORITY" -nolisten tcp \
      3>"$fdfile" >"$RUN/xvnc.log" 2>&1 &
  echo $! > "$RUN/xvnc.pid"

  local n=""
  for _ in $(seq 1 50); do
    n="$(tr -d '\n' < "$fdfile")"
    [ -n "$n" ] && break
    sleep 0.1
  done
  if [ -z "$n" ]; then echo "Xvnc did not report a display; see $RUN/xvnc.log" >&2; exit 1; fi
  export DISPLAY=":$n"
  xauth -f "$XAUTHORITY" add "$DISPLAY" . "$(mcookie)" 2>/dev/null

  # Private runtime dir and session bus: apps must not attach to the real desktop.
  export XDG_RUNTIME_DIR="$RUN/xdg"
  mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
  local businfo
  businfo="$(dbus-daemon --session --fork --print-address=1 --print-pid=1)"
  export DBUS_SESSION_BUS_ADDRESS="$(echo "$businfo" | sed -n 1p)"
  echo "$businfo" | sed -n 2p > "$RUN/dbus.pid"

  {
    echo "export DISPLAY='$DISPLAY'"
    echo "export XAUTHORITY='$XAUTHORITY'"
    echo "export XDG_RUNTIME_DIR='$XDG_RUNTIME_DIR'"
    echo "export DBUS_SESSION_BUS_ADDRESS='$DBUS_SESSION_BUS_ADDRESS'"
    echo "export XDG_SESSION_TYPE=x11"
    echo "unset WAYLAND_DISPLAY"
    echo "export GDK_BACKEND=x11"
    echo "export QT_ACCESSIBILITY=1"
    echo "export ACCESSIBILITY_ENABLED=1"
    echo "export AGENT_DESKTOP_HOME='$BASE/state'"
    echo "export PATH='$BASE/ad/node_modules/.bin':\"\$PATH\""
  } > "$ENVFILE"
  # shellcheck disable=SC1090
  . "$ENVFILE"

  start_wm
  sleep 1
  echo "started: DISPLAY=$DISPLAY geometry=$geometry wm=${BSCREEN_WM:-openbox} maximize=${BSCREEN_MAXIMIZE:-0} vnc=$RUN/vnc.sock"
}

status() {
  [ -f "$ENVFILE" ] || { echo "no session"; exit 1; }
  # shellcheck disable=SC1090
  . "$ENVFILE"
  echo "DISPLAY=$DISPLAY"
  for p in xvnc dbus wm; do
    pid="$(cat "$RUN/$p.pid" 2>/dev/null)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then echo "$p: running ($pid)"; else echo "$p: NOT running"; fi
  done
  ls -la "$RUN/vnc.sock" 2>&1
}

stop() {
  [ -d "$RUN" ] || { echo "no session"; exit 0; }
  if [ -f "$ENVFILE" ]; then
    # shellcheck disable=SC1090
    . "$ENVFILE"
    # Kill clients of our display first (they all carry our DISPLAY in their environment).
    for pid in $(pgrep -u "$USER"); do
      if { tr '\0' '\n' < "/proc/$pid/environ"; } 2>/dev/null | grep -qx "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"; then
        [ "$pid" != "$$" ] && kill "$pid" 2>/dev/null
      fi
    done
  fi
  for p in wm dbus xvnc; do
    pid="$(cat "$RUN/$p.pid" 2>/dev/null)"
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
  done
  sleep 0.5
  rm -rf "$RUN"
  echo "stopped"
}

case "${1:-}" in
  start) shift; start "$@" ;;
  env) echo "$ENVFILE" ;;
  status) status ;;
  stop) stop ;;
  *) echo "usage: $0 {start [WxH]|env|status|stop}" >&2; exit 2 ;;
esac
