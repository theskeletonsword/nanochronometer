// SPDX-License-Identifier: Apache-2.0
//! Reading what the loader left behind.
//!
//! Only the framebuffer tag is parsed. The rest of the multiboot2 information
//! structure — memory maps, modules, the command line — describes resources
//! this kernel does not manage, and parsing tags it will never act on would
//! be code with no way to be wrong loudly.

use crate::framebuffer::Framebuffer;

/// Tag type 8: the framebuffer the loader actually set up.
const TAG_FRAMEBUFFER: u32 = 8;
/// Tag type 0 ends the list.
const TAG_END: u32 = 0;

/// Framebuffer type 1 is a linear RGB surface. Type 2 is EGA text, which this
/// cannot draw on, and type 0 is indexed colour, which would need the palette
/// programmed first.
const FRAMEBUFFER_TYPE_RGB: u8 = 1;

/// Finds the framebuffer the loader set up, if it set one up.
///
/// # Safety
/// `info` must be the multiboot2 information pointer the loader passed, or
/// zero. Reads it as the specification lays it out.
pub unsafe fn framebuffer(info: u64) -> Option<Framebuffer> {
    if info == 0 || info % 8 != 0 {
        return None;
    }
    let base = info as usize;

    // The structure starts with its total size and a reserved word; tags
    // follow, each 8-byte aligned.
    // SAFETY: forwarded from this function's own contract.
    let total = unsafe { core::ptr::read_volatile(base as *const u32) } as usize;
    if !(16..0x10_0000).contains(&total) {
        return None;
    }

    let mut offset = 8;
    while offset + 8 <= total {
        // SAFETY: bounded by the total size the header declares.
        let (kind, size) = unsafe {
            (
                core::ptr::read_volatile((base + offset) as *const u32),
                core::ptr::read_volatile((base + offset + 4) as *const u32) as usize,
            )
        };
        if kind == TAG_END || size < 8 {
            break;
        }
        if kind == TAG_FRAMEBUFFER && size >= 32 {
            let tag = base + offset;
            // SAFETY: the tag's declared size covers these fields.
            let fb = unsafe {
                let address = core::ptr::read_unaligned((tag + 8) as *const u64);
                let pitch = core::ptr::read_unaligned((tag + 16) as *const u32);
                let width = core::ptr::read_unaligned((tag + 20) as *const u32);
                let height = core::ptr::read_unaligned((tag + 24) as *const u32);
                let bpp = core::ptr::read_unaligned((tag + 28) as *const u8);
                let fb_type = core::ptr::read_unaligned((tag + 29) as *const u8);

                if fb_type != FRAMEBUFFER_TYPE_RGB {
                    return None;
                }
                // The identity map covers the first four gigabytes, which is
                // sized for exactly this: firmware puts framebuffers in high
                // MMIO space. Anything beyond it would fault on the first
                // store, so it is refused rather than written to.
                if address == 0 || address >= (4u64 << 30) {
                    return None;
                }
                Framebuffer::new(address as *mut u8, width, height, pitch, bpp)
            };
            return fb.is_usable().then_some(fb);
        }
        // Tags are padded to an 8-byte boundary.
        offset += size.div_ceil(8) * 8;
    }
    None
}
