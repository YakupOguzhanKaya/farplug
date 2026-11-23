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
start_agent() { $BIN/farplug agent >>"$T/agent.log" 2>&1 & AGENT=$!; wait_for 5 test -S $T/agent/agent.sock; }
start_agent
$BIN/farplug --server 127.0.0.1:3241 attach fake1 nosuch >/dev/null
wait_for 10 fake_enumerated && pass "device enumerated on vhci_hcd" || fail "device not enumerated"
$BIN/farplug status >"$T/status.txt"
grep -q 'fake1 .*attached' "$T/status.txt" && pass "status shows attached" || fail "status: $(cat $T/status.txt)"
wait_for 5 sh -c "$BIN/farplug status | grep nosuch | grep -q 'no such device'" \
    && pass "unknown id reported" || fail "unknown id not reported"
$BIN/farplug alias 127.0.0.1:3241 testbox >/dev/null
$BIN/farplug --server testbox list 2>&1 | grep -q '^fake1 ' && pass "alias usable as --server" || fail "alias as --server"
$BIN/farplug status | grep -q '^testbox (127.0.0.1:3241) .*fake1' && pass "status shows alias" \
    || fail "status: $($BIN/farplug status)"

# A session that predates the group change: setpriv drops all supplementary
# groups, as a login from before `usermod -aG` would have.
chmod 711 "$T"
getent group farplug >/dev/null || groupadd --system farplug
id alice >/dev/null 2>&1 || useradd alice
as_alice() { setpriv --reuid alice --regid alice --clear-groups "$@"; }
as_alice $BIN/farplug status 2>&1 | grep -q 'not in the `farplug` group' \
    && pass "non-member refused" || fail "non-member: $(as_alice $BIN/farplug status 2>&1)"
usermod -aG farplug alice
as_alice $BIN/farplug status 2>&1 | grep -q 'fake1 .*attached' \
    && pass "new group member allowed without logging in again" \
    || fail "new member: $(as_alice $BIN/farplug status 2>&1)"
gpasswd -d alice farplug >/dev/null

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
mkdir -p $S/bus/usb/drivers/usbip-host $S/bus/usb/drivers/usb
for f in bind unbind rebind match_busid; do : >$S/bus/usb/drivers/usbip-host/$f; done
: >$S/bus/usb/drivers/usb/unbind
fakedev() { # busid vid pid product devnum
    local D=$S/bus/usb/devices/$1
    mkdir -p $D/$1:1.0
    while read -r k v; do echo "$v" >$D/$k; done <<EOF
idVendor $2
idProduct $3
bcdDevice 0100
bDeviceClass 00
bDeviceSubClass 00
bDeviceProtocol 00
bConfigurationValue 1
bNumConfigurations 1
bNumInterfaces 1
busnum 2
devnum $5
speed 12
product $4
usbip_status 1
EOF
    : >$D/usbip_sockfd
    echo ff >$D/$1:1.0/bInterfaceClass
    ln -s ../../drivers/usb $D/driver
}
fakedev 2-1 10c4 ea60 CP2102N 3
D=$S/bus/usb/devices/2-1
cat >$T/config.toml <<EOF
listen = "127.0.0.1:3242"
[device.zigbee]
label = "SMLIGHT SLZB-07"
vid = "10c4"
pid = "ea60"
EOF
$BIN/farplugd --config $T/config.toml --sysfs-root $S >"$T/server.log" 2>&1 &
SERVER=$!
sleep 1
[ "$(cat $S/bus/usb/drivers/usbip-host/bind)" = 2-1 ] && pass "server bound device" || fail "server did not bind"

usbip --tcp-port 3242 list -r 127.0.0.1 >"$T/usbip-list.txt" 2>&1
grep -q 'zigbee: .*10c4:ea60' "$T/usbip-list.txt" && pass "stock usbip list" || fail "stock usbip list"
$BIN/farplug --server 127.0.0.1:3242 list >"$T/farplug-list.txt" 2>&1
grep -q '^zigbee .*available .*SMLIGHT SLZB-07' "$T/farplug-list.txt" \
    && pass "farplug list shows label and availability" || fail "farplug list: $(cat $T/farplug-list.txt)"

# The fake server hands the socket to a plain file, not the kernel, so the
# device drops right after; only the handshake is under test here.
if usbip --tcp-port 3242 attach -r 127.0.0.1 -b zigbee >"$T/usbip-attach.txt" 2>&1; then
    pass "stock usbip attach handshake"
else
    fail "stock usbip attach handshake"
fi
[ -n "$(cat $D/usbip_sockfd)" ] && pass "server wrote sockfd" || fail "no sockfd written"
echo 2 >$D/usbip_status
$BIN/farplug --server 127.0.0.1:3242 list >"$T/farplug-list-used.txt" 2>&1
grep -q '^zigbee .*in use by 127.0.0.1' "$T/farplug-list-used.txt" \
    && pass "farplug list shows who uses a device" || fail "farplug list: $(cat $T/farplug-list-used.txt)"

$BIN/farplugd --config $T/config.toml --sysfs-root $S check >"$T/check.txt" 2>&1
grep -q '^zigbee .*2-1 .*ok' "$T/check.txt" && pass "farplugd check" || fail "farplugd check: $(cat $T/check.txt)"

# Plug in a second device, add it interactively, and expect the running
# server to export it without a restart; then remove it from the config.
fakedev 2-2 1a86 55d4 "ZST39" 4
printf '1\nzwave\n\ny\n' | $BIN/farplugd --config $T/config.toml --sysfs-root $S add >"$T/add.txt" 2>&1
grep -q '^\[device.zwave\]' $T/config.toml && pass "farplugd add wrote the entry" || fail "add: $(cat $T/add.txt)"
wait_for 5 sh -c "$BIN/farplug --server 127.0.0.1:3242 list | grep -q '^zwave '" \
    && pass "running server applied the added device" || fail "added device not exported"
sed -i '/^\[device.zwave\]/,$d' $T/config.toml
wait_for 5 sh -c "! $BIN/farplug --server 127.0.0.1:3242 list | grep -q '^zwave '" \
    && pass "running server dropped the removed device" || fail "removed device still exported"
grep -q 2-2 $S/bus/usb/drivers/usbip-host/rebind && pass "removed device returned to the host" \
    || fail "removed device not released"
$BIN/farplug --server 127.0.0.1:3242 list | grep -q '^zigbee ' && pass "unchanged export kept" \
    || fail "zigbee lost on reload"

# export_all: the unlisted device is exported, the network adapter is not.
fakedev 2-3 0bda 8153 RTL8153 5
mkdir -p $S/bus/usb/devices/2-3/2-3:1.0/net/eth1
sed -i '1i export_all = true' $T/config.toml
wait_for 5 sh -c "$BIN/farplug --server 127.0.0.1:3242 list | grep -q '^zst39-2-2 '" \
    && pass "export_all exports unlisted devices" || fail "export_all: $($BIN/farplug --server 127.0.0.1:3242 list)"
$BIN/farplug --server 127.0.0.1:3242 list | grep -q 8153 && fail "export_all exported a network adapter" \
    || pass "export_all skips network adapters"
$BIN/farplugd --config $T/config.toml --sysfs-root $S check >"$T/check-all.txt" 2>&1
grep -q '^2-3 .*skipped: network adapter' "$T/check-all.txt" && pass "check explains export_all skips" \
    || fail "check: $(cat $T/check-all.txt)"
kill $SERVER; wait $SERVER 2>/dev/null
grep 'stopping; returning it to the host' "$T/server.log" | grep -q zigbee \
    && pass "stopping the server releases its devices" || fail "no release on stop"

for f in "$T"/*.log "$T"/*.txt; do echo "--- $(basename "$f")"; cat "$f"; done
[ $FAILED = 0 ] && echo "ALL PASSED" || echo "SOME FAILED"
exit $FAILED
