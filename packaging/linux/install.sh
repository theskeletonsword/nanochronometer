#!/usr/bin/env bash
# Installs NanoChronometer for the current user.
#
# Everything lands under ~/.local, so no root is required and an uninstall is
# just removing the same paths. Pass --system to install under /usr/local
# instead, which does need root.
set -euo pipefail

prefix="${HOME}/.local"
if [[ "${1:-}" == "--system" ]]; then
    prefix="/usr/local"
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
app_id="io.nanochronometer.NanoChrono"

echo "Building release binaries..."
cargo build --release --manifest-path "${repo_root}/Cargo.toml" \
    -p nanochrono-cli -p nanochrono-gui -p nanochrono-ffi

install -Dm755 "${repo_root}/target/release/nanochrono"     "${prefix}/bin/nanochrono"
install -Dm755 "${repo_root}/target/release/nanochrono-gui" "${prefix}/bin/nanochrono-gui"
install -Dm644 "${repo_root}/target/release/libnanochrono.so" "${prefix}/lib/libnanochrono.so"
# The header is generated from the Rust FFI crate, never hand-written.
"${repo_root}/tools/gen-header.sh"
install -Dm644 "${repo_root}/include/nanochrono.h"          "${prefix}/include/nanochrono.h"

install -Dm644 "${repo_root}/packaging/linux/${app_id}.desktop" \
    "${prefix}/share/applications/${app_id}.desktop"

# The icon is shipped as .ico; hicolor wants a named file per size, and the
# scalable SVG covers every size at once where the theme supports it.
install -Dm644 "${repo_root}/assets/nanochronometer_logo.svg" \
    "${prefix}/share/icons/hicolor/scalable/apps/${app_id}.svg"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${prefix}/share/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${prefix}/share/icons/hicolor" 2>/dev/null || true
fi

echo
echo "Installed to ${prefix}"
echo "  nanochrono-gui   desktop application"
echo "  nanochrono       command-line toolkit"
echo "  libnanochrono.so C ABI for the language wrappers"
echo
if [[ ":${PATH}:" != *":${prefix}/bin:"* ]]; then
    echo "Note: ${prefix}/bin is not on PATH."
fi
