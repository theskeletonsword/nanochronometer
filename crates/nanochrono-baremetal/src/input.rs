// SPDX-License-Identifier: Apache-2.0
//! Keyboard, mouse and touchpad, through the 8042 controller.
//!
//! # Why PS/2 and not USB
//!
//! Under every hypervisor this runs on — QEMU, VMware, VirtualBox, Hyper-V —
//! the emulated keyboard and mouse *are* 8042 devices, so this is the native
//! path, not a fallback. On real hardware the peripherals are USB, and
//! firmware translates them to 8042 for exactly this situation: a loader or a
//! kernel that has not brought up a USB stack yet. That translation is
//! "legacy USB support" in the firmware setup, on by default on essentially
//! every machine that still ships a BIOS-compatible mode.
//!
//! Where it is switched off, this reports no device rather than appearing to
//! work. See `docs/BAREMETAL_DRIVERS.md` for what a real USB stack would
//! involve and why it is not here.
//!
//! # Reference
//!
//! The controller command set and the initialisation order follow FreeBSD's
//! `sys/dev/atkbdc/atkbdcreg.h` and `psm.c`. The constants below carry the
//! names that file uses, so the two can be read side by side. No code was
//! copied; this is a much smaller driver with no interrupts and no queueing.

use crate::arch::x86::{inb, outb};

/// Data port. Reads take a byte from whichever device the controller last
/// steered; writes go to the keyboard unless preceded by `WRITE_TO_AUX`.
const DATA: u16 = 0x60;
/// Status on read, command on write.
const STATUS: u16 = 0x64;

/// Status bits.
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
/// Set when the byte waiting came from the auxiliary port — the mouse. This
/// is the only thing separating a mouse packet from a keystroke.
const STATUS_AUX_DATA: u8 = 1 << 5;

/// Controller commands, named as in `atkbdcreg.h`.
const KBDC_GET_COMMAND_BYTE: u8 = 0x20;
const KBDC_SET_COMMAND_BYTE: u8 = 0x60;
const KBDC_DISABLE_AUX_PORT: u8 = 0xA7;
const KBDC_ENABLE_AUX_PORT: u8 = 0xA8;
const KBDC_TEST_AUX_PORT: u8 = 0xA9;
const KBDC_ENABLE_KBD_PORT: u8 = 0xAE;
const KBDC_WRITE_TO_AUX: u8 = 0xD4;

/// Command-byte bits.
const KBD_TRANSLATION: u8 = 0x40;
const KBD_DISABLE_KBD_PORT: u8 = 0x10;
const KBD_DISABLE_AUX_PORT_BIT: u8 = 0x20;

/// Device commands.
const DEV_RESET: u8 = 0xFF;
const DEV_ENABLE: u8 = 0xF4;
const DEV_SET_DEFAULTS: u8 = 0xF6;
const DEV_SET_SAMPLE_RATE: u8 = 0xF3;
const DEV_GET_DEVICE_ID: u8 = 0xF2;

/// Device replies.
const ACK: u8 = 0xFA;
const RESET_DONE: u8 = 0xAA;

/// How long to spin waiting for the controller.
///
/// The 8042 is slow and a real one can take milliseconds. This is a bounded
/// spin rather than a timer because there is no timer yet, and a driver that
/// hangs forever on absent hardware is worse than one that gives up.
const SPIN_LIMIT: u32 = 1_000_000;

/// What the auxiliary port turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    /// Three-byte packets: two buttons plus movement.
    Standard,
    /// Four-byte packets with a scroll wheel. Device ID 3.
    Wheel,
    /// Four-byte packets with five buttons. Device ID 4.
    FiveButton,
    /// Nothing answered on the auxiliary port.
    None,
}

impl PointerKind {
    pub const fn name(self) -> &'static str {
        match self {
            PointerKind::Standard => "ps/2 (3-byte)",
            PointerKind::Wheel => "ps/2 wheel (4-byte)",
            PointerKind::FiveButton => "ps/2 5-button (4-byte)",
            PointerKind::None => "none",
        }
    }

    const fn packet_len(self) -> usize {
        match self {
            PointerKind::Standard => 3,
            PointerKind::Wheel | PointerKind::FiveButton => 4,
            PointerKind::None => 0,
        }
    }
}

/// One pointer movement, already decoded.
///
/// A touchpad is not distinguished from a mouse here, and does not need to
/// be: a PS/2 touchpad reports through the same protocol. Synaptics and ALPS
/// extensions would give gestures and absolute positions, but this interface
/// wants a cursor, and the standard protocol carries one.
#[derive(Debug, Clone, Copy, Default)]
pub struct Motion {
    pub dx: i32,
    pub dy: i32,
    pub wheel: i32,
    pub left: bool,
    pub right: bool,
    pub middle: bool,
}

/// A key press or release, as a set 1 scancode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub scancode: u8,
    pub pressed: bool,
}

/// What arrived from the controller.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    Key(Key),
    Motion(Motion),
}

/// The 8042 and whatever is attached to it.
pub struct Input {
    pointer: PointerKind,
    /// Bytes of a mouse packet collected so far.
    packet: [u8; 4],
    packet_len: usize,
    /// Whether the keyboard answered its reset.
    keyboard: bool,
}

impl Input {
    /// Brings up the controller and both ports.
    ///
    /// # Safety
    /// Drives I/O ports; requires ring 0 and exclusive use of the 8042.
    pub unsafe fn init() -> Input {
        let mut input = Input {
            pointer: PointerKind::None,
            packet: [0; 4],
            packet_len: 0,
            keyboard: false,
        };

        // SAFETY: caller guarantees ring 0. The order below is FreeBSD's:
        // quiesce both ports before touching the command byte, so a device
        // cannot inject a byte into the middle of the sequence.
        unsafe {
            command(KBDC_DISABLE_AUX_PORT);
            drain();

            command(KBDC_GET_COMMAND_BYTE);
            let mut byte = read_data().unwrap_or(0);

            // Interrupts stay off — there is no IDT — so this polls. What
            // matters is translation: with it set the controller converts set
            // 2 scancodes to set 1, which is what every scancode table in
            // existence assumes, including this one.
            byte |= KBD_TRANSLATION;
            byte &= !(KBD_DISABLE_KBD_PORT | KBD_DISABLE_AUX_PORT_BIT);
            // Interrupt-enable bits cleared: an interrupt with no handler is
            // a triple fault.
            byte &= !0x03;

            command(KBDC_SET_COMMAND_BYTE);
            write_data(byte);

            command(KBDC_ENABLE_KBD_PORT);
            input.keyboard = reset_device(false);

            // The auxiliary port only exists on a controller that has one;
            // testing it first avoids a long spin on a machine with none.
            command(KBDC_TEST_AUX_PORT);
            if read_data() == Some(0x00) {
                command(KBDC_ENABLE_AUX_PORT);
                if reset_device(true) {
                    input.pointer = identify_pointer();
                    aux_command(DEV_SET_DEFAULTS);
                    aux_command(DEV_ENABLE);
                }
            }
            drain();
        }
        input
    }

    pub fn pointer_kind(&self) -> PointerKind {
        self.pointer
    }

    pub fn has_keyboard(&self) -> bool {
        self.keyboard
    }

    /// Takes the next event, or `None` if nothing is waiting.
    ///
    /// Non-blocking: this is polled from a draw loop, and a blocking read
    /// would stop the interface updating.
    ///
    /// # Safety
    /// Reads I/O ports; requires ring 0.
    pub unsafe fn poll(&mut self) -> Option<Event> {
        // SAFETY: reading the status port has no side effects.
        let status = unsafe { inb(STATUS) };
        if status & STATUS_OUTPUT_FULL == 0 {
            return None;
        }
        // SAFETY: the status bit says a byte is waiting.
        let byte = unsafe { inb(DATA) };

        if status & STATUS_AUX_DATA == 0 {
            // Bit 7 of a set 1 scancode marks a release. Extended keys are
            // prefixed with 0xE0, which is reported as its own event rather
            // than merged — nothing here binds an extended key.
            return Some(Event::Key(Key {
                scancode: byte & 0x7F,
                pressed: byte & 0x80 == 0,
            }));
        }

        self.accumulate(byte)
    }

    /// Collects a mouse packet, returning it once complete.
    fn accumulate(&mut self, byte: u8) -> Option<Event> {
        let len = self.pointer.packet_len();
        if len == 0 {
            return None;
        }

        // Bit 3 of the first byte is always set in a valid packet. Using it
        // to resynchronise is what keeps a dropped byte from turning every
        // subsequent packet into nonsense — the failure that makes a PS/2
        // cursor wander diagonally forever.
        if self.packet_len == 0 && byte & 0x08 == 0 {
            return None;
        }

        self.packet[self.packet_len] = byte;
        self.packet_len += 1;
        if self.packet_len < len {
            return None;
        }
        self.packet_len = 0;

        let flags = self.packet[0];
        // Overflow bits 6 and 7: the movement exceeded what the packet can
        // carry, so the value is meaningless and the packet is dropped.
        if flags & 0xC0 != 0 {
            return None;
        }

        // Movement is nine-bit two's complement: eight bits in the byte, the
        // sign in the flags. Sign-extending by hand is what the protocol
        // requires; casting the byte alone gives a cursor that only moves
        // right and down.
        let dx = sign_extend(self.packet[1], flags & 0x10 != 0);
        let dy = sign_extend(self.packet[2], flags & 0x20 != 0);

        let wheel = match self.pointer {
            // The fourth byte's low nibble is a four-bit signed delta.
            PointerKind::Wheel | PointerKind::FiveButton => {
                let z = self.packet[3] & 0x0F;
                if z & 0x08 != 0 {
                    z as i32 - 16
                } else {
                    z as i32
                }
            }
            _ => 0,
        };

        Some(Event::Motion(Motion {
            dx,
            // The protocol's Y axis points up and a framebuffer's points
            // down, so this is negated once here rather than at every use.
            dy: -dy,
            wheel,
            left: flags & 0x01 != 0,
            right: flags & 0x02 != 0,
            middle: flags & 0x04 != 0,
        }))
    }
}

fn sign_extend(value: u8, negative: bool) -> i32 {
    if negative {
        value as i32 - 256
    } else {
        value as i32
    }
}

/// Waits for the input buffer to drain, then sends a controller command.
///
/// # Safety
/// Requires ring 0.
unsafe fn command(byte: u8) {
    // SAFETY: caller guarantees ring 0; polling the status port is harmless.
    unsafe {
        for _ in 0..SPIN_LIMIT {
            if inb(STATUS) & STATUS_INPUT_FULL == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(STATUS, byte);
    }
}

/// # Safety
/// Requires ring 0.
unsafe fn write_data(byte: u8) {
    // SAFETY: as `command`.
    unsafe {
        for _ in 0..SPIN_LIMIT {
            if inb(STATUS) & STATUS_INPUT_FULL == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        outb(DATA, byte);
    }
}

/// Reads a byte, or `None` if nothing arrives.
///
/// # Safety
/// Requires ring 0.
unsafe fn read_data() -> Option<u8> {
    // SAFETY: caller guarantees ring 0.
    unsafe {
        for _ in 0..SPIN_LIMIT {
            if inb(STATUS) & STATUS_OUTPUT_FULL != 0 {
                return Some(inb(DATA));
            }
            core::hint::spin_loop();
        }
    }
    None
}

/// Discards anything already waiting.
///
/// # Safety
/// Requires ring 0.
unsafe fn drain() {
    // SAFETY: caller guarantees ring 0.
    unsafe {
        for _ in 0..64 {
            if inb(STATUS) & STATUS_OUTPUT_FULL == 0 {
                return;
            }
            let _ = inb(DATA);
        }
    }
}

/// Sends a command to the mouse rather than the keyboard.
///
/// # Safety
/// Requires ring 0.
unsafe fn aux_command(byte: u8) -> Option<u8> {
    // SAFETY: caller guarantees ring 0. `WRITE_TO_AUX` steers exactly the
    // next data write to the auxiliary port.
    unsafe {
        command(KBDC_WRITE_TO_AUX);
        write_data(byte);
        read_data()
    }
}

/// Resets one device and waits for its self-test result.
///
/// # Safety
/// Requires ring 0.
unsafe fn reset_device(aux: bool) -> bool {
    // SAFETY: caller guarantees ring 0.
    unsafe {
        let ack = if aux {
            aux_command(DEV_RESET)
        } else {
            write_data(DEV_RESET);
            read_data()
        };
        if ack != Some(ACK) {
            return false;
        }
        // Self-test result. A device that fails it is not used.
        if read_data() != Some(RESET_DONE) {
            return false;
        }
        // A mouse follows with its device ID; a keyboard does not. Reading
        // it here keeps it out of the event stream either way.
        if aux {
            let _ = read_data();
        }
        true
    }
}

/// Runs the "magic knock" that unlocks the extended protocols.
///
/// Three sample-rate settings in a fixed order make an IntelliMouse report
/// ID 3, and a second sequence makes an Explorer report ID 4. A device that
/// does not know the sequence keeps reporting 0, which is why this is safe to
/// attempt unconditionally.
///
/// # Safety
/// Requires ring 0.
unsafe fn identify_pointer() -> PointerKind {
    // SAFETY: caller guarantees ring 0.
    unsafe {
        for rate in [200u8, 100, 80] {
            aux_command(DEV_SET_SAMPLE_RATE);
            aux_command(rate);
        }
        aux_command(DEV_GET_DEVICE_ID);
        if read_data() != Some(3) {
            return PointerKind::Standard;
        }

        for rate in [200u8, 200, 80] {
            aux_command(DEV_SET_SAMPLE_RATE);
            aux_command(rate);
        }
        aux_command(DEV_GET_DEVICE_ID);
        if read_data() == Some(4) {
            PointerKind::FiveButton
        } else {
            PointerKind::Wheel
        }
    }
}
