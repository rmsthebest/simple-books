#!/bin/sh
# Builds Simple Books for the Kobo and installs it over SSH, for
# development: what the website's installer copies, and optionally books.
#
#   scripts/kobo-deploy.sh 192.168.1.94 [book.book ...]
#
# Turn SSH on in the reader's Sync screen first; it listens on port 2222,
# and a key in .adds/simple-books/authorized_keys saves typing the
# password. KOBO_SSH replaces the ssh command, for other options or a
# known_hosts file; the host is still the first argument.
#
# The Nickel plugin goes straight into /usr/local/Kobo/imageformats, where
# Nickel loads it when it next starts, rather than through .kobo/Kobo.tgz
# and a restart. KOBO_PLUGIN=0 leaves it out. See docs/reader-kobo.md.
set -eu

host="${1:?usage: scripts/kobo-deploy.sh <kobo address> [book.book ...]}"
shift
ssh_command="${KOBO_SSH:-ssh -p 2222}"

cd "$(dirname -- "$0")/.."
files="$(sh scripts/build-kobo-files.sh)"

# One tar stream for the app folder. Files are replaced, never deleted, so
# the reader's own files there (its place, pairing and settings) stay.
tar -C "${files}/.adds" -cf - simple-books |
    ${ssh_command} "root@${host}" 'tar -C /mnt/onboard/.adds -xf - && mkdir -p /mnt/onboard/.adds/simple-books/books'
if [ "${KOBO_PLUGIN:-1}" != 0 ]; then
    # Written beside it and renamed over it, so a running Nickel keeps the
    # one it loaded.
    gzip -dc "${files}/.kobo/Kobo.tgz" | tar -xOf - imageformats/libsimplebooks.so |
        ${ssh_command} "root@${host}" 'cat >/usr/local/Kobo/imageformats/libsimplebooks.so.new &&
            chmod 755 /usr/local/Kobo/imageformats/libsimplebooks.so.new &&
            mv /usr/local/Kobo/imageformats/libsimplebooks.so.new /usr/local/Kobo/imageformats/libsimplebooks.so'
fi
for book in "$@"; do
    name="$(basename -- "${book}")"
    ${ssh_command} "root@${host}" "cat > '/mnt/onboard/.adds/simple-books/books/${name}'" <"${book}"
done
${ssh_command} "root@${host}" 'sync && cat /mnt/onboard/.adds/simple-books/version && ls -la /mnt/onboard/.adds/simple-books/books'
