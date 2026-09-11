// SPDX-License-Identifier: Apache-2.0
//! Keyboard, mouse and touchpad, through the 8042 controller.
//!
//! # Both stacks, not one or the other
//!
//! Under every hypervisor this runs on — QEMU, VMware, VirtualBox, Hyper-V —
//! the emulated keyboard and mouse *are* 8042 devices, so PS/2 is the native
//! path there, not a fallback. On real hardware it is usually mixed: a
//! notebook's built-in keyboard reaches the 8042 through the embedded
//! controller, while anything plugged into a USB port does not, and the
//! built-in touchpad may be on neither.
//!
//! So both stacks are brought up whenever either is incomplete. An earlier
//! version only tried USB when the 8042 reported *nothing at all*, which on a
//! laptop is never: the built-in keyboard answers, USB is therefore skipped,
//! and a plugged-in mouse is never found. That is a keyboard that works and a
//! pointer that does not, which is exactly what a notebook reports.
//!
//! # The third stack: I2C-HID
//!
//! Many recent notebooks put the built-in touchpad on **I2C-HID**, which is
//! on neither of the buses above. It is not enumerable: an I2C bus has no
//! discovery, so the only way to find the device is to read its address out
//! of the firmware's AML — which means interpreting AML. That is
//! [`crate::i2c_hid`], and it is tried when the other two stacks produce no
//! pointer.
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
/// Controller self-test. Answers `0x55`.
const KBDC_SELF_TEST: u8 = 0xAA;
const KBDC_DISABLE_KBD_PORT_CMD: u8 = 0xAD;
const KBDC_ENABLE_KBD_PORT: u8 = 0xAE;
const KBDC_WRITE_TO_AUX: u8 = 0xD4;

/// Command-byte bits.
const KBD_TRANSLATION: u8 = 0x40;
const KBD_DISABLE_KBD_PORT: u8 = 0x10;
const KBD_DISABLE_AUX_PORT_BIT: u8 = 0x20;
/// The system flag, bit 2. Firmware sets it after a successful POST, and a
/// controller that sees it cleared believes it is still in one. Preserving it
/// is not cosmetic: writing it back as zero is a way to make a working
/// keyboard stop.
const KBD_SYSTEM_FLAG: u8 = 0x04;

/// The controller self-test's reply.
const SELF_TEST_PASSED: u8 = 0x55;

/// Set 2's break prefix. A byte of this on the keyboard port means the *next*
/// byte is a release — and it means this controller is not translating.
const SET2_BREAK: u8 = 0xF0;

/// Set 2 scan code to set 1, indexed by the set 2 code.
///
/// This is the table the 8042 itself applies when translation is enabled, and
/// it is here because translation cannot be relied on. A controller left with
/// it off delivers set 2, where `1` is `0x16` rather than `0x02` — so a
/// driver written for set 1, as every scancode constant in this project is,
/// sees a keyboard that is plainly working do nothing at all.
///
/// Rather than force the controller's configuration and hope, the arrival of
/// a `0xF0` says which set is in use, and this converts.
#[rustfmt::skip]
const SET2_TO_SET1: [u8; 132] = [
    0xFF, 0x43, 0x41, 0x3F, 0x3D, 0x3B, 0x3C, 0x58, 0x64, 0x44, 0x42, 0x40, 0x3E, 0x0F, 0x29, 0x59,
    0x65, 0x38, 0x2A, 0x70, 0x1D, 0x10, 0x02, 0x5A, 0x66, 0x71, 0x2C, 0x1F, 0x1E, 0x11, 0x03, 0x5B,
    0x67, 0x2E, 0x2D, 0x20, 0x12, 0x05, 0x04, 0x5C, 0x68, 0x39, 0x2F, 0x21, 0x14, 0x13, 0x06, 0x5D,
    0x69, 0x31, 0x30, 0x23, 0x22, 0x15, 0x07, 0x5E, 0x6A, 0x72, 0x32, 0x24, 0x16, 0x08, 0x09, 0x5F,
    0x6B, 0x33, 0x25, 0x17, 0x18, 0x0B, 0x0A, 0x60, 0x6C, 0x34, 0x35, 0x26, 0x27, 0x19, 0x0C, 0x61,
    0x6D, 0x73, 0x28, 0x74, 0x1A, 0x0D, 0x62, 0x6E, 0x3A, 0x36, 0x1C, 0x1B, 0x75, 0x2B, 0x63, 0x76,
    0x55, 0x56, 0x77, 0x78, 0x79, 0x7A, 0x0E, 0x7B, 0x7C, 0x4F, 0x7D, 0x4B, 0x47, 0x7E, 0x7F, 0x6F,
    0x52, 0x53, 0x50, 0x4C, 0x4D, 0x48, 0x01, 0x45, 0x57, 0x4E, 0x51, 0x4A, 0x37, 0x49, 0x46, 0x54,
    0x80, 0x81, 0x82, 0x41,
];

/// Device commands.
const DEV_RESET: u8 = 0xFF;
/// Enable scanning / reporting. Sent to **both** devices.
///
/// The specification says a keyboard resumes scanning after a reset, and most
/// do. Notebook embedded controllers are the exception often enough that
/// leaving it out is how the built-in keyboard comes up detected and silent —
/// the 8042 answers every probe, `keyboard` is set, and no key ever arrives.
const DEV_ENABLE: u8 = 0xF4;
const DEV_SET_DEFAULTS: u8 = 0xF6;
const DEV_SET_SAMPLE_RATE: u8 = 0xF3;
const DEV_GET_DEVICE_ID: u8 = 0xF2;

/// HID usage ID to set 1 scancode.
///
/// The two stacks number keys differently — the 8042 reports set 1 scancodes
/// (after the controller's translation), USB HID reports usage IDs from the
/// HID usage tables — and an interface that binds keys needs one numbering,
/// not two. Set 1 is the one chosen because it is what the PS/2 path already
/// produces and what every scancode constant in this project is written in.
///
/// The table covers the printable keys, the navigation keys and the function
/// row: everything the interface binds and everything a person is likely to
/// press expecting something to happen. A usage outside it maps to zero,
/// which no key uses.
///
/// Indexed from usage 0x04, the first key usage.
#[rustfmt::skip]
const HID_TO_SET1: [u8; 0x50] = [
    // 0x04..0x1D: a b c d e f g h i j k l m n o p q r s t u v w x y z
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23,
    0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
    0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D,
    0x15, 0x2C,
    // 0x1E..0x27: 1 2 3 4 5 6 7 8 9 0
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
    // 0x28..0x2C: enter escape backspace tab space
    0x1C, 0x01, 0x0E, 0x0F, 0x39,
    // 0x2D..0x38: - = [ ] \\ non-us# ; ' ` , . /
    0x0C, 0x0D, 0x1A, 0x1B, 0x2B, 0x2B, 0x27, 0x28, 0x29, 0x33, 0x34, 0x35,
    // 0x39: caps lock
    0x3A,
    // 0x3A..0x45: F1..F12
    0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58,
    // 0x46..0x4E: print scroll pause insert home pageup delete end pagedown
    0x00, 0x46, 0x00, 0x52, 0x47, 0x49, 0x53, 0x4F, 0x51,
    // 0x4F..0x52: right left down up
    0x4D, 0x4B, 0x50, 0x48,
    // 0x53: num lock
    0x45,
];

/// The first HID usage the table covers.
const HID_FIRST_USAGE: u8 = 0x04;

/// Translates a HID usage ID to a set 1 scancode, or zero for one this
/// interface has no name for.
fn hid_to_scancode(usage: u8) -> u8 {
    HID_TO_SET1
        .get(usage.wrapping_sub(HID_FIRST_USAGE) as usize)
        .copied()
        .unwrap_or(0)
}

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
    /// A USB HID boot-protocol mouse, reached through xHCI rather than the
    /// 8042.
    Usb,
    /// A touchpad on the chipset's I2C bus, found through the firmware's AML.
    I2cHid,
    /// Nothing answered on the auxiliary port.
    None,
}

impl PointerKind {
    pub const fn name(self) -> &'static str {
        match self {
            PointerKind::Standard => "ps/2 (3-byte)",
            PointerKind::Wheel => "ps/2 wheel (4-byte)",
            PointerKind::FiveButton => "ps/2 5-button (4-byte)",
            PointerKind::Usb => "usb hid boot",
            PointerKind::I2cHid => "i2c-hid touchpad",
            PointerKind::None => "none",
        }
    }

    const fn packet_len(self) -> usize {
        match self {
            PointerKind::Standard => 3,
            PointerKind::Wheel | PointerKind::FiveButton => 4,
            // Not an 8042 device: nothing is accumulated for it here.
            PointerKind::Usb | PointerKind::I2cHid | PointerKind::None => 0,
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

/// The 8042 and whatever is attached to it, plus any USB HID devices found.
///
/// Both are polled every frame. PS/2 first, because where firmware translates
/// USB it is the *same* device arriving by a cheaper path — a port read
/// rather than a transfer — and because a translated keypress that also
/// arrives over USB would otherwise be delivered twice.
pub struct Input {
    pointer: PointerKind,
    /// Bytes of a mouse packet collected so far.
    packet: [u8; 4],
    packet_len: usize,
    /// Whether the keyboard answered its reset.
    keyboard: bool,
    /// The USB stack, when it was brought up and found something.
    usb: Option<UsbInput>,
    /// The I2C-HID touchpad, when there is one and it could be reached.
    i2c: Option<crate::i2c_hid::I2cHid>,
    /// Why there is not one, when there is not. Reported rather than
    /// swallowed: "no touchpad on this machine" and "a touchpad this could
    /// not reach" need different answers from whoever is reading the screen.
    i2c_failure: Option<crate::i2c_hid::Failure>,
    /// When the touchpad may next be read, and how often.
    ///
    /// **A rate limit, not an optimisation.** `poll` is called from the draw
    /// loop's spin as well as once a frame, so an unlimited caller asks
    /// thousands of times per frame. For the 8042 that is a port read; for
    /// I2C it is a whole bus transaction, and a device that is slow to answer
    /// then costs more than the frame it was polled for. A touchpad reports
    /// at a couple of hundred hertz at most, so asking faster learns nothing.
    i2c_next: u64,
    i2c_interval: u64,
    /// The last few bytes the 8042 handed over, raw.
    ///
    /// Diagnostic, and the most useful thing this driver can show on a
    /// machine where keys do nothing. It separates the two failures that look
    /// identical from the outside: no bytes at all means the controller or
    /// the keyboard is not delivering, while bytes arriving with unexpected
    /// values means they are being delivered in a scan code set this does not
    /// read — `1` is `0x02` in set 1 and `0x16` in set 2, and a driver
    /// reading the wrong one sees a working keyboard do nothing.
    recent: [u8; RECENT_BYTES],
    recent_len: usize,
    /// The previous I2C-HID keyboard report, for turning held-key state into
    /// press and release events.
    i2c_previous: [u8; 8],
    /// Set once a `0xF0` arrives: this controller delivers set 2.
    set2: bool,
    /// The next byte is a set 2 release.
    set2_break: bool,
}

/// How many raw bytes to remember.
const RECENT_BYTES: usize = 8;

/// How often the touchpad is read, in hertz.
///
/// Twice what a Precision Touchpad reports at, so nothing is missed, and far
/// below the rate an unthrottled draw loop would ask at.
const TOUCHPAD_POLL_HZ: u64 = 250;

/// What each stack turned up, one field per question worth asking.
#[derive(Debug, Clone, Copy)]
pub struct Found {
    pub ps2_keyboard: bool,
    pub ps2_pointer: Option<PointerKind>,
    /// An xHCI controller was found on the PCI bus and brought up.
    pub usb_controller: bool,
    pub usb_keyboard: bool,
    pub usb_pointer: bool,
    /// The I2C-HID touchpad, when one was brought up.
    pub i2c: Option<I2cFound>,
    /// Whether it came up and then stopped answering.
    pub i2c_abandoned: bool,
    /// Why it was not, when it was not.
    pub i2c_failure: Option<crate::i2c_hid::Failure>,
}

/// What the I2C-HID probe turned up, for the interface to report.
#[derive(Debug, Clone, Copy)]
pub struct I2cFound {
    pub slave_address: u16,
    /// Bus, slot and function of the controller it hangs off.
    pub controller: (u8, u8, u8),
    pub vendor: u16,
    pub product: u16,
    pub report_id: Option<u8>,
    pub input_register: u16,
    pub interrupt_pin: Option<u16>,
    /// Whether that pin is being read as a readiness signal, and how it was
    /// found. See [`crate::gpio`].
    pub gate: crate::i2c_hid::GateState,
    /// How the address above was arrived at — the firmware's word, or the
    /// bus's.
    pub discovery: crate::i2c_hid::Discovery,
}

/// The xHCI controller and the HID devices enumerated on it.
pub struct UsbInput {
    controller: crate::xhci::Xhci,
    /// Which device index is the keyboard, and which the pointer. Separate
    /// because a notebook has both and they are different devices — keeping
    /// one `kind` is what limits a stack to whichever answered first.
    keyboard: Option<usize>,
    pointer: Option<usize>,
    /// Previous keyboard report, to turn a held-key state into press and
    /// release events.
    previous: [u8; 8],
    /// Which device to service next, so one that reports constantly cannot
    /// starve the other.
    next: usize,
}

impl Input {
    /// Brings up the controller and both ports.
    ///
    /// # Safety
    /// Drives I/O ports; requires ring 0 and exclusive use of the 8042.
    pub unsafe fn init(ticks_per_us: u64) -> Input {
        let mut input = Input {
            pointer: PointerKind::None,
            packet: [0; 4],
            packet_len: 0,
            keyboard: false,
            usb: None,
            i2c: None,
            i2c_failure: None,
            i2c_next: 0,
            i2c_interval: ticks_per_us.max(1) * 1_000_000 / TOUCHPAD_POLL_HZ,
            recent: [0; RECENT_BYTES],
            recent_len: 0,
            i2c_previous: [0; 8],
            set2: false,
            set2_break: false,
        };

        // SAFETY: caller guarantees ring 0. Both ports are quiesced before
        // the command byte is touched, so a device cannot inject a byte into
        // the middle of the sequence — the order FreeBSD's `atkbdc` uses.
        unsafe {
            command(KBDC_DISABLE_KBD_PORT_CMD);
            command(KBDC_DISABLE_AUX_PORT);
            drain();

            command(KBDC_GET_COMMAND_BYTE);
            let existing = read_data();

            // **Only written if it could be read.** A controller whose
            // command byte did not come back is one this does not understand
            // the state of, and writing a fabricated byte over a working
            // configuration is how a keyboard that was fine stops being fine.
            if let Some(current) = existing {
                // Interrupts stay off — there is no IDT, and one that fired
                // would be a triple fault. What matters is translation: with
                // it set the controller converts set 2 scancodes to set 1,
                // which is what every scancode table here assumes.
                //
                // The system flag is carried through untouched.
                let byte = (current | KBD_TRANSLATION | KBD_SYSTEM_FLAG)
                    & !(KBD_DISABLE_KBD_PORT | KBD_DISABLE_AUX_PORT_BIT)
                    & !0x03;
                command(KBDC_SET_COMMAND_BYTE);
                write_data(byte);

                // The self-test is worth running *after* the command byte,
                // because several controllers reset it as part of the test —
                // so the byte is written again if the test says it passed.
                command(KBDC_SELF_TEST);
                if read_data() == Some(SELF_TEST_PASSED) {
                    command(KBDC_SET_COMMAND_BYTE);
                    write_data(byte);
                }
            }

            command(KBDC_ENABLE_KBD_PORT);
            drain();
            input.keyboard = if cfg!(feature = "force-usb") {
                false
            } else {
                keyboard_up()
            };

            // The auxiliary port only exists on a controller that has one;
            // testing it first avoids a long spin on a machine with none.
            command(KBDC_TEST_AUX_PORT);
            if !cfg!(feature = "force-usb") && read_data() == Some(0x00) {
                command(KBDC_ENABLE_AUX_PORT);
                if reset_device(true) {
                    input.pointer = identify_pointer();
                    aux_command(DEV_SET_DEFAULTS);
                    aux_command(DEV_ENABLE);
                }
            }
            drain();
        }

        // USB whenever *either* device is missing, not only when both are.
        //
        // On a notebook the built-in keyboard answers the 8042 and the
        // touchpad does not, so the old condition — both missing — was never
        // true and the USB stack was never brought up. A plugged-in mouse
        // went unfound on the one machine that most needed it.
        //
        // Where the 8042 already supplies a device, the USB one for the same
        // role is still enumerated but not read: `poll` prefers PS/2, so a
        // keypress that firmware translates is not delivered twice.
        if !input.keyboard || input.pointer == PointerKind::None {
            // SAFETY: forwarded from this function's own contract.
            input.usb = unsafe { UsbInput::bring_up() };
            if let Some(usb) = &input.usb {
                if usb.keyboard.is_some() {
                    input.keyboard = true;
                }
                if usb.pointer.is_some() && input.pointer == PointerKind::None {
                    input.pointer = PointerKind::Usb;
                }
            }
        }
        // Last, and only for a pointer. A notebook's built-in touchpad is on
        // neither of the buses above, and this is the one stack that can
        // reach it — but it is also the one that has to interpret firmware
        // bytecode to do so, which is worth not doing when a mouse is already
        // answering.
        if input.pointer == PointerKind::None {
            // SAFETY: forwarded from this function's own contract.
            match unsafe { crate::i2c_hid::I2cHid::probe(ticks_per_us) } {
                Ok(touchpad) => {
                    input.i2c = Some(touchpad);
                    input.pointer = PointerKind::I2cHid;
                }
                Err(why) => input.i2c_failure = Some(why),
            }
        }
        input
    }

    /// What supplied the input, for the status bar.
    ///
    /// Named rather than assumed: "no cursor because there is no pointer" and
    /// "no cursor because the driver failed" look identical from the outside,
    /// and a cursor that never appears with no explanation is worse than a
    /// line saying why.
    pub fn source(&self) -> &'static str {
        if self.pointer == PointerKind::I2cHid {
            return if self.keyboard {
                "ps/2 keyboard + i2c-hid touchpad"
            } else {
                "i2c-hid touchpad"
            };
        }
        let usb_keyboard = self.usb.as_ref().is_some_and(|u| u.keyboard.is_some());
        let usb_pointer = self.usb.as_ref().is_some_and(|u| u.pointer.is_some());
        let ps2_only_keyboard = self.keyboard && !usb_keyboard;

        match (
            self.keyboard,
            self.pointer != PointerKind::None,
            usb_keyboard || usb_pointer,
        ) {
            (true, true, true) if ps2_only_keyboard => "ps/2 keyboard + usb pointer",
            (true, true, true) => "usb hid",
            (true, true, false) => "ps/2",
            (true, false, _) => "keyboard only (no pointer found)",
            (false, true, _) => "pointer only (no keyboard found)",
            (false, false, _) => "no input device",
        }
    }

    /// Whether a touchpad or mouse was found at all.
    pub fn has_pointer(&self) -> bool {
        self.pointer != PointerKind::None
    }

    /// Records a raw byte for the diagnostic readout.
    fn remember(&mut self, byte: u8) {
        if self.recent_len == RECENT_BYTES {
            self.recent.rotate_left(1);
            self.recent[RECENT_BYTES - 1] = byte;
        } else {
            self.recent[self.recent_len] = byte;
            self.recent_len += 1;
        }
    }

    /// The last bytes the 8042 delivered, oldest first.
    pub fn recent_bytes(&self) -> &[u8] {
        &self.recent[..self.recent_len]
    }

    /// One line naming what did not come up, and what it means.
    ///
    /// Written for someone looking at a screen they cannot type into, so it
    /// says *which* stack failed rather than that something did. The keyboard
    /// case leads, because a machine with no keyboard cannot navigate to the
    /// panel that says more.
    pub fn trouble(&self) -> &'static str {
        use crate::i2c_hid::Failure;
        match (self.has_keyboard(), self.has_pointer()) {
            (false, false) => {
                "no keyboard and no pointer answered. Keys are read from the \
                 8042 regardless, so they may still work"
            }
            (false, true) => {
                "no keyboard answered its probes. Keys are read from the 8042 \
                 regardless, so they may still work"
            }
            (true, false) => match self.i2c_failure {
                None => "no pointer answered on any bus",
                Some(Failure::NoDevice) => {
                    "no pointer: nothing on PS/2 or USB, and no I2C-HID device in this firmware"
                }
                Some(Failure::NoDsdt) => "no pointer: the DSDT could not be read",
                Some(Failure::NoControllerDevice) => {
                    "no pointer: no ACPI table declares the controller named in _CRS"
                }
                Some(Failure::NoControllerAddress) => {
                    "no pointer: the I2C controller declares no _ADR, so its PCI slot is unknown"
                }
                Some(Failure::NoControllerPci) => {
                    "no pointer: no LPSS I2C controller at the slot _ADR names"
                }
                Some(Failure::ControllerFailed) => {
                    "no pointer: the I2C controller did not leave reset"
                }
                Some(Failure::NoHidDescriptor) => {
                    "no pointer: the touchpad did not answer at its descriptor register"
                }
                Some(Failure::ResetFailed) => "no pointer: the touchpad would not reset",
                Some(Failure::ReportDescriptorFailed) | Some(Failure::DescriptorTooLarge) => {
                    "no pointer: the touchpad's report descriptor could not be read"
                }
                Some(Failure::NoPointerReport) => {
                    "no pointer: the touchpad reports contacts only, not a cursor"
                }
            },
            (true, true) => "",
        }
    }

    /// What each stack found, for the interface to report.
    ///
    /// Reported rather than summarised: "no cursor" has several causes and
    /// they need different answers from the person reading it. A pointer
    /// missing from both stacks on a notebook means the touchpad is on
    /// I2C-HID and an external mouse will work; one missing only from USB
    /// means nothing was plugged in.
    pub fn found(&self) -> Found {
        Found {
            ps2_keyboard: self.keyboard && !self.usb.as_ref().is_some_and(|u| u.keyboard.is_some()),
            ps2_pointer: match self.pointer {
                PointerKind::Usb | PointerKind::None => None,
                kind => Some(kind),
            },
            usb_controller: self.usb.is_some(),
            usb_keyboard: self.usb.as_ref().is_some_and(|u| u.keyboard.is_some()),
            usb_pointer: self.usb.as_ref().is_some_and(|u| u.pointer.is_some()),
            i2c_abandoned: self.i2c.as_ref().is_some_and(|pad| pad.abandoned()),
            i2c: self.i2c.as_ref().map(|pad| I2cFound {
                slave_address: pad.slave_address(),
                controller: pad.controller,
                vendor: pad.vendor,
                product: pad.product,
                report_id: pad.report_id(),
                input_register: pad.input_register(),
                interrupt_pin: pad.interrupt_pin,
                gate: pad.gate_state(),
                discovery: pad.discovery,
            }),
            i2c_failure: self.i2c_failure,
        }
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
            // SAFETY: forwarded from this function's own contract.
            if let Some(event) = unsafe { self.poll_usb() } {
                return Some(event);
            }
            // SAFETY: as above.
            return unsafe { self.poll_i2c() };
        }
        // SAFETY: the status bit says a byte is waiting.
        let byte = unsafe { inb(DATA) };
        self.remember(byte);

        if status & STATUS_AUX_DATA == 0 {
            return self.keystroke(byte);
        }

        self.accumulate(byte)
    }

    /// Turns one keyboard byte into an event, in whichever set arrives.
    ///
    /// The set is not configured, it is *observed*: a `0xF0` is set 2's break
    /// prefix and cannot appear as a make code, so seeing one settles which
    /// numbering this controller is delivering. Forcing the configuration
    /// instead — writing the command byte's translation bit and hoping — is
    /// what leaves a keyboard silent on a machine that ignored the write.
    fn keystroke(&mut self, byte: u8) -> Option<Event> {
        if self.set2_break {
            self.set2_break = false;
            return Some(Event::Key(Key {
                scancode: translate_set2(byte),
                pressed: false,
            }));
        }
        if byte == SET2_BREAK {
            self.set2 = true;
            self.set2_break = true;
            return None;
        }
        if self.set2 {
            return Some(Event::Key(Key {
                scancode: translate_set2(byte),
                pressed: true,
            }));
        }
        // Set 1: bit 7 marks a release. Extended keys are prefixed with 0xE0,
        // reported as its own event rather than merged — nothing here binds
        // an extended key.
        Some(Event::Key(Key {
            scancode: byte & 0x7F,
            pressed: byte & 0x80 == 0,
        }))
    }

    /// Which scan code set the keyboard turned out to be using.
    pub fn scancode_set(&self) -> u8 {
        if self.set2 {
            2
        } else {
            1
        }
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

impl Input {
    /// Polls the I2C-HID touchpad, if there is one.
    ///
    /// # Safety
    /// Drives the I2C controller; requires ring 0.
    unsafe fn poll_i2c(&mut self) -> Option<Event> {
        // Not yet due. Compared by wrapped difference, so a counter that
        // rolls over does not make every deadline fire at once.
        let now = crate::arch::counter_ordered();
        if (self.i2c_next.wrapping_sub(now) as i64) > 0 {
            return None;
        }
        self.i2c_next = now.wrapping_add(self.i2c_interval);

        let device = self.i2c.as_mut()?;
        // SAFETY: forwarded from this function's own contract.
        let report = unsafe { device.poll() }?;

        match report {
            crate::i2c_hid::Report::Motion(movement) => {
                // A report with no movement and no button is what the device
                // sends when a finger settles. Passing it on would cost a
                // cursor redraw a frame for nothing.
                if movement == Default::default() {
                    return None;
                }
                Some(Event::Motion(Motion {
                    dx: movement.dx,
                    // The HID axis points down, the same way a framebuffer's
                    // does — unlike PS/2, which is why this is not negated
                    // and that one is.
                    dy: movement.dy,
                    wheel: movement.wheel,
                    left: movement.left,
                    right: movement.right,
                    middle: movement.middle,
                }))
            }
            crate::i2c_hid::Report::Keys(report) => {
                // The same held-key diffing the USB path does, against the
                // same eight-byte shape — which is why the layout converts to
                // it rather than inventing a third representation.
                let event = keys_changed(&self.i2c_previous, &report);
                self.i2c_previous = report;
                event
            }
        }
    }

    /// Polls the USB HID device, if there is one.
    ///
    /// # Safety
    /// Drives the controller; requires ring 0.
    unsafe fn poll_usb(&mut self) -> Option<Event> {
        let usb = self.usb.as_mut()?;
        // SAFETY: forwarded from this function's own contract.
        unsafe { usb.poll() }
    }
}

impl UsbInput {
    /// Finds an xHCI controller and enumerates the HID devices on it.
    ///
    /// Every connected port is tried, and the first keyboard and the first
    /// pointer are both kept. On a laptop the root ports carry a webcam, a
    /// Bluetooth radio and a fingerprint reader as well, and which of them
    /// answers first is a property of the board — so stopping at port one, as
    /// this used to, finds a device that is not an input device and reports
    /// success.
    ///
    /// # Safety
    /// Drives PCI and the controller; requires ring 0 and an identity map.
    unsafe fn bring_up() -> Option<UsbInput> {
        use crate::pci;
        use crate::xhci::HidKind;

        // Stops at the first xHCI rather than enumerating the rest of the
        // machine: this is the only device wanted, and every further slot
        // probed is port I/O paid for nothing.
        // SAFETY: forwarded from this function's own contract.
        let found = unsafe { pci::find(|dev| dev.usb_kind() == Some(pci::UsbKind::Xhci)) }?;

        // SAFETY: as above; the BAR is inside the identity map. This is the
        // only place a controller is brought up, so nothing else is holding
        // one when it is reset.
        let controller = unsafe { crate::xhci::Xhci::new(&found) }?;

        let mut usb = UsbInput {
            controller,
            keyboard: None,
            pointer: None,
            previous: [0; 8],
            next: 0,
        };

        // Collected first: `connected_ports` borrows the controller, and the
        // enumeration below needs it mutably. A fixed array rather than a
        // vector, there being no allocator.
        let mut ports = [0u8; 32];
        let mut count = 0;
        // SAFETY: as above.
        for port in unsafe { usb.controller.connected_ports() } {
            if count == ports.len() {
                break;
            }
            ports[count] = port;
            count += 1;
        }

        for &port in &ports[..count] {
            if usb.keyboard.is_some() && usb.pointer.is_some() {
                break;
            }
            // A device that fails its reset is skipped rather than abandoning
            // the scan: one unresponsive port on a hub is common and says
            // nothing about the next.
            // SAFETY: as above.
            if !unsafe { usb.controller.reset_port(port) } {
                continue;
            }
            // SAFETY: as above.
            let Some((index, hid)) = (unsafe { usb.controller.enumerate(port) }) else {
                continue;
            };
            match hid.kind {
                HidKind::Keyboard if usb.keyboard.is_none() => usb.keyboard = Some(index),
                HidKind::Pointer if usb.pointer.is_none() => usb.pointer = Some(index),
                // A device that is not a boot-protocol HID, or a second of a
                // kind already held. Its slot stays addressed but nothing
                // reads it.
                _ => {}
            }
        }

        (usb.keyboard.is_some() || usb.pointer.is_some()).then_some(usb)
    }

    /// Takes one event from whichever device has one waiting.
    ///
    /// The two devices are serviced in turn rather than keyboard-first, so a
    /// mouse being moved continuously cannot starve the keyboard or the other
    /// way round.
    ///
    /// # Safety
    /// Drives the controller; requires ring 0.
    unsafe fn poll(&mut self) -> Option<Event> {
        for _ in 0..2 {
            let take_keyboard = self.next == 0;
            self.next ^= 1;

            let event = if take_keyboard {
                // SAFETY: forwarded from this function's own contract.
                unsafe { self.poll_keyboard() }
            } else {
                // SAFETY: as above.
                unsafe { self.poll_pointer() }
            };
            if event.is_some() {
                return event;
            }
        }
        None
    }

    /// # Safety
    /// Drives the controller; requires ring 0.
    unsafe fn poll_keyboard(&mut self) -> Option<Event> {
        let device = self.keyboard?;
        let mut report = [0u8; 8];
        // SAFETY: forwarded from this function's own contract.
        unsafe { self.controller.poll_report(device, &mut report)? };

        // Boot protocol: byte 0 is modifiers, byte 1 reserved, bytes 2..8 are
        // up to six held keycodes. It reports *state*, not events, so a press
        // is a keycode that was not in the previous report and a release is
        // one that has left it.
        for &code in &report[2..8] {
            if code == 0 || self.previous[2..8].contains(&code) {
                continue;
            }
            self.previous = report;
            return Some(Event::Key(Key {
                scancode: hid_to_scancode(code),
                pressed: true,
            }));
        }
        for &code in &self.previous[2..8] {
            if code == 0 || report[2..8].contains(&code) {
                continue;
            }
            self.previous = report;
            return Some(Event::Key(Key {
                scancode: hid_to_scancode(code),
                pressed: false,
            }));
        }
        self.previous = report;
        None
    }

    /// # Safety
    /// Drives the controller; requires ring 0.
    unsafe fn poll_pointer(&mut self) -> Option<Event> {
        let device = self.pointer?;
        let mut report = [0u8; 8];
        // SAFETY: forwarded from this function's own contract.
        unsafe { self.controller.poll_report(device, &mut report)? };

        // Boot protocol: buttons, then signed X and Y as single bytes —
        // already two's complement, unlike PS/2's nine-bit split across the
        // flags byte.
        Some(Event::Motion(Motion {
            dx: report[1] as i8 as i32,
            dy: report[2] as i8 as i32,
            wheel: report[3] as i8 as i32,
            left: report[0] & 0x01 != 0,
            right: report[0] & 0x02 != 0,
            middle: report[0] & 0x04 != 0,
        }))
    }
}

/// Turns a change between two boot-protocol reports into one event.
///
/// Shared by the USB and I2C-HID keyboard paths: both deliver *state* — the
/// keys currently held — where the interface wants events, and the difference
/// between two reports is where the events are.
fn keys_changed(previous: &[u8; 8], current: &[u8; 8]) -> Option<Event> {
    for &code in &current[2..8] {
        if code != 0 && !previous[2..8].contains(&code) {
            return Some(Event::Key(Key {
                scancode: hid_to_scancode(code),
                pressed: true,
            }));
        }
    }
    for &code in &previous[2..8] {
        if code != 0 && !current[2..8].contains(&code) {
            return Some(Event::Key(Key {
                scancode: hid_to_scancode(code),
                pressed: false,
            }));
        }
    }
    None
}

/// Maps a set 2 code to set 1, leaving anything outside the table alone.
fn translate_set2(code: u8) -> u8 {
    SET2_TO_SET1.get(code as usize).copied().unwrap_or(code)
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

/// Brings the keyboard up, and says whether anything is there.
///
/// The reset is attempted twice and then given up on, but a failed reset is
/// *not* taken as "no keyboard". A notebook's keyboard reaches the 8042
/// through the embedded controller, and an EC that is slow to answer a reset —
/// or that swallows it entirely, which several do — still delivers
/// keystrokes perfectly well afterwards. Treating a missed reset as an absent
/// device is how a built-in keyboard ends up detected as missing on the one
/// kind of machine that always has one.
///
/// What actually decides it is `ENABLE`: a device that acknowledges the
/// command to start scanning is a device that is going to send scancodes.
///
/// # Safety
/// Requires ring 0.
unsafe fn keyboard_up() -> bool {
    // SAFETY: caller guarantees ring 0.
    unsafe {
        // **Enable scanning first, reset only if that fails.**
        //
        // Firmware has already initialised this keyboard — it is what the
        // boot menu was typed on. A reset throws that away and asks an
        // embedded controller to redo it, and an EC that answers a reset out
        // of the expected order, or takes longer than the wait allows, ends
        // up in a state the driver then misreads. Asking it to start
        // scanning is the smaller request, and on a keyboard firmware left
        // configured it is the only one needed.
        write_data(DEV_ENABLE);
        if read_data() == Some(ACK) {
            return true;
        }
        drain();

        // It did not acknowledge. Now a reset is worth the risk, because the
        // alternative is no keyboard at all.
        for _ in 0..2 {
            if reset_device(false) {
                // Scanning again, since the reset cleared it.
                write_data(DEV_ENABLE);
                let _ = read_data();
                return true;
            }
            drain();
        }

        // Neither worked. The port is still enabled and `poll` reads it
        // regardless, so a keyboard that simply does not answer probes can
        // still deliver scancodes — this only decides what the interface
        // reports.
        false
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
