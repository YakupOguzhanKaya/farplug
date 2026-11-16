#!/bin/bash
# Runs inside a privileged Linux container; see run-e2e.sh.
#
# 1. Client against the real vhci_hcd driver, with tests/fake_device.py as the
#    server: attach, enumerate, survive a server restart, detach on SIGTERM.
# 2. Server handshake against the stock `usbip` tool, using a fake sysfs tree
#    because Docker Desktop's kernel has no usbip-host.
set -u
cd /work
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null && apt-get install -y -qq python3 usbip >/dev/null 2>&1
cargo build -q -p farplug -p farplugd || exit 1
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
fake_enumerated() { grep -lx "Fake Device" /sys/bus/usb/devices/*/product >/dev/null 2>&1; }
port_used() { grep -qE '^hs +[0-9]+ 006 ' /sys/devices/platform/vhci_hcd.0/status; }

echo "=== client vs real vhci_hcd"
python3 tests/fake_device.py 3241 fake1 >"$T/fake.log" 2>&1 &
FAKE=$!
sleep 0.5

if $BIN/farplug --server 127.0.0.1:3241 list | grep -q '^fake1 .*1209:0001'; then
    pass "list"
else
    fail "list"
fi

$BIN/farplug check >/dev/null && pass "check finds vhci_hcd" || fail "check"
export FARPLUG_AGENT_DIR=$T/agent
start_agent() { $BIN/farplug agent >>"$T/agent.log" 2>&1 & AGENT=$!; wait_for 5 test -f $T/agent/agent.json; }
start_agent
$BIN/farplug --server 127.0.0.1:3241 attach fake1 nosuch >/dev/null
wait_for 10 fake_enumerated && pass "device enumerated on vhci_hcd" || fail "device not enumerated"
$BIN/farplug status >"$T/status.txt"
grep -q 'fake1 .*attached' "$T/status.txt" && pass "status shows attached" || fail "status: $(cat $T/status.txt)"
wait_for 5 sh -c "$BIN/farplug status | grep nosuch | grep -q 'no such device'" \
    && pass "unknown id reported" || fail "unknown id not reported"

kill $FAKE; wait $FAKE 2>/dev/null
wait_for 10 sh -c '! grep -qE "^hs +[0-9]+ 006 " /sys/devices/platform/vhci_hcd.0/status' \
    && pass "port freed when server died" || fail "port not freed"
python3 tests/fake_device.py 3241 fake1 >>"$T/fake.log" 2>&1 &
FAKE=$!
wait_for 20 fake_enumerated && pass "re-attached after server restart" || fail "no re-attach"

kill -TERM $AGENT; wait $AGENT
if port_used; then fail "port still in use after agent stopped"; else pass "agent detaches on SIGTERM"; fi
start_agent
wait_for 10 fake_enumerated && pass "remembered device re-attached on agent start" || fail "not re-attached on start"

$BIN/farplug detach --all >/dev/null
wait_for 10 sh -c '! grep -qE "^hs +[0-9]+ 006 " /sys/devices/platform/vhci_hcd.0/status' \
    && pass "detach" || fail "detach"
$BIN/farplug status | grep -q 'no devices attached' && pass "detach forgets" || fail "detach did not forget"
kill -TERM $AGENT; wait $AGENT

$BIN/farplug --server 127.0.0.1:3241 attach --foreground fake1 >>"$T/foreground.log" 2>&1 &
FG=$!
wait_for 10 fake_enumerated && pass "foreground attach" || fail "foreground attach"
kill -TERM $FG; wait $FG
if port_used; then fail "foreground: port in use after SIGTERM"; else pass "foreground detaches on SIGTERM"; fi
kill $FAKE

echo "=== farplugd vs stock usbip"
S=$T/sys
mkdir -p $S/bus/usb/drivers/usbip-host $S/bus/usb/drivers/usb $S/bus/usb/devices/2-1/2-1:1.0
for f in bind unbind rebind match_busid; do : >$S/bus/usb/drivers/usbip-host/$f; done
: >$S/bus/usb/drivers/usb/unbind
D=$S/bus/usb/devices/2-1
while read -r k v; do echo "$v" >$D/$k; done <<EOF
idVendor 10c4
idProduct ea60
bcdDevice 0100
bDeviceClass 00
bDeviceSubClass 00
bDeviceProtocol 00
bConfigurationValue 1
bNumConfigurations 1
bNumInterfaces 1
busnum 2
devnum 3
speed 12
product CP2102N
usbip_status 1
EOF
: >$D/usbip_sockfd
echo ff >$D/2-1:1.0/bInterfaceClass
ln -s ../../drivers/usb $D/driver
cat >$T/config.toml <<EOF
listen = "127.0.0.1:3242"
[device.zigbee]
vid = "10c4"
pid = "ea60"
EOF
$BIN/farplugd --config $T/config.toml --sysfs-root $S >"$T/server.log" 2>&1 &
SERVER=$!
sleep 1
[ "$(cat $S/bus/usb/drivers/usbip-host/bind)" = 2-1 ] && pass "server bound device" || fail "server did not bind"

usbip --tcp-port 3242 list -r 127.0.0.1 >"$T/usbip-list.txt" 2>&1
grep -q 'zigbee: .*10c4:ea60' "$T/usbip-list.txt" && pass "stock usbip list" || fail "stock usbip list"

# The fake server hands the socket to a plain file, not the kernel, so the
# device drops right after; only the handshake is under test here.
if usbip --tcp-port 3242 attach -r 127.0.0.1 -b zigbee >"$T/usbip-attach.txt" 2>&1; then
    pass "stock usbip attach handshake"
else
    fail "stock usbip attach handshake"
fi
[ -n "$(cat $D/usbip_sockfd)" ] && pass "server wrote sockfd" || fail "no sockfd written"
kill $SERVER

for f in "$T"/*.log "$T"/*.txt; do echo "--- $(basename "$f")"; cat "$f"; done
[ $FAILED = 0 ] && echo "ALL PASSED" || echo "SOME FAILED"
exit $FAILED
