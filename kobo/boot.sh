#!/bin/sh
# Starts the simple-books reader when the Kobo boots, before Nickel.
# /etc/rc.local runs it, from the line boot-install.sh adds, and starts
# Nickel as usual once this returns, however the reader exits. So any
# failure here ends in the stock boot.
#
# It returns at once, and the Kobo boots into Nickel, when:
#   - there's no `boot` in the app folder (Boot into Nickel removes it);
#   - the last boot started the reader, and it neither reached home nor
#     returned, because `boot-started` is still here: a hang and a forced
#     restart. The next boot tries the reader again.
# The reader itself returns at once if a finger is on the screen.
#
# Installed as /mnt/onboard/.adds/simple-books/boot.sh. See
# docs/reader-kobo.md.

PATH="/sbin:/bin:/usr/sbin:/usr/bin"
APP_DIR="/mnt/onboard/.adds/simple-books"
LOG="${APP_DIR}/reader.log"

# Run a copy from /tmp, so replacing this file mid-run can't confuse sh.
if [ "$(dirname -- "$0")" != "/tmp" ]; then
    cp -f "$0" /tmp/simple-books-boot.sh || exit 0
    chmod 755 /tmp/simple-books-boot.sh
    exec /tmp/simple-books-boot.sh "$@"
fi

say() {
    echo "$(date +%s) boot.sh: $*" >>"${LOG}"
}

[ -f "${APP_DIR}/boot" ] || exit 0
[ -x "${APP_DIR}/reader-kobo" ] || exit 0
if [ -e "${APP_DIR}/boot-started" ]; then
    rm -f "${APP_DIR}/boot-started"
    sync
    say "the last boot never reached home: Nickel this time"
    exit 0
fi

: >"${APP_DIR}/boot-started"
sync
say "starting the reader"
"${APP_DIR}/reader-kobo" --boot </dev/null >/dev/null 2>&1
status=$?
say "reader-kobo exited with ${status}"
# It returned, so it didn't hang. The reader removes this too, once home is
# up.
rm -f "${APP_DIR}/boot-started"
# Nothing of ours is left on for Nickel: SSH, Wi-Fi (Nickel doesn't like
# finding it up), or the screen's 8-bit mode.
"${APP_DIR}/ssh.sh" stop
"${APP_DIR}/wifi.sh" down
# Power off (88) or restart (89), from the reader's power screen. Neither
# comes back here, so rc.local doesn't go on to start Nickel.
"${APP_DIR}/power.sh" "${status}"
"${APP_DIR}/reader-kobo" --restore-screen >>"${LOG}" 2>&1
sync
exit 0
