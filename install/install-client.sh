#!/bin/sh
# Install the farplug client. Linux, macOS, or Windows from Git Bash; needs
# no Rust toolchain.
#
#   curl -fsSL .../install-client.sh | sh
#
# On Windows without Git Bash, use install-client.ps1 from PowerShell instead.
#
# For a private repository, export a token with 'repo' scope first:
#   export GITHUB_TOKEN=ghp_...
set -eu

REPO="${FARPLUG_REPO:-YakupOguzhanKaya/farplug}"
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
        fetch -o "$PS1" "https://raw.githubusercontent.com/$REPO/${FARPLUG_BRANCH:-master}/install/install-client.ps1"
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

TAG="${FARPLUG_VERSION:-$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)}"
[ -n "$TAG" ] || { echo "could not determine latest release; is the repo private? set GITHUB_TOKEN" >&2; exit 1; }

ASSET="farplug-$TARGET.tar.gz"
echo "installing farplug $TAG ($TARGET)"

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
SRC="$TMP/farplug-$TARGET"

if [ -w "$PREFIX/bin" ]; then
    install -d "$PREFIX/bin" && install -m0755 "$SRC/farplug" "$PREFIX/bin/farplug"
else
    sudo install -D -m0755 "$SRC/farplug" "$PREFIX/bin/farplug"
fi

# Gatekeeper quarantines anything fetched by curl; the binary is unsigned.
[ "$(uname -s)" = "Darwin" ] && xattr -d com.apple.quarantine "$PREFIX/bin/farplug" 2>/dev/null || true

echo
echo "installed $PREFIX/bin/farplug"

if [ "$(uname -s)" = "Darwin" ]; then
    if [ -w "$PREFIX/bin" ]; then
        install -m0755 "$SRC/farplug-gui" "$PREFIX/bin/farplug-gui"
    else
        sudo install -m0755 "$SRC/farplug-gui" "$PREFIX/bin/farplug-gui"
    fi
    xattr -d com.apple.quarantine "$PREFIX/bin/farplug-gui" 2>/dev/null || true

    # The agent runs per user as a LaunchAgent, started now and at every login.
    PLIST="$HOME/Library/LaunchAgents/com.farplug.agent.plist"
    mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
    sed -e "s|@HOME@|$HOME|g" -e "s|/usr/local/bin/farplug|$PREFIX/bin/farplug|" \
        "$SRC/com.farplug.agent.plist" > "$PLIST"
    launchctl bootout "gui/$(id -u)/com.farplug.agent" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "$PLIST"

    echo "started the farplug agent (log: ~/Library/Logs/farplug-agent.log)"
    echo "note: attaching devices does not work on macOS yet; listing and the GUI do"
    echo "try:   farplug --server <server-host> list"
    echo "       farplug-gui"
    exit 0
fi

if ! sudo modprobe vhci-hcd 2>/dev/null; then
    echo "warning: the vhci-hcd kernel module is missing. On Debian/Ubuntu install" >&2
    echo "         linux-modules-extra-\$(uname -r) (or your distro's equivalent)." >&2
fi
if [ -d /run/systemd/system ]; then
    getent group farplug >/dev/null || sudo groupadd --system farplug
    sudo usermod -aG farplug "$(id -un)"
    sudo install -D -m0644 "$SRC/farplug-agent.service" /etc/systemd/system/farplug-agent.service
    sudo systemctl daemon-reload
    sudo systemctl enable farplug-agent
    # An agent left running from an older version would not understand the new CLI.
    sudo systemctl restart farplug-agent
    echo "started farplug-agent"
else
    echo "no systemd detected; run 'sudo farplug agent' yourself"
fi

# On a desktop, also install the GUI and start its tray icon at login.
if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ] || [ "${FARPLUG_GUI:-}" = 1 ]; then
    GUI="farplug-gui-$(echo "$TARGET" | sed 's/musl$/gnu/')"
    if download "$GUI.tar.gz" 2>/dev/null; then
        tar xzf "$TMP/$GUI.tar.gz" -C "$TMP"
        sudo install -D -m0755 "$TMP/$GUI/farplug-gui" "$PREFIX/bin/farplug-gui"
        sudo install -D -m0644 "$TMP/$GUI/farplug.desktop" /usr/share/applications/farplug.desktop
        install -D -m0644 "$TMP/$GUI/farplug-tray.desktop" "$HOME/.config/autostart/farplug-tray.desktop"
        if [ -f "$TMP/$GUI/farplug-tray.service" ]; then
            sed "s|/usr/local/bin/farplug-gui|$PREFIX/bin/farplug-gui|" "$TMP/$GUI/farplug-tray.service" \
                | sudo tee /etc/systemd/user/farplug-tray.service >/dev/null
            systemctl --user daemon-reload 2>/dev/null || true
            systemctl --user enable farplug-tray 2>/dev/null || true
        fi
        # Start it now rather than at the next login; a second copy exits on its own.
        pkill -f 'farplug-gui --tray' 2>/dev/null || true
        if ! { systemctl --user is-active -q graphical-session.target 2>/dev/null \
                && systemctl --user restart farplug-tray 2>/dev/null; }; then
            (setsid "$PREFIX/bin/farplug-gui" --tray >/dev/null 2>&1 &)
        fi
        echo "installed farplug-gui and started its tray icon; it also starts at every login"
        echo "(the tray icon needs libayatana-appindicator3-1, or on GNOME the AppIndicator extension)"
    else
        echo "warning: could not download $GUI.tar.gz; skipping the GUI" >&2
    fi
fi

echo "try:   farplug --server <server-host> list"
echo "then:  farplug --server <server-host> attach <device-id>   (or open farplug-gui)"
