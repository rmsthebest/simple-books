#!/bin/sh
# Checks that a deployed stack delivers Kobo responses intact, imitating Nickel.
# Run this from outside your network, before touching the device.
#
#   scripts/edge-check.sh https://books.example.com <kobo-token>
#
# It tests whichever stack is live. If the responses show Cloudflare, it also
# checks for the ways Cloudflare can interfere.
set -eu

url=${1:?usage: edge-check.sh <public-url> <kobo-token>}
token=${2:?usage: edge-check.sh <public-url> <kobo-token>}
base="${url%/}/kobo/$token"
ua='Mozilla/5.0 (Linux; U; Android 2.0; en-us;) AppleWebKit/538.1 (KHTML, like Gecko) Version/4.0 Mobile Safari/538.1 (Kobo Touch 0386/6.0.279225)'

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
failures=0

fail() {
    # The token never goes in the output.
    printf 'FAIL %s: %s\n' "$1" "$2"
    failures=$((failures + 1))
}

fetch() { # method path
    curl -sS -X "$1" -A "$ua" -H 'Accept-Encoding: gzip' -D "$tmp/headers" -o "$tmp/body" "$base/$2"
}

header() {
    tr -d '\r' <"$tmp/headers" | grep -i "^$1:" | tail -1 | cut -d: -f2- | sed 's/^ *//' || true
}

check_common() { # name
    status=$(tr -d '\r' <"$tmp/headers" | grep '^HTTP/' | tail -1 | cut -d' ' -f2)
    [ "$status" = 200 ] || fail "$1" "status $status"

    length=$(header content-length)
    actual=$(wc -c <"$tmp/body" | tr -d ' ')
    if [ -z "$length" ]; then
        fail "$1" "no Content-Length (Nickel needs it)"
    elif [ "$length" != "$actual" ]; then
        fail "$1" "Content-Length is $length, body is $actual bytes"
    fi

    [ -z "$(header transfer-encoding)" ] || fail "$1" "chunked: $(header transfer-encoding)"
    [ -z "$(header content-encoding)" ] || fail "$1" "compressed: $(header content-encoding)"

    case "$(header cache-control)" in
    *no-transform*) ;;
    *) fail "$1" "Cache-Control lacks no-transform: $(header cache-control)" ;;
    esac

    if [ -n "$(header cf-ray)" ]; then
        seen_cloudflare=1
        [ "$(header cf-cache-status)" != HIT ] || fail "$1" "served from Cloudflare's cache"
        [ -z "$(header cf-mitigated)" ] || fail "$1" "challenged by Cloudflare: $(header cf-mitigated)"
        if grep -q '/cdn-cgi/' "$tmp/body"; then
            fail "$1" "Cloudflare altered or replaced the body"
        fi
    fi
}

check_json() { # name
    check_common "$1"
    case "$(header content-type)" in
    application/json*) ;;
    *) fail "$1" "not JSON: $(header content-type)" ;;
    esac
    python3 -c 'import json,sys; json.load(open(sys.argv[1]))' "$tmp/body" 2>/dev/null \
        || fail "$1" "body doesn't parse as JSON"
}

seen_cloudflare=0

fetch GET v1/initialization
check_json initialization
[ "$(header x-kobo-apitoken)" = 'e30=' ] || fail initialization "missing x-kobo-apitoken"

fetch POST v1/auth/device
check_json auth

fetch GET v1/library/sync
check_json sync
[ -n "$(header x-kobo-synctoken)" ] || fail sync "missing x-kobo-synctoken"
# Later fetches overwrite the body, and the download check below needs this one.
cp "$tmp/body" "$tmp/sync"

fetch GET v1/some/unknown/route
check_json catch-all

# Check a real download, if the library has a book.
download=$(python3 -c '
import json, sys
for entry in json.load(open(sys.argv[1])):
    new = entry.get("NewEntitlement") or entry.get("ChangedEntitlement") or {}
    urls = new.get("BookMetadata", {}).get("DownloadUrls") or []
    if urls:
        print(urls[0]["Url"].split("/kobo/", 1)[1].split("/", 1)[1], urls[0]["Size"])
        break
' "$tmp/sync")

if [ -n "$download" ]; then
    path=${download% *}
    size=${download##* }
    fetch GET "$path"
    check_common download
    actual=$(wc -c <"$tmp/body" | tr -d ' ')
    [ "$actual" = "$size" ] || fail download "body is $actual bytes, metadata promised $size"
else
    printf 'SKIP download: the library has no books yet\n'
fi

if [ "$seen_cloudflare" = 1 ]; then
    printf 'Responses came through Cloudflare.\n'
else
    printf 'Responses did not come through Cloudflare.\n'
fi

if [ "$failures" -gt 0 ]; then
    printf '%d check(s) failed.\n' "$failures"
    exit 1
fi
printf 'All checks passed.\n'
