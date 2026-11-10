#!/bin/sh
# Turns Wi-Fi on and off for the simple-books reader, which runs with Nickel
# stopped. It connects to the networks Nickel saved, from Nickel's
# wpa_supplicant.conf, which is passed by path and never read here.
#
#   wifi.sh up        connects and gets an address; fails after about 50 s
#   wifi.sh down      disconnects and powers the chip off; safe when it's off
#   wifi.sh address   prints the Wi-Fi address, if it has one
#
# Adapted from KOReader's platform/kobo/enable-wifi.sh, obtain-ip.sh,
# release-ip.sh and disable-wifi.sh (AGPL-3.0), keeping only the MediaTek
# path the Clara BW uses (docs/clara-bw-hardware.md). The programs it starts
# get no terminal and write nowhere, so whoever runs this isn't kept
# waiting on their output. A problem is printed as one line, for the screen.
#
# Installed as /mnt/onboard/.adds/simple-books/wifi.sh. See
# docs/reader-kobo.md.

PATH="/sbin:/bin:/usr/sbin:/usr/bin"
WIFI_MODULE="${WIFI_MODULE:-wlan_drv_gen4m}"
INTERFACE="${INTERFACE:-wlan0}"

quiet() {
    "$@" </dev/null >/dev/null 2>&1
}

# Loads a driver module, if it isn't already. They're loaded at boot and
# never unloaded, so this is rarely needed.
load() {
    grep -q "^$1 " /proc/modules && return 0
    [ -n "${PLATFORM}" ] && quiet insmod "/drivers/${PLATFORM}/mt66xx/$1.ko"
    usleep 250000
    grep -q "^$1 " /proc/modules
}

associated() {
    wpa_cli -i "${INTERFACE}" status 2>/dev/null | grep -q '^wpa_state=COMPLETED'
}

down() {
    quiet dhcpcd -d -k "${INTERFACE}"
    quiet killall -q -TERM udhcpc default.script
    quiet wpa_cli -i "${INTERFACE}" terminate
    quiet ifconfig "${INTERFACE}" down
    # The driver stays loaded; the chip is powered off instead.
    [ -e /dev/wmtWifi ] && echo 0 >/dev/wmtWifi
    return 0
}

up() {
    if [ "${WIFI_MODULE}" != wlan_drv_gen4m ]; then
        echo "This Kobo's Wi-Fi isn't supported."
        return 1
    fi
    if associated && ifconfig "${INTERFACE}" | grep -q 'inet addr:'; then
        return 0
    fi
    for module in wmt_drv wmt_chrdev_wifi wmt_cdev_bt "${WIFI_MODULE}"; do
        if ! load "${module}"; then
            echo "The Wi-Fi driver isn't loaded. Turn Wi-Fi on once in Nickel."
            return 1
        fi
    done
    # Nickel starts this, so it isn't running yet when the reader starts at
    # boot. It looks after the Wi-Fi and Bluetooth chip.
    if ! pkill -0 wmt_launcher; then
        quiet wmt_launcher -p /lib/firmware &
        sleep 2
    fi
    # As Nickel does: the driver's debug interface, then power on.
    echo "0xDB9DB9" >/proc/driver/wmt_dbg
    echo "7 9 0" >/proc/driver/wmt_dbg
    sleep 1
    echo "0xDB9DB9" >/proc/driver/wmt_dbg
    echo "7 9 1" >/proc/driver/wmt_dbg
    echo 1 >/dev/wmtWifi
    sleep 1
    quiet ifconfig "${INTERFACE}" up

    # The networks moved here in firmware 5.
    conf=/mnt/onboard/.kobo/wpa_supplicant.conf
    extra=""
    if [ ! -f "${conf}" ]; then
        conf=/etc/wpa_supplicant/wpa_supplicant.conf
        extra="-s"
    fi
    if ! pkill -0 wpa_supplicant; then
        # shellcheck disable=SC2086
        quiet wpa_supplicant -D nl80211 ${extra} -i "${INTERFACE}" -c "${conf}" \
            -C /var/run/wpa_supplicant -B
    fi

    tries=0
    until associated; do
        tries=$((tries + 1))
        if [ ${tries} -gt 40 ]; then
            down
            echo "Couldn't join a Wi-Fi network. Check Wi-Fi works in Nickel."
            return 1
        fi
        usleep 500000
    done

    quiet dhcpcd -d -k "${INTERFACE}"
    quiet killall -q -TERM udhcpc default.script
    if [ -x /sbin/dhcpcd ]; then
        quiet dhcpcd -d -t 30 -w "${INTERFACE}"
    else
        quiet udhcpc -S -i "${INTERFACE}" -s /etc/udhcpc.d/default.script -x hostname:kobo -b -q
    fi
    if ! ifconfig "${INTERFACE}" | grep -q 'inet addr:'; then
        down
        echo "Joined the Wi-Fi network but got no address."
        return 1
    fi
}

case "$1" in
    up) up ;;
    down) down ;;
    address)
        ifconfig "${INTERFACE}" 2>/dev/null | sed -n 's/.*inet addr:\([0-9.]*\).*/\1/p'
        ;;
    *)
        echo "usage: wifi.sh up | down | address" >&2
        exit 2
        ;;
esac
