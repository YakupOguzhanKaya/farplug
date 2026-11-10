#!/bin/bash
# Renders the GUI against a live agent under Xvfb and saves a screenshot.
# Runs inside the privileged container (see run-e2e.sh); output: /work/target/gui.png
set -eu
cd /work
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null
apt-get install -y -qq python3 xvfb x11-apps imagemagick libgl1-mesa-dri libegl1 libxkbcommon-x11-0 \
    libxcursor1 libxrandr2 libxi6 fonts-dejavu-core >/dev/null 2>&1
cargo build -q -p usbridge -p usbridge-gui
BIN=$CARGO_TARGET_DIR/debug
T=$(mktemp -d)
export USBRIDGE_AGENT_DIR=$T/agent
python3 tests/fake_device.py 3241 zigbee >/dev/null 2>&1 &
$BIN/usbridge agent >$T/agent.log 2>&1 &
AGENT=$!
sleep 1
$BIN/usbridge --server 127.0.0.1:3241 attach zigbee ghost >/dev/null
Xvfb :99 -screen 0 800x600x24 >/dev/null 2>&1 &
sleep 1
DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1 $BIN/usbridge-gui >$T/gui.log 2>&1 &
GUI=$!
sleep 8
DISPLAY=:99 import -window root /work/target/gui.png
kill $GUI || true
kill -TERM $AGENT; wait $AGENT || true
cat $T/gui.log
