#!/bin/sh
set -eu

BASE=/userdata/jetkvm
BIN_DIR="$BASE/bin"
INIT_DIR=/userdata/init.d
HERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

if [ "$(id -u)" != "0" ]; then
    echo "Run this installer as root" >&2
    exit 1
fi

for file in jetkvm_app.update jetkvm-rdp S90jetkvm-rdp; do
    if [ ! -f "$HERE/$file" ]; then
        echo "Missing bundle file: $file" >&2
        exit 1
    fi
done

mkdir -p "$BIN_DIR" "$INIT_DIR"

# Keep one known-good copy of the stock application for simple SSH rollback.
if [ -f "$BIN_DIR/jetkvm_app" ] && [ ! -f "$BIN_DIR/jetkvm_app.stock-backup" ]; then
    cp "$BIN_DIR/jetkvm_app" "$BIN_DIR/jetkvm_app.stock-backup"
    chmod 0755 "$BIN_DIR/jetkvm_app.stock-backup"
fi

# Stock JetKVM installs this file as the active application during the next boot.
cp "$HERE/jetkvm_app.update" "$BASE/jetkvm_app.update"
chmod 0755 "$BASE/jetkvm_app.update"

cp "$HERE/jetkvm-rdp" "$BIN_DIR/jetkvm-rdp"
chmod 0755 "$BIN_DIR/jetkvm-rdp"

cp "$HERE/S90jetkvm-rdp" "$INIT_DIR/S90jetkvm-rdp"
chmod 0755 "$INIT_DIR/S90jetkvm-rdp"

# First boot is intentionally app-only. Enable RDP only after the custom app
# has booted successfully and the normal JetKVM web console has been verified.
rm -f "$BASE/rdp.enable" /run/jetkvm-rdp.pid
sync

echo
echo "RDP userland bundle installed with RDP DISABLED."
echo "Reboot now and verify the normal JetKVM UI first."
echo
echo "After verification, enable RDP with:"
echo "  touch $BASE/rdp.enable"
echo "  $INIT_DIR/S90jetkvm-rdp start"
echo
echo "Rollback the custom app with:"
echo "  $HERE/rollback-app.sh"
