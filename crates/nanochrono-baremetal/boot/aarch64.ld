/* SPDX-License-Identifier: Apache-2.0
 *
 * QEMU's `virt` machine loads a `-kernel` image at 0x4008_0000 and enters at
 * its first byte, so there is no header to place and no loader protocol to
 * satisfy — which is why AArch64 needs no equivalent of boot32.S. The entry
 * stub is `global_asm!` in Rust; this script only fixes the load address and
 * the section order.
 */

ENTRY(_start)

SECTIONS {
    . = 0x40080000;

    .text : ALIGN(4K) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) { *(.rodata .rodata.*) }
    .data   : ALIGN(4K) { *(.data .data.*) }

    .bss : ALIGN(4K) {
        __bss_start = .;
        *(COMMON)
        *(.bss .bss.*)
        __bss_end = .;
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
