#!/bin/sh
# Builds the reader for the Kobo and installs it over KOReader's SSH server,
# with the launch script and the Nickelite menu entry, and optionally books.
#
#   scripts/kobo-deploy.sh 192.168.1.94 [book.book ...]
#
# KOBO_SSH replaces the ssh command, for other options or a known_hosts
# file; the host is still the first argument. The app goes in one tar
# stream, and each book in one more. See docs/reader-kobo.md.
set -eu

host="${1:?usage: scripts/kobo-deploy.sh <kobo address> [book.book ...]}"
shift
ssh_command="${KOBO_SSH:-ssh -p 2222}"
target=armv7-unknown-linux-musleabihf

cd "$(dirname -- "$0")/.."
cargo build --release -p reader-kobo --target "${target}"

# Overwritten on every run, so nothing needs deleting.
staging="target/kobo-deploy"
mkdir -p "${staging}/simple-books" "${staging}/nm"
if command -v llvm-strip >/dev/null; then
    llvm-strip -o "${staging}/simple-books/reader-kobo" "target/${target}/release/reader-kobo"
else
    cp "target/${target}/release/reader-kobo" "${staging}/simple-books/reader-kobo"
fi
cp kobo/reader.sh "${staging}/simple-books/reader.sh"
cp kobo/ssh.sh "${staging}/simple-books/ssh.sh"
cp kobo/wifi.sh "${staging}/simple-books/wifi.sh"
cp kobo/nickelite-menu "${staging}/nm/simple-books"
chmod 755 "${staging}/simple-books/reader-kobo" "${staging}/simple-books/reader.sh" \
    "${staging}/simple-books/ssh.sh" "${staging}/simple-books/wifi.sh"

tar -C "${staging}" -cf - simple-books nm |
    ${ssh_command} "root@${host}" 'tar -C /mnt/onboard/.adds -xf - && mkdir -p /mnt/onboard/.adds/simple-books/books'
for book in "$@"; do
    name="$(basename -- "${book}")"
    ${ssh_command} "root@${host}" "cat > '/mnt/onboard/.adds/simple-books/books/${name}'" <"${book}"
done
${ssh_command} "root@${host}" 'sync && ls -la /mnt/onboard/.adds/simple-books /mnt/onboard/.adds/simple-books/books'
