#!/bin/sh
# Tests kobo/boot-install.sh against a made-up rc.local: on adds exactly one
# line after the boot animation is killed, on again changes nothing, off
# gives back the original byte for byte, and an rc.local it doesn't know is
# left alone. Run by scripts/check.sh.
set -eu

cd "$(dirname -- "$0")/.."
dir=target/boot-install-test
mkdir -p "${dir}/app" "${dir}/aux"
# Left from the last run, if any. Everything else is overwritten.
rm -f target/boot-install-test/app/boot target/boot-install-test/aux/rc.local.orig

cat >"${dir}/stock" <<'EOF'
#!/bin/sh -e
export DBUS_SESSION_BUS_ADDRESS=`dbus-daemon --session --print-address --fork`
killall animator.sh
pulseaudio --system --daemonize --disallow-exit
(/usr/local/Kobo/nickel -platform kobo 2>&1 | multilog t /tmp/nickel-log s32768 n2) &
exit 0
EOF
cp "${dir}/stock" "${dir}/rc.local"

RC_LOCAL="${dir}/rc.local"
SAVED="${dir}/aux/rc.local.orig"
APP_DIR="${dir}/app"
KNOWN_RC_LOCAL="0000 $(sha256sum "${dir}/stock" | cut -d ' ' -f 1)"
export RC_LOCAL SAVED APP_DIR KNOWN_RC_LOCAL

fail() {
    echo "boot-install: $*" >&2
    exit 1
}

sh kobo/boot-install.sh on >/dev/null || fail "on failed"
[ -f "${dir}/app/boot" ] || fail "on didn't create boot"
cmp -s "${dir}/stock" "${SAVED}" || fail "on didn't keep the original"
[ -x "${dir}/rc.local" ] || fail "rc.local isn't executable"
line="[ -x ${dir}/app/boot.sh ] && ${dir}/app/boot.sh || true # simple-books"
[ "$(sed -n 4p "${dir}/rc.local")" = "${line}" ] || fail "the line isn't after killall animator.sh"
[ "$(wc -l <"${dir}/rc.local")" = 7 ] || fail "on added more than one line"
sh -n "${dir}/rc.local" || fail "rc.local isn't valid sh"

cp "${dir}/rc.local" "${dir}/once"
sh kobo/boot-install.sh on >/dev/null || fail "on again failed"
cmp -s "${dir}/once" "${dir}/rc.local" || fail "on again changed rc.local"

sh kobo/boot-install.sh off >/dev/null || fail "off failed"
[ ! -e "${dir}/app/boot" ] || fail "off left boot"
cmp -s "${dir}/stock" "${dir}/rc.local" || fail "off didn't restore rc.local"
sh kobo/boot-install.sh off >/dev/null || fail "off again failed"

# An rc.local it doesn't know: on refuses and touches nothing.
echo "# changed" >>"${dir}/rc.local"
cp "${dir}/rc.local" "${dir}/changed"
if sh kobo/boot-install.sh on >/dev/null; then
    fail "on accepted an unknown rc.local"
fi
cmp -s "${dir}/changed" "${dir}/rc.local" || fail "on changed an unknown rc.local"
[ ! -e "${dir}/app/boot" ] || fail "on created boot for an unknown rc.local"

echo "boot-install.sh: ok"
