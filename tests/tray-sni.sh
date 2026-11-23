#!/bin/bash
# The Linux tray over D-Bus, with no panel: its StatusNotifierItem menu, a left
# click opening the window, a menu click detaching a device, a second tray
# exiting, and Quit. Runs in the privileged container (see run-e2e.sh):
#   docker run ... rust:1-slim-bookworm bash /work/tests/tray-sni.sh
set -u
cd /work
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq >/dev/null
    apt-get install -y -qq python3 xvfb dbus libglib2.0-bin procps libgl1-mesa-dri libegl1 \
        libxkbcommon-x11-0 libxcursor1 libxrandr2 libxi6 pkg-config >/dev/null 2>&1
    cargo build -q -p farplug -p farplug-gui || exit 1
    exec dbus-run-session -- bash "$0"
fi

BIN=$CARGO_TARGET_DIR/debug
T=$(mktemp -d)
FAILED=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; FAILED=1; }
wait_for() { # seconds, command...
    local n=$1; shift
    for _ in $(seq $((n * 5))); do "$@" && return 0; sleep 0.2; done
    return 1
}
export FARPLUG_AGENT_DIR=$T/agent DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1
python3 tests/fake_device.py 3241 fake1 >/dev/null 2>&1 &
$BIN/farplug agent >$T/agent.log 2>&1 &
wait_for 5 test -S $T/agent/agent.sock
$BIN/farplug --server 127.0.0.1:3241 attach fake1 >/dev/null
Xvfb :99 >/dev/null 2>&1 &
sleep 1

$BIN/farplug-gui --tray >$T/tray.log 2>&1 &
TRAY=$!
item() { gdbus call --session -d org.freedesktop.DBus -o /org/freedesktop/DBus \
    -m org.freedesktop.DBus.ListNames | grep -o 'org.kde.StatusNotifierItem-[0-9]*-[0-9]*'; }
wait_for 10 item >/dev/null && pass "tray registered on D-Bus" || fail "no StatusNotifierItem"
NAME=$(item)
layout() { gdbus call --session -d "$NAME" -o /MenuBar -m com.canonical.dbusmenu.GetLayout 0 -- -1 '@as []'; }
# The id of the menu entry whose label is $1.
menu_id() { layout | python3 -c 'import re,sys; m=re.search(r"\((\d+), \{[^}]*'"'"'label'"'"': <'"'"'"+re.escape(sys.argv[1])+r"'"'"'>", sys.stdin.read()); print(m.group(1) if m else "")' "$1"; }

lists_fake1() { layout | grep -q "'fake1'"; }
wait_for 10 lists_fake1 && pass "menu lists the attached device" || fail "menu: $(layout 2>&1)"
layout | grep -q "'Open farplug" && layout | grep -q "'Quit tray icon'" \
    && pass "menu has Open and Quit" || fail "menu lacks Open/Quit"

gdbus call --session -d "$NAME" -o /StatusNotifierItem -m org.kde.StatusNotifierItem.Activate 0 0 >/dev/null
wait_for 10 pgrep -fx "$BIN/farplug-gui" >/dev/null && pass "left click opens the window" \
    || fail "no window after Activate"

ID=$(menu_id fake1)
gdbus call --session -d "$NAME" -o /MenuBar -m com.canonical.dbusmenu.Event "$ID" clicked '<0>' 0 >/dev/null
wait_for 10 sh -c "$BIN/farplug status | grep -q 'no devices attached'" \
    && pass "menu click detaches the device" || fail "status: $($BIN/farplug status)"

timeout 5 $BIN/farplug-gui --tray && pass "a second tray exits" || fail "second tray did not exit"

ID=$(menu_id 'Quit tray icon')
gdbus call --session -d "$NAME" -o /MenuBar -m com.canonical.dbusmenu.Event "$ID" clicked '<0>' 0 >/dev/null
wait_for 5 sh -c "! kill -0 $TRAY 2>/dev/null" && pass "Quit stops the tray" || fail "tray still running"

pkill -f "$BIN/farplug-gui" 2>/dev/null
[ $FAILED = 0 ] && echo "ALL PASSED" || { echo "SOME FAILED"; cat $T/tray.log; }
exit $FAILED
