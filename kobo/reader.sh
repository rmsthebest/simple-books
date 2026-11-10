#!/bin/sh
# Starts the simple-books reader in place of a running Nickel, and always
# brings Nickel back when the reader exits, however it exits. The helper
# our Nickel plugin starts runs it (`reader-kobo --in-nickel`), after a
# web install and when a computer is unplugged.
#
# Adapted from KOReader's platform/kobo/koreader.sh and nickel.sh
# (AGPL-3.0), with the firmware 6 fixes from idetedi's fork recorded in
# docs/koreader-clara-bw.md. Only the parts the Clara BW on firmware 6 needs
# are kept.
#
# Installed as /mnt/onboard/.adds/simple-books/reader.sh, beside the
# reader-kobo binary. See docs/reader-kobo.md.

PATH="/sbin:/bin:/usr/sbin:/usr/bin"
APP_DIR="/mnt/onboard/.adds/simple-books"
LOG="${APP_DIR}/reader.log"

# Run a copy from /tmp, so replacing this file mid-run can't confuse sh.
if [ "$(dirname -- "$0")" != "/tmp" ]; then
    cp -f "$0" /tmp/simple-books-reader.sh
    chmod 755 /tmp/simple-books-reader.sh
    exec /tmp/simple-books-reader.sh "$@"
fi

say() {
    echo "$(date +%s) reader.sh: $*" >>"${LOG}"
}

# Waits up to 5 s for every process named $1 to exit.
wait_for_exit() {
    tries=0
    while pkill -0 "$1" && [ ${tries} -lt 20 ]; do
        usleep 250000
        tries=$((tries + 1))
    done
}

# The reader turns Wi-Fi on only while it syncs, and Nickel doesn't like
# finding Wi-Fi up when it starts. Safe to run when it's already off.
wifi_off() {
    if [ -n "${WIFI_MODULE}" ] && grep -q "^${WIFI_MODULE} " /proc/modules; then
        killall -q -TERM restore-wifi-async.sh enable-wifi.sh obtain-ip.sh
        "${APP_DIR}/wifi.sh" down
    fi
}

# Firmware 6 starts Nickel from /etc/rc.local, so this runs it again, after
# clearing what would stop it starting.
restart_nickel() {
    cd / || return
    if [ -e /etc/init.d/display-init.sh ] && [ -e /usr/libexec/platform/utils.sh ]; then
        # shellcheck disable=SC1091
        . /usr/libexec/platform/utils.sh
        PRODUCT="$(get_product_name)"
        export PRODUCT
        animator.sh /etc/images/"${PRODUCT}"-on-*.raw.gz &
    fi
    [ -e /etc/init.d/on-animator.sh ] && /etc/init.d/on-animator.sh &
    wifi_off
    unset LD_LIBRARY_PATH
    # rc.local runs under sh -e, so a daemon it starts that's still alive
    # would stop it before it reaches Nickel.
    for daemon in pulseaudio speech-dispatcher; do
        if grep -q "${daemon}" /etc/rc.local; then
            killall -q "${daemon}"
            wait_for_exit "${daemon}"
        fi
    done
    # Nickel makes the FIFO again, and rc.local replays the udev events
    # that queued up meanwhile.
    rm -f /run/kobo/udev/fifo /run/kobo/udev/lock
    sync
    # The boot-time multilog still holds /tmp/nickel-log's lock; a second
    # one would die and take Nickel with it.
    killall -q multilog
    wait_for_exit multilog
    say "restarting Nickel"
    /etc/rc.local </dev/null >/dev/null 2>&1
}

VIA_NICKEL=false
if pkill -0 nickel; then
    VIA_NICKEL=true
    # What restarting Nickel and Wi-Fi need from its setup.
    # shellcheck disable=SC2046
    export $(grep -s -E -e '^(DBUS_SESSION_BUS_ADDRESS|NICKEL_HOME|WIFI_MODULE|LANG|INTERFACE|PLATFORM)=' \
        "/proc/$(pidof -s nickel)/environ")
    if [ -e /sys/devices/platform/bt/rfkill/rfkill0/state ]; then
        echo 0 >/sys/devices/platform/bt/rfkill/rfkill0/state
    fi
    sync
    say "stopping Nickel"
    killall -q -TERM nickel hindenburg sickel fickel strickel fontickel adobehost foxitpdf \
        iink dhcpcd-dbus dhcpcd bluealsa bluetoothd fmon nanoclock.lua memorylogger \
        QtWebEngineProcess
    wait_for_exit nickel
    rm -f /tmp/nickel-hardware-status /run/kobo/udev/fifo
    # Bringing Nickel back runs rc.local again, which starts a new session
    # bus, and the new Nickel starts another wmt_launcher. Without this,
    # every trip to the reader left one of each behind. The system bus
    # stays, and so does the first wmt_launcher, for the Wi-Fi chip.
    for pid in $(pidof dbus-daemon); do
        if grep -q -- --session "/proc/${pid}/cmdline" 2>/dev/null; then
            kill "${pid}"
        fi
    done
    for pid in $(pidof wmt_launcher | tr ' ' '\n' | sort -n | sed 1d); do
        kill "${pid}"
    done
    wifi_off
fi

"${APP_DIR}/reader-kobo" "$@" </dev/null >/dev/null 2>&1
status=$?
say "reader-kobo exited with ${status}"
# SSH, if the reader turned it on, went with it: the server stops when
# the reader does.
# Power off (88) or restart (89), from the reader's power screen. It drew
# what the panel keeps showing, so the screen is left as it is.
"${APP_DIR}/power.sh" "${status}"
# Nickel's screen settings, if the reader didn't get to restore them.
"${APP_DIR}/reader-kobo" --restore-screen >>"${LOG}" 2>&1

if [ "${VIA_NICKEL}" = true ]; then
    restart_nickel &
fi
exit 0
