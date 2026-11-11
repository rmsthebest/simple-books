#!/bin/sh
# Turns Wi-Fi on and off for the simple-books reader, which runs with Nickel
# stopped. It connects to the networks Nickel saved, from Nickel's
# wpa_supplicant.conf, which is passed by path and never read here.
#
#   wifi.sh up        connects and gets an address; fails within about a minute
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

# Loads a driver module, if it isn't already. Nickel loads them when it
# starts and never unloads them, so this is needed before Nickel has run.
load() {
    grep -q "^$1 " /proc/modules && return 0
    [ -n "${PLATFORM}" ] && quiet insmod "/drivers/${PLATFORM}/mt66xx/$1.ko"
    usleep 250000
    grep -q "^$1 " /proc/modules
}

associated() {
    wpa_cli -i "${INTERFACE}" status 2>/dev/null | grep -q '^wpa_state=COMPLETED'
}

# Stops the DHCP client, keeping the lease: the next `up` asks for the same
# address again, which the router answers sooner than a new request. Without
# the address it's left on the interface (dhcpcd.conf says `persistent`), so
# a DHCP that fails next time can't pass for one that worked.
stop_dhcp() {
    quiet dhcpcd -d -x "${INTERFACE}"
    quiet killall -q -TERM udhcpc default.script
    quiet ip addr flush dev "${INTERFACE}"
}

# Turns the chip on, trying every tenth of a second, $1 times.
power_on() {
    tries=0
    until echo 1 >/dev/wmtWifi 2>/dev/null; do
        tries=$((tries + 1))
        [ ${tries} -ge "$1" ] && return 1
        usleep 100000
    done
}

# As Nickel does once it has started the launcher: power the chip down and
# up through the driver's debug interface. Only needed then: with the
# launcher already running, the chip turns on without it, 2.3 s sooner.
power_cycle() {
    echo "0xDB9DB9" >/proc/driver/wmt_dbg
    echo "7 9 0" >/proc/driver/wmt_dbg
    sleep 1
    echo "0xDB9DB9" >/proc/driver/wmt_dbg
    echo "7 9 1" >/proc/driver/wmt_dbg
}

down() {
    stop_dhcp
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
    # Nickel starts these once the drivers are in, so before it has run, at
    # boot, they aren't, and the chip refuses to turn on ("wmt lib not
    # ready"). wmt_loader sets the driver up and exits; wmt_launcher stays,
    # loading the chip's patch and looking after it. They get no terminal
    # and no pipe, so nothing waits on them.
    launched=""
    if ! pkill -0 wmt_launcher; then
        launched=1
        wmt_loader </dev/null >/dev/null 2>&1 &
        loader=$!
        tries=0
        while kill -0 "${loader}" 2>/dev/null && [ ${tries} -lt 20 ]; do
            usleep 250000
            tries=$((tries + 1))
        done
        wmt_launcher -p /lib/firmware </dev/null >/dev/null 2>&1 &
    fi
    # The driver refuses until the chip is ready: about a second and a half
    # after the launcher starts, at boot. Should the chip not turn on
    # without the power cycle, it gets one after all.
    if [ -n "${launched}" ] || ! power_on 10; then
        power_cycle
        if ! power_on 100; then
            echo "The Wi-Fi chip didn't turn on."
            return 1
        fi
    fi
    # The interface comes up once the chip is on: at once, on the Clara BW.
    tries=0
    until quiet ifconfig "${INTERFACE}" up; do
        tries=$((tries + 1))
        if [ ${tries} -gt 50 ]; then
            echo "The Wi-Fi chip didn't turn on."
            return 1
        fi
        usleep 100000
    done

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
        if [ ${tries} -gt 200 ]; then
            down
            echo "Couldn't join a Wi-Fi network. Check Wi-Fi works in Nickel."
            return 1
        fi
        usleep 100000
    done

    stop_dhcp
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
