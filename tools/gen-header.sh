#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Regenerates include/nanochrono.h from the Rust FFI crate.
#
# The header is generated, never edited: crates/nanochrono-ffi/src/lib.rs is
# the single source of truth for the C ABI.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v cbindgen >/dev/null 2>&1; then
    echo "error: cbindgen not found. Install it with:" >&2
    echo "         cargo install cbindgen --locked" >&2
    exit 1
fi

mkdir -p "${repo_root}/include"
cbindgen \
    --config "${repo_root}/cbindgen.toml" \
    --crate nanochrono-ffi \
    --output "${repo_root}/include/nanochrono.h" \
    "${repo_root}/crates/nanochrono-ffi"

echo "wrote include/nanochrono.h ($(wc -l < "${repo_root}/include/nanochrono.h") lines)"
