#!/bin/sh
# Powers the Kobo off or restarts it, when the reader exits asking for one
# from its power screen: 88 is power off and 89 restart, as with KOReader.
# Anything else returns at once, and the caller goes on to Nickel.
#
#   power.sh STATUS
#
# Run by reader.sh and boot.sh, after they've stopped SSH. Adapted from
# KOReader's frontend/device/kobo/device.lua (AGPL-3.0): power off without
# init's help, and restart through init. Neither returns: power off is
# immediate, and init stops everything else while it restarts.
#
# Installed as /mnt/onboard/.adds/simple-books/power.sh. See
# docs/reader-kobo.md.

PATH="/sbin:/bin:/usr/sbin:/usr/bin"
APP_DIR="/mnt/onboard/.adds/simple-books"
LOG="${APP_DIR}/reader.log"

case "$1" in
    88) what="powering off" ;;
    89) what="restarting" ;;
    *) exit 0 ;;
esac

echo "$(date +%s) power.sh: ${what}" >>"${LOG}"
"${APP_DIR}/wifi.sh" down
sync
if [ "$1" = 88 ]; then
    poweroff -f
else
    reboot
fi
# Until init stops this, so nothing goes on to start Nickel.
while :; do
    sleep 60
done
