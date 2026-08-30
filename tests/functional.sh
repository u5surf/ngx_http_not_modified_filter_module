#!/bin/sh
#
# Drives a real nginx with this module loaded and checks that conditional
# requests get the status codes they should.
#
# Usage:
#   tests/functional.sh <nginx-binary> <module-file>
#
# The module has to have been built against the same nginx, otherwise nginx
# refuses to load it with a version mismatch.

set -eu

nginx_bin=${1:?usage: $0 <nginx-binary> <module-file>}
module=${2:?usage: $0 <nginx-binary> <module-file>}

for f in "$nginx_bin" "$module"; do
    [ -f "$f" ] || { echo "not found: $f" >&2; exit 1; }
done

nginx_bin=$(cd "$(dirname "$nginx_bin")" && pwd)/$(basename "$nginx_bin")
module=$(cd "$(dirname "$module")" && pwd)/$(basename "$module")

port=${TEST_PORT:-8099}
prefix=$(mktemp -d)
trap 'if [ -f "$prefix/logs/nginx.pid" ]; then kill "$(cat "$prefix/logs/nginx.pid")" 2>/dev/null || true; fi; rm -rf "$prefix"' EXIT INT TERM

mkdir -p "$prefix/conf" "$prefix/logs" "$prefix/html"
printf 'hello from the rust filter\n' > "$prefix/html/index.html"

cat > "$prefix/conf/nginx.conf" <<EOF
daemon on;
load_module $module;
error_log $prefix/logs/error.log debug;
pid $prefix/logs/nginx.pid;
events { worker_connections 64; }
http {
    access_log off;
    server {
        listen 127.0.0.1:$port;
        root $prefix/html;
        etag on;
        location / { }
    }
}
EOF

"$nginx_bin" -p "$prefix" -c conf/nginx.conf

url="http://127.0.0.1:$port/index.html"

# Wait for the listener rather than sleeping a fixed amount.
i=0
while [ "$i" -lt 50 ]; do
    if curl -s -o /dev/null "$url" 2>/dev/null; then break; fi
    i=$((i + 1))
    sleep 0.1 2>/dev/null || sleep 1
done
[ "$i" -lt 50 ] || { echo "nginx did not come up" >&2; cat "$prefix/logs/error.log" >&2; exit 1; }

etag=$(curl -sI "$url" | tr -d '\r' | sed -n 's/^[Ee][Tt]ag: //p')
last_modified=$(curl -sI "$url" | tr -d '\r' | sed -n 's/^[Ll]ast-[Mm]odified: //p')

[ -n "$etag" ] || { echo "no ETag in response; is etag on?" >&2; exit 1; }
[ -n "$last_modified" ] || { echo "no Last-Modified in response" >&2; exit 1; }

failures=0
checked=0

check() {
    description=$1
    expected=$2
    shift 2

    actual=$(curl -s -o /dev/null -w '%{http_code}' "$@" "$url")
    checked=$((checked + 1))

    if [ "$actual" = "$expected" ]; then
        printf 'ok       %-42s %s\n' "$description" "$actual"
    else
        printf 'FAILED   %-42s expected %s, got %s\n' "$description" "$expected" "$actual"
        failures=$((failures + 1))
    fi
}

check "plain GET"                     200
check "If-None-Match: <etag>"         304 -H "If-None-Match: $etag"
check "If-None-Match: bogus"          200 -H 'If-None-Match: "bogus"'
check "If-None-Match: *"              304 -H 'If-None-Match: *'
check "If-None-Match: W/<etag>"       304 -H "If-None-Match: W/$etag"
check "If-None-Match: list with match" 304 -H "If-None-Match: \"aaa\", $etag, \"bbb\""
check "If-None-Match: list no match"  200 -H 'If-None-Match: "aaa", "bbb"'
check "If-Modified-Since: <lm>"       304 -H "If-Modified-Since: $last_modified"
check "If-Modified-Since: past"       200 -H 'If-Modified-Since: Mon, 01 Jan 1990 00:00:00 GMT'
check "If-Match: <etag>"              200 -H "If-Match: $etag"
check "If-Match: bogus"               412 -H 'If-Match: "bogus"'
check "If-Match: *"                   200 -H 'If-Match: *'
check "If-Unmodified-Since: past"     412 -H 'If-Unmodified-Since: Mon, 01 Jan 1990 00:00:00 GMT'
check "If-Unmodified-Since: future"   200 -H 'If-Unmodified-Since: Fri, 01 Jan 2099 00:00:00 GMT'

echo

# The built-in not_modified filter would produce the same status codes, so the
# debug log is what proves this module is the one deciding. It only logs the
# comparisons when it is entered with a 200 still in place.
entered=$(grep -c 'rust not_modified: entry status:200' "$prefix/logs/error.log" || true)
if [ "$entered" -eq 0 ]; then
    echo "FAILED   the module never ran ahead of the built-in filter" >&2
    failures=$((failures + 1))
else
    echo "ok       module decided $entered responses ahead of the built-in filter"
fi

echo
if [ "$failures" -eq 0 ]; then
    echo "$checked checks passed"
else
    echo "$failures of $checked checks failed" >&2
    echo "--- error.log (last 40 lines) ---" >&2
    tail -40 "$prefix/logs/error.log" >&2
    exit 1
fi
