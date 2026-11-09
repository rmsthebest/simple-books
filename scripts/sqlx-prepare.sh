#!/bin/sh
# Regenerates the .sqlx/ query cache. Run after changing a query or migration.
# With --check, fails if the committed cache is out of date instead.
set -eu
cd "$(dirname "$0")/.."

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT HUP INT TERM
url="sqlite://$dir/prepare.sqlite"

cargo sqlx database create --database-url "$url"
cargo sqlx migrate run --database-url "$url"
# The query macros record queries only when they run. If Cargo reuses an earlier
# build, prepare finds none and would empty the cache.
touch src/lib.rs
DATABASE_URL="$url" cargo sqlx prepare --workspace "$@" --database-url "$url" -- --all-targets --features ssr
