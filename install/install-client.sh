#!/bin/sh
# Install the usbridge client. macOS or Linux; needs no Rust toolchain.
#
#   curl -fsSL .../install-client.sh | sh
#
# For a private repository, export a token with 'repo' scope first:
#   export GITHUB_TOKEN=ghp_...
set -eu

REPO="${USBRIDGE_REPO:-YakupOguzhanKaya/usbridge}"
PREFIX="${PREFIX:-/usr/local}"

case "$(uname -s)" in
    Darwin) TARGET=universal-apple-darwin ;;
    Linux)
        case "$(uname -m)" in
            x86_64|amd64)  TARGET=x86_64-unknown-linux-musl ;;
            aarch64|arm64) TARGET=aarch64-unknown-linux-musl ;;
            *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
        esac ;;
    *) echo "unsupported system: $(uname -s)" >&2; exit 1 ;;
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

ASSET="usbridge-$TARGET.tar.gz"
echo "installing usbridge $TAG ($TARGET)"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if [ -n "${GITHUB_TOKEN:-}" ]; then
    ID=$(fetch "https://api.github.com/repos/$REPO/releases/tags/$TAG" \
        | tr ',' '\n' | grep -B5 "\"name\": *\"$ASSET\"" | sed -n 's/.*"id": *\([0-9]*\).*/\1/p' | head -1)
    [ -n "$ID" ] || { echo "asset $ASSET not found in $TAG" >&2; exit 1; }
    curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/octet-stream" \
        -o "$TMP/$ASSET" "https://api.github.com/repos/$REPO/releases/assets/$ID"
else
    curl -fsSL -o "$TMP/$ASSET" "https://github.com/$REPO/releases/download/$TAG/$ASSET"
fi

tar xzf "$TMP/$ASSET" -C "$TMP"
SRC="$TMP/usbridge-$TARGET"

if [ -w "$PREFIX/bin" ]; then
    install -d "$PREFIX/bin" && install -m0755 "$SRC/usbridge" "$PREFIX/bin/usbridge"
else
    sudo install -D -m0755 "$SRC/usbridge" "$PREFIX/bin/usbridge"
fi

# Gatekeeper quarantines anything fetched by curl; the binary is unsigned.
[ "$(uname -s)" = "Darwin" ] && xattr -d com.apple.quarantine "$PREFIX/bin/usbridge" 2>/dev/null || true

echo
echo "installed $PREFIX/bin/usbridge"
echo "try:  usbridge --server <gateway-host> list"
