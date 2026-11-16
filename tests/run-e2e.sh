#!/bin/bash
# End-to-end tests in a privileged Linux container. Needs a kernel with
# vhci_hcd, which Docker Desktop's VM has. The container attaches a fake USB
# device to that VM's kernel and detaches it again.
set -eu
cd "$(dirname "$0")/.."
docker volume create farplug-cargo >/dev/null
exec docker run --rm --privileged \
    -v "$PWD":/work -w /work \
    -v farplug-cargo:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/work/target/docker \
    rust:1-slim-bookworm bash /work/tests/e2e-linux.sh
