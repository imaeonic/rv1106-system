#!/bin/sh
set -eu

BASE=/userdata/jetkvm
BACKUP="$BASE/bin/jetkvm_app.stock-backup"
UPDATE="$BASE/jetkvm_app.update"

if [ "$(id -u)" != "0" ]; then
    echo "Run this rollback as root" >&2
    exit 1
fi

if [ ! -f "$BACKUP" ]; then
    echo "Stock application backup not found: $BACKUP" >&2
    exit 1
fi

rm -f "$BASE/rdp.enable"
cp "$BACKUP" "$UPDATE"
chmod 0755 "$UPDATE"
sync

echo "Stock jetkvm_app staged for the next boot and RDP disabled."
echo "Reboot the JetKVM to complete rollback."
