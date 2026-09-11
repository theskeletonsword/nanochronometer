/* SPDX-License-Identifier: Apache-2.0
 *
 * Embeds the NanoChronometer ARM64 kernel ELF into the UEFI loader image.
 *
 * `.incbin` copies the file verbatim into `.rodata`, where it survives the
 * PE/COFF link and can be copied out by efi_loader_aarch64.c at boot.  A
 * rebuilt kernel path is supplied on the command line (-DKERNEL_BLOB=...).
 */

.section .rodata
.global kernel_image
kernel_image:
.incbin KERNEL_BLOB
.global kernel_image_end
kernel_image_end:
