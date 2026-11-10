#!/bin/sh
# Install the usbridge client. Linux, macOS, or Windows from Git Bash; needs
# no Rust toolchain.
#
#   curl -fsSL .../install-client.sh | sh
#
# On Windows without Git Bash, use install-client.ps1 from PowerShell instead.
#
# For a private repository, export a token with 'repo' scope first:
#   export GITHUB_TOKEN=ghp_...
set -eu

REPO="${USBRIDGE_REPO:-YakupOguzhanKaya/usbridge}"
PREFIX="${PREFIX:-/usr/local}"

fetch() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$@"
    else
        curl -fsSL "$@"
    fi
}

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        # Git Bash and friends: the PowerShell installer does the Windows work.
        PS1="$(mktemp -d)/install-client.ps1"
        fetch -o "$PS1" "https://raw.githubusercontent.com/$REPO/${USBRIDGE_BRANCH:-master}/install/install-client.ps1"
        exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$(cygpath -w "$PS1")"
        ;;
    Darwin) TARGET=universal-apple-darwin ;;
    Linux)
        case "$(uname -m)" in
            x86_64|amd64)  TARGET=x86_64-unknown-linux-musl ;;
            aarch64|arm64) TARGET=aarch64-unknown-linux-musl ;;
            *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
        esac ;;
    *) echo "unsupported system: $(uname -s)" >&2; exit 1 ;;
esac

TAG="${USBRIDGE_VERSION:-$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)}"
[ -n "$TAG" ] || { echo "could not determine latest release; is the repo private? set GITHUB_TOKEN" >&2; exit 1; }

ASSET="usbridge-$TARGET.tar.gz"
echo "installing usbridge $TAG ($TARGET)"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# download ASSET: fetches a release asset into $TMP.
download() {
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        ID=$(fetch "https://api.github.com/repos/$REPO/releases/tags/$TAG" \
            | tr ',' '\n' | grep -B5 "\"name\": *\"$1\"" | sed -n 's/.*"id": *\([0-9]*\).*/\1/p' | head -1)
        [ -n "$ID" ] || { echo "asset $1 not found in $TAG" >&2; return 1; }
        curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" -H "Accept: application/octet-stream" \
            -o "$TMP/$1" "https://api.github.com/repos/$REPO/releases/assets/$ID"
    else
        curl -fsSL -o "$TMP/$1" "https://github.com/$REPO/releases/download/$TAG/$1"
    fi
}

download "$ASSET" || exit 1
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

if [ "$(uname -s)" = "Darwin" ]; then
    if [ -w "$PREFIX/bin" ]; then
        install -m0755 "$SRC/usbridge-gui" "$PREFIX/bin/usbridge-gui"
    else
        sudo install -m0755 "$SRC/usbridge-gui" "$PREFIX/bin/usbridge-gui"
    fi
    xattr -d com.apple.quarantine "$PREFIX/bin/usbridge-gui" 2>/dev/null || true

    # The agent runs per user as a LaunchAgent, started now and at every login.
    PLIST="$HOME/Library/LaunchAgents/com.usbridge.agent.plist"
    mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
    sed -e "s|@HOME@|$HOME|g" -e "s|/usr/local/bin/usbridge|$PREFIX/bin/usbridge|" \
        "$SRC/com.usbridge.agent.plist" > "$PLIST"
    launchctl bootout "gui/$(id -u)/com.usbridge.agent" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$PLIST"

    echo "started the usbridge agent (log: ~/Library/Logs/usbridge-agent.log)"
    echo "note: attaching devices does not work on macOS yet; listing and the GUI do"
    echo "try:   usbridge --server <server-host> list"
    echo "       usbridge-gui"
    exit 0
fi

if ! sudo modprobe vhci-hcd 2>/dev/null; then
    echo "warning: the vhci-hcd kernel module is missing. On Debian/Ubuntu install" >&2
    echo "         linux-modules-extra-\$(uname -r) (or your distro's equivalent)." >&2
fi
if [ -d /run/systemd/system ]; then
    getent group usbridge >/dev/null || sudo groupadd --system usbridge
    sudo usermod -aG usbridge "$(id -un)"
    sudo install -D -m0644 "$SRC/usbridge-agent.service" /etc/systemd/system/usbridge-agent.service
    sudo systemctl daemon-reload
    sudo systemctl enable --now usbridge-agent
    echo "started usbridge-agent; log out and in again so the usbridge group applies"
else
    echo "no systemd detected; run 'sudo usbridge agent' yourself"
fi

# On a desktop, also install the GUI and start its tray icon at login.
if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ] || [ "${USBRIDGE_GUI:-}" = 1 ]; then
    GUI="usbridge-gui-$(echo "$TARGET" | sed 's/musl$/gnu/')"
    if download "$GUI.tar.gz" 2>/dev/null; then
        tar xzf "$TMP/$GUI.tar.gz" -C "$TMP"
        sudo install -D -m0755 "$TMP/$GUI/usbridge-gui" "$PREFIX/bin/usbridge-gui"
        sudo install -D -m0644 "$TMP/$GUI/usbridge.desktop" /usr/share/applications/usbridge.desktop
        install -D -m0644 "$TMP/$GUI/usbridge-tray.desktop" "$HOME/.config/autostart/usbridge-tray.desktop"
        echo "installed usbridge-gui; the tray icon starts at your next login"
        echo "(the tray icon needs libayatana-appindicator3-1, or on GNOME the AppIndicator extension)"
    else
        echo "warning: could not download $GUI.tar.gz; skipping the GUI" >&2
    fi
fi

echo "try:   usbridge --server <server-host> list"
echo "then:  usbridge --server <server-host> attach <device-id>   (or open usbridge-gui)"
