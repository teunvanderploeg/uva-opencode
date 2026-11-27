#!/bin/sh
# Starts the virtual screen (for the one-time login) and the bridge. See README.md.
set -eu
: "${UVA_BRIDGE_KEY:?UVA_BRIDGE_KEY is required}"

if [ "${UVA_VNC:-}" = "1" ]; then
  : "${VNC_PASSWORD:?VNC_PASSWORD is required when UVA_VNC=1}"

  # /tmp may be an empty directory reused across a container restart: clear the display
  # lock and socket an earlier Xvnc may have left behind.
  rm -f /tmp/.X99-lock /tmp/.X11-unix/X99

  # One process is the X display and its VNC port (TigerVNC Xvnc). The earlier Xvfb +
  # x11vnc pair left x11vnc at 100% CPU and unresponsive to every client, so x11vnc is
  # no longer used. The VNC password file is private (umask 077) and lives in /tmp,
  # not on the data volume.
  umask 077
  printf '%s\n' "$VNC_PASSWORD" | vncpasswd -f >/tmp/vncpasswd
  Xvnc :99 -geometry 1280x800 -depth 24 -nolisten tcp -rfbport 5900 -localhost \
    -SecurityTypes VncAuth -PasswordFile /tmp/vncpasswd >/tmp/xvnc.log 2>&1 &
  xvnc_pid=$!

  # websockify needs the display socket; wait for it instead of sleeping a fixed time.
  i=0
  until [ -S /tmp/.X11-unix/X99 ] || [ "$i" -ge 50 ] || ! kill -0 "$xvnc_pid" 2>/dev/null; do
    sleep 0.2; i=$((i + 1))
  done
  [ -S /tmp/.X11-unix/X99 ] || { echo "Xvnc did not start; see /tmp/xvnc.log" >&2; exit 1; }

  websockify --web /usr/share/novnc 6080 localhost:5900 >/tmp/websockify.log 2>&1 &
fi

# Xvnc and websockify are not supervised: if one dies, only the noVNC login view stops,
# and the bridge keeps serving.
exec python /app/bridge.py
