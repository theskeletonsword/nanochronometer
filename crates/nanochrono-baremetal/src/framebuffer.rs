// SPDX-License-Identifier: Apache-2.0
//! A linear framebuffer, and what can be drawn on one with no allocator.
//!
//! The loader is asked for a graphics mode through the multiboot2 header and
//! reports what it produced in the boot information structure. Everything
//! here works from that: a base address, a pitch, a size and a pixel format.
//! There is no GPU driver and no acceleration — every pixel is a store.
//!
//! That is enough for the interface this kernel needs. It is not enough for
//! the desktop GUI: `iced` renders through `wgpu` onto a surface `winit` gets
//! from a window server, none of which exists here. What this draws is the
//! same *design*, not the same code.

/// Where the loader put the framebuffer, and how it is laid out.
#[derive(Debug, Clone, Copy)]
pub struct Framebuffer {
    base: *mut u8,
    pub width: u32,
    pub height: u32,
    /// Bytes per scanline. Not always `width * bytes_per_pixel`: the loader
    /// may pad rows, and assuming it does not is how a display ends up
    /// sheared diagonally.
    pitch: u32,
    bytes_per_pixel: u32,
}

/// A colour, as the framebuffer wants it.
pub type Colour = u32;

impl Framebuffer {
    /// Wraps a framebuffer the loader described.
    ///
    /// # Safety
    /// `base` must be a linear framebuffer of at least `pitch * height`
    /// bytes, mapped and writable.
    pub const unsafe fn new(
        base: *mut u8,
        width: u32,
        height: u32,
        pitch: u32,
        bits_per_pixel: u8,
    ) -> Framebuffer {
        Framebuffer {
            base,
            width,
            height,
            pitch,
            bytes_per_pixel: (bits_per_pixel as u32).div_ceil(8),
        }
    }

    /// Whether this describes a usable surface.
    pub fn is_usable(&self) -> bool {
        !self.base.is_null()
            && self.width > 0
            && self.height > 0
            && self.bytes_per_pixel >= 2
            && self.bytes_per_pixel <= 4
    }

    /// Writes one pixel, ignoring anything outside the surface.
    #[inline]
    pub fn set(&self, x: u32, y: u32, colour: Colour) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * self.pitch + x * self.bytes_per_pixel) as usize;
        // SAFETY: the bounds check above keeps the offset inside the surface
        // `new`'s caller guaranteed, and a framebuffer write has no aliasing
        // requirements beyond that.
        unsafe {
            match self.bytes_per_pixel {
                4 => core::ptr::write_volatile(self.base.add(offset).cast::<u32>(), colour),
                3 => {
                    let p = self.base.add(offset);
                    core::ptr::write_volatile(p, colour as u8);
                    core::ptr::write_volatile(p.add(1), (colour >> 8) as u8);
                    core::ptr::write_volatile(p.add(2), (colour >> 16) as u8);
                }
                _ => {
                    // 16-bit: 5:6:5, the only packed format worth supporting.
                    let r = (colour >> 19) & 0x1F;
                    let g = (colour >> 10) & 0x3F;
                    let b = (colour >> 3) & 0x1F;
                    let packed = ((r << 11) | (g << 5) | b) as u16;
                    core::ptr::write_volatile(self.base.add(offset).cast::<u16>(), packed);
                }
            }
        }
    }

    /// Reads one pixel back.
    ///
    /// A framebuffer is readable memory, which is what lets a software cursor
    /// restore what it covered — there is no hardware overlay to put it on.
    /// Reads from device memory are slow, so this is used for the cursor's
    /// hundred pixels and nothing larger.
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> Colour {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let offset = (y * self.pitch + x * self.bytes_per_pixel) as usize;
        // SAFETY: the bounds check keeps the offset inside the surface
        // `new`'s caller guaranteed.
        unsafe {
            match self.bytes_per_pixel {
                4 => core::ptr::read_volatile(self.base.add(offset).cast::<u32>()),
                3 => {
                    let p = self.base.add(offset);
                    core::ptr::read_volatile(p) as u32
                        | (core::ptr::read_volatile(p.add(1)) as u32) << 8
                        | (core::ptr::read_volatile(p.add(2)) as u32) << 16
                }
                _ => {
                    // 5:6:5 expanded back to 8:8:8. The low bits were lost
                    // when it was written, so this is not exact — on a 16-bit
                    // mode a restored pixel can differ by a shade.
                    let packed = core::ptr::read_volatile(self.base.add(offset).cast::<u16>());
                    let r = ((packed >> 11) & 0x1F) as u32;
                    let g = ((packed >> 5) & 0x3F) as u32;
                    let b = (packed & 0x1F) as u32;
                    (r << 19) | (g << 10) | (b << 3)
                }
            }
        }
    }

    /// Fills a rectangle.
    pub fn fill(&self, x: u32, y: u32, w: u32, h: u32, colour: Colour) {
        for row in y..y.saturating_add(h).min(self.height) {
            for col in x..x.saturating_add(w).min(self.width) {
                self.set(col, row, colour);
            }
        }
    }

    /// Fills the whole surface.
    pub fn clear(&self, colour: Colour) {
        self.fill(0, 0, self.width, self.height, colour);
    }

    /// Draws a one-pixel rectangle outline.
    pub fn outline(&self, x: u32, y: u32, w: u32, h: u32, colour: Colour) {
        if w == 0 || h == 0 {
            return;
        }
        self.fill(x, y, w, 1, colour);
        self.fill(x, y + h - 1, w, 1, colour);
        self.fill(x, y, 1, h, colour);
        self.fill(x + w - 1, y, 1, h, colour);
    }
}

// SAFETY: a framebuffer is memory-mapped device storage. This kernel runs on
// one core with interrupts masked, so there is no concurrent access to race
// with; the marker exists so a `Framebuffer` can live in a static.
unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}
