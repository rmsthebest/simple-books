#!/bin/sh
# Everything that must pass before a change is done. CI runs this too.
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$1"; }

step "format"
cargo fmt --all --check

step "clippy (server)"
cargo clippy --workspace --all-targets --features ssr -- -D warnings

step "clippy (wasm)"
cargo clippy --lib --target wasm32-unknown-unknown --features hydrate -- -D warnings

step "tests"
cargo test --workspace --features ssr

step "reader engine links for the Kobo"
cargo build -p reader-core --examples --target armv7-unknown-linux-musleabihf

step "sync client builds for the Kobo"
cargo clippy -p reader-sync --target armv7-unknown-linux-musleabihf -- -D warnings

step "clippy (Kobo app)"
cargo clippy -p reader-kobo --target armv7-unknown-linux-musleabihf -- -D warnings

step "launch scripts parse"
sh -n kobo/reader.sh
sh -n kobo/ssh.sh
sh -n kobo/wifi.sh
sh -n kobo/boot.sh
sh -n kobo/boot-install.sh
sh -n scripts/test-boot-install.sh
sh scripts/test-boot-install.sh
sh -n scripts/kobo-deploy.sh

step "sqlx cache is current"
scripts/sqlx-prepare.sh --check

step "no raw HTML in views"
if grep -rn 'inner_html' src/; then
    echo "inner_html is banned: book metadata is untrusted." >&2
    exit 1
fi

step "supply chain"
cargo deny --features ssr check
cargo audit

printf '\nAll checks passed.\n'
