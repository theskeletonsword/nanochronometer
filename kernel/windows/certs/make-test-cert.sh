#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Generates a SELF-SIGNED TEST-ONLY certificate used to sign the driver.
#
# The private key lives in kernel/windows/certs-private/ which is gitignored;
# only the public certificate (certs/nanochrono-test.crt) is committed.
# Disable test driver signing enforcement in the guest before loading:
#   bcdedit /set testsigning on
#
# Creates:
#   certs-private/nanochrono-test.key   (private, never committed)
#   certs-private/nanochrono-test.pem   (public cert, PEM)
#   certs/nanochrono-test.crt           (public cert, DER — committed)
#   certs-private/nanochrono-test.pfx   (PKCS12 for sign.bat / signtool on Windows)

set -euo pipefail

DRIVER="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$DRIVER"
KEYS="$DIR/certs-private"
PUB="$DIR/certs"

mkdir -p "$KEYS" "$PUB"

SUBJ="/CN=NanoChronometer Test (DO NOT TRUST)/O=NanoChronometer Test/emailAddress=none"

if [[ ! -f "$KEYS/nanochrono-test.key" ]]; then
    openssl req -x509 -newkey rsa:2048 -sha256 -nodes \
        -keyout "$KEYS/nanochrono-test.key" \
        -out "$KEYS/nanochrono-test.pem" \
        -days 1825 \
        -subj "$SUBJ"
    echo "== generated $KEYS/nanochrono-test.key/.pem"
else
    echo "== reusing existing key $KEYS/nanochrono-test.key"
fi

# DER copy for the repo (public only).
openssl x509 -in "$KEYS/nanochrono-test.pem" -outform der -out "$PUB/nanochrono-test.crt"

# PKCS12 for signtool/osslsigncode on Windows. Export with an empty password.
if ! command -v openssl >/dev/null; then
    echo "error: openssl required" >&2
    exit 1
fi
openssl pkcs12 -export \
    -inkey "$KEYS/nanochrono-test.key" \
    -in "$KEYS/nanochrono-test.pem" \
    -out "$KEYS/nanochrono-test.pfx" \
    -passout pass:

echo "== public cert:  $PUB/nanochrono-test.crt"
echo "== private pair: $KEYS/ (do NOT commit)"
echo "== PKCS12:       $KEYS/nanochrono-test.pfx (for sign.bat)"