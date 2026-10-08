#!/bin/sh
# Install the usbridge gateway. Linux only; needs no Rust toolchain.
#
#   curl -fsSL .../install-server.sh | sh
#
# For a private repository, export a token with 'repo' scope first:
#   export GITHUB_TOKEN=ghp_...
set -eu

REPO="${USBRIDGE_REPO:-YakupOguzhanKaya/usbridge}"
PREFIX="${PREFIX:-/usr/local}"
CONFDIR="${CONFDIR:-/etc/usbridge}"

[ "$(uname -s)" = "Linux" ] || { echo "the gateway runs on Linux only" >&2; exit 1; }

case "$(uname -m)" in
    x86_64|amd64)  TARGET=x86_64-unknown-linux-gnu ;;
    aarch64|arm64) TARGET=aarch64-unknown-linux-gnu ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

fetch() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$@"
    else
        curl -fsSL "$@"
    fi
}

TAG="${USBRIDGE_VERSION:-$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)}"
[ -n "$TAG" ] || { echo "could not determine latest release; is the repo private? set GITHUB_TOKEN" >&2; exit 1; }

ASSET="usbridged-$TARGET.tar.gz"
echo "installing usbridged $TAG ($TARGET)"

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
SRC="$TMP/usbridged-$TARGET"

sudo install -D -m0755 "$SRC/usbridged" "$PREFIX/bin/usbridged"

if [ ! -f "$CONFDIR/config.toml" ]; then
    sudo install -D -m0644 "$SRC/config.example.toml" "$CONFDIR/config.toml"
    echo "wrote starter config to $CONFDIR/config.toml -- edit it before starting"
else
    echo "keeping existing $CONFDIR/config.toml"
fi

if [ -d /run/systemd/system ]; then
    id usbridge >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin usbridge
    sudo install -D -m0644 "$SRC/usbridged.service" /etc/systemd/system/usbridged.service
    sudo systemctl daemon-reload
    echo
    echo "next:  sudo \$EDITOR $CONFDIR/config.toml"
    echo "       sudo systemctl enable --now usbridged"
else
    echo "no systemd detected; run $PREFIX/bin/usbridged --config $CONFDIR/config.toml yourself"
fi
