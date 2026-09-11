// SPDX-License-Identifier: Apache-2.0
//! The interface, dispatched per architecture.
//!
//! The x86 build boots a multiboot loader that hands over a linear framebuffer
//! and a memory map; its interface lives in [`gui_x86_64`]. The AArch64 side
//! has neither a multiboot loader nor a free VGA, and its port — plus the
//! hardware its framebuffer and console live behind — is [`gui_arm64`].
//!
//! This module exists so the rest of the crate can call `gui::run` without
//! branching on the target; it re-exports whichever side compiles for the
//! machine at hand. See each module for why the two interfaces differ.

#[cfg(target_arch = "x86_64")]
pub use crate::gui_x86_64::*;
#[cfg(target_arch = "aarch64")]
pub use crate::gui_arm64::*;