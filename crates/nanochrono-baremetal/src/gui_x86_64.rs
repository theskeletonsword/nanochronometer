// SPDX-License-Identifier: Apache-2.0
//! The interface, on a machine with no window system.
//!
//! # What this is, and what it is not
//!
//! The desktop build's GUI is `iced` drawing through `wgpu` onto a surface
//! `winit` obtained from a window server. None of that exists here: there is
//! no GPU driver, no compositor, no window and no event loop to hook into. So
//! this is **not the same code** as the Windows, Linux and macOS GUI, and no
//! amount of arrangement would make it so.
//!
//! What it is: the same *application*, drawn pixel by pixel. Same palette,
//! same header, the same three modes along the top — clock, stopwatch, timer
//! — the same chips selecting what the lower panel reports, the same large
//! readout down the middle and the same status bar underneath. The one
//! deliberate difference is the corner where a desktop window puts minimise
//! and close: there is no window to minimise and nothing to close to, so
//! those two become restart and shut down, through [`crate::acpi`].
//!
//! # Where the animation comes from, with no timer interrupt
//!
//! A moving interface needs to know how much time has passed, and the usual
//! way — programme a timer, take an interrupt on every tick — is the one
//! thing this project must not do: a handler running between two counter
//! reads becomes part of what the chronometer measures.
//!
//! It does not need one. The counter *is* the clock. [`crate::clock`]
//! calibrates it once at startup, the loop below reads it, and a frame is
//! drawn when enough nanoseconds have gone by. Everything that moves is a
//! [`Tween`] stepped once per frame. There is no scheduler, no interrupt and
//! no timer — the same instruction that measures an interval also paces the
//! animation, which means the interface cannot disagree with the measurement
//! it is displaying.
//!
//! Redraws go through the back buffer in [`crate::framebuffer`], and only the
//! rectangle that changed is copied to the screen. Without that, none of this
//! would be possible: an uncached full-screen redraw on a 1080p panel is tens
//! of milliseconds, and a "frame rate" measured in single digits is not an
//! animation.

use crate::acpi;
use crate::clock::Clock;
use crate::draw::{self, Palette, Tween};
use crate::gpio::Mapping;
use crate::i2c_hid::{Discovery, GateState};
use crate::framebuffer::{Colour, Framebuffer};
use crate::input::{Event, Input, Motion};
use crate::multiboot::Memory;
use crate::pmu::{CorePmu, CounterRoute};
use crate::text::{self, Text};
use crate::typeface::{Face, BODY, HEADING, READOUT, READOUT_BIG, TITLE};

// Set 1 scancodes for everything the interface binds.
const SCAN_1: u8 = 0x02;
const SCAN_2: u8 = 0x03;
const SCAN_3: u8 = 0x04;
const SCAN_B: u8 = 0x30;
const SCAN_C: u8 = 0x2E;
const SCAN_H: u8 = 0x23;
const SCAN_L: u8 = 0x26;
const SCAN_M: u8 = 0x32;
const SCAN_N: u8 = 0x31;
const SCAN_P: u8 = 0x19;
const SCAN_R: u8 = 0x13;
const SCAN_S: u8 = 0x1F;
const SCAN_U: u8 = 0x16;
const SCAN_Z: u8 = 0x2C;
const SCAN_SPACE: u8 = 0x39;
const SCAN_TAB: u8 = 0x0F;
const SCAN_UP: u8 = 0x48;
const SCAN_DOWN: u8 = 0x50;

/// How long a frame is meant to take.
///
/// Sixty a second. Nothing enforces it — there is no vertical blank to wait
/// for and no compositor to hand a frame to — so this is a floor on how often
/// the loop redraws, not a ceiling on how fast it could.
const FRAME_NS: u64 = 1_000_000_000 / 60;

/// How many input events to take before drawing.
///
/// Bounded so a pointer being moved continuously cannot starve the draw: an
/// unbounded drain is how an interface stops repainting while the mouse is in
/// motion, which looks exactly like a hang.
const EVENTS_PER_FRAME: usize = 24;

/// The three things this application is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Clock,
    Stopwatch,
    Timer,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Clock, Tab::Stopwatch, Tab::Timer];

    const fn name(self) -> &'static str {
        match self {
            Tab::Clock => "CLOCK",
            Tab::Stopwatch => "STOPWATCH",
            Tab::Timer => "TIMER",
        }
    }
}

/// What the lower panel reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Cpu,
    Pmu,
    Counter,
    Memory,
    Usb,
    Hypervisor,
}

impl Panel {
    const ALL: [Panel; 6] = [
        Panel::Cpu,
        Panel::Pmu,
        Panel::Counter,
        Panel::Memory,
        Panel::Usb,
        Panel::Hypervisor,
    ];

    const fn name(self) -> &'static str {
        match self {
            Panel::Cpu => "CPU",
            Panel::Pmu => "PMU",
            Panel::Counter => "COUNTER",
            Panel::Memory => "MEMORY",
            Panel::Usb => "USB",
            Panel::Hypervisor => "HYPERVISOR",
        }
    }

    /// The label to use when the long one will not fit.
    const fn short(self) -> &'static str {
        match self {
            Panel::Counter => "CNT",
            Panel::Memory => "MEM",
            Panel::Hypervisor => "HYP",
            other => other.name(),
        }
    }
}

/// How much of a duration the readout shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Precision {
    /// `hh:mm:ss:mmm` — a wall clock.
    Simple,
    /// `hh:mm:ss:mmm:uuu:sss` — what this instrument is for.
    Nano,
}

impl Precision {
    const fn nanoseconds(self) -> bool {
        matches!(self, Precision::Nano)
    }
}

/// Where everything sits, worked out once from the mode the loader gave us.
///
/// Computed rather than constant because the same layout has to hold at
/// 800x600, which is what a BIOS machine with no `gfxpayload` produces, and
/// at 1920x1200, which is what a laptop panel produces. Proportions rather
/// than pixel offsets, with floors so nothing collapses at the small end.
struct Layout {
    width: u32,
    header_h: u32,
    tabs_y: u32,
    tabs_h: u32,
    /// The readout's own box, which is narrower than the screen. Only this is
    /// cleared and copied out each frame: the readout is the one thing that
    /// changes sixty times a second, and on a 1920-wide panel the difference
    /// between copying the full width and copying the digits is most of the
    /// frame.
    readout_x: u32,
    readout_w: u32,
    readout_y: u32,
    readout_h: u32,
    cards_y: u32,
    cards_h: u32,
    /// How many cards fit, and how wide each is. Worked out here rather than
    /// at the draw because the card height depends on it: a narrow mode folds
    /// the machine summary into the session card, and that card then needs
    /// room for both.
    columns: u32,
    card_w: u32,
    hint_y: u32,
    status_y: u32,
    status_h: u32,
    margin: u32,
    /// Which readout face fits this width.
    readout: &'static Face,
}

/// Hex digits as strings, so a byte can be rendered without an allocator and
/// without a formatting machinery this has no other use for.
#[rustfmt::skip]
const HEX_DIGITS: [&str; 16] = [
    "0", "1", "2", "3", "4", "5", "6", "7",
    "8", "9", "A", "B", "C", "D", "E", "F",
];

/// The widest string the readout can ever hold, used to pick a face and to
/// reserve its box.
const READOUT_SAMPLE: &str = "00:00:00:000:000:000";

impl Layout {
    fn for_screen(fb: &Framebuffer) -> Layout {
        let width = fb.width;
        let height = fb.height;
        let margin = (width / 40).clamp(12, 32);

        let header_h = (height / 18).clamp(34, 64);
        let tabs_h = (height / 16).clamp(34, 56);
        let tabs_y = header_h;
        let content_y = tabs_y + tabs_h;

        let status_h = BODY.line_height as u32 * 2 + 22;
        let status_y = height.saturating_sub(status_h);
        let hint_h = BODY.line_height as u32 + 14;
        let hint_y = status_y.saturating_sub(hint_h);

        // The large face if its box fits with margins to spare, the smaller
        // one otherwise. A bitmap cannot be scaled, so this is a choice
        // between two baked sizes rather than a computed one.
        let readout = if READOUT_BIG.width_of(READOUT_SAMPLE) + margin * 4 <= width {
            &READOUT_BIG
        } else {
            &READOUT
        };
        // The face's line box, the caption under it, and the space between.
        // Sized here rather than at the draw, because a caption that does not
        // fit is not drawn at all — and a box measured without it is exactly
        // how the caption disappears on every mode.
        let readout_h = readout.line_height as u32 + BODY.line_height as u32 + 30;

        // The readout sits in the upper part of the content area and the
        // cards fill what is left. Where there is not enough room for both —
        // a short mode — the cards get whatever remains, down to nothing.
        // How many cards fit. Three at a width that still holds a label
        // beside its value, two when that would make them narrower, one on a
        // small mode. The threshold is measured from the widest ordinary pair
        // rather than picked, so it stays right if the typeface is
        // regenerated at another size.
        let total = width.saturating_sub(margin * 2);
        let narrowest = BODY.width_of("invariant TSC") + BODY.width_of("unavailable") + 70;
        let columns = match total / 3 {
            w if w >= narrowest => 3,
            _ if total / 2 >= narrowest => 2,
            _ => 1,
        };
        let card_w = (total - margin * (columns - 1)) / columns;

        // Two cards means the machine summary folds into the session card, so
        // that card has to hold both. Three means the session card holds at
        // most its own two lines and six laps.
        let card_rows = if columns < 3 { 15 } else { 9 };

        // The cards are sized by what they hold and anchored to the bottom,
        // and the readout is centred in what is left. Letting the cards
        // stretch to fill instead leaves most of a 1200-pixel panel as empty
        // card, which reads as a layout that ran out of things to say.
        let card_content =
            30 + HEADING.line_height as u32 + card_rows * (BODY.line_height as u32 + 7) + 18;
        let available = hint_y.saturating_sub(content_y);
        let readout_box = readout_h.min(available);
        let cards_h = card_content.min(available.saturating_sub(readout_box + margin + 8));
        let cards_y = hint_y.saturating_sub(cards_h + 8);
        let readout_y = content_y + (cards_y.saturating_sub(content_y + readout_box)) / 2;

        // Wide enough for the longest readout the face can produce.
        let readout_w = (readout.width_of(READOUT_SAMPLE) + margin * 2).min(width);
        let readout_x = (width - readout_w) / 2;

        Layout {
            width,
            header_h,
            tabs_y,
            tabs_h,
            readout_x,
            readout_w,
            readout_y,
            readout_h: readout_box,
            cards_y,
            cards_h,
            columns,
            card_w,
            hint_y,
            status_y,
            status_h,
            margin,
            readout,
        }
    }
}

/// A rectangle a click can be tested against.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Hitbox {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Hitbox {
    fn contains(&self, x: i32, y: i32) -> bool {
        self.w > 0
            && x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.w) as i32
            && y < (self.y + self.h) as i32
    }
}

/// What the machine turned out to be. Measured once, at boot, with nothing
/// else running — which is the only condition under which these numbers mean
/// anything.
struct Machine {
    features: nanochrono_core::cpu::CpuFeatures,
    pmu: CorePmu,
    route: CounterRoute,
    cycles_per_op: Option<u64>,
    read_overhead: u64,
    worst_read: u64,
    hypervisor: crate::hypervisor::Report,
    memory: Memory,
    footprint: u64,
    acpi: Option<acpi::PowerRegisters>,
}

impl Machine {
    /// # Safety
    /// Programs the PMU and reads firmware tables; requires ring 0.
    unsafe fn probe() -> Machine {
        use crate::progress::{self, Phase};

        progress::enter(Phase::Acpi);
        // SAFETY: forwarded. Firmware tables are whatever the firmware says
        // they are, which is why every address out of them is checked before
        // it is followed.
        let acpi = unsafe { acpi::power_registers() };
        progress::leave(Phase::Acpi);

        let mut pmu = CorePmu::detect();
        let mut route = CounterRoute::None;
        let mut cycles_per_op = None;
        if pmu.leaf.is_available() {
            // SAFETY: forwarded from this function's own contract.
            route = unsafe { pmu.enable() };
            if route != CounterRoute::None {
                const ITERATIONS: u64 = 100_000;
                let mut acc = 0u64;
                // SAFETY: the PMU was just enabled on this core, at ring 0.
                let (out, cycles) = unsafe {
                    pmu.measure(|| {
                        for i in 0..ITERATIONS {
                            acc = acc.wrapping_add(i).rotate_left(3);
                        }
                        acc
                    })
                };
                core::hint::black_box(out);
                cycles_per_op = cycles.map(|c| c / ITERATIONS);
            }
        }

        const ROUNDS: u32 = 1024;
        let mut read_overhead = u64::MAX;
        let mut worst_read = 0;
        for _ in 0..ROUNDS {
            let a = crate::arch::counter_ordered();
            let b = crate::arch::counter_ordered();
            let d = b.wrapping_sub(a);
            read_overhead = read_overhead.min(d);
            worst_read = worst_read.max(d);
        }

        // SAFETY: forwarded from this function's own contract.
        let hypervisor = unsafe { crate::hypervisor::detect() };

        Machine {
            features: nanochrono_core::cpu::features(),
            pmu,
            route,
            cycles_per_op,
            read_overhead,
            worst_read,
            hypervisor,
            memory: Memory::default(),
            footprint: crate::multiboot::kernel_footprint(),
            acpi,
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A stopwatch, with the accumulator held redundantly.
///
/// [`Protected`](nanochrono_core::Protected) is not decoration here. This is a
/// machine with no operating system, no ECC scrubber and no memory manager:
/// a bit flipped by a cosmic ray in the accumulated total is a wrong
/// measurement that nothing else would ever notice. The value carries a
/// Hamming code and three replicas, so a single flip is corrected on read and
/// a double flip is reported rather than believed.
struct Stopwatch {
    running: bool,
    /// Counter ticks banked from previous runs.
    accumulated: nanochrono_core::Protected,
    /// The counter when the current run started.
    started_at: u64,
    laps: [u64; Stopwatch::MAX_LAPS],
    lap_count: usize,
    /// The worst integrity verdict seen since the last reset.
    integrity: nanochrono_core::Integrity,
}

impl Stopwatch {
    const MAX_LAPS: usize = 6;

    fn new() -> Stopwatch {
        Stopwatch {
            running: false,
            accumulated: nanochrono_core::Protected::new(0),
            started_at: 0,
            laps: [0; Stopwatch::MAX_LAPS],
            lap_count: 0,
            integrity: nanochrono_core::Integrity::Clean,
        }
    }

    /// Ticks elapsed, verifying the accumulator as it is read.
    fn ticks(&mut self, now: u64) -> u64 {
        let (banked, verdict) = self.accumulated.get_verified();
        self.integrity = self.integrity.max(verdict);
        if self.running {
            banked + now.wrapping_sub(self.started_at)
        } else {
            banked
        }
    }

    fn toggle(&mut self, now: u64) {
        if self.running {
            let banked = self.ticks(now);
            self.accumulated.set(banked);
            self.running = false;
        } else {
            self.started_at = now;
            self.running = true;
        }
    }

    fn reset(&mut self) {
        self.running = false;
        self.accumulated.set(0);
        self.lap_count = 0;
        self.laps = [0; Stopwatch::MAX_LAPS];
        self.integrity = nanochrono_core::Integrity::Clean;
    }

    /// Records a lap, dropping the oldest once the table is full.
    fn lap(&mut self, now: u64) {
        let ticks = self.ticks(now);
        if self.lap_count == Stopwatch::MAX_LAPS {
            self.laps.rotate_left(1);
            self.laps[Stopwatch::MAX_LAPS - 1] = ticks;
        } else {
            self.laps[self.lap_count] = ticks;
            self.lap_count += 1;
        }
    }
}

/// A countdown.
struct Timer {
    running: bool,
    /// What it counts down from, in nanoseconds.
    target_ns: u64,
    /// Counter ticks already spent.
    spent: u64,
    started_at: u64,
    /// Set when it reaches zero, cleared by a reset. What makes the readout
    /// go red and stay there rather than blinking past.
    expired: bool,
}

impl Timer {
    /// A minute, which is the length a timer with no keypad most often wants.
    const DEFAULT_NS: u64 = 60 * 1_000_000_000;
    /// How much a press of up or down moves the target.
    const STEP_NS: u64 = 10 * 1_000_000_000;

    fn new() -> Timer {
        Timer {
            running: false,
            target_ns: Timer::DEFAULT_NS,
            spent: 0,
            started_at: 0,
            expired: false,
        }
    }

    fn elapsed_ticks(&self, now: u64) -> u64 {
        if self.running {
            self.spent + now.wrapping_sub(self.started_at)
        } else {
            self.spent
        }
    }

    /// Nanoseconds left, saturating at zero.
    fn remaining_ns(&mut self, now: u64, clock: &Clock) -> u64 {
        let spent = clock.calibration.ticks_to_ns(self.elapsed_ticks(now));
        if spent >= self.target_ns {
            if self.running {
                // Stopped rather than left running past zero: the counter
                // would keep climbing and the display would be showing a
                // saturated number that is no longer measuring anything.
                self.spent = self.elapsed_ticks(now);
                self.running = false;
                self.expired = true;
            }
            return 0;
        }
        self.target_ns - spent
    }

    fn toggle(&mut self, now: u64) {
        if self.expired {
            return;
        }
        if self.running {
            self.spent = self.elapsed_ticks(now);
            self.running = false;
        } else {
            self.started_at = now;
            self.running = true;
        }
    }

    fn reset(&mut self) {
        self.running = false;
        self.spent = 0;
        self.expired = false;
    }

    fn adjust(&mut self, up: bool) {
        if self.running {
            return;
        }
        self.target_ns = if up {
            self.target_ns.saturating_add(Timer::STEP_NS)
        } else {
            self.target_ns
                .saturating_sub(Timer::STEP_NS)
                .max(Timer::STEP_NS)
        };
        self.expired = false;
        self.spent = 0;
    }
}

/// One control that can be clicked and can light up under the pointer.
#[derive(Clone, Copy, Default)]
struct Control {
    box_: Hitbox,
    /// How lit it is, 0 to 1000. A tween rather than a flag, which is the
    /// difference between a control that responds and one that switches.
    highlight: Tween,
}

impl Control {
    fn new() -> Control {
        Control {
            box_: Hitbox::default(),
            highlight: Tween::with_rate(0, 200),
        }
    }

    /// Aims the highlight at where it should be for this state, and steps it.
    fn step(&mut self, hovered: bool, selected: bool) -> bool {
        self.highlight.retarget(if selected {
            1000
        } else if hovered {
            420
        } else {
            0
        });
        self.highlight.step()
    }

    fn colour(&self, p: &Palette) -> Colour {
        let t = self.highlight.value().clamp(0, 1000) as u32;
        if t <= 420 {
            draw::mix_colour(p.panel, p.hover, t * 1000 / 420)
        } else {
            draw::mix_colour(p.hover, p.accent, (t - 420) * 1000 / 580)
        }
    }

    fn label_colour(&self, p: &Palette) -> Colour {
        let t = self.highlight.value().clamp(0, 1000) as u32;
        // The selected chip's background is the accent, so its label has to
        // go dark or it disappears into it.
        draw::mix_colour(p.text, p.background, t)
    }
}

/// Everything that changes.
struct Ui {
    tab: Tab,
    panel: Panel,
    precision: Precision,

    stopwatch: Stopwatch,
    timer: Timer,

    /// The tab controls, the panel chips, the precision toggle and the two
    /// title-bar buttons.
    tabs: [Control; 3],
    chips: [Control; 6],
    precision_chip: Control,
    restart: Control,
    shutdown: Control,

    /// Where the selected tab's underline is, sliding between tabs.
    underline_x: Tween,
    underline_w: Tween,

    /// The two status meters, easing towards their readings.
    memory_meter: Tween,
    load_meter: Tween,

    /// How faded in the readout and the cards are. Restarted on a tab or
    /// panel change, which is what makes a switch read as a transition rather
    /// than a repaint.
    readout_fade: Tween,
    cards_fade: Tween,

    /// What to say instead of the key legend, when a stack came up short.
    input_note: Option<&'static str>,

    /// Set when something changed a label in the tab row without moving any
    /// of its highlights. The row is only repainted when it moved, which is
    /// what keeps it off the frame budget — but the precision chip's *text*
    /// changes with no motion at all, so it needs a way to ask.
    chrome_dirty: bool,
    /// The same for the cards, whose contents change with the session state.
    /// Separate from the fade because starting a stopwatch should update the
    /// card, not replay the transition every time the space bar is pressed.
    cards_dirty: bool,

    cursor: Cursor,
    pointer_x: i32,
    pointer_y: i32,

    /// Counter ticks spent drawing in the current second, and the total the
    /// second covered, which together are the only honest "CPU" figure a
    /// kernel with no scheduler can report.
    busy_ticks: u64,
    window_ticks: u64,
    window_started: u64,
    load_permille: u32,

    /// What the readout showed last frame, so an unchanged one is not
    /// redrawn.
    last_readout: Text<24>,
    last_header_clock: Text<24>,
    last_load: Text<8>,
}

impl Ui {
    fn new() -> Ui {
        Ui {
            tab: Tab::Stopwatch,
            panel: Panel::Cpu,
            precision: Precision::Nano,
            stopwatch: Stopwatch::new(),
            timer: Timer::new(),
            tabs: [Control::new(); 3],
            chips: [Control::new(); 6],
            precision_chip: Control::new(),
            restart: Control::new(),
            shutdown: Control::new(),
            underline_x: Tween::with_rate(0, 180),
            underline_w: Tween::with_rate(0, 180),
            memory_meter: Tween::with_rate(0, 90),
            load_meter: Tween::with_rate(0, 90),
            readout_fade: Tween::with_rate(1000, 130),
            cards_fade: Tween::with_rate(1000, 110),
            input_note: None,
            chrome_dirty: false,
            cards_dirty: false,
            cursor: Cursor::new(),
            pointer_x: 0,
            pointer_y: 0,
            busy_ticks: 0,
            window_ticks: 0,
            window_started: 0,
            load_permille: 0,
            last_readout: Text::new(),
            last_header_clock: Text::new(),
            last_load: Text::new(),
        }
    }

    /// Restarts the readout's fade-in. Called when what it shows changes
    /// kind, not when its digits change.
    fn transition_readout(&mut self) {
        self.readout_fade = Tween::with_rate(0, 130);
        self.readout_fade.retarget(1000);
        self.last_readout.clear();
    }

    fn transition_cards(&mut self) {
        self.cards_fade = Tween::with_rate(0, 110);
        self.cards_fade.retarget(1000);
        self.cards_dirty = true;
    }

    /// Redraws the cards without replaying the fade.
    ///
    /// For a state change the cards report — starting the stopwatch,
    /// adjusting the timer — where the card is stale but nothing has
    /// *arrived*. A fade on every keypress reads as a stutter rather than as
    /// motion.
    fn refresh_cards(&mut self) {
        self.cards_dirty = true;
    }

    fn select_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.transition_readout();
        // The session card reports the selected mode, so it changes with the
        // tab. Without this the stopwatch's laps stay on screen under the
        // timer's readout, which is worse than a stale number: it is a
        // reading attached to the wrong instrument.
        self.transition_cards();
    }

    /// Switches between the full and the abbreviated readout.
    fn toggle_precision(&mut self) {
        self.precision = match self.precision {
            Precision::Nano => Precision::Simple,
            Precision::Simple => Precision::Nano,
        };
        self.chrome_dirty = true;
        self.transition_readout();
        // The header clock is drawn at the same precision, and it is only
        // repainted when its text changes — which it would not, for the
        // fraction of a second in which the seconds field has not ticked.
        self.last_header_clock.clear();
    }

    fn select_panel(&mut self, panel: Panel) {
        if self.panel == panel {
            return;
        }
        self.panel = panel;
        self.transition_cards();
    }
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// Draws the interface and drives it. Never returns.
///
/// # Safety
/// Reads I/O ports, programs the PMU and writes the framebuffer; requires
/// ring 0.
pub unsafe fn run(fb: &Framebuffer, memory: Memory) -> ! {
    use crate::progress::{self, Phase};

    let p = Palette::APP;

    // SAFETY: forwarded from this function's own contract. Calibrating the
    // counter comes first: everything below is timed by it, including the
    // frame pacing of this loop.
    let clock = unsafe { Clock::start() };

    // SAFETY: as above.
    let mut machine = unsafe { Machine::probe() };
    machine.memory = memory;

    progress::enter(Phase::Interface);

    let layout = Layout::for_screen(fb);
    let mut ui = Ui::new();
    // Centred, the way a display server places a cursor. Set on the cursor as
    // well as on the pointer: they are separate because the cursor remembers
    // where it was last *drawn*, and leaving that at the origin is how a
    // cursor ends up invisible under the header.
    ui.pointer_x = fb.width as i32 / 2;
    ui.pointer_y = fb.height as i32 / 2;
    ui.cursor.move_to(ui.pointer_x, ui.pointer_y);
    ui.window_started = clock.elapsed_ticks();

    // The first frame is a full repaint; everything after it is regional.
    fb.clear(p.background);
    header(fb, &p, &layout, &mut ui, &machine);
    tab_bar(fb, &p, &layout, &mut ui);
    hint_bar(fb, &p, &layout, &ui);
    status_frame(fb, &p, &layout);

    // **Painted before the input stacks are brought up, not after.**
    //
    // Finding a keyboard, enumerating USB and reading a touchpad out of the
    // firmware's AML are all things that can take time or fail slowly on real
    // hardware — a controller that never leaves reset, a device that never
    // answers. Doing them first means a machine where one of them stalls
    // shows nothing at all, which is indistinguishable from a kernel that
    // never started. Now the interface is up first and says what it is doing.
    draw::text_centred(
        fb,
        &HEADING,
        layout.readout_x,
        layout.readout_w,
        layout.readout_y + layout.readout_h / 2,
        "detecting input devices",
        p.muted,
    );
    fb.present(0, 0, fb.width, fb.height);
    fb.discard_damage();

    // SAFETY: as above. A microsecond's worth of counter ticks, so the I2C
    // driver's timeouts are denominated in time rather than in loop
    // iterations — see `i2c`.
    let mut input = unsafe { Input::init(clock.calibration.hz / 1_000_000) };

    // If a stack came up short, open on the panel that says which. The
    // diagnosis was previously behind a keypress, which is no use at all on
    // the one machine that most needs it: the keyboard is what did not work.
    if !input.has_keyboard() || !input.has_pointer() {
        ui.panel = Panel::Usb;
        ui.input_note = Some(input.trouble());
        // The legend was drawn before the probe, when there was nothing yet
        // to say about it.
        hint_bar(fb, &p, &layout, &ui);
        fb.present_damage();
    }

    // No `leave` for this phase: the interface being on screen *is* the
    // evidence it finished, and painting a marker over a finished frame
    // leaves a square on the header.

    ui.transition_readout();
    ui.transition_cards();

    let mut next_frame = 0u64;

    loop {
        // --- input, bounded so a moving pointer cannot starve the draw
        for _ in 0..EVENTS_PER_FRAME {
            // SAFETY: forwarded from this function's own contract.
            let Some(event) = (unsafe { input.poll() }) else {
                break;
            };
            // SAFETY: as above; the handler can power the machine off.
            unsafe { handle(event, &mut ui, fb, &p, &layout, &clock, &machine) };
        }

        // --- pacing
        let now_ns = clock.elapsed_ns();
        if now_ns < next_frame {
            core::hint::spin_loop();
            continue;
        }
        // Set from the current time rather than incremented, so a frame that
        // overran does not leave the loop trying to catch up with a burst of
        // frames it cannot draw either.
        next_frame = now_ns + FRAME_NS;

        let frame_start = clock.elapsed_ticks();

        // --- the frame
        if input.has_pointer() {
            ui.cursor.erase(fb);
            fb.present_damage();
        }

        header_clock(fb, &p, &layout, &mut ui, &clock);
        fb.present_damage();

        if tab_bar(fb, &p, &layout, &mut ui) {
            fb.present_damage();
        }

        readout(fb, &p, &layout, &mut ui, &clock);
        fb.present_damage();

        if core::mem::take(&mut ui.cards_dirty) | !ui.cards_fade.settled() {
            ui.cards_fade.step();
            cards(fb, &p, &layout, &mut ui, &machine, &clock, &input);
            fb.present_damage();
        }

        status_bar(fb, &p, &layout, &mut ui, &machine, &clock, &input);
        fb.present_damage();

        if input.has_pointer() {
            ui.cursor.draw(fb, &p);
            fb.present_damage();
        }

        // --- how much of the frame was spent drawing
        let spent = clock.elapsed_ticks().wrapping_sub(frame_start);
        ui.busy_ticks += spent;
        let elapsed = clock.elapsed_ticks().wrapping_sub(ui.window_started);
        if elapsed >= clock.calibration.hz {
            ui.window_ticks = elapsed;
            ui.load_permille = (ui.busy_ticks * 1000 / elapsed.max(1)).min(1000) as u32;
            ui.busy_ticks = 0;
            ui.window_started = clock.elapsed_ticks();
        }
    }
}

/// Acts on one input event.
///
/// # Safety
/// May restart or power off the machine through ACPI; requires ring 0.
unsafe fn handle(
    event: Event,
    ui: &mut Ui,
    fb: &Framebuffer,
    p: &Palette,
    layout: &Layout,
    clock: &Clock,
    machine: &Machine,
) {
    let now = clock.elapsed_ticks();

    // The input panel shows the raw bytes the 8042 delivered, so it has to be
    // repainted when another one arrives. This is the one place where a
    // keypress that binds to nothing still has to change the screen — on a
    // machine where keys appear to do nothing, seeing the byte is the answer.
    if matches!(event, Event::Key(_)) && ui.panel == Panel::Usb {
        ui.refresh_cards();
    }

    match event {
        Event::Key(k) if k.pressed => match k.scancode {
            SCAN_R => {
                // SAFETY: forwarded from this function's own contract.
                unsafe { acpi::reboot(machine.acpi.as_ref()) }
            }
            SCAN_S => {
                // SAFETY: as above.
                unsafe { power_off(fb, p, layout, machine.acpi.as_ref()) }
            }
            SCAN_1 | SCAN_C => ui.select_tab(Tab::Clock),
            SCAN_2 => ui.select_tab(Tab::Stopwatch),
            SCAN_3 => ui.select_tab(Tab::Timer),
            SCAN_SPACE | SCAN_P => {
                match ui.tab {
                    Tab::Stopwatch => ui.stopwatch.toggle(now),
                    Tab::Timer => ui.timer.toggle(now),
                    Tab::Clock => {}
                }
                ui.refresh_cards();
            }
            SCAN_L if ui.tab == Tab::Stopwatch => {
                ui.stopwatch.lap(now);
                ui.transition_cards();
            }
            SCAN_Z => match ui.tab {
                Tab::Stopwatch => {
                    ui.stopwatch.reset();
                    ui.transition_cards();
                }
                Tab::Timer => {
                    ui.timer.reset();
                    ui.refresh_cards();
                }
                Tab::Clock => {}
            },
            SCAN_UP if ui.tab == Tab::Timer => {
                ui.timer.adjust(true);
                ui.refresh_cards();
            }
            SCAN_DOWN if ui.tab == Tab::Timer => {
                ui.timer.adjust(false);
                ui.refresh_cards();
            }
            SCAN_N => ui.toggle_precision(),
            // The panel chips, by initial where each is unambiguous.
            SCAN_B => ui.select_panel(Panel::Pmu),
            SCAN_M => ui.select_panel(Panel::Memory),
            SCAN_U => ui.select_panel(Panel::Usb),
            SCAN_H => ui.select_panel(Panel::Hypervisor),
            SCAN_TAB => {
                let next = Panel::ALL
                    .iter()
                    .position(|&panel| panel == ui.panel)
                    .map_or(0, |i| (i + 1) % Panel::ALL.len());
                ui.select_panel(Panel::ALL[next]);
            }
            _ => {}
        },
        Event::Key(_) => {}
        Event::Motion(m) => {
            // SAFETY: forwarded from this function's own contract.
            unsafe { pointer(m, ui, fb, p, layout, clock, machine) }
        }
    }
}

/// Moves the cursor and acts on a click.
///
/// # Safety
/// May restart or power off the machine; requires ring 0.
unsafe fn pointer(
    m: Motion,
    ui: &mut Ui,
    fb: &Framebuffer,
    p: &Palette,
    layout: &Layout,
    clock: &Clock,
    machine: &Machine,
) {
    // Clamped rather than wrapped: a cursor that leaves one edge and appears
    // at the other is not a cursor.
    ui.pointer_x = (ui.pointer_x + m.dx).clamp(0, fb.width as i32 - 1);
    ui.pointer_y = (ui.pointer_y + m.dy).clamp(0, fb.height as i32 - 1);
    ui.cursor.move_to(ui.pointer_x, ui.pointer_y);

    if !m.left {
        return;
    }
    let (x, y) = (ui.pointer_x, ui.pointer_y);

    if ui.restart.box_.contains(x, y) {
        // SAFETY: forwarded from this function's own contract.
        unsafe { acpi::reboot(machine.acpi.as_ref()) }
    }
    if ui.shutdown.box_.contains(x, y) {
        // SAFETY: as above.
        unsafe { power_off(fb, p, layout, machine.acpi.as_ref()) }
    }
    for (i, tab) in Tab::ALL.iter().enumerate() {
        if ui.tabs[i].box_.contains(x, y) {
            ui.select_tab(*tab);
        }
    }
    for (i, panel) in Panel::ALL.iter().enumerate() {
        if ui.chips[i].box_.contains(x, y) {
            ui.select_panel(*panel);
        }
    }
    if ui.precision_chip.box_.contains(x, y) {
        ui.toggle_precision();
    }
    // Clicking the readout starts and stops it, the way a stopwatch face
    // does. The largest target on screen for the action it is most likely to
    // be asked for.
    let readout_box = Hitbox {
        x: layout.readout_x,
        y: layout.readout_y,
        w: layout.readout_w,
        h: layout.readout_h,
    };
    if readout_box.contains(x, y) {
        let now = clock.elapsed_ticks();
        match ui.tab {
            Tab::Stopwatch => ui.stopwatch.toggle(now),
            Tab::Timer => ui.timer.toggle(now),
            Tab::Clock => {}
        }
        ui.refresh_cards();
    }
}

/// # Safety
/// Requires ring 0.
unsafe fn power_off(
    fb: &Framebuffer,
    p: &Palette,
    layout: &Layout,
    power: Option<&acpi::PowerRegisters>,
) {
    // SAFETY: forwarded from this function's own contract.
    unsafe { acpi::shutdown(power) };
    // Every method returned, so none of them worked. Saying so beats a
    // machine that looks hung for no stated reason.
    let y = layout.hint_y;
    fb.fill(0, y, layout.width, BODY.line_height as u32 + 4, p.panel);
    draw::text(
        fb,
        &BODY,
        layout.margin,
        y,
        "shutdown: no method this platform answers",
        p.danger,
    );
    fb.present_damage();
}

// ---------------------------------------------------------------------------
// The header
// ---------------------------------------------------------------------------

/// Where the live clock sits in the header, so only that box is repainted.
fn header_clock_box(layout: &Layout) -> Hitbox {
    let logo = layout.header_h * 2 / 3;
    let mut x = layout.margin + logo + 12;
    x += TITLE.width_of("NanoChrono").min(layout.width / 3);
    x += BODY.width_of(" v") + BODY.width_of(crate::VERSION) + 26;
    // Wide enough for the longest form this can draw, suffix included.
    // Reserving less leaves the tail of the previous string on screen when a
    // shorter one replaces it, because the repaint only covers the box.
    let w = BODY.width_of("00:00:00:000:000:000 RTC") + 12;
    Hitbox {
        x,
        y: layout.header_h.saturating_sub(BODY.line_height as u32) / 2,
        w: w.min(layout.width.saturating_sub(x)),
        h: BODY.line_height as u32,
    }
}

/// The title bar: identity on the left, restart and shut down on the right.
fn header(fb: &Framebuffer, p: &Palette, layout: &Layout, ui: &mut Ui, machine: &Machine) {
    let h = layout.header_h;
    // A gradient rather than a flat fill. It costs one extra loop and is most
    // of what separates a header from a coloured rectangle.
    draw::gradient(fb, 0, 0, layout.width, h, p.header_from, p.header_to);
    fb.fill(0, h - 1, layout.width, 1, p.divider);

    // The mark: a disc with the application's initial, drawn rather than
    // decoded. `assets/` holds an `.ico` and an `.svg`, and parsing either
    // would mean a PNG or SVG decoder in a kernel — far more code, and more
    // attack surface, than a plotted circle.
    let logo = h * 2 / 3;
    let cx = layout.margin + logo / 2;
    let cy = h / 2;
    disc(fb, cx, cy, logo / 2, p.accent);
    disc(fb, cx, cy, logo / 2 - 2, p.header_from);
    draw::text_centred(
        fb,
        &BODY,
        layout.margin,
        logo,
        cy - BODY.line_height as u32 / 2,
        "N",
        p.accent,
    );

    let mut x = layout.margin + logo + 12;
    let title_y = h.saturating_sub(TITLE.line_height as u32) / 2;
    draw::text(fb, &TITLE, x, title_y, "NanoChrono", p.title);
    x += TITLE.width_of("NanoChrono") + 10;

    let small_y = h.saturating_sub(BODY.line_height as u32) / 2;
    let mut version = Text::<16>::new();
    version.str("v").str(crate::VERSION);
    draw::text(fb, &BODY, x, small_y, version.as_str(), p.muted);

    // Everything to the right of the clock: what the counter and the SIMD
    // backend actually are. Laid out after the clock's reserved box so the
    // two cannot collide as the clock's width changes.
    let clock_box = header_clock_box(layout);
    let mut pen = clock_box.x + clock_box.w + 18;

    let simd = nanochrono_core::Backend::best().name();
    if pen + BODY.width_of(simd) < layout.width / 2 {
        draw::text(fb, &BODY, pen, small_y, simd, p.muted);
        pen += BODY.width_of(simd) + 14;
        draw::text(fb, &BODY, pen, small_y, "|", p.divider);
        pen += BODY.width_of("|") + 14;
    }
    let counter = if machine.features.invariant_counter {
        "INVARIANT"
    } else {
        "COUNTER"
    };
    if pen + BODY.width_of(counter) < layout.width / 2 {
        draw::text(fb, &BODY, pen, small_y, counter, p.accent);
    }

    // Where a desktop window puts minimise and close. Restart and shut down
    // instead: there is no window manager to minimise into and nothing to
    // close to.
    let bw = (h * 5 / 4).clamp(40, 64);
    let bh = (h * 3 / 5).clamp(26, 44);
    let by = (h - bh) / 2;
    let off_x = layout.width.saturating_sub(bw + layout.margin);
    let restart_x = off_x.saturating_sub(bw + 10);

    ui.restart.box_ = Hitbox {
        x: restart_x,
        y: by,
        w: bw,
        h: bh,
    };
    ui.shutdown.box_ = Hitbox {
        x: off_x,
        y: by,
        w: bw,
        h: bh,
    };
    header_buttons(fb, p, ui);
}

/// Repaints the two title-bar buttons at their current highlight.
fn header_buttons(fb: &Framebuffer, p: &Palette, ui: &Ui) {
    for (control, danger) in [(&ui.restart, false), (&ui.shutdown, true)] {
        let b = control.box_;
        if b.w == 0 {
            continue;
        }
        let lit = control.highlight.value().clamp(0, 1000) as u32;
        let base = draw::mix_colour(p.button, if danger { p.danger } else { p.accent }, lit / 3);
        draw::rounded(fb, b.x, b.y, b.w, b.h, 10, base);
        draw::rounded_outline(
            fb,
            b.x,
            b.y,
            b.w,
            b.h,
            10,
            if danger { p.danger } else { p.button_edge },
        );
        let ink = if danger { p.danger } else { p.title };
        if danger {
            glyph_power(fb, b.x + b.w / 2, b.y + b.h / 2, ink);
        } else {
            glyph_restart(fb, b.x + b.w / 2, b.y + b.h / 2, ink);
        }
    }
}

/// Repaints just the live clock in the header.
fn header_clock(fb: &Framebuffer, p: &Palette, layout: &Layout, ui: &mut Ui, clock: &Clock) {
    let mut label = Text::<24>::new();
    match clock.wall_ns() {
        Some(ns) => {
            label.str(text::duration(ns, ui.precision.nanoseconds()).as_str());
            // The RTC is read exactly as the firmware keeps it, and firmware
            // is configured either way. Marked rather than silently called
            // one or the other, which would be a guess presented as a fact.
            label.str(" RTC");
        }
        None => {
            label.str("no rtc");
        }
    }
    if label.as_str() == ui.last_header_clock.as_str() {
        return;
    }
    ui.last_header_clock.clear();
    ui.last_header_clock.str(label.as_str());

    let b = header_clock_box(layout);
    // The header is a gradient, so the box is repainted from the gradient
    // rather than from a flat colour — filling it with either end would leave
    // a visible rectangle.
    for col in 0..b.w {
        let t = ((b.x + col) * 255 / layout.width.max(1)) as u8;
        fb.fill(
            b.x + col,
            b.y,
            1,
            b.h,
            draw::blend(p.header_from, p.header_to, t),
        );
    }
    draw::text(fb, &BODY, b.x, b.y, label.as_str(), p.text);

    // The buttons share the repaint: their highlight is stepped here, and a
    // control that only lit up when something else happened to redraw would
    // respond a frame late.
    let hovered_restart = ui.restart.box_.contains(ui.pointer_x, ui.pointer_y);
    let hovered_off = ui.shutdown.box_.contains(ui.pointer_x, ui.pointer_y);
    let moved = ui.restart.step(hovered_restart, false) | ui.shutdown.step(hovered_off, false);
    if moved {
        header_buttons(fb, p, ui);
    }
}

/// A filled circle.
fn disc(fb: &Framebuffer, cx: u32, cy: u32, r: u32, colour: Colour) {
    let r = r as i32;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r * r {
                continue;
            }
            let (x, y) = (cx as i32 + dx, cy as i32 + dy);
            if x >= 0 && y >= 0 {
                fb.set(x as u32, y as u32, colour);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tabs and chips
// ---------------------------------------------------------------------------

/// The row under the header: modes on the left, panel chips on the right.
///
/// Returns whether anything moved, so the caller only presents a row that
/// actually changed.
fn tab_bar(fb: &Framebuffer, p: &Palette, layout: &Layout, ui: &mut Ui) -> bool {
    let y = layout.tabs_y;
    let h = layout.tabs_h;
    let (px, py) = (ui.pointer_x, ui.pointer_y);

    // --- layout, which does not depend on state
    let pad = 20;
    let chip_h = (h * 3 / 5).clamp(22, 34);
    let chip_y = y + (h - chip_h) / 2;

    let mut x = layout.margin;
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let w = BODY.width_of(tab.name()) + pad * 2;
        ui.tabs[i].box_ = Hitbox { x, y, w, h };
        x += w;
    }

    // The chips are laid out from the right so the row stays anchored to the
    // edge, and any that will not fit are dropped rather than overlapping the
    // tabs. Short labels are tried before a chip is given up entirely.
    let mut right = layout.width.saturating_sub(layout.margin);
    let precision_label = match ui.precision {
        Precision::Nano => "NANO",
        Precision::Simple => "SIMPLE",
    };

    let mut boxes = [Hitbox::default(); Panel::ALL.len()];
    let mut short = false;
    for pass in 0..2 {
        short = pass == 1;
        let mut cursor = right;
        let mut fits = true;
        for (i, panel) in Panel::ALL.iter().enumerate().rev() {
            let label = if short { panel.short() } else { panel.name() };
            let w = BODY.width_of(label) + 22;
            if cursor.saturating_sub(w) <= x + 24 {
                fits = false;
                break;
            }
            cursor -= w + 8;
            boxes[i] = Hitbox {
                x: cursor,
                y: chip_y,
                w,
                h: chip_h,
            };
        }
        if fits {
            right = cursor;
            break;
        }
        boxes = [Hitbox::default(); Panel::ALL.len()];
    }
    for (i, b) in boxes.iter().enumerate() {
        ui.chips[i].box_ = *b;
    }

    let precision_w = BODY.width_of("SIMPLE") + 22;
    ui.precision_chip.box_ = if right.saturating_sub(precision_w + 12) > x + 24 {
        Hitbox {
            x: right - precision_w - 12,
            y: chip_y,
            w: precision_w,
            h: chip_h,
        }
    } else {
        Hitbox::default()
    };

    // --- state: step every highlight, and note whether any of them moved
    let mut moved = core::mem::take(&mut ui.chrome_dirty);
    for (i, tab) in Tab::ALL.iter().enumerate() {
        moved |= ui.tabs[i].step(ui.tabs[i].box_.contains(px, py), *tab == ui.tab);
    }
    for (i, panel) in Panel::ALL.iter().enumerate() {
        moved |= ui.chips[i].step(ui.chips[i].box_.contains(px, py), *panel == ui.panel);
    }
    moved |= ui
        .precision_chip
        .step(ui.precision_chip.box_.contains(px, py), false);

    // The underline chases the selected tab rather than jumping to it, which
    // is the single most recognisable piece of motion in a modern interface.
    let selected = ui.tabs[Tab::ALL.iter().position(|&t| t == ui.tab).unwrap_or(0)].box_;
    ui.underline_x.retarget(selected.x as i32);
    ui.underline_w.retarget(selected.w as i32);
    moved |= ui.underline_x.step() | ui.underline_w.step();

    if !moved {
        return false;
    }

    // --- draw
    fb.fill(0, y, layout.width, h, p.background);
    fb.fill(0, y + h - 1, layout.width, 1, p.divider);

    for (i, tab) in Tab::ALL.iter().enumerate() {
        let b = ui.tabs[i].box_;
        let lit = ui.tabs[i].highlight.value().clamp(0, 1000) as u32;
        // A tab is not a pill: it fills its cell and is marked by the
        // underline, which is what makes the sliding indicator legible.
        if lit > 0 {
            fb.fill(
                b.x,
                b.y,
                b.w,
                b.h - 1,
                draw::mix_colour(p.background, p.panel, lit),
            );
        }
        let ink = draw::mix_colour(p.muted, p.title, lit);
        draw::text_centred(
            fb,
            &BODY,
            b.x,
            b.w,
            b.y + (b.h - BODY.line_height as u32) / 2,
            tab.name(),
            ink,
        );
    }

    draw::underline(
        fb,
        ui.underline_x.value().max(0) as u32,
        y + h - 3,
        ui.underline_w.value().max(0) as u32,
        p.accent,
        p.glow,
    );

    for (i, panel) in Panel::ALL.iter().enumerate() {
        let b = ui.chips[i].box_;
        if b.w == 0 {
            continue;
        }
        let label = if short { panel.short() } else { panel.name() };
        draw::chip(
            fb,
            &BODY,
            b.x,
            b.y,
            b.w,
            b.h,
            label,
            ui.chips[i].colour(p),
            ui.chips[i].label_colour(p),
        );
    }

    let b = ui.precision_chip.box_;
    if b.w > 0 {
        draw::chip(
            fb,
            &BODY,
            b.x,
            b.y,
            b.w,
            b.h,
            precision_label,
            draw::mix_colour(p.panel, p.hover, 600),
            p.accent,
        );
    }
    true
}

// ---------------------------------------------------------------------------
// The readout
// ---------------------------------------------------------------------------

/// The large elapsed-time display, and the one line of context under it.
fn readout(fb: &Framebuffer, p: &Palette, layout: &Layout, ui: &mut Ui, clock: &Clock) {
    let now = clock.elapsed_ticks();
    let running;
    let value_ns = match ui.tab {
        Tab::Clock => {
            running = true;
            clock.wall_ns().unwrap_or_else(|| clock.elapsed_ns())
        }
        Tab::Stopwatch => {
            running = ui.stopwatch.running;
            let ticks = ui.stopwatch.ticks(now);
            clock.calibration.ticks_to_ns(ticks)
        }
        Tab::Timer => {
            running = ui.timer.running;
            ui.timer.remaining_ns(now, clock)
        }
    };

    let label = text::duration(value_ns, ui.precision.nanoseconds());
    let fading = ui.readout_fade.step();
    if !fading && label.as_str() == ui.last_readout.as_str() {
        return;
    }
    ui.last_readout.clear();
    ui.last_readout.str(label.as_str());

    let face = layout.readout;
    let text_w = face.width_of(label.as_str());
    let x = layout.readout_x + layout.readout_w.saturating_sub(text_w) / 2;
    let y = layout.readout_y + layout.readout_h.saturating_sub(face.line_height as u32) / 2;

    // The whole box is cleared rather than the text's own width: switching
    // precision or crossing from one to two hours changes the width, and a
    // clear that only covers the new string leaves the old one's tail behind.
    fb.fill(
        layout.readout_x,
        layout.readout_y,
        layout.readout_w,
        layout.readout_h,
        p.background,
    );

    let ink = if ui.tab == Tab::Timer && ui.timer.expired {
        p.danger
    } else if running {
        p.accent
    } else {
        // A stopped stopwatch is still a reading, so it stays legible — just
        // not lit.
        p.text
    };
    let halo = if running { p.glow } else { p.shadow };

    draw::text_glow(fb, face, x, y, label.as_str(), ink, halo, 2);

    // One line of context, so the number is not left to speak for itself.
    let mut note = Text::<64>::new();
    match ui.tab {
        Tab::Clock => {
            note.str("wall clock · rtc + counter · ")
                .str(clock.calibration.source.name());
        }
        Tab::Stopwatch => {
            note.str(if ui.stopwatch.running {
                "running · SPACE stops · L records a lap"
            } else if ui.stopwatch.accumulated.get() == 0 {
                "ready · SPACE starts"
            } else {
                "stopped · SPACE resumes · Z zeroes"
            });
        }
        Tab::Timer => {
            if ui.timer.expired {
                note.str("elapsed · Z resets");
            } else {
                note.str("counting down from ")
                    .str(text::duration(ui.timer.target_ns, false).as_str())
                    .str(" · UP and DOWN adjust");
            }
        }
    }
    let note_y = y + face.line_height as u32 + 6;
    if note_y + BODY.line_height as u32 <= layout.readout_y + layout.readout_h {
        draw::text_centred(
            fb,
            &BODY,
            layout.readout_x,
            layout.readout_w,
            note_y,
            note.as_str(),
            p.muted,
        );
    }

    // The fade, applied over the finished box. A readout that appears rather
    // than blinks into place is most of what a tab switch reads as.
    let alpha = (ui.readout_fade.value().clamp(0, 1000) * 255 / 1000) as u8;
    draw::fade_region(
        fb,
        layout.readout_x,
        layout.readout_y,
        layout.readout_w,
        layout.readout_h,
        p.background,
        alpha,
    );
}

// ---------------------------------------------------------------------------
// The cards
// ---------------------------------------------------------------------------

/// Three cards: the selected panel, the current mode, and what this machine
/// is. Redrawn on a change rather than every frame — the numbers in them are
/// measurements taken once, and repainting a settled measurement sixty times
/// a second is work with no reader.
#[allow(clippy::too_many_arguments)]
fn cards(
    fb: &Framebuffer,
    p: &Palette,
    layout: &Layout,
    ui: &mut Ui,
    machine: &Machine,
    clock: &Clock,
    input: &Input,
) {
    if layout.cards_h < 80 {
        return;
    }
    // Both settled in `Layout`, because the card height depends on the column
    // count and the layout is what decides the height.
    let (columns, card_w) = (layout.columns, layout.card_w);

    fb.fill(
        0,
        layout.cards_y,
        layout.width,
        layout.cards_h,
        p.background,
    );

    for column in 0..columns {
        let x = layout.margin + column * (card_w + layout.margin);
        let mut y = card(
            fb,
            p,
            x,
            layout.cards_y,
            card_w,
            layout.cards_h,
            match column {
                0 => ui.panel.name(),
                1 if columns < 3 => "SESSION / MACHINE",
                1 => "SESSION",
                _ => "MACHINE",
            },
        );
        match column {
            0 => panel_rows(fb, p, x, card_w, &mut y, ui.panel, machine, clock, input),
            1 => {
                session_rows(fb, p, x, card_w, &mut y, ui, clock);
                // On a mode too narrow for three cards the machine summary
                // moves in under the session rather than disappearing. There
                // is room: the cards are sized by the layout, not by their
                // contents, and none of them fills its height.
                if columns < 3 {
                    y += 6;
                    fb.fill(x + 20, y, card_w - 40, 1, p.divider);
                    y += 10;
                    machine_rows(fb, p, x, card_w, &mut y, machine, clock, fb.composited());
                }
            }
            _ => machine_rows(fb, p, x, card_w, &mut y, machine, clock, fb.composited()),
        }
    }

    let alpha = (ui.cards_fade.value().clamp(0, 1000) * 255 / 1000) as u8;
    draw::fade_region(
        fb,
        0,
        layout.cards_y,
        layout.width,
        layout.cards_h,
        p.background,
        alpha,
    );
}

#[allow(clippy::too_many_arguments)]
fn panel_rows(
    fb: &Framebuffer,
    p: &Palette,
    x: u32,
    w: u32,
    y: &mut u32,
    panel: Panel,
    machine: &Machine,
    clock: &Clock,
    input: &Input,
) {
    let f = &machine.features;
    match panel {
        Panel::Cpu => {
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "backend",
                nanochrono_core::Backend::best().name(),
            );
            *y = row(fb, p, x, w, *y, "AVX2", yes_no(f.avx2));
            *y = row(fb, p, x, w, *y, "AVX-512F", yes_no(f.avx512f));
            *y = row(fb, p, x, w, *y, "AES-NI", yes_no(f.aesni));
            *y = row(fb, p, x, w, *y, "SHA-NI", yes_no(f.shani));
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "invariant TSC",
                yes_no(f.invariant_counter),
            );
        }
        Panel::Pmu => {
            *y = row(fb, p, x, w, *y, "core type", machine.pmu.core_type.name());
            *y = value_row(fb, p, x, w, *y, "version", machine.pmu.leaf.version as u64);
            *y = value_row(
                fb,
                p,
                x,
                w,
                *y,
                "fixed counters",
                machine.pmu.leaf.fixed_counters as u64,
            );
            *y = value_row(
                fb,
                p,
                x,
                w,
                *y,
                "general counters",
                machine.pmu.leaf.general_counters as u64,
            );
            *y = row(fb, p, x, w, *y, "route", machine.route.name());
            match machine.cycles_per_op {
                Some(c) => *y = value_row(fb, p, x, w, *y, "cycles/op", c),
                None => *y = row(fb, p, x, w, *y, "cycles/op", "unavailable"),
            }
        }
        Panel::Counter => {
            *y = value_row(fb, p, x, w, *y, "read overhead", machine.read_overhead);
            *y = value_row(fb, p, x, w, *y, "worst read", machine.worst_read);
            *y = value_row(
                fb,
                p,
                x,
                w,
                *y,
                "jitter",
                machine.worst_read.saturating_sub(machine.read_overhead),
            );
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "frequency",
                text::frequency(clock.calibration.hz).as_str(),
            );
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "rate from",
                clock.calibration.source.name(),
            );
        }
        Panel::Memory => {
            let total = machine.memory.total;
            // Bound before the call: the formatter returns an owned buffer,
            // and borrowing from it inside the argument list would drop it
            // before `row` reads it.
            let installed = text::bytes(total);
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "installed",
                if total > 0 {
                    installed.as_str()
                } else {
                    "unreported"
                },
            );
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "kernel image",
                text::bytes(machine.footprint).as_str(),
            );
            *y = value_row(
                fb,
                p,
                x,
                w,
                *y,
                "map entries",
                machine.memory.regions as u64,
            );
            *y = row(fb, p, x, w, *y, "allocator", "none");
            // Not a detail: with no allocator the image *is* the footprint,
            // which is why the figure below the meter can be exact.
            *y = row(fb, p, x, w, *y, "paging", "identity, 1 GiB pages");
        }
        Panel::Usb => {
            let found = input.found();
            // The touchpad first: on the machine this runs on it is the one
            // that was hardest to reach and the one most likely to be
            // missing, so it is the row worth reading first.
            match found.i2c {
                Some(pad) => {
                    let mut address = Text::<48>::new();
                    address.str("0x").pad(pad.slave_address as u64, 2);
                    address
                        .str(" on ")
                        .pad(pad.controller.1 as u64, 2)
                        .push(b'.')
                        .num(pad.controller.2 as u64);
                    // Where the address came from. On firmware whose `_CRS`
                    // is a method over vendor helpers, the declared address
                    // is a default for a different vendor's part, and the
                    // one that answered was found by scanning — which is
                    // worth saying rather than quietly presenting as fact.
                    address.str(match pad.discovery {
                        Discovery::Firmware => "",
                        Discovery::ProbedRegister => " (probed)",
                        Discovery::Scanned => " (found by scan)",
                    });
                    *y = row(fb, p, x, w, *y, "i2c-hid pad", address.as_str());

                    let mut ids = Text::<32>::new();
                    ids.str("0x").pad(pad.vendor as u64, 4);
                    ids.str(":0x").pad(pad.product as u64, 4);
                    *y = row(fb, p, x, w, *y, "  vendor:product", ids.as_str());

                    let mut detail = Text::<32>::new();
                    detail.str("report ");
                    match pad.report_id {
                        Some(id) => detail.num(id as u64),
                        None => detail.str("none"),
                    };
                    detail.str(" @ 0x").pad(pad.input_register as u64, 4);
                    *y = row(fb, p, x, w, *y, "  input", detail.as_str());

                    // The readiness gate. Worth a row of its own: it is the
                    // difference between the touchpad costing most of a
                    // frame's bus time and costing none of it, and while it
                    // is calibrating this is a live measurement.
                    let mut gate = Text::<48>::new();
                    match pad.gate {
                        GateState::Blind => {
                            gate.str("blind — every poll reads the bus");
                        }
                        GateState::Learning {
                            watching,
                            reports,
                            quiet,
                        } => {
                            gate.str("learning ")
                                .num(watching as u64)
                                .str(" pads (")
                                .num(reports as u64)
                                .push(b'/')
                                .num(quiet as u64)
                                .push(b')');
                        }
                        GateState::Gated { mapping, skipped } => {
                            gate.str(match mapping {
                                Mapping::Tabled(part) => part,
                                Mapping::Calibrated => "calibrated",
                            });
                            gate.str(", skipped ").num(skipped as u64);
                        }
                    }
                    *y = row(fb, p, x, w, *y, "  gpio gate", gate.as_str());
                }
                None => {
                    *y = row(
                        fb,
                        p,
                        x,
                        w,
                        *y,
                        "i2c-hid pad",
                        found.i2c_failure.map_or("not probed", |why| why.name()),
                    );
                }
            }
            *y = row(fb, p, x, w, *y, "ps/2 keyboard", yes_no(found.ps2_keyboard));
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "ps/2 pointer",
                found.ps2_pointer.map_or("none", |kind| kind.name()),
            );
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "xhci",
                if found.usb_controller {
                    "up"
                } else {
                    "not found"
                },
            );
            *y = row(fb, p, x, w, *y, "usb keyboard", yes_no(found.usb_keyboard));
            *y = row(fb, p, x, w, *y, "usb pointer", yes_no(found.usb_pointer));
            *y = row(fb, p, x, w, *y, "ehci / ohci", "seen, not driven");

            // The raw bytes, which is the row that actually settles an
            // argument about a keyboard that does nothing: none arriving and
            // the wrong ones arriving are different faults with the same
            // symptom.
            let mut raw = Text::<48>::new();
            if input.recent_bytes().is_empty() {
                raw.str("none yet - press a key");
            } else {
                for byte in input.recent_bytes() {
                    raw.str(HEX_DIGITS[(byte >> 4) as usize])
                        .str(HEX_DIGITS[(byte & 0x0F) as usize])
                        .push(b' ');
                }
            }
            *y = row(fb, p, x, w, *y, "8042 bytes", raw.as_str());

            let mut set = Text::<16>::new();
            set.str("set ").num(input.scancode_set() as u64);
            if input.scancode_set() == 2 {
                set.str(" (untranslated)");
            }
            *y = row(fb, p, x, w, *y, "  scancodes", set.as_str());
        }
        Panel::Hypervisor => {
            let sig = machine.hypervisor.signature_str();
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "signature",
                if sig.is_empty() { "none" } else { sig },
            );
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "hypercall",
                yes_no(machine.hypervisor.hypercall_ok),
            );
            match machine.hypervisor.pairing {
                Some(pair) => {
                    *y = value_row(fb, p, x, w, *y, "host ns", pair.host_ns);
                    *y = row(fb, p, x, w, *y, "paired", "yes");
                }
                None => {
                    *y = row(fb, p, x, w, *y, "paired", "no");
                }
            }
            // Live, not the value taken at boot. A hypercall traps into the
            // host kernel and costs microseconds — and a variable number of
            // them, since what happens on the other side is another
            // scheduler. So the host clock is asked once, at boot, and the
            // stopwatch then reads the counter and nothing else. This row is
            // how that stays true: it should read `1` here and `1` an hour
            // into a session, and if it ever climbs while the stopwatch runs,
            // a hypercall has got into the frame loop.
            let mut calls = Text::<32>::new();
            calls.num(crate::hypervisor::hypercalls() as u64);
            calls.str(" (once, at boot)");
            *y = row(fb, p, x, w, *y, "hypercalls", calls.as_str());
        }
    }
}

fn session_rows(
    fb: &Framebuffer,
    p: &Palette,
    x: u32,
    w: u32,
    y: &mut u32,
    ui: &mut Ui,
    clock: &Clock,
) {
    match ui.tab {
        Tab::Clock => {
            match clock.date {
                Some(d) => {
                    let mut date = Text::<16>::new();
                    date.pad(d.year as u64, 4)
                        .push(b'-')
                        .pad(d.month as u64, 2)
                        .push(b'-')
                        .pad(d.day as u64, 2);
                    *y = row(fb, p, x, w, *y, "date", date.as_str());
                    *y = row(fb, p, x, w, *y, "source", "cmos rtc");
                }
                None => {
                    *y = row(fb, p, x, w, *y, "date", "no rtc");
                    *y = row(fb, p, x, w, *y, "showing", "time since boot");
                }
            }
            *y = row(fb, p, x, w, *y, "sub-second from", "counter");
            *y = row(fb, p, x, w, *y, "resolution", "1 ns");
        }
        Tab::Stopwatch => {
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "state",
                if ui.stopwatch.running {
                    "running"
                } else {
                    "stopped"
                },
            );
            *y = row(fb, p, x, w, *y, "ecc", ui.stopwatch.integrity.name());
            if ui.stopwatch.lap_count == 0 {
                *y = row(fb, p, x, w, *y, "laps", "none — press L");
            }
            for (i, &ticks) in ui.stopwatch.laps[..ui.stopwatch.lap_count]
                .iter()
                .enumerate()
            {
                let ns = clock.calibration.ticks_to_ns(ticks);
                let mut label = Text::<8>::new();
                label.str("lap ").num(i as u64 + 1);
                let value = text::duration(ns, false);
                *y = row(fb, p, x, w, *y, label.as_str(), value.as_str());
            }
        }
        Tab::Timer => {
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "set to",
                text::duration(ui.timer.target_ns, false).as_str(),
            );
            *y = row(
                fb,
                p,
                x,
                w,
                *y,
                "state",
                if ui.timer.expired {
                    "elapsed"
                } else if ui.timer.running {
                    "running"
                } else {
                    "stopped"
                },
            );
            *y = row(fb, p, x, w, *y, "step", "10 s");
            *y = row(fb, p, x, w, *y, "alarm", "visual only (no sound device)");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn machine_rows(
    fb: &Framebuffer,
    p: &Palette,
    x: u32,
    w: u32,
    y: &mut u32,
    machine: &Machine,
    clock: &Clock,
    composited: bool,
) {
    *y = row(fb, p, x, w, *y, "operating system", "none");
    // SAFETY: reads firmware memory only; the interface runs at ring 0.
    let (source, xsdt) = unsafe { acpi::root_source() };
    *y = row(fb, p, x, w, *y, "acpi root", source.name());
    *y = row(
        fb,
        p,
        x,
        w,
        *y,
        "root table",
        if xsdt {
            "xsdt (64-bit)"
        } else {
            "rsdt (32-bit)"
        },
    );
    *y = row(
        fb,
        p,
        x,
        w,
        *y,
        "counter rate",
        text::frequency(clock.calibration.hz).as_str(),
    );
    *y = row(
        fb,
        p,
        x,
        w,
        *y,
        "acpi",
        if machine.acpi.is_some() {
            "present"
        } else {
            "absent"
        },
    );
    *y = row(
        fb,
        p,
        x,
        w,
        *y,
        "compositing",
        if composited { "back buffer" } else { "direct" },
    );
    *y = row(fb, p, x, w, *y, "interrupts", "masked");
}

/// Draws a card and returns the y its rows start at.
fn card(fb: &Framebuffer, p: &Palette, x: u32, y: u32, w: u32, h: u32, title: &str) -> u32 {
    // A one-pixel offset fill under the card, which reads as a shadow at this
    // contrast and costs one more rectangle.
    draw::rounded(fb, x + 2, y + 3, w, h, 14, p.shadow);
    draw::rounded(fb, x, y, w, h, 14, p.panel);
    draw::rounded_outline(fb, x, y, w, h, 14, p.divider);

    draw::text(fb, &HEADING, x + 20, y + 14, title, p.accent);
    fb.fill(
        x + 20,
        y + 18 + HEADING.line_height as u32,
        w - 40,
        1,
        p.divider,
    );
    y + 28 + HEADING.line_height as u32
}

/// One label/value row, with the value right-aligned inside the card.
///
/// The value is what the row exists to show, so it keeps its place and the
/// label gives way. Truncation is by measured width, not character count:
/// the typeface is proportional, so counting characters is only ever
/// approximately right.
fn row(fb: &Framebuffer, p: &Palette, x: u32, w: u32, y: u32, label: &str, value: &str) -> u32 {
    let pad = 20;
    let right = x + w - pad;
    draw::text_right(fb, &BODY, right, y, value, p.text);

    let room = right.saturating_sub(BODY.width_of(value) + BODY.width_of("  ") + x + pad);
    draw::text(
        fb,
        &BODY,
        x + pad,
        y,
        truncate_to_width(label, room),
        p.muted,
    );

    y + BODY.line_height as u32 + 7
}

/// The same, for a number.
#[allow(clippy::too_many_arguments)]
fn value_row(
    fb: &Framebuffer,
    p: &Palette,
    x: u32,
    w: u32,
    y: u32,
    label: &str,
    value: u64,
) -> u32 {
    let mut text = Text::<24>::new();
    text.num(value);
    row(fb, p, x, w, y, label, text.as_str())
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

/// The longest prefix of `s` that fits in `width` pixels.
fn truncate_to_width(s: &str, width: u32) -> &str {
    if BODY.width_of(s) <= width {
        return s;
    }
    let mut used = 0;
    let mut end = 0;
    for (i, b) in s.bytes().enumerate() {
        let advance = BODY.glyph(b).advance as u32;
        if used + advance > width {
            break;
        }
        used += advance;
        end = i + 1;
    }
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ---------------------------------------------------------------------------
// The bars along the bottom
// ---------------------------------------------------------------------------

/// The key legend — or, where there is no keyboard, what happened instead.
///
/// A legend of keyboard shortcuts is worse than useless on a machine whose
/// keyboard did not come up: it is the most prominent line on screen telling
/// the reader to press things that do nothing. When a stack came up short
/// this says so there instead.
fn hint_bar(fb: &Framebuffer, p: &Palette, layout: &Layout, ui: &Ui) {
    let y = layout.hint_y;
    let h = layout.status_y.saturating_sub(y);
    fb.fill(0, y, layout.width, h, p.background);

    let ty = y + h.saturating_sub(BODY.line_height as u32) / 2;

    if let Some(note) = ui.input_note {
        draw::text(fb, &BODY, layout.margin, ty, "INPUT", p.danger);
        draw::text(
            fb,
            &BODY,
            layout.margin + BODY.width_of("INPUT  "),
            ty,
            note,
            p.text,
        );
        return;
    }
    // Laid out one hint at a time and stopped when the row is full, rather
    // than as one string that would simply run off the edge on a narrow mode.
    let hints: [(&str, &str); 8] = [
        ("1-3", "Mode"),
        ("SPACE", "Start"),
        ("L", "Lap"),
        ("Z", "Zero"),
        ("N", "Precision"),
        ("TAB", "Panel"),
        ("R", "Restart"),
        ("S", "Shut down"),
    ];

    let mut pen = layout.margin;
    for (key, action) in hints {
        let mut label = Text::<24>::new();
        label.str("[").str(key).str("] ").str(action);
        let w = BODY.width_of(label.as_str());
        if pen + w + layout.margin > layout.width {
            break;
        }
        // The bracketed key in the accent and the action muted, so the row
        // scans as keys rather than as a sentence.
        let mut bracket = Text::<12>::new();
        bracket.str("[").str(key).str("]");
        draw::text(fb, &BODY, pen, ty, bracket.as_str(), p.accent);
        draw::text(
            fb,
            &BODY,
            pen + BODY.width_of(bracket.as_str()) + BODY.width_of(" "),
            ty,
            action,
            p.muted,
        );
        pen += w + 22;
    }
}

/// The panel the status readings sit on, drawn once.
fn status_frame(fb: &Framebuffer, p: &Palette, layout: &Layout) {
    fb.fill(0, layout.status_y, layout.width, layout.status_h, p.panel);
    fb.fill(0, layout.status_y, layout.width, 1, p.divider);
}

/// The two rows of readings at the bottom.
///
/// Redrawn only when something in it moved. Most of what it shows was
/// measured once and does not change, and repainting a settled number sixty
/// times a second is the difference between a status bar and a busy loop.
fn status_bar(
    fb: &Framebuffer,
    p: &Palette,
    layout: &Layout,
    ui: &mut Ui,
    machine: &Machine,
    clock: &Clock,
    input: &Input,
) {
    // --- what the meters should read
    let used = machine.footprint;
    let total = machine.memory.total;
    let memory_permille = (used * 1000)
        .checked_div(total)
        .map_or(0, |permille| permille.min(1000) as u32);
    ui.memory_meter.retarget(memory_permille as i32);
    ui.load_meter.retarget(ui.load_permille as i32);
    let moving = ui.memory_meter.step() | ui.load_meter.step();

    // The load figure is recomputed once a second; without that, this row
    // would repaint every frame for a number that had not changed.
    let mut load = Text::<8>::new();
    load.num((ui.load_meter.value().max(0) as u32 / 10) as u64)
        .push(b'%');
    if !moving && load.as_str() == ui.last_load.as_str() {
        return;
    }
    ui.last_load.clear();
    ui.last_load.str(load.as_str());

    let row_h = BODY.line_height as u32;
    let y1 = layout.status_y + 7;
    let y2 = y1 + row_h + 6;
    fb.fill(
        0,
        layout.status_y + 1,
        layout.width,
        layout.status_h - 1,
        p.panel,
    );

    let meter_w = (layout.width / 8).clamp(70, 150);
    let meter_h = (row_h / 2).max(6);
    let meter_y = y1 + (row_h - meter_h) / 2;

    // --- row one: memory, load, counter rate
    let mut pen = layout.margin;
    draw::text(fb, &BODY, pen, y1, "MEM", p.muted);
    pen += BODY.width_of("MEM") + 10;
    draw::meter(
        fb,
        pen,
        meter_y,
        meter_w,
        meter_h,
        ui.memory_meter.value().max(0) as u32,
        p.track,
        p.accent,
    );
    pen += meter_w + 12;

    // `9.5 MB / 31.7 GB`, not `0.3 / 31.7 GB`. Each side is scaled to its own
    // magnitude, because a kernel using nine megabytes of a thirty-gigabyte
    // machine expressed in the machine's units is a number that rounds to
    // nothing and tells the reader nothing.
    let mut memory = Text::<32>::new();
    memory.str(text::bytes(used).as_str());
    memory.str(" / ");
    if total > 0 {
        memory.str(text::bytes(total).as_str());
    } else {
        memory.str("unreported");
    }
    draw::text(fb, &BODY, pen, y1, memory.as_str(), p.text);
    pen += BODY.width_of(memory.as_str()) + 28;

    if pen + meter_w + 90 < layout.width {
        draw::text(fb, &BODY, pen, y1, "CPU", p.muted);
        pen += BODY.width_of("CPU") + 10;
        let permille = ui.load_meter.value().max(0) as u32;
        draw::meter(
            fb,
            pen,
            meter_y,
            meter_w,
            meter_h,
            permille,
            p.track,
            if permille > 800 { p.warn } else { p.accent },
        );
        pen += meter_w + 12;
        draw::text(fb, &BODY, pen, y1, load.as_str(), p.text);
    }

    // Right of row one: the counter's rate and where the rate came from,
    // which is what every number above it is denominated in.
    let mut rate = Text::<32>::new();
    rate.str(text::frequency(clock.calibration.hz).as_str())
        .str("  ")
        .str(clock.calibration.source.name());
    draw::text_right(
        fb,
        &BODY,
        layout.width - layout.margin,
        y1,
        rate.as_str(),
        if clock.calibration.source.trustworthy() {
            p.accent
        } else {
            p.danger
        },
    );

    // --- row two: what the counter is and what read it
    let mut left = Text::<96>::new();
    left.str("route: ")
        .str(machine.route.name())
        .str("   overhead: ")
        .num(machine.read_overhead)
        .str("   jitter: ")
        .num(machine.worst_read.saturating_sub(machine.read_overhead))
        .str("   ecc: ")
        .str(ui.stopwatch.integrity.name());
    draw::text(fb, &BODY, layout.margin, y2, left.as_str(), p.muted);

    let mut right = Text::<64>::new();
    right.str(input.source());
    let signature = machine.hypervisor.signature_str();
    if !signature.is_empty() {
        right.str("   ").str(signature);
    }
    let right_w = BODY.width_of(right.as_str());
    if layout.margin + BODY.width_of(left.as_str()) + right_w + 40 < layout.width {
        draw::text_right(
            fb,
            &BODY,
            layout.width - layout.margin,
            y2,
            right.as_str(),
            p.muted,
        );
    }
}

// ---------------------------------------------------------------------------
// The cursor
// ---------------------------------------------------------------------------

/// Cursor side, in pixels. Square and small: every move copies this many
/// pixels twice, and it is drawn from a polled loop.
const CURSOR: u32 = 12;

/// A software cursor: the framebuffer has no hardware overlay, so the pixels
/// under it are saved and restored as it moves.
///
/// With a back buffer this saves and restores *back buffer* pixels, which is
/// what keeps a cursor from becoming part of the background it is drawn over:
/// the buffer persists between frames, so a cursor drawn into it and not
/// erased would smear.
struct Cursor {
    x: i32,
    y: i32,
    /// Where it was when `under` was filled, which is not where it is now if
    /// it has been moved since.
    drawn_x: i32,
    drawn_y: i32,
    under: [Colour; (CURSOR * CURSOR) as usize],
    drawn: bool,
}

impl Cursor {
    fn new() -> Cursor {
        Cursor {
            x: 0,
            y: 0,
            drawn_x: 0,
            drawn_y: 0,
            under: [0; (CURSOR * CURSOR) as usize],
            drawn: false,
        }
    }

    fn move_to(&mut self, x: i32, y: i32) {
        self.x = x;
        self.y = y;
    }

    fn draw(&mut self, fb: &Framebuffer, p: &Palette) {
        self.drawn_x = self.x;
        self.drawn_y = self.y;
        for row in 0..CURSOR {
            for col in 0..CURSOR {
                // An arrow, which reads as a pointer where a square does not:
                // a triangle with a one-pixel dark edge so it stays visible
                // over the accent as well as over the background.
                if col > row || row >= CURSOR - col / 2 {
                    continue;
                }
                let x = self.drawn_x as u32 + col;
                let y = self.drawn_y as u32 + row;
                self.under[(row * CURSOR + col) as usize] = fb.get(x, y);
                let edge = col == 0 || col == row || row + 1 >= CURSOR - col / 2;
                fb.set(x, y, if edge { p.background } else { p.title });
            }
        }
        self.drawn = true;
    }

    fn erase(&mut self, fb: &Framebuffer) {
        if !self.drawn {
            return;
        }
        for row in 0..CURSOR {
            for col in 0..CURSOR {
                if col > row || row >= CURSOR - col / 2 {
                    continue;
                }
                fb.set(
                    self.drawn_x as u32 + col,
                    self.drawn_y as u32 + row,
                    self.under[(row * CURSOR + col) as usize],
                );
            }
        }
        self.drawn = false;
    }
}

// ---------------------------------------------------------------------------
// Glyphs drawn rather than decoded
// ---------------------------------------------------------------------------

/// A circular arrow: restart.
fn glyph_restart(fb: &Framebuffer, cx: u32, cy: u32, colour: Colour) {
    arc(fb, cx, cy, 9.0, 0.7, 5.4, 2, colour);
    // The head, at the open end.
    fb.fill(cx + 4, cy - 11, 8, 2, colour);
    fb.fill(cx + 10, cy - 11, 2, 8, colour);
}

/// A power symbol: a broken ring with a stem.
fn glyph_power(fb: &Framebuffer, cx: u32, cy: u32, colour: Colour) {
    arc(fb, cx, cy, 9.0, 5.5, 10.9, 2, colour);
    fb.fill(cx - 1, cy - 12, 2, 11, colour);
}

/// Plots an arc from `start` to `end` radians, `thickness` pixels wide.
///
/// Eight parameters, and every one is a distinct scalar the caller chooses.
/// Grouping them into a struct would move the same list one line up and add
/// a type to read through.
#[allow(clippy::too_many_arguments)]
fn arc(
    fb: &Framebuffer,
    cx: u32,
    cy: u32,
    r: f32,
    start: f32,
    end: f32,
    thickness: u32,
    c: Colour,
) {
    // Step chosen so consecutive samples land within a pixel of each other at
    // this radius; a coarser one draws a dotted line.
    let steps = ((end - start) * r * 2.0) as u32 + 1;
    for i in 0..=steps {
        let a = start + (end - start) * i as f32 / steps as f32;
        let (sin, cos) = sin_cos(a);
        let x = cx as i32 + (cos * r) as i32;
        let y = cy as i32 + (sin * r) as i32;
        if x < 0 || y < 0 {
            continue;
        }
        fb.fill(x as u32, y as u32, thickness, thickness, c);
    }
}

/// Sine and cosine by Taylor series.
///
/// `libm` is not linked and `core` has no floating-point maths. Four terms is
/// far more precision than a twenty-pixel circle can show.
fn sin_cos(a: f32) -> (f32, f32) {
    const TAU: f32 = 6.283_185_5;
    let mut x = a;
    while x > TAU / 2.0 {
        x -= TAU;
    }
    while x < -TAU / 2.0 {
        x += TAU;
    }
    let x2 = x * x;
    let sin = x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0)));
    let cos = 1.0 - x2 / 2.0 * (1.0 - x2 / 12.0 * (1.0 - x2 / 30.0));
    (sin, cos)
}
