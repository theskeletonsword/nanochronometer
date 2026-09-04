#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Builds the freestanding kernel and, with `run`, boots it under QEMU.
#
# The crate is outside the workspace: it targets `x86_64-unknown-none` /
# `aarch64-unknown-none`, has no `main`, and links through its own script, so
# `cargo build --workspace` would try to build it for the host and fail.
#
# Usage:
#   packaging/baremetal/build.sh                 # both architectures
#   packaging/baremetal/build.sh x86_64          # one
#   packaging/baremetal/build.sh run x86_64      # build and boot under QEMU
#   packaging/baremetal/build.sh test            # the host-side unit tests
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
crate="${repo_root}/crates/nanochrono-baremetal"
out_dir="${repo_root}/dist/baremetal"

# `x86_64-nanochrono-none` is a custom spec: the built-in `x86_64-unknown-none`
# has a soft-float ABI where no vector register can be allocated, so the SIMD
# probes cannot be built for it. That needs nightly and `rust-src`; pass
# `--stable` to fall back to the built-in target with SIMD off.
declare -A arch_target=(
    [x86_64]="x86_64-nanochrono-none"
    [aarch64]="aarch64-unknown-none"
)
declare -A arch_features=(
    [x86_64]="--features simd"
    [aarch64]=""
)
toolchain="+nightly"
export RUST_TARGET_PATH="${crate}/targets"

build_one() {
    local arch="$1" target="${arch_target[$1]}"
    echo "=== ${arch} (${target})"
    rustup target add "${target}" >/dev/null 2>&1 || true
    # shellcheck disable=SC2086
    (cd "${crate}" && cargo ${toolchain} build --release --target "${target}" \
        ${arch_features[${arch}]})

    mkdir -p "${out_dir}/${arch}"
    local elf="${crate}/target/${target}/release/nanochrono-kernel"
    cp "${elf}" "${out_dir}/${arch}/nanochrono-kernel.elf"
    cp "${crate}/target/${target}/release/libnanochrono_baremetal.a" "${out_dir}/${arch}/"

    # The shared object needs its own spec: position independent, dynamically
    # linkable, and without the kernel code model. It also needs a loader that
    # does not exist on bare metal — see docs/BAREMETAL_LIBRARIES.md.
    if [[ "${arch}" == "x86_64" ]]; then
        # shellcheck disable=SC2086
        (cd "${crate}" && cargo ${toolchain} rustc --release --lib \
            --target "${target}-dylib" --crate-type cdylib \
            ${arch_features[${arch}]}) || true
        local so="${crate}/target/${target}-dylib/release/libnanochrono_baremetal.so"
        [[ -f "${so}" ]] && cp "${so}" "${out_dir}/${arch}/"
    fi

    if [[ "${arch}" == "x86_64" ]]; then
        # QEMU's multiboot loader takes a 32-bit ELF only, because multiboot
        # entry is 32-bit protected mode. The image is ELF64 with a 32-bit
        # entry stub, so the container is rewritten rather than the code: every
        # address is below 4 GiB, so nothing is lost. GRUB accepts either.
        objcopy -O elf32-i386 "${elf}" "${out_dir}/${arch}/nanochrono-kernel.mb.elf"
    fi
}

run_one() {
    local arch="$1"
    case "${arch}" in
        x86_64)
            echo "=== booting x86_64 under QEMU (Ctrl-A X to quit)"
            # `-cpu max` is the widest feature set QEMU's TCG offers. It has no
            # architectural PMU: CPUID.0AH reports version 0, and the kernel
            # says so rather than reading counters that are not there. A PMU
            # needs KVM and `-cpu host`.
            # From the ISO when there is one: QEMU's own -kernel loader
            # implements multiboot1 only, which carries no framebuffer
            # request, so the interface would have nothing to draw on.
            if [[ -f "${out_dir}/nanochronometer.iso" ]]; then
                qemu-system-x86_64 -cdrom "${out_dir}/nanochronometer.iso" \
                    -cpu max -m 256 -vga std -no-reboot -serial stdio
            else
                qemu-system-x86_64 \
                    -kernel "${out_dir}/${arch}/nanochrono-kernel.mb.elf" \
                    -cpu max -m 128 -display none -no-reboot -serial stdio
            fi
            ;;
        aarch64)
            echo "=== booting aarch64 under QEMU (Ctrl-A X to quit)"
            # `virt` maps a PL011 at 0x09000000, which the serial driver
            # assumes, and its `-cpu max` does implement PMCCNTR_EL0.
            qemu-system-aarch64 \
                -M virt -cpu max -m 128 \
                -kernel "${out_dir}/${arch}/nanochrono-kernel.elf" \
                -display none -no-reboot -serial stdio
            ;;
    esac
}

mode="build"
if [[ "${1:-}" == "run" || "${1:-}" == "test" ]]; then
    mode="$1"
    shift
fi

if [[ "${mode}" == "test" ]]; then
    # The decoding logic — the CPUID performance-monitoring leaf, the hybrid
    # core type, the counter-width mask — is pure and runs on the host, which
    # is the only place it can be driven with the values that matter: one
    # thread on a hybrid part can never observe both core types.
    (cd "${crate}" && cargo test --target x86_64-unknown-linux-gnu)
    exit 0
fi

requested=("$@")
[[ ${#requested[@]} -eq 0 ]] && requested=(x86_64 aarch64)

rm -rf "${out_dir}"
for arch in "${requested[@]}"; do
    if [[ -z "${arch_target[${arch}]:-}" ]]; then
        echo "error: unknown architecture '${arch}' (want x86_64 or aarch64)" >&2
        exit 1
    fi
    build_one "${arch}"
    [[ "${mode}" == "run" ]] && run_one "${arch}"
done

# A bootable image, so the kernel can be written to a USB stick and started
# on real hardware. grub-mkrescue produces a hybrid ISO: an MBR with a boot
# signature plus El Torito images for both BIOS and UEFI, which is what makes
# it work with dd, Rufus, Ventoy, YUMI and UNetbootin alike.
build_iso() {
    local grub_mkrescue
    grub_mkrescue="$(command -v grub-mkrescue || command -v grub2-mkrescue || true)"
    if [[ -z "${grub_mkrescue}" ]]; then
        echo "note: no grub-mkrescue; skipping the ISO"
        echo "      the ELF still boots with qemu -kernel"
        return
    fi

    local staging="${out_dir}/.iso"
    rm -rf "${staging}"
    mkdir -p "${staging}/boot/grub"
    cp "${out_dir}/x86_64/nanochrono-kernel.elf" "${staging}/boot/nanochrono-kernel"
    cat > "${staging}/boot/grub/grub.cfg" <<'CFG'
set timeout=0
set default=0
menuentry "NanoChronometer (freestanding)" {
    # all_video pulls in the framebuffer drivers, without which GRUB hands
    # over a text mode and the interface has nothing to draw on.
    insmod all_video
    multiboot2 /boot/nanochrono-kernel
    boot
}
CFG
    "${grub_mkrescue}" -o "${out_dir}/nanochronometer.iso" "${staging}" >/dev/null 2>&1
    rm -rf "${staging}"
}

if [[ "${mode}" == "build" ]]; then
    [[ -f "${out_dir}/x86_64/nanochrono-kernel.elf" ]] && build_iso
    cp "${repo_root}/LICENSE" "${repo_root}/NOTICE" "${out_dir}/"
    cp "${repo_root}/docs/BAREMETAL_LIBRARIES.md" "${repo_root}/docs/BAREMETAL_DRIVERS.md" \
        "${out_dir}/"
    echo
    echo "=== ${out_dir}"
    find "${out_dir}" -type f -printf '%p  %s bytes\n' | sort
fi
