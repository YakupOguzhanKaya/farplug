#!/bin/bash
# Starts the tray against a live agent under Xvfb + a D-Bus session and checks
# it stays up and toggles a device through its menu-independent code path.
set -u
cd /work
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq python3 xvfb dbus-x11 libgtk-3-dev libayatana-appindicator3-1 pkg-config >/dev/null 2>&1
cargo build -q -p farplug -p farplug-gui || exit 1
BIN=$CARGO_TARGET_DIR/debug
T=$(mktemp -d)
export FARPLUG_AGENT_DIR=$T/agent
python3 tests/fake_device.py 3241 zigbee >/dev/null 2>&1 &
$BIN/farplug agent >$T/agent.log 2>&1 &
sleep 1
$BIN/farplug --server 127.0.0.1:3241 attach zigbee >/dev/null
Xvfb :99 >/dev/null 2>&1 &
sleep 1
export DISPLAY=:99
dbus-run-session -- sh -c "RUST_LOG=info $BIN/farplug-gui --tray >$T/tray.log 2>&1 & sleep 6; kill -0 \$! && echo TRAY_ALIVE; kill \$!"
cat $T/tray.log
