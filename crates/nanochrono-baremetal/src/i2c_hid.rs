// SPDX-License-Identifier: Apache-2.0
//! HID over I2C: the notebook touchpad.
//!
//! # The whole chain
//!
//! A built-in touchpad is the least discoverable device in a modern laptop.
//! Reaching one needs four separate things to line up, and this module is
//! where they meet:
//!
//! | Step | Where |
//! |---|---|
//! | Walk the firmware's AML for a `PNP0C50` device | [`nanochrono_core::aml`] |
//! | Find the I2C controller it hangs off, as a PCI device | here, via `_ADR` |
//! | Drive that controller | [`crate::i2c`] |
//! | Understand the reports it sends | [`nanochrono_core::hid_report`] |
//!
//! Miss any one and there is no cursor. That is why a touchpad works under
//! every operating system and under no bootloader: the chain is long, and
//! most of it is firmware interpretation rather than hardware access.
//!
//! # Why the device is in mouse mode, and why that is wanted
//!
//! A Windows Precision Touchpad declares two collections: a digitizer that
//! reports absolute contacts, and an ordinary relative mouse. It sends the
//! mouse reports until the host writes an Input Mode feature report asking
//! for the other. Desktop drivers do that, because they want gestures.
//!
//! This deliberately does not. The interface here wants a cursor, and the
//! mouse collection *is* a cursor — already integrated by the device's own
//! firmware, with no gesture recognition, no contact tracking and no
//! report-descriptor machinery beyond finding which report it is.
//!
//! # Polled, but gated on the interrupt line
//!
//! The `_CRS` declares a `GpioInt`: the pin the device raises when it has a
//! report. Every other kernel routes that to an interrupt controller and
//! sleeps. This one will not — an interrupt handler running between two
//! counter reads becomes part of what the chronometer is measuring — so the
//! input register is read on a schedule instead. The specification defines
//! what that returns when there is nothing waiting, a length of zero, which
//! is what makes polling legal rather than a guess.
//!
//! Polling blind is expensive, though: a thirty-byte transfer at 400 kHz is
//! roughly 700 µs, paid every frame whether or not the finger moved. So the
//! pin is *read* even though it is not serviced — one MMIO load through
//! [`crate::gpio`], a few hundred nanoseconds — and the bus is only touched
//! when it says there is something to fetch. Same schedule, same absence of
//! interrupts, two orders of magnitude less bus time.
//!
//! Gating is an optimisation and never a gate on correctness: if the pad
//! cannot be identified, or stops agreeing with what the polls find, the
//! driver falls back to reading the bus every time and nothing is lost but
//! the saving.
//!
//! # Reference
//!
//! Command opcodes and the descriptor layout follow the HID over I2C
//! specification and FreeBSD's `sys/dev/iicbus/iichid.c`, whose BSD-2-Clause
//! terms permit reproducing its form and ask for attribution in return; see
//! `NOTICE`. Linux's `drivers/hid/i2c-hid/` is GPL and was not imitated.

use crate::gpio;
use crate::i2c::{I2cMaster, Speed};
use crate::pci;
use nanochrono_core::aml::{GpioInterrupt, Namespace, Path, Provenance, Value};
use nanochrono_core::hid_report::{self, KeyboardLayout, Movement, PointerLayout};

/// Command opcodes, from the specification's command register format.
const CMD_RESET: u8 = 0x01;
const CMD_SET_POWER: u8 = 0x08;
/// Power state 0 is on.
const POWER_ON: u8 = 0x00;

/// The fixed size of the HID descriptor.
const HID_DESCRIPTOR_LEN: usize = 30;
/// What its first field must read for this to be a HID descriptor at all.
const HID_DESCRIPTOR_LENGTH_FIELD: u16 = 30;
/// Version 1.00, the only one defined.
const HID_VERSION: u16 = 0x0100;

/// How large a report descriptor this will read.
///
/// The touchpad this was written against declares 675 bytes. Two kilobytes is
/// past anything a pointer device produces; a descriptor larger than this is
/// refused rather than truncated, because a truncated descriptor parses into
/// field offsets that are quietly wrong.
const MAX_REPORT_DESCRIPTOR: usize = 2048;

/// How large an input report this will read.
const MAX_INPUT_REPORT: usize = 64;

/// Scratch space for the report descriptor.
///
/// A static because there is no allocator, and two kilobytes is more than a
/// stack frame should carry in a kernel whose stack is 64 KiB.
static mut REPORT_DESCRIPTOR: [u8; MAX_REPORT_DESCRIPTOR] = [0; MAX_REPORT_DESCRIPTOR];

/// The registers a device's HID descriptor names.
#[derive(Debug, Clone, Copy, Default)]
struct HidDescriptor {
    report_descriptor_length: u16,
    report_descriptor_register: u16,
    /// Where input reports live. Recorded but not written before a read:
    /// the specification has the device's read pointer already sitting there,
    /// and `iichid` reads it plainly. It is kept because a descriptor whose
    /// input register is zero is a descriptor that was misread, and because
    /// the interface reports it.
    input_register: u16,
    max_input_length: u16,
    command_register: u16,
    #[allow(dead_code)]
    data_register: u16,
    vendor: u16,
    product: u16,
}

impl HidDescriptor {
    /// Reads the fixed layout out of the thirty bytes the device returned.
    fn parse(bytes: &[u8; HID_DESCRIPTOR_LEN]) -> Option<HidDescriptor> {
        let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);

        let length = word(0);
        let version = word(2);
        // Both are fixed by the specification. A device that answers with
        // neither is not an I2C-HID device at this register — most likely the
        // `_DSM` was misread, and going on would mean interpreting arbitrary
        // bytes as register addresses.
        if length != HID_DESCRIPTOR_LENGTH_FIELD || version != HID_VERSION {
            return None;
        }
        // A register of zero is not a register. Two of these are read from
        // and one is written to, and a zero in any of them means the thirty
        // bytes that arrived were not a descriptor even though the first two
        // fields happened to match.
        if word(6) == 0 || word(8) == 0 || word(14) == 0 {
            return None;
        }
        Some(HidDescriptor {
            report_descriptor_length: word(4),
            report_descriptor_register: word(6),
            input_register: word(8),
            max_input_length: word(10),
            command_register: word(14),
            data_register: word(16),
            vendor: word(20),
            product: word(22),
        })
    }
}

/// What the device turned out to be.
///
/// Both kinds are wanted, and on a notebook both may be present: the touchpad
/// is always here, and the keyboard is too on machines that do not put it on
/// the 8042. Nothing else is — a device that is neither is addressed and then
/// left alone.
#[derive(Debug, Clone, Copy)]
enum Layout {
    Pointer(PointerLayout),
    Keyboard(KeyboardLayout),
}

/// What a poll produced.
#[derive(Debug, Clone, Copy)]
pub enum Report {
    Motion(Movement),
    /// Held keys, in the eight-byte shape a boot keyboard sends: modifiers, a
    /// reserved byte, then up to six usages. Chosen so the keyboard handling
    /// this kernel already has for USB works unchanged.
    Keys([u8; 8]),
}

/// Which kind a device is, for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pointer,
    Keyboard,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Pointer => "touchpad",
            Kind::Keyboard => "keyboard",
        }
    }
}

/// How the driver decides whether the bus is worth touching.
///
/// Three states, and the driver moves between them on evidence alone:
///
/// * [`Gate::Blind`] — no usable GPIO controller. Every poll reads the bus.
///   This is what the driver did before there was a GPIO driver, and it is
///   still what it does on a machine whose controller cannot be reached.
/// * [`Gate::Learning`] — the controller is open and the pad is not known
///   yet. Every poll still reads the bus, and each one also contributes a
///   labelled sample to the correlation in [`gpio::Calibration`].
/// * [`Gate::Gated`] — the pad is known. The bus is read only when the pin
///   says a report is waiting.
///
/// The last transition is not one-way. A gate that starts disagreeing with
/// the device — the pin quiet while reports are in fact waiting — is dropped
/// and the driver goes back to reading blind, because a wrong gate loses
/// input and a missing one only costs bus time.
enum Gate {
    Blind,
    Learning(gpio::Controller, gpio::Calibration),
    Gated {
        controller: gpio::Controller,
        pad: gpio::Pad,
        /// The level that means "a report is waiting".
        asserted: bool,
        /// How the pad was arrived at, for reporting.
        mapping: gpio::Mapping,
        /// Polls skipped because the pin was quiet. Reported, because it is
        /// the whole benefit and it should be visible.
        skipped: u32,
        /// Consecutive times the gate said quiet and a forced read found a
        /// report anyway. See [`GATE_STRIKES`].
        strikes: u32,
    },
}

/// Wrong-gate strikes tolerated before the gate is abandoned.
///
/// One is not enough: a report can arrive in the window between reading the
/// pin and reading the bus, which is a race the gate cannot win and does not
/// need to — the report is still delivered on the next poll. Three in a row
/// is not a race, it is the wrong pad.
const GATE_STRIKES: u32 = 3;

/// How often a gated driver reads the bus even though the pin said quiet.
///
/// The gate is an optimisation built on a guess about which wire is which, so
/// it is audited rather than trusted: one poll in this many ignores the pin
/// entirely. If those forced reads keep finding reports the pin did not
/// announce, the gate is wrong and gets dropped. Sixty-four at 250 Hz is an
/// audit every quarter second, which costs about 0.4% of the bus time the
/// gate saves.
const AUDIT_EVERY: u32 = 64;

/// A touchpad, brought up and ready to poll.
pub struct I2cHid {
    master: I2cMaster,
    address: u16,
    descriptor: HidDescriptor,
    layout: Layout,
    /// Where the controller sits, for reporting.
    pub controller: (u8, u8, u8),
    pub vendor: u16,
    pub product: u16,
    /// The pin it raises when it has a report, from `_CRS`.
    pub interrupt_pin: Option<u16>,
    /// How its address and descriptor register were arrived at.
    pub discovery: Discovery,
    /// Whether its resources came from an evaluated `_CRS` or from the
    /// buffers the device declares.
    pub provenance: Provenance,
    /// Whether, and how, that pin is being read.
    gate: Gate,
    /// Polls taken, for the audit schedule.
    polls: u32,
    /// Consecutive failed reads. A device that has stopped answering stops
    /// being asked: every failed read costs a full bus timeout, and paying
    /// that once a frame is an interface that stops responding.
    failures: u32,
}

/// How many failed reads in a row before the touchpad is given up on.
const GIVE_UP_AFTER: u32 = 16;

impl I2cHid {
    /// Finds and brings up the machine's I2C-HID pointer, if it has one.
    ///
    /// Every step can fail on its own, and each failure means something
    /// different — which is why [`Failure`] names them rather than returning
    /// a bare `None`.
    ///
    /// # Safety
    /// Reads firmware tables, drives PCI configuration space and I2C MMIO;
    /// requires ring 0 and an identity map.
    pub unsafe fn probe(ticks_per_us: u64) -> Result<I2cHid, Failure> {
        // SAFETY: forwarded from this function's own contract.
        unsafe { I2cHid::probe_nth(0, ticks_per_us) }
    }

    /// The `index`th I2C-HID device in the firmware's namespace.
    ///
    /// # Safety
    /// As [`probe`](Self::probe).
    pub unsafe fn probe_nth(index: usize, ticks_per_us: u64) -> Result<I2cHid, Failure> {
        // SAFETY: forwarded from this function's own contract.
        let table = unsafe { crate::acpi::dsdt() }.ok_or(Failure::NoDsdt)?;
        let namespace = Namespace::new(table).ok_or(Failure::NoDsdt)?;

        // The device, looked for in every table rather than only the DSDT.
        // A touchpad node is usually in the DSDT, but nothing requires it —
        // and the controller it names very often is not.
        let mut scratch = [0u8; 32];
        // SAFETY: as above.
        let device = unsafe { find_device_nth(&namespace, index, &mut scratch) }
            .ok_or(Failure::NoDevice)?;

        // The controller the `_CRS` named, as a PCI address. `_ADR` on the
        // controller's namespace node holds device and function; the bus is
        // the host bridge's, which is zero on every machine with one root.
        //
        // Searched across *every* table, not just the DSDT. The namespace is
        // the union of the DSDT and the SSDTs, and firmware splits it: the
        // machine this was written against has `Device (TPD0)` in the DSDT
        // and the `Device (I2C5)` it hangs off — with the `_ADR` that says
        // where on the PCI bus it is — in one of sixteen SSDTs. Looking only
        // in the DSDT finds the touchpad, resolves its controller by name,
        // and then cannot find that name anywhere.
        // SAFETY: as above.
        let address = unsafe { controller_address(&namespace, &device.bus.controller) }?;
        let slot = ((address >> 16) & 0x1F) as u8;
        let function = (address & 0x07) as u8;

        // SAFETY: as above.
        let found = unsafe {
            pci::find(|candidate| {
                candidate.slot == slot
                    && candidate.function == function
                    && candidate.class == LPSS_CLASS
                    && candidate.subclass == LPSS_SUBCLASS
            })
        }
        .ok_or(Failure::NoControllerPci)?;

        // SAFETY: as above; the BAR is inside the identity map.
        let mut master = unsafe {
            I2cMaster::new(
                &found,
                Speed::for_hz(device.bus.connection_speed),
                ticks_per_us,
            )
            .ok_or(Failure::ControllerFailed)?
        };

        // Where the device actually is, which is not always where the
        // firmware said — see `discover`.
        // SAFETY: as above.
        let (address, _register, descriptor, discovery) = unsafe {
            discover(
                &mut master,
                device.bus.slave_address,
                device.descriptor_register,
            )
        }
        .ok_or(Failure::NoHidDescriptor)?;

        // SAFETY: as above.
        unsafe { power_on(&mut master, address, &descriptor, ticks_per_us).ok_or(Failure::ResetFailed)? };

        let length = descriptor.report_descriptor_length as usize;
        let _ = index;
        if length == 0 || length > MAX_REPORT_DESCRIPTOR {
            return Err(Failure::DescriptorTooLarge);
        }
        // SAFETY: as above. `REPORT_DESCRIPTOR` is only touched here and by
        // the parse below, both before this function returns, so nothing
        // aliases it.
        let report_descriptor = unsafe {
            let buffer = core::ptr::addr_of_mut!(REPORT_DESCRIPTOR).cast::<u8>();
            let into = core::slice::from_raw_parts_mut(buffer, length);
            let register = descriptor.report_descriptor_register.to_le_bytes();
            if !master.write_read(address, &register, into) {
                return Err(Failure::ReportDescriptorFailed);
            }
            core::slice::from_raw_parts(buffer.cast_const(), length)
        };

        // A pointer first, because a touchpad that also declares a keyboard
        // page — several do, for their media keys — is a pointer.
        let layout = match hid_report::find_pointer(report_descriptor) {
            Some(pointer) => Layout::Pointer(pointer),
            None => Layout::Keyboard(
                hid_report::find_keyboard(report_descriptor).ok_or(Failure::NoPointerReport)?,
            ),
        };

        // The readiness gate, if the machine will give one up. Everything
        // below is best-effort by construction: each `?` in `open_gate` ends
        // in `Gate::Blind`, which is exactly what the driver did before the
        // GPIO reader existed.
        // SAFETY: as above.
        let gate = unsafe { open_gate(&namespace, device.interrupt) };

        Ok(I2cHid {
            master,
            address,
            descriptor,
            layout,
            controller: (found.bus, found.slot, found.function),
            vendor: descriptor.vendor,
            product: descriptor.product,
            interrupt_pin: device.interrupt.map(|gpio| gpio.pin),
            discovery,
            provenance: device.provenance,
            gate,
            polls: 0,
            failures: 0,
        })
    }

    /// Reads one report, if the device has one waiting.
    ///
    /// Never blocks. A device with nothing to say answers with a length of
    /// zero, which the specification defines precisely so a host can ask
    /// without an interrupt — see the module docs.
    ///
    /// # Safety
    /// Drives I2C MMIO; requires ring 0.
    pub unsafe fn poll(&mut self) -> Option<Report> {
        if self.failures >= GIVE_UP_AFTER {
            return None;
        }
        let want = (self.descriptor.max_input_length as usize).min(MAX_INPUT_REPORT);
        if want < 3 {
            return None;
        }

        self.polls = self.polls.wrapping_add(1);
        let audit = self.polls % AUDIT_EVERY == 0;

        // Read the pin *before* the bus. Reading the input register is what
        // makes the device drop its line, so a level sampled afterwards says
        // nothing about whether there was anything to fetch.
        let sample = match &self.gate {
            Gate::Learning(controller, calibration) => Some(calibration.sample(controller)),
            _ => None,
        };
        if let Gate::Gated {
            controller,
            pad,
            asserted,
            skipped,
            ..
        } = &mut self.gate
        {
            if !audit && controller.level(*pad) != *asserted {
                *skipped = skipped.saturating_add(1);
                return None;
            }
        }

        let mut buffer = [0u8; MAX_INPUT_REPORT];

        // A plain read of the input register: no register address is written
        // first, because the device's read pointer already sits there.
        // SAFETY: forwarded from this function's own contract.
        if !unsafe {
            self.master
                .write_read(self.address, &[], &mut buffer[..want])
        } {
            self.failures += 1;
            return None;
        }
        self.failures = 0;

        // The first two bytes are the length of this report, the whole
        // record included. Zero means "nothing waiting" and is the answer
        // this gets on most polls.
        let length = u16::from_le_bytes([buffer[0], buffer[1]]) as usize;
        // Zero is "nothing waiting", which is the answer to most polls. A
        // length larger than was asked for is a device disagreeing with its
        // own descriptor — not something to read past the buffer for — and a
        // length below three cannot hold its own header and a report.
        let had_report = length >= 3 && length <= want;

        self.judge(sample, audit, had_report);

        if !had_report {
            return None;
        }

        match self.layout {
            Layout::Pointer(pointer) => pointer.decode(&buffer[2..length]).map(Report::Motion),
            Layout::Keyboard(keyboard) => keyboard.decode(&buffer[2..length]).map(Report::Keys),
        }
    }

    /// Files a poll's outcome against the gate, and moves it if warranted.
    ///
    /// Two jobs, because both depend on the same fact and it is only known
    /// here — after the bus has answered:
    ///
    /// * While learning, the sample taken before the read gets its label, and
    ///   the moment the correlation resolves the gate closes.
    /// * While gated, a forced audit read that finds a report the pin did not
    ///   announce is a strike. [`GATE_STRIKES`] of them in a row means the
    ///   pad is wrong, and the gate is dropped rather than kept and believed.
    fn judge(&mut self, sample: Option<gpio::Sample>, audit: bool, had_report: bool) {
        // Taken out and put back rather than borrowed in place: a resolved
        // calibration has to move its controller into the gated variant, and
        // that is a move out of the very thing being matched on.
        let gate = core::mem::replace(&mut self.gate, Gate::Blind);
        self.gate = match gate {
            Gate::Blind => Gate::Blind,
            Gate::Learning(controller, mut calibration) => {
                let Some(sample) = sample else {
                    return self.gate = Gate::Learning(controller, calibration);
                };
                calibration.observe(sample, had_report);
                match calibration.settled() {
                    Some((pad, asserted)) => Gate::Gated {
                        controller,
                        pad,
                        asserted,
                        mapping: gpio::Mapping::Calibrated,
                        skipped: 0,
                        strikes: 0,
                    },
                    None => Gate::Learning(controller, calibration),
                }
            }
            Gate::Gated {
                controller,
                pad,
                asserted,
                mapping,
                skipped,
                mut strikes,
            } => {
                if audit {
                    if had_report {
                        strikes = strikes.saturating_add(1);
                    } else {
                        strikes = 0;
                    }
                }
                if strikes >= GATE_STRIKES {
                    Gate::Blind
                } else {
                    Gate::Gated {
                        controller,
                        pad,
                        asserted,
                        mapping,
                        skipped,
                        strikes,
                    }
                }
            }
        };
    }

    /// One line describing the readiness gate, for the interface.
    ///
    /// Worth showing rather than hiding: it is the difference between a
    /// touchpad costing 700 µs of bus time per frame and costing none, and
    /// on an unrecognised machine it is also a live report of a measurement
    /// in progress.
    pub fn gate_state(&self) -> GateState {
        match &self.gate {
            Gate::Blind => GateState::Blind,
            Gate::Learning(_, calibration) => {
                let (reports, quiet) = calibration.progress();
                GateState::Learning {
                    watching: calibration.watching(),
                    reports,
                    quiet,
                }
            }
            Gate::Gated {
                mapping, skipped, ..
            } => GateState::Gated {
                mapping: *mapping,
                skipped: *skipped,
            },
        }
    }

    /// Whether this device is a pointer or a keyboard.
    pub fn kind(&self) -> Kind {
        match self.layout {
            Layout::Pointer(_) => Kind::Pointer,
            Layout::Keyboard(_) => Kind::Keyboard,
        }
    }

    /// The slave address, for reporting.
    pub fn slave_address(&self) -> u16 {
        self.address
    }

    /// Which report the pointer is on, for reporting.
    pub fn report_id(&self) -> Option<u8> {
        match self.layout {
            Layout::Pointer(pointer) => pointer.report_id,
            Layout::Keyboard(keyboard) => keyboard.report_id,
        }
    }

    /// The register input reports are read from, for reporting.
    pub fn input_register(&self) -> u16 {
        self.descriptor.input_register
    }

    /// Whether the device has stopped answering and been given up on.
    pub fn abandoned(&self) -> bool {
        self.failures >= GIVE_UP_AFTER
    }
}

/// What the readiness gate is doing, for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    /// No GPIO controller; every poll reads the bus.
    Blind,
    /// Watching pads, waiting for the correlation to resolve.
    Learning {
        watching: usize,
        reports: u32,
        quiet: u32,
    },
    /// The pin is known and is deciding which polls touch the bus.
    Gated {
        mapping: gpio::Mapping,
        skipped: u32,
    },
}

/// Opens a readiness gate for a device, or decides there will not be one.
///
/// Two routes, tried in that order:
///
/// 1. The pad-group table, which is exact when it applies. Its answer is
///    checked against the hardware — a pin ACPI called an interrupt source
///    that does not read back as a host-owned GPIO input means the table is
///    for a different variant of the part — and rejected if it does not hold.
/// 2. Correlation, which applies everywhere and takes a few hundred
///    milliseconds of ordinary polling to resolve.
///
/// Either way the failure mode is [`Gate::Blind`], which is not a failure so
/// much as the absence of an optimisation.
///
/// # Safety
///
/// Maps and reads firmware-declared MMIO; requires ring 0 and an identity
/// map. Reads only.
unsafe fn open_gate(namespace: &Namespace, interrupt: Option<GpioInterrupt>) -> Gate {
    let Some(interrupt) = interrupt else {
        return Gate::Blind;
    };

    // The controller's own `_HID` selects the pad-group table. A controller
    // with no `_HID` this recognises still opens; it just has to be
    // calibrated for instead.
    let hid = namespace
        .find_device(|candidate| candidate.path.ends_with(&interrupt.controller))
        .and_then(|candidate| candidate.hid);
    let hid = hid.as_ref().map(|id| id.as_str());

    // SAFETY: forwarded from this function's own contract.
    let Some(controller) = (unsafe { gpio::Controller::open(namespace, &interrupt.controller, hid) })
    else {
        return Gate::Blind;
    };

    if let Some(pad) = controller.resolve(interrupt.pin) {
        if controller.verify(pad) {
            let platform = controller.platform().unwrap_or("tabled");
            return Gate::Gated {
                controller,
                pad,
                // An interrupt line idles released and is pulled *down* to
                // signal, which is what every I2C-HID touchpad does and what
                // `_CRS` says with an active-low polarity. Taken as read here
                // rather than measured, because the table route has no
                // measurement to offer — the audit in `judge` is what catches
                // it if this is wrong.
                asserted: false,
                mapping: gpio::Mapping::Tabled(platform),
                skipped: 0,
                strikes: 0,
            };
        }
    }

    let calibration = controller.begin_calibration();
    if calibration.watching() == 0 {
        return Gate::Blind;
    }
    Gate::Learning(controller, calibration)
}

/// Why a probe did not produce a touchpad.
///
/// Named individually because each says something different to whoever is
/// looking at the screen: a machine with no `PNP0C50` device has no I2C-HID
/// touchpad at all, while one whose controller would not come up has one and
/// this could not reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// No DSDT, or one whose checksum did not verify.
    NoDsdt,
    /// The namespace holds no `PNP0C50` device, or its `_CRS` and `_DSM`
    /// could not be read.
    NoDevice,
    /// The `_CRS` named a controller, and no table in the namespace declares
    /// a device by that name. The controller is usually in an SSDT rather
    /// than the DSDT, so this means every table was searched and none had it.
    NoControllerDevice,
    /// The controller device was found but has no readable `_ADR`, so there
    /// is nothing to say where on the PCI bus it sits.
    NoControllerAddress,
    /// The `_ADR` resolved to a slot and function with no LPSS I2C
    /// controller at it.
    NoControllerPci,
    /// The controller did not come out of reset.
    ControllerFailed,
    /// The device did not answer at its descriptor register, or answered
    /// with something that is not a HID descriptor.
    NoHidDescriptor,
    /// It would not reset or power on.
    ResetFailed,
    /// Its report descriptor is larger than this will read.
    DescriptorTooLarge,
    /// The report descriptor could not be read off the bus.
    ReportDescriptorFailed,
    /// It has neither a relative pointer report nor a key array — a
    /// digitizer-only device, which would need contact tracking this
    /// deliberately does not do.
    NoPointerReport,
}

impl Failure {
    pub const fn name(self) -> &'static str {
        match self {
            Failure::NoDsdt => "no dsdt",
            Failure::NoDevice => "no pnp0c50 device",
            Failure::NoControllerDevice => "controller device in no acpi table",
            Failure::NoControllerAddress => "controller has no _ADR",
            Failure::NoControllerPci => "no pci device at the controller's _ADR",
            Failure::ControllerFailed => "controller reset failed",
            Failure::NoHidDescriptor => "no hid descriptor",
            Failure::ResetFailed => "reset failed",
            Failure::DescriptorTooLarge => "report descriptor too large",
            Failure::ReportDescriptorFailed => "report descriptor unreadable",
            Failure::NoPointerReport => "no relative pointer report",
        }
    }
}

/// Reads the thirty-byte HID descriptor from the register `_DSM` named.
///
/// # Safety
/// Drives I2C MMIO; requires ring 0.
/// Finds the `index`th I2C-HID device across the whole namespace.
///
/// The DSDT first, then each SSDT in the order the root table lists them,
/// with `index` counting across all of them so a machine that declares its
/// touchpad in one table and its keyboard in another is enumerated as one
/// list rather than two.
///
/// # Safety
///
/// Reads firmware tables; requires ring 0 and an identity map.
unsafe fn find_device_nth(
    dsdt: &Namespace,
    index: usize,
    scratch: &mut [u8],
) -> Option<nanochrono_core::aml::I2cHidDevice> {
    if let Some(device) = dsdt.find_i2c_hid_nth(index, scratch) {
        return Some(device);
    }
    let mut skipped = dsdt.i2c_hid_count();
    let mut found = None;

    // SAFETY: forwarded from this function's own contract.
    unsafe {
        crate::acpi::for_each_ssdt(|table| {
            let Some(ssdt) = Namespace::new(table) else {
                return true;
            };
            let count = ssdt.i2c_hid_count();
            if index < skipped + count {
                found = ssdt.find_i2c_hid_nth(index - skipped, scratch);
                return false;
            }
            skipped += count;
            true
        })
    };
    found
}

/// PCI class and subclass of an Intel LPSS I2C controller: a serial bus
/// controller of a kind with no assigned subclass.
const LPSS_CLASS: u8 = 0x0C;
const LPSS_SUBCLASS: u8 = 0x80;

/// Resolves a controller named in a `_CRS` to its PCI slot and function.
///
/// Looks in the table the device was found in first — usually the DSDT, and
/// usually where the controller is too — then in every SSDT. The ACPI
/// namespace spans all of them and firmware routinely puts a device in one
/// table and the bus it sits on in another.
///
/// Each SSDT is walked as its own namespace rather than being spliced into
/// one. That is not the whole of ACPI's model — a `Scope` in one table can
/// add objects to a device declared in another, and this will not see those
/// — but it is enough for the question being asked, which is where a named
/// controller's `_ADR` is, and it needs no allocator.
///
/// # Safety
///
/// Reads firmware tables; requires ring 0 and an identity map.
unsafe fn controller_address(namespace: &Namespace, wanted: &Path) -> Result<u64, Failure> {
    /// Looks for the controller in one table, and reads its `_ADR`.
    fn in_table(namespace: &Namespace, wanted: &Path) -> Option<u64> {
        let controller =
            namespace.find_device(|candidate| candidate.path.ends_with(wanted))?;
        match namespace.evaluate(&controller, b"_ADR", &[]) {
            Some(Value::Integer(value)) => Some(value),
            _ => None,
        }
    }

    if let Some(address) = in_table(namespace, wanted) {
        return Ok(address);
    }

    let mut address = None;
    let mut named_but_addressless = false;
    // SAFETY: forwarded from this function's own contract.
    unsafe {
        crate::acpi::for_each_ssdt(|table| {
            let Some(ssdt) = Namespace::new(table) else {
                return true;
            };
            if let Some(found) = in_table(&ssdt, wanted) {
                address = Some(found);
                return false;
            }
            // Found by name but with no readable `_ADR`. Worth remembering:
            // it makes "the controller is in no table" and "the controller
            // is there but will not say where" different lines on screen.
            if ssdt
                .find_device(|candidate| candidate.path.ends_with(wanted))
                .is_some()
            {
                named_but_addressless = true;
            }
            true
        })
    };

    match address {
        Some(address) => Ok(address),
        None if named_but_addressless => Err(Failure::NoControllerAddress),
        None => Err(Failure::NoControllerDevice),
    }
}

/// How the device's address and descriptor register were arrived at.
///
/// Worth carrying, because they are not equally trustworthy and the
/// difference is visible on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    /// The firmware's own numbers, used as given.
    Firmware,
    /// The firmware's address, with a register found by trying the two the
    /// specification uses in its examples.
    ProbedRegister,
    /// Neither worked. The bus was scanned and the device answered somewhere
    /// else. See [`discover`].
    Scanned,
}

/// The lowest and highest addresses a seven-bit I2C device may use.
///
/// Below eight and above 0x77 are reserved by the I2C specification for
/// general calls, ten-bit addressing and the like. Probing them is not
/// useful and, for the reserved broadcast address in particular, not polite.
const FIRST_ADDRESS: u16 = 0x08;
const LAST_ADDRESS: u16 = 0x77;

/// Finds the device, rather than assuming the firmware said where it is.
///
/// # Why this is not simply reading `_CRS`
///
/// Because on real firmware `_CRS` frequently cannot be read, and what is
/// readable instead is a *default*. This machine's touchpad node declares
/// four vendors and picks between them at run time:
///
/// ```asl
/// If (LEqual (TPTY, One))  { Store ("ELAN06FA", _HID); Store (0x15, BADR) }
/// If (LEqual (TPTY, 0x02)) { Store ("SYNA2BA6", _HID); Store (0x2C, BADR) }
/// If (LEqual (TPTY, 0x04)) { Store ("GXTP5100", _HID); Store (0x5D, BADR) }
/// If (LEqual (TPTY, 0x05)) { Store ("FTCS0038", _HID); Store (0x38, BADR) }
/// ```
///
/// and `_CRS` then builds its descriptor from `BADR` through a vendor helper
/// method. The resource template the device declares as a plain buffer — the
/// only part a reader without a full interpreter can get at — carries `0x2C`,
/// the Synaptics address, on a machine whose touchpad is the ELAN at `0x15`.
/// Trusting it addresses a device that is not there, which is exactly the
/// silent failure this whole path exists to avoid.
///
/// # What it does instead
///
/// Asks the bus. An I2C-HID device's descriptor is thirty bytes whose first
/// two fields are fixed by the specification and three of whose registers
/// cannot be zero, so [`HidDescriptor::parse`] is a strong acceptance test —
/// strong enough that a wrong address or a wrong register is rejected rather
/// than misread. That turns "where is it?" from a question needing firmware
/// cooperation into one the hardware can be asked directly.
///
/// Three rounds, cheapest and most trustworthy first:
///
/// 1. The firmware's address and register, if it gave both.
/// 2. The firmware's address, with each candidate register.
/// 3. Every address on the bus, with each candidate register.
///
/// Round three costs one addressing phase per address that is not there — a
/// controller NAK, which the master reports immediately rather than waiting
/// out a timeout — so a full sweep is milliseconds, and it only runs when the
/// first two have already failed.
///
/// # Safety
///
/// Drives I2C MMIO; requires ring 0.
unsafe fn discover(
    master: &mut I2cMaster,
    declared_address: u16,
    declared_register: Option<u16>,
) -> Option<(u16, u16, HidDescriptor, Discovery)> {
    let candidates = |register: Option<u16>| {
        let mut list = [0u16; 3];
        let mut count = 0;
        if let Some(register) = register {
            list[count] = register;
            count += 1;
        }
        for candidate in nanochrono_core::aml::I2cHidDevice::DESCRIPTOR_REGISTER_CANDIDATES {
            if !list[..count].contains(&candidate) {
                list[count] = candidate;
                count += 1;
            }
        }
        (list, count)
    };

    // Rounds one and two: the address the firmware named.
    let (registers, count) = candidates(declared_register);
    for (index, register) in registers[..count].iter().enumerate() {
        // SAFETY: forwarded from this function's own contract.
        if let Some(descriptor) = unsafe { read_hid_descriptor(master, declared_address, *register) }
        {
            let how = if index == 0 && declared_register.is_some() {
                Discovery::Firmware
            } else {
                Discovery::ProbedRegister
            };
            return Some((declared_address, *register, descriptor, how));
        }
    }

    // Round three: the bus itself.
    let (registers, count) = candidates(None);
    for address in FIRST_ADDRESS..=LAST_ADDRESS {
        if address == declared_address {
            continue;
        }
        // A plain one-byte read first. It writes nothing, so an address that
        // turns out to belong to something else is not poked with a register
        // number it might act on; and an address with nothing at it NAKs
        // here, which skips both descriptor reads below.
        let mut probe = [0u8; 1];
        // SAFETY: as above.
        if !unsafe { master.write_read(address, &[], &mut probe) } {
            continue;
        }
        for register in &registers[..count] {
            // SAFETY: as above.
            if let Some(descriptor) = unsafe { read_hid_descriptor(master, address, *register) } {
                return Some((address, *register, descriptor, Discovery::Scanned));
            }
        }
    }

    None
}

unsafe fn read_hid_descriptor(
    master: &mut I2cMaster,
    address: u16,
    register: u16,
) -> Option<HidDescriptor> {
    let mut bytes = [0u8; HID_DESCRIPTOR_LEN];
    // SAFETY: forwarded from this function's own contract.
    if !unsafe { master.write_read(address, &register.to_le_bytes(), &mut bytes) } {
        return None;
    }
    HidDescriptor::parse(&bytes)
}

/// Powers the device on and resets it.
///
/// Three steps, in this order, following FreeBSD's `iichid`:
///
/// 1. **Power on.** A device in its low-power state acknowledges a reset and
///    then does nothing with it, so this comes first.
/// 2. **Pause.** The specification says a device that needs time after a
///    power-on should stretch the clock; not all of them do. `iichid` leaves
///    a millisecond here because Windows does, on the reasoning that devices
///    are tested against Windows and some will depend on it.
/// 3. **Reset**, then wait for the device to say it finished.
///
/// # Safety
/// Drives I2C MMIO; requires ring 0.
unsafe fn power_on(
    master: &mut I2cMaster,
    address: u16,
    descriptor: &HidDescriptor,
    ticks_per_us: u64,
) -> Option<()> {
    let command = descriptor.command_register.to_le_bytes();

    // The command register format: the register address, then a byte holding
    // the report type and ID, then the opcode.
    let set_power = [command[0], command[1], POWER_ON, CMD_SET_POWER];

    // SAFETY: forwarded from this function's own contract.
    if !unsafe { master.write_read(address, &set_power, &mut []) } {
        return None;
    }
    wait_us(ticks_per_us, POWER_SETTLE_US);

    let reset = [command[0], command[1], 0x00, CMD_RESET];
    // SAFETY: as above.
    if !unsafe { master.write_read(address, &reset, &mut []) } {
        return None;
    }

    // A reset completes by the device raising its interrupt and the host
    // reading the input register, which returns a zero length. With no
    // interrupt to wait for, the read is retried: the specification allows up
    // to five seconds, and a device that never finishes is one this does not
    // then go on to misread.
    let mut buffer = [0u8; 4];
    let deadline =
        crate::arch::counter_ordered().wrapping_add(ticks_per_us.max(1) * RESET_TIMEOUT_US);
    while (deadline.wrapping_sub(crate::arch::counter_ordered()) as i64) > 0 {
        // SAFETY: as above.
        if unsafe { master.write_read(address, &[], &mut buffer) } {
            let length = u16::from_le_bytes([buffer[0], buffer[1]]);
            if length == 0 {
                return Some(());
            }
        }
    }
    // The reset was acknowledged on the bus; only the completion read did not
    // come. Several devices simply do not send it, and refusing to use one
    // that is otherwise answering would be the wrong call.
    Some(())
}

/// How long to look for the reset completion.
///
/// FreeBSD allows five seconds, which is right for a general-purpose kernel
/// bringing a device up in the background. This runs before the interface can
/// be used, so a quarter of a second is the trade: a device that has not
/// answered by then is treated as one whose completion read is not coming,
/// which is not fatal — the device is already acknowledging on the bus.
const RESET_TIMEOUT_US: u64 = 250_000;

/// How long to wait between the power-on and the reset.
///
/// A millisecond, as `iichid` does. Its reasoning is worth keeping: devices
/// are tested against the Windows driver, the Windows driver waits here, and
/// so some devices will have come to depend on it.
const POWER_SETTLE_US: u64 = 1_000;

/// Waits, by the counter.
fn wait_us(ticks_per_us: u64, microseconds: u64) {
    let deadline = crate::arch::counter_ordered().wrapping_add(ticks_per_us.max(1) * microseconds);
    while (deadline.wrapping_sub(crate::arch::counter_ordered()) as i64) > 0 {
        core::hint::spin_loop();
    }
}
