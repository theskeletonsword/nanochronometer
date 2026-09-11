#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Signs the built drivers with the test certificate using osslsigncode
# (Linux side). Run `make all` first, then `make sign`.
#
# Output: build/signed/nanochrono_{x64,arm64}.sys
#
# The .pfx is created by certs/make-test-cert.sh and is never committed.

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"

OUT="build/signed"
KEYS="certs-private"

command -v osslsigncode >/dev/null || {
    echo "osslsigncode not found (dnf install osslsigncode)" >&2
    exit 1
}

if [[ ! -f "$KEYS/nanochrono-test.pfx" ]]; then
    echo "== generating test certificate ($KEYS/ is gitignored) =="
    certs/make-test-cert.sh
fi

mkdir -p "$OUT"

for f in build/nanochrono_x64.sys build/nanochrono_arm64.sys; do
    [[ -f "$f" ]] || { echo "missing $f — run 'make all' first" >&2; exit 1; }
    base="$(basename "$f")"
    echo "== signing $f"
    rm -f "$OUT/$base"
    osslsigncode sign \
        -certs "$KEYS/nanochrono-test.pem" \
        -key "$KEYS/nanochrono-test.key" \
        -h sha256 \
        -n "NanoChronometer test driver" \
        -i "https://github.com/skels/nanochronometer" \
        -in "$f" \
        -out "$OUT/$base"
done

echo "== verifying signatures"
for f in "$OUT"/*.sys; do
    echo "--- $f"
    osslsigncode verify -CAfile "$KEYS/nanochrono-test.pem" "$f" | grep -E "Signature verification|Verifying|error|ok" | head -n 6
done

echo "== done: signed drivers in $OUT"