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
# Connect, when plugged into a computer, shares the storage over USB and
# comes back to the reader, without Nickel. Update starts the new version,
# and the last one again if the new one fails before home.
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

# Nothing of ours keeps a directory on the storage in use.
cd / || exit 0

say() {
    echo "$(date +%s) boot.sh: $*" >>"${LOG}"
}

# The reader exits with this when Connect is tapped: share the storage with
# the computer, then start the reader again.
EXIT_USB=90
# And with this once Update, in Settings, Kobo, has put a new version in
# place: start it. It leaves update-trying until the new version is up.
EXIT_UPDATE=91

# Puts back what the last update replaced, which it kept in previous/, when
# the new version never got as far as home.
roll_back() {
    say "the update never got as far as home: back to the last version"
    plugin=false
    [ -f "${APP_DIR}/previous/libsimplebooks.so" ] && plugin=true
    for old in "${APP_DIR}"/previous/*; do
        [ -f "${old}" ] && mv -f "${old}" "${APP_DIR}/${old##*/}"
    done
    if [ "${plugin}" = true ]; then
        cp -f "${APP_DIR}/libsimplebooks.so" /usr/local/Kobo/imageformats/libsimplebooks.so.new &&
            chmod 755 /usr/local/Kobo/imageformats/libsimplebooks.so.new &&
            mv -f /usr/local/Kobo/imageformats/libsimplebooks.so.new \
                /usr/local/Kobo/imageformats/libsimplebooks.so
    fi
    rm -f "${APP_DIR}/update-trying"
    sync
}

# Shares the storage over USB until the computer ejects it or the cable
# comes out, from a copy of the reader in /tmp: the storage is unmounted
# while the computer has it, which anything running from it would stop.
share_over_usb() {
    # Nothing of the reader's may keep using the storage. Its SSH server
    # stops once the reader has gone, but not quite at once.
    tries=0
    while pkill -0 -f /simple-books-ssh && [ ${tries} -lt 20 ]; do
        usleep 250000
        tries=$((tries + 1))
    done
    pkill -KILL -f /simple-books-ssh
    "${APP_DIR}/wifi.sh" down
    if ! cp -f "${APP_DIR}/reader-kobo" /tmp/simple-books-usb; then
        say "no room in /tmp to share the storage over USB"
        return
    fi
    chmod 755 /tmp/simple-books-usb
    sync
    /tmp/simple-books-usb --usb --dir "${APP_DIR}" </dev/null >/dev/null 2>&1
    rm -f /tmp/simple-books-usb
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
set -- --boot
while :; do
    "${APP_DIR}/reader-kobo" "$@" </dev/null >/dev/null 2>&1
    status=$?
    say "reader-kobo exited with ${status}"
    # Not --boot again: it's done what the boot needed.
    if [ "${status}" -eq "${EXIT_UPDATE}" ]; then
        set --
        continue
    fi
    if [ -e "${APP_DIR}/update-trying" ]; then
        roll_back
        set --
        continue
    fi
    [ "${status}" -eq "${EXIT_USB}" ] || break
    share_over_usb
    set -- --after-usb
done
# It returned, so it didn't hang. The reader removes this too, once home is
# up.
rm -f "${APP_DIR}/boot-started"
# Nothing of ours is left on for Nickel: Wi-Fi (Nickel doesn't like
# finding it up), or the screen's 8-bit mode. SSH stopped with the reader.
"${APP_DIR}/wifi.sh" down
# Power off (88) or restart (89), from the reader's power screen. Neither
# comes back here, so rc.local doesn't go on to start Nickel.
"${APP_DIR}/power.sh" "${status}"
"${APP_DIR}/reader-kobo" --restore-screen >>"${LOG}" 2>&1
sync
exit 0
