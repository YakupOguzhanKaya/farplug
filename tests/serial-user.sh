#!/bin/bash
# The userspace client macOS uses, run on Linux with FARPLUG_BACKEND=user: a
# remote USB-serial adapter (emulated by fake_device.py, echoing its input)
# becomes a local serial port. Needs no privileges:
#   docker run --rm -v "$PWD":/work -w /work rust:1-slim-bookworm bash tests/serial-user.sh
set -u
cd /work
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null && apt-get install -y -qq python3 procps >/dev/null 2>&1
cargo build -q -p farplug || exit 1
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
export FARPLUG_AGENT_DIR=$T/agent FARPLUG_BACKEND=user
RUST_LOG=info $BIN/farplug agent >$T/agent.log 2>&1 &
wait_for 5 test -S $T/agent/agent.sock

# echo_through LINK: a line written to the port comes back.
echo_through() {
    exec 3<>"$1" || return 1
    printf 'hello farplug\n' >&3
    local line=""
    read -t 5 -r line <&3
    exec 3>&-
    [ "$line" = "hello farplug" ]
}

port=3300
for kind in cp210x cdcacm; do
    echo "=== $kind"
    port=$((port + 1))
    python3 tests/fake_device.py $port $kind $kind >$T/$kind.log 2>&1 &
    FAKE=$!
    sleep 0.5
    LINK=$T/agent/serial/$kind
    $BIN/farplug --server 127.0.0.1:$port attach $kind >/dev/null
    wait_for 10 test -L $LINK && pass "$kind: serial port linked at $(readlink $LINK)" \
        || { fail "$kind: no link"; cat $T/agent.log; continue; }
    stty -F $LINK raw -echo
    echo_through $LINK && pass "$kind: data goes both ways" || fail "$kind: no echo"

    stty -F $LINK 115200
    case $kind in
        cp210x) want='SET_BAUDRATE 115200' ;;
        cdcacm) want='SET_LINE_CODING 115200 8N1' ;;
    esac
    wait_for 5 grep -q "$want" $T/$kind.log && pass "$kind: baud rate reaches the chip" \
        || fail "$kind: $(tail -3 $T/$kind.log)"
    $BIN/farplug status | grep -q "^127.0.0.1:$port .*$kind .*attached" \
        && pass "$kind: status attached" || fail "$kind: $($BIN/farplug status)"

    kill $FAKE; wait $FAKE 2>/dev/null
    wait_for 10 sh -c "$BIN/farplug status | grep '$kind' | grep -qv ' attached '" \
        && pass "$kind: noticed the server went away" || fail "$kind: still attached"
    python3 tests/fake_device.py $port $kind $kind >>$T/$kind.log 2>&1 &
    FAKE=$!
    wait_for 15 sh -c "$BIN/farplug status | grep '$kind' | grep -q ' attached '" \
        && sleep 0.5 && stty -F $LINK raw -echo && echo_through $LINK \
        && pass "$kind: re-attached, same link works" || fail "$kind: after reconnect"

    $BIN/farplug detach $kind >/dev/null
    wait_for 5 sh -c "! test -e $LINK" && pass "$kind: detach removes the link" \
        || fail "$kind: link left behind"
    kill $FAKE; wait $FAKE 2>/dev/null
done

echo "=== keyboard and mouse"
python3 tests/fake_device.py 3310 hid hid >$T/hid.log 2>&1 &
FAKE=$!
sleep 0.5
$BIN/farplug --server 127.0.0.1:3310 attach hid >/dev/null
for want in 'Key { usage: 225, down: true }' 'Key { usage: 4, down: true }' \
    'Key { usage: 4, down: false }' 'Button { button: 0, down: true }' \
    'Motion { dx: 5, dy: -2 }' 'Scroll { lines: -1 }'; do
    wait_for 10 grep -qF "$want" $T/agent.log && pass "input: $want" || fail "input: no $want"
done
$BIN/farplug status | grep -q '^127.0.0.1:3310 .*hid .*attached' && pass "input: status attached" \
    || fail "input: $($BIN/farplug status)"
$BIN/farplug detach hid >/dev/null
kill $FAKE; wait $FAKE 2>/dev/null

[ $FAILED = 0 ] && echo "ALL PASSED" || { echo "SOME FAILED"; tail -20 $T/agent.log; }
exit $FAILED
