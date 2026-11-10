#!/bin/sh
# Turns booting into the simple-books reader on and off: the toggle on the
# reader's Kobo screen, and the website's installer, through the helper
# our Nickel plugin starts (`reader-kobo --in-nickel`).
#
#   boot-install.sh on    adds one line to /etc/rc.local, and creates `boot`
#   boot-install.sh off   removes `boot`, and puts back the stock rc.local
#
# The line runs boot.sh before Nickel starts; see boot.sh for when it
# doesn't start the reader. rc.local runs under `sh -e`, so the line can't
# fail: if boot.sh is missing it does nothing, and Nickel starts as always.
# Only an rc.local with a known hash is changed, and the copy kept on
# /mnt/aux has to match one too, so nothing else is ever written there.
# What it did is printed, for the reader's Kobo screen and reader.log.
#
# RC_LOCAL, SAVED, APP_DIR and KNOWN_RC_LOCAL are for tests. See
# docs/reader-kobo.md.

PATH="/sbin:/bin:/usr/sbin:/usr/bin"
RC_LOCAL="${RC_LOCAL:-/etc/rc.local}"
SAVED="${SAVED:-/mnt/aux/simple-books/rc.local.orig}"
APP_DIR="${APP_DIR:-/mnt/onboard/.adds/simple-books}"
# SHA-256 of the stock rc.local, by firmware: 6.0.279225.
KNOWN_RC_LOCAL="${KNOWN_RC_LOCAL:-1ea9a76b94ed759ec181b2dea830e6f6edcd2b890b41c2ba410bf818c7b4b958}"
MARK="# simple-books"
LINE="[ -x ${APP_DIR}/boot.sh ] && ${APP_DIR}/boot.sh || true ${MARK}"
# The line goes after this one: once the boot animation is gone, and
# before rc.local starts the daemons that the reader doesn't run with.
AFTER="killall animator.sh"

known() {
    sum="$(sha256sum "$1" | cut -d ' ' -f 1)"
    for good in ${KNOWN_RC_LOCAL}; do
        [ "${sum}" = "${good}" ] && return 0
    done
    return 1
}

ours() {
    grep -q -F -x "${LINE}" "${RC_LOCAL}"
}

# Replaces rc.local with $1 in one step, keeping it executable.
replace() {
    chmod 755 "$1" && sync && mv -f "$1" "${RC_LOCAL}" && sync
}

on() {
    if ours; then
        : >"${APP_DIR}/boot"
        sync
        echo "Simple Books already starts when the Kobo is switched on."
        return 0
    fi
    if ! known "${RC_LOCAL}"; then
        echo "Not changed: this firmware's /etc/rc.local isn't one Simple Books knows."
        return 1
    fi
    mkdir -p "$(dirname -- "${SAVED}")" &&
        cp -f "${RC_LOCAL}" "${SAVED}.new" &&
        known "${SAVED}.new" &&
        mv -f "${SAVED}.new" "${SAVED}" || {
        echo "Not changed: couldn't keep a copy of /etc/rc.local."
        return 1
    }
    new="${RC_LOCAL}.simple-books"
    awk -v after="${AFTER}" -v line="${LINE}" '
        { print }
        $0 == after && !done { print line; done = 1 }
    ' "${RC_LOCAL}" >"${new}"
    # Exactly one line added, and taking it out gives back the original.
    if [ "$(grep -c -F -x "${LINE}" "${new}")" != 1 ] ||
        ! grep -v -F -x "${LINE}" "${new}" | cmp -s - "${RC_LOCAL}"; then
        rm -f "${new}"
        echo "Not changed: /etc/rc.local didn't look as expected."
        return 1
    fi
    replace "${new}" || {
        rm -f "${new}"
        echo "Not changed: couldn't write /etc/rc.local."
        return 1
    }
    : >"${APP_DIR}/boot"
    sync
    echo "Simple Books will start when the Kobo is switched on."
    echo "To start Nickel instead, keep a finger on the screen while it starts."
}

off() {
    rm -f "${APP_DIR}/boot"
    sync
    if ! ours; then
        echo "The Kobo already starts Nickel."
        return 0
    fi
    new="${RC_LOCAL}.simple-books"
    grep -v -F -x "${LINE}" "${RC_LOCAL}" >"${new}"
    if known "${new}"; then
        replace "${new}" && echo "The Kobo will start Nickel again." && return 0
    elif [ -f "${SAVED}" ] && known "${SAVED}"; then
        cp -f "${SAVED}" "${new}" &&
            replace "${new}" &&
            echo "The Kobo will start Nickel again, with the saved /etc/rc.local." &&
            return 0
    fi
    rm -f "${new}"
    echo "Couldn't put back /etc/rc.local, so it still runs boot.sh."
    echo "With no boot file boot.sh does nothing, and the Kobo starts Nickel."
    return 1
}

case "$1" in
    on) on ;;
    off) off ;;
    *)
        echo "usage: boot-install.sh on | off" >&2
        exit 2
        ;;
esac
