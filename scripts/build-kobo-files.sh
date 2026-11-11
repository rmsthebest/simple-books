#!/bin/sh
# Builds everything Simple Books puts on a Kobo, laid out as on its drive:
#
#   target/kobo-files/.adds/simple-books/   the reader, its SSH server and
#                                           scripts, the English–Swedish
#                                           dictionary, `version`, and the
#                                           plugin again, for updates
#   target/kobo-files/.kobo/Kobo.tgz        the Nickel plugin, which the
#                                           Kobo unpacks into /usr/local/Kobo
#                                           when it next starts
#
# The website's installer copies these onto the Kobo, and
# scripts/kobo-deploy.sh copies them over SSH. The server image is built
# with it too. Set KOBO_FILES to stage somewhere else.
set -eu

cd "$(dirname -- "$0")/.."
target=armv7-unknown-linux-musleabihf
out="${KOBO_FILES:-target/kobo-files}"
app="${out}/.adds/simple-books"

cargo build --quiet --release -p reader-kobo -p kobo-ssh --target "${target}"
plugin="$(sh scripts/build-kobo-plugin.sh)"

stage="target/kobo-plugin/stage"
mkdir -p "${app}" "${out}/.kobo" "${stage}/imageformats"
for binary in reader-kobo simple-books-ssh; do
    if command -v llvm-strip >/dev/null; then
        llvm-strip -o "${app}/${binary}" "target/${target}/release/${binary}"
    else
        cp "target/${target}/release/${binary}" "${app}/${binary}"
    fi
done
# Made from Folkets lexikon (CC BY-SA 2.5); see crates/dictionary/data.
cargo run --quiet --release -p dictionary --features convert --bin build-dictionary -- \
    crates/dictionary/data/folkets_en_sv_public.xml.gz "${app}/dict-en-sv" >&2
for script in reader.sh wifi.sh boot.sh boot-install.sh power.sh; do
    cp "kobo/${script}" "${app}/${script}"
done
chmod 755 "${app}/reader-kobo" "${app}/simple-books-ssh" "${app}/reader.sh" "${app}/wifi.sh" \
    "${app}/boot.sh" "${app}/boot-install.sh" "${app}/power.sh"
# The commit, or without git (as in the image's build) the server's version.
git describe --always --dirty >"${app}/version" 2>/dev/null ||
    sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1 >"${app}/version"

cp "${plugin}" "${stage}/imageformats/libsimplebooks.so"
chmod 755 "${stage}/imageformats/libsimplebooks.so"
# A reader updating itself gets the app folder's files only, so the
# plugin is there too, for it to put in place.
cp "${plugin}" "${app}/libsimplebooks.so"
chmod 755 "${app}/libsimplebooks.so"
# Only the file, so unpacking leaves the folder Nickel's plugins are in as
# it is. Owned by root, as the rest of /usr/local/Kobo is, and with no
# timestamp, so the same build gives the same file.
tar -C "${stage}" --owner=0 --group=0 --numeric-owner --mtime=@0 \
    -cf - imageformats/libsimplebooks.so | gzip -n -9 >"${out}/.kobo/Kobo.tgz"
echo "${out}"
