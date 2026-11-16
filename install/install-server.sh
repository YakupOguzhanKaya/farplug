#!/bin/sh
# Install the farplug server. Linux only; needs no Rust toolchain.
#
#   curl -fsSL .../install-server.sh | sh
#
# For a private repository, export a token with 'repo' scope first:
#   export GITHUB_TOKEN=ghp_...
set -eu

REPO="${FARPLUG_REPO:-YakupOguzhanKaya/farplug}"
PREFIX="${PREFIX:-/usr/local}"
CONFDIR="${CONFDIR:-/etc/farplug}"

[ "$(uname -s)" = "Linux" ] || { echo "the server runs on Linux only" >&2; exit 1; }

case "$(uname -m)" in
    x86_64|amd64)  TARGET=x86_64-unknown-linux-musl ;;
    aarch64|arm64) TARGET=aarch64-unknown-linux-musl ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

fetch() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$@"
    else
        curl -fsSL "$@"
    fi
}

TAG="${FARPLUG_VERSION:-$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)}"
[ -n "$TAG" ] || { echo "could not determine latest release; is the repo private? set GITHUB_TOKEN" >&2; exit 1; }

ASSET="farplugd-$TARGET.tar.gz"
echo "installing farplugd $TAG ($TARGET)"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if [ -n "${GITHUB_TOKEN:-}" ]; then
    # Private releases are only downloadable through the API, with the octet-stream
    # Accept header; the browser_download_url returns HTML for unauthenticated callers.
    ID=$(fetch "https://api.github.com/repos/$REPO/releases/tags/$TAG" \
        | tr ',' '\n' | grep -B5 "\"name\": *\"$ASSET\"" | sed -n 's/.*"id": *\([0-9]*\).*/\1/p' | head -1)
    [ -n "$ID" ] || { echo "asset $ASSET not found in $TAG" >&2; exit 1; }
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/octet-stream" \
        -o "$TMP/$ASSET" "https://api.github.com/repos/$REPO/releases/assets/$ID"
else
    curl -fsSL -o "$TMP/$ASSET" "https://github.com/$REPO/releases/download/$TAG/$ASSET"
fi

tar xzf "$TMP/$ASSET" -C "$TMP"
SRC="$TMP/farplugd-$TARGET"

sudo install -D -m0755 "$SRC/farplugd" "$PREFIX/bin/farplugd"

if [ ! -f "$CONFDIR/config.toml" ]; then
    sudo install -D -m0644 "$SRC/config.example.toml" "$CONFDIR/config.toml"
    echo "wrote starter config to $CONFDIR/config.toml -- edit it before starting"
else
    echo "keeping existing $CONFDIR/config.toml"
fi

if ! sudo modprobe usbip-host 2>/dev/null; then
    echo "warning: the usbip-host kernel module is missing. On Debian/Ubuntu install" >&2
    echo "         linux-modules-extra-\$(uname -r) (or your distro's equivalent)." >&2
fi

if [ -d /run/systemd/system ]; then
    sudo install -D -m0644 "$SRC/farplugd.service" /etc/systemd/system/farplugd.service
    sudo systemctl daemon-reload
    echo
    echo "next:  sudo farplugd scan              # find your devices"
    echo "       sudo \$EDITOR $CONFDIR/config.toml"
    echo "       sudo systemctl enable --now farplugd"
else
    echo "no systemd detected; run $PREFIX/bin/farplugd --config $CONFDIR/config.toml yourself"
fi
