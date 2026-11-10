#!/bin/sh
# Builds libsimplebooks.so, the Nickel plugin (crates/kobo-plugin), for the
# Kobo, and checks it's what Nickel can load: a 32-bit ARM hard-float shared
# object with Qt's metadata note, its two Qt functions, a constructor, and
# no libraries or interpreter of its own.
#
#   scripts/build-kobo-plugin.sh      prints the path of the library
#
# A no_std cdylib for the musl target needs static linking off and panics
# that abort, for this crate alone, so it builds in its own target folder.
set -eu

cd "$(dirname -- "$0")/.."
target=armv7-unknown-linux-musleabihf
out="target/kobo-plugin"
CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="-C target-feature=-crt-static -C panic=abort" \
    cargo build --quiet --release -p kobo-plugin --target "${target}" --target-dir "${out}"
lib="${out}/${target}/release/libsimplebooks.so"

fail() {
    echo "build-kobo-plugin: ${lib}: $*" >&2
    exit 1
}

readelf -h "${lib}" | grep -q 'Type:.*DYN' || fail "not a shared object"
readelf -h "${lib}" | grep -q 'Machine:.*ARM' || fail "not ARM"
readelf -h "${lib}" | grep -q 'hard-float ABI' || fail "not hard-float"
readelf -S "${lib}" | grep -q '\.note\.qt\.metadata *NOTE' || fail "no Qt metadata note"
readelf -S "${lib}" | grep -q '\.init_array' || fail "no constructor"
if readelf -d "${lib}" | grep -q NEEDED; then
    fail "needs a library"
fi
if readelf -l "${lib}" | grep -q INTERP; then
    fail "has an interpreter"
fi
for symbol in qt_plugin_query_metadata_v2 qt_plugin_instance; do
    nm -D --defined-only "${lib}" | grep -q " T ${symbol}\$" || fail "doesn't export ${symbol}"
done
echo "${lib}"
