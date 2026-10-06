#!/bin/bash
# End-to-end test: socat provides a loopback pty as the "device", the gateway
# exports it, the client attaches it, and a byte string is round-tripped.
#
# Runs in a Linux container because serialport-rs on macOS issues an IOKit-only
# ioctl that ptys reject with ENOTTY, so a fake device cannot stand in for real
# hardware on a Mac host. The gateway's real target is Linux anyway.
set -eu
cd "$(dirname "$0")/.."
exec docker run --rm -v "$PWD":/work -w /work rust:1-slim-bookworm bash /work/tests/smoke-linux.sh
