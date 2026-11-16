#!/bin/bash
# Shows the tray icon in stalonetray under Xvfb, clicks it, and screenshots
# the menu to /work/target/tray.png.
set -u
cd /work
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq python3 xvfb dbus-x11 libgtk-3-dev libayatana-appindicator3-1 pkg-config \
    trayer xdotool imagemagick fonts-dejavu-core >/dev/null 2>&1
cargo build -q -p farplug -p farplug-gui || exit 1
BIN=$CARGO_TARGET_DIR/debug
T=$(mktemp -d)
export FARPLUG_AGENT_DIR=$T/agent
python3 tests/fake_device.py 3241 zigbee >/dev/null 2>&1 &
$BIN/farplug agent >$T/agent.log 2>&1 &
sleep 1
$BIN/farplug --server 127.0.0.1:3241 attach zigbee ghost >/dev/null
Xvfb :99 -screen 0 640x400x24 >/dev/null 2>&1 &
sleep 1
export DISPLAY=:99
trayer --edge top --align left --widthtype pixel --width 48 --heighttype pixel --height 36 --tint 0x303030 --transparent false >/dev/null 2>&1 &
sleep 1
dbus-run-session -- sh -c "
  $BIN/farplug-gui --tray >$T/tray.log 2>&1 &
  sleep 6
  xdotool mousemove 18 18 sleep 0.3 mousedown 1 sleep 0.2 mouseup 1 2>/dev/null; sleep 2
  import -window root /work/target/tray.png
"
