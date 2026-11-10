#!/bin/sh
# Starts or stops an SSH server from Nickelite, so the Kobo can be reached
# from Nickel without opening KOReader. It runs KOReader's own patched
# dropbear the way KOReader's SSH plugin does: from KOReader's folder, so
# it uses the same host keys under settings/SSH, on port 2222, with -n for
# passwordless root logins, and the same pid file, so KOReader sees it as
# its own and can stop it too.
#
#   ssh.sh start | stop
#
# Anyone on the network can log in while it runs. reader.sh stops it when
# the reader starts, since the reader turns Wi-Fi off; a restart stops it
# too. Installed as /mnt/onboard/.adds/simple-books/ssh.sh. See
# docs/reader-kobo.md.

PATH="/sbin:/bin:/usr/sbin:/usr/bin"
KOREADER="/mnt/onboard/.adds/koreader"
PID_FILE="/tmp/dropbear_koreader.pid"

running() {
    [ -e "${PID_FILE}" ] && kill -0 "$(cat "${PID_FILE}")" 2>/dev/null
}

case "$1" in
    start)
        running && exit 0
        cd "${KOREADER}" || exit 1
        [ -x ./dropbear ] || exit 1
        # Kobo's init scripts don't set up pseudoterminals.
        if [ ! -d /dev/pts ]; then
            mkdir -p /dev/pts
            mount -t devpts devpts /dev/pts
        fi
        exec ./dropbear -E -R -p 2222 -P "${PID_FILE}" -n
        ;;
    stop)
        if running; then
            kill -TERM "$(cat "${PID_FILE}")"
        fi
        rm -f /tmp/dropbear_koreader.pid
        ;;
    *)
        echo "usage: ssh.sh start | stop" >&2
        exit 2
        ;;
esac
