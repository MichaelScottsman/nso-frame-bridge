#!/bin/sh
# Installs the bridge on a Steam Frame (or other SteamOS/Linux box).
# Run from the crate root after `cargo build --release`, or from an extracted
# release tarball (which has the binary next to frame/).
# Then once, for the 7.5 ms / ~130 Hz interval:
#   sudo setcap cap_net_raw+ep ~/.local/share/nso-gc-bridge/nso-gc-bridge
set -e
BIN=target/release/nso-gc-bridge
[ -f "$BIN" ] || BIN=./nso-gc-bridge
[ -f "$BIN" ] || { echo "nso-gc-bridge binary not found; build it or run from the release folder" >&2; exit 1; }
DEST="$HOME/.local/share/nso-gc-bridge"
mkdir -p "$DEST" "$HOME/.local/share/applications"
systemctl --user stop nso-gc-bridge 2>/dev/null || true
install -m 755 "$BIN" "$DEST/nso-gc-bridge"
install -m 755 frame/nsogc "$DEST/nsogc"
install -m 644 frame/icon.png "$DEST/icon.png"
install -m 644 frame/rumble_test.py "$DEST/rumble_test.py"
sed "s|/home/steamos|$HOME|g" frame/nso-gc-bridge.desktop > "$HOME/.local/share/applications/nso-gc-bridge.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "installed to $DEST"
getcap "$DEST/nso-gc-bridge" 2>/dev/null | grep -q cap_net_raw || echo "note: run 'sudo setcap cap_net_raw+ep $DEST/nso-gc-bridge' for the 7.5 ms interval"
