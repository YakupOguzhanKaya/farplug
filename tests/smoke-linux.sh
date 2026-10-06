#!/bin/bash
set -u
cd /work
apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq socat python3 >/dev/null 2>&1
cargo build
mkdir -p /tmp/t
cat > /tmp/t/test.toml <<'EOF'
name = "test-gw"
listen = "127.0.0.1:7071"

[device.fake]
label = "Loopback test device"
path = "/tmp/usbridge-fake-tty"
baud = 115200
EOF

socat PTY,link=/tmp/usbridge-fake-tty,raw,echo=0 EXEC:"/bin/cat" >/tmp/t/socat.log 2>&1 &
sleep 1
RUST_LOG=info ./target/debug/usbridged --config /tmp/t/test.toml >/tmp/t/server.log 2>&1 &
sleep 1

echo "--- list ---"
./target/debug/usbridge --server 127.0.0.1:7071 list

echo "--- attach ---"
RUST_LOG=info ./target/debug/usbridge --server 127.0.0.1:7071 \
  attach fake --link /tmp/t/client-tty --once >/tmp/t/attach.out 2>/tmp/t/client.log &
sleep 2

echo "--- roundtrip ---"
python3 - <<'PY'
import os, sys, time, tty
fd = os.open('/tmp/t/client-tty', os.O_RDWR | os.O_NOCTTY)
tty.setraw(fd)
os.write(fd, b'ping-over-the-bridge\n')
time.sleep(1.5)
os.set_blocking(fd, False)
try:
    data = os.read(fd, 4096)
except BlockingIOError:
    data = b''
print('echoed back:', data)
sys.exit(0 if b'ping-over-the-bridge' in data else 1)
PY
RC=$?
echo "roundtrip_exit=$RC"
echo "--- server.log ---"; cat /tmp/t/server.log
echo "--- client.log ---"; cat /tmp/t/client.log
exit $RC
