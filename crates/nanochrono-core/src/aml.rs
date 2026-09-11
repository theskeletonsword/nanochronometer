// SPDX-License-Identifier: Apache-2.0
//! Enough ACPI Machine Language to find a device and ask it two questions.
//!
//! # Why this exists
//!
//! A modern notebook's touchpad is an I2C-HID device, and nothing about it is
//! discoverable from hardware. There is no enumeration on an I2C bus: a
//! controller can only talk to an address it already knows. The address, the
//! bus it is on, the register the HID descriptor lives at — all of it is
//! written in the firmware's **AML bytecode**, and the only way to read it is
//! to execute that bytecode.
//!
//! That is why a touchpad that a `lspci` cannot see is nevertheless usable by
//! every operating system: they all ship an AML interpreter.
//!
//! # What is here, and what is deliberately not
//!
//! ACPICA — the reference interpreter, which Linux and FreeBSD both use — is
//! on the order of a hundred thousand lines. It implements the whole language
//! because a general-purpose kernel must: it runs `_PTS` and `_WAK` across
//! sleep, drives thermal zones, evaluates methods that touch operation
//! regions backed by SMBus and embedded controllers.
//!
//! None of that is needed to find a touchpad. What is needed is:
//!
//! 1. **Walk the namespace.** Scopes and devices nest; each device carries a
//!    `_HID` and sometimes a `_CID`. Finding "the device whose compatible ID
//!    is `PNP0C50`" is a tree walk, not an evaluation.
//! 2. **Read `_CRS`.** The current resource settings: a byte buffer of
//!    resource descriptors, one of which is an `I2cSerialBus` carrying the
//!    slave address, the bus speed and the name of the controller it hangs
//!    off. On most firmware this is a plain `Name`, and reading it is
//!    parsing, not execution.
//! 3. **Evaluate `_DSM`.** The device-specific method, which is where the
//!    I2C-HID specification puts the HID descriptor register. This one is a
//!    real method and does have to be *run* — but the shape it takes is
//!    narrow: compare a UUID, compare a function index, return a constant.
//!
//! So this is an interpreter for the subset that reaches those three, and it
//! **fails rather than guesses** on anything outside it. A wrong address on
//! an I2C bus is not a wrong answer, it is a transaction with some other
//! device, and there is no telling what that device does with it.
//!
//! # Reference
//!
//! Opcode values and encodings are from the ACPI specification, section 20
//! (ACPI Machine Language). The structure of the walk follows FreeBSD's use
//! of ACPICA in `sys/dev/iicbus/acpi_iicbus.c` and
//! `sys/dev/iicbus/iichid.c` — which is to say: find the device, read its
//! `_CRS` for the `I2cSerialBus`, call `_DSM` for the descriptor register.
//! No code was copied; ACPICA's is a general interpreter and this is not.

/// Half-open byte range into a table.
///
/// `core::ops::Range` would say the same thing, but it is deliberately not
/// `Copy` — it is an iterator, and a copied iterator is nearly always a bug.
/// Here the ranges are pure locations that get copied constantly, so they get
/// a type that copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    pub const fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub const fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// The bytes it names, or `None` if it falls outside `code`.
    pub fn of<'a>(&self, code: &'a [u8]) -> Option<&'a [u8]> {
        code.get(self.start..self.end)
    }
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// One four-character ACPI name segment, as it appears in the bytecode.
pub type NameSeg = [u8; 4];

/// How deep a namespace path this can hold.
///
/// `\_SB.PC00.I2C5.TPD0` is four segments, which is typical. Eight is past
/// anything real firmware nests to, and a path deeper than this is not
/// truncated — it is refused, because a truncated path names a different
/// device.
pub const MAX_PATH: usize = 8;

/// An absolute path in the ACPI namespace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Path {
    segments: [NameSeg; MAX_PATH],
    len: usize,
}

impl Path {
    pub const fn root() -> Path {
        Path {
            segments: [[b'_'; 4]; MAX_PATH],
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn segments(&self) -> &[NameSeg] {
        &self.segments[..self.len]
    }

    /// Appends a segment. Returns `false` if the path is already full, which
    /// the caller must treat as "this device could not be named".
    pub fn push(&mut self, segment: NameSeg) -> bool {
        if self.len == MAX_PATH {
            return false;
        }
        self.segments[self.len] = segment;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) {
        self.len = self.len.saturating_sub(1);
    }

    /// The last segment, which is the object's own name.
    pub fn leaf(&self) -> Option<NameSeg> {
        self.len.checked_sub(1).map(|i| self.segments[i])
    }

    /// Whether `self` ends with the same segments as `other`.
    ///
    /// How a resource's `ResourceSource` string — which names a controller by
    /// path — is matched against a device found in the walk. Firmware writes
    /// these both absolutely and relatively, so a suffix match is what
    /// actually holds.
    pub fn ends_with(&self, other: &Path) -> bool {
        if other.len > self.len {
            return false;
        }
        self.segments[self.len - other.len..self.len] == other.segments[..other.len]
    }

    /// Renders as `\SEG0.SEG1...`, trailing underscores trimmed.
    ///
    /// Written into a caller's buffer because there is no allocator. Returns
    /// how many bytes were used.
    pub fn render(&self, out: &mut [u8]) -> usize {
        let mut used = 0;
        let put = |byte: u8, out: &mut [u8], used: &mut usize| {
            if *used < out.len() {
                out[*used] = byte;
                *used += 1;
            }
        };
        put(b'\\', out, &mut used);
        for (i, segment) in self.segments().iter().enumerate() {
            if i > 0 {
                put(b'.', out, &mut used);
            }
            // Trailing underscores are padding in the encoding, not part of
            // the name: `I2C5` is stored as `I2C5` but `SB` as `SB__`.
            let end = segment
                .iter()
                .rposition(|&b| b != b'_')
                .map_or(0, |last| last + 1);
            for &byte in &segment[..end.max(1)] {
                put(byte, out, &mut used);
            }
        }
        used
    }
}

impl core::fmt::Debug for Path {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut buf = [0u8; MAX_PATH * 5 + 1];
        let used = self.render(&mut buf);
        f.write_str(core::str::from_utf8(&buf[..used]).unwrap_or("<path>"))
    }
}

/// Parses an ASCII path such as `\_SB.PC00.I2C5.TPD0` into a [`Path`].
///
/// Used for the `ResourceSource` strings inside a resource template, which
/// are text rather than encoded name segments.
pub fn parse_path(text: &[u8]) -> Option<Path> {
    let mut path = Path::root();
    let mut segment = [b'_'; 4];
    let mut filled = 0;

    for &byte in text {
        match byte {
            // Leading `\` is the root marker and `^` a parent reference;
            // neither contributes a segment. A relative path is matched by
            // suffix, so dropping the marker loses nothing.
            b'\\' | b'^' => {}
            b'.' => {
                if filled == 0 {
                    return None;
                }
                if !path.push(segment) {
                    return None;
                }
                segment = [b'_'; 4];
                filled = 0;
            }
            0 => break,
            _ => {
                if filled == 4 {
                    return None;
                }
                segment[filled] = byte;
                filled += 1;
            }
        }
    }
    if filled > 0 && !path.push(segment) {
        return None;
    }
    (!path.is_empty()).then_some(path)
}

// ---------------------------------------------------------------------------
// Opcodes
// ---------------------------------------------------------------------------

mod op {
    pub const ZERO: u8 = 0x00;
    pub const ONE: u8 = 0x01;
    pub const ALIAS: u8 = 0x06;
    pub const NAME: u8 = 0x08;
    pub const BYTE_PREFIX: u8 = 0x0A;
    pub const WORD_PREFIX: u8 = 0x0B;
    pub const DWORD_PREFIX: u8 = 0x0C;
    pub const STRING_PREFIX: u8 = 0x0D;
    pub const QWORD_PREFIX: u8 = 0x0E;
    pub const SCOPE: u8 = 0x10;
    pub const BUFFER: u8 = 0x11;
    pub const PACKAGE: u8 = 0x12;
    pub const VAR_PACKAGE: u8 = 0x13;
    pub const METHOD: u8 = 0x14;
    /// `External`, which some compilers emit into the table even though it is
    /// a declaration rather than an object.
    pub const EXTERNAL: u8 = 0x15;
    pub const EXT_PREFIX: u8 = 0x5B;
    pub const LOCAL0: u8 = 0x60;
    pub const LOCAL7: u8 = 0x67;
    pub const ARG0: u8 = 0x68;
    pub const ARG6: u8 = 0x6E;
    pub const STORE: u8 = 0x70;
    pub const ADD: u8 = 0x72;
    pub const SUBTRACT: u8 = 0x74;
    pub const INCREMENT: u8 = 0x75;
    pub const DECREMENT: u8 = 0x76;
    pub const SHIFT_LEFT: u8 = 0x79;
    pub const SHIFT_RIGHT: u8 = 0x7A;
    pub const AND: u8 = 0x7B;
    pub const NAND: u8 = 0x7C;
    pub const OR: u8 = 0x7D;
    pub const NOR: u8 = 0x7E;
    pub const XOR: u8 = 0x7F;
    pub const MULTIPLY: u8 = 0x77;
    pub const CONCAT: u8 = 0x73;
    pub const FIND_SET_LEFT_BIT: u8 = 0x81;
    pub const FIND_SET_RIGHT_BIT: u8 = 0x82;
    pub const REF_OF: u8 = 0x71;
    pub const TO_HEX_STRING: u8 = 0x98;
    pub const TO_DECIMAL_STRING: u8 = 0x97;
    pub const SIZE_OF: u8 = 0x87;
    /// The `CreateXField` family, which carve a named field out of a buffer.
    /// All take two term arguments and a name; `CREATE_FIELD` below takes
    /// three.
    pub const CREATE_DWORD_FIELD: u8 = 0x8A;
    pub const CREATE_WORD_FIELD: u8 = 0x8B;
    pub const CREATE_BYTE_FIELD: u8 = 0x8C;
    pub const CREATE_BIT_FIELD: u8 = 0x8D;
    pub const CREATE_QWORD_FIELD: u8 = 0x8F;
    pub const INDEX: u8 = 0x88;
    pub const DEREF_OF: u8 = 0x83;
    pub const LAND: u8 = 0x90;
    pub const LOR: u8 = 0x91;
    pub const LNOT: u8 = 0x92;
    pub const LEQUAL: u8 = 0x93;
    pub const LGREATER: u8 = 0x94;
    pub const LLESS: u8 = 0x95;
    pub const TO_BUFFER: u8 = 0x96;
    pub const TO_INTEGER: u8 = 0x99;
    pub const IF: u8 = 0xA0;
    pub const ELSE: u8 = 0xA1;
    pub const WHILE: u8 = 0xA2;
    pub const NOOP: u8 = 0xA3;
    pub const RETURN: u8 = 0xA4;
    pub const BREAK: u8 = 0xA5;
    pub const ONES: u8 = 0xFF;

    // Extended, following `EXT_PREFIX`.
    pub const EXT_MUTEX: u8 = 0x01;
    pub const EXT_EVENT: u8 = 0x02;
    pub const EXT_OP_REGION: u8 = 0x80;
    pub const EXT_FIELD: u8 = 0x81;
    pub const EXT_DEVICE: u8 = 0x82;
    pub const EXT_PROCESSOR: u8 = 0x83;
    pub const EXT_POWER_RES: u8 = 0x84;
    pub const EXT_THERMAL_ZONE: u8 = 0x85;
    pub const EXT_INDEX_FIELD: u8 = 0x86;
    pub const EXT_BANK_FIELD: u8 = 0x87;
    pub const EXT_CREATE_FIELD: u8 = 0x13;
    /// Extended opcodes that can stand where a term argument does. Firmware
    /// uses `CondRefOf` in particular as an `If` predicate — "is this object
    /// defined?" — which is how a table guards against an object another
    /// table may or may not have declared.
    pub const NOTIFY: u8 = 0x86;
    pub const CONTINUE: u8 = 0x9F;
    /// Extended statements: things a method *does*, which a term list inside
    /// an `If` is full of.
    pub const EXT_LOAD: u8 = 0x20;
    pub const EXT_STALL: u8 = 0x21;
    pub const EXT_SLEEP: u8 = 0x22;
    pub const EXT_SIGNAL: u8 = 0x24;
    pub const EXT_RESET: u8 = 0x26;
    pub const EXT_RELEASE: u8 = 0x27;
    pub const EXT_UNLOAD: u8 = 0x2A;
    pub const EXT_FATAL: u8 = 0x32;
    pub const EXT_COND_REF_OF: u8 = 0x12;
    pub const EXT_LOAD_TABLE: u8 = 0x1F;
    pub const EXT_ACQUIRE: u8 = 0x23;
    pub const EXT_WAIT: u8 = 0x25;
    pub const EXT_FROM_BCD: u8 = 0x28;
    pub const EXT_TO_BCD: u8 = 0x29;
    pub const EXT_REVISION: u8 = 0x30;
    pub const EXT_DEBUG: u8 = 0x31;
    pub const EXT_TIMER: u8 = 0x33;
}

/// A cursor over AML bytecode.
///
/// Every read is bounds-checked and returns `None` past the end. Firmware is
/// not always well formed, and a table this walks off the end of is a page
/// fault in a kernel with no handler.
#[derive(Debug, Clone, Copy)]
struct Cursor<'a> {
    code: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn at(code: &'a [u8], at: usize) -> Cursor<'a> {
        Cursor { code, at }
    }

    fn peek(&self) -> Option<u8> {
        self.code.get(self.at).copied()
    }

    fn byte(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.code.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn integer(&mut self, bytes: usize) -> Option<u64> {
        let slice = self.take(bytes)?;
        let mut value = 0u64;
        for (i, &byte) in slice.iter().enumerate() {
            value |= (byte as u64) << (8 * i);
        }
        Some(value)
    }

    /// Decodes a PkgLength, returning the range of the package *body*.
    ///
    /// The encoded length counts the length bytes themselves, which is the
    /// detail that turns an off-by-one into a walk through the middle of the
    /// next object.
    fn package(&mut self) -> Option<Span> {
        let start = self.at;
        let lead = self.byte()?;
        let follow = (lead >> 6) as usize;
        let total = if follow == 0 {
            (lead & 0x3F) as usize
        } else {
            let mut value = (lead & 0x0F) as usize;
            for i in 0..follow {
                value |= (self.byte()? as usize) << (4 + 8 * i);
            }
            value
        };
        let end = start.checked_add(total)?;
        if end > self.code.len() || end < self.at {
            return None;
        }
        Some(Span::new(self.at, end))
    }

    /// Reads a NameString, returning its segments and whether it was absolute.
    fn name(&mut self) -> Option<(Path, bool)> {
        let mut path = Path::root();
        let mut absolute = false;

        match self.peek()? {
            b'\\' => {
                self.at += 1;
                absolute = true;
            }
            b'^' => {
                while self.peek() == Some(b'^') {
                    self.at += 1;
                }
            }
            _ => {}
        }

        match self.peek()? {
            // NullName: a name that refers to the current scope.
            0x00 => {
                self.at += 1;
            }
            // DualNamePrefix.
            0x2E => {
                self.at += 1;
                for _ in 0..2 {
                    let seg = self.take(4)?;
                    path.push([seg[0], seg[1], seg[2], seg[3]]);
                }
            }
            // MultiNamePrefix, followed by a count.
            0x2F => {
                self.at += 1;
                let count = self.byte()?;
                for _ in 0..count {
                    let seg = self.take(4)?;
                    if !path.push([seg[0], seg[1], seg[2], seg[3]]) {
                        return None;
                    }
                }
            }
            _ => {
                let seg = self.take(4)?;
                path.push([seg[0], seg[1], seg[2], seg[3]]);
            }
        }
        Some((path, absolute))
    }
}

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// A `_HID` or `_CID` value, normalised to its seven-character text form.
///
/// Firmware writes these two ways — as an EISA-compressed integer, or as a
/// string — and the same device is routinely described with one in `_HID` and
/// the other in `_CID`. Normalising at the point of reading means a caller
/// matches on `"PNP0C50"` once instead of on both encodings.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HardwareId {
    text: [u8; 16],
    len: usize,
}

impl HardwareId {
    pub fn as_bytes(&self) -> &[u8] {
        &self.text[..self.len]
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    pub fn matches(&self, name: &str) -> bool {
        self.as_bytes().eq_ignore_ascii_case(name.as_bytes())
    }

    fn from_text(bytes: &[u8]) -> HardwareId {
        let mut id = HardwareId {
            text: [0; 16],
            len: 0,
        };
        for &byte in bytes {
            if byte == 0 || id.len == id.text.len() {
                break;
            }
            id.text[id.len] = byte;
            id.len += 1;
        }
        id
    }

    /// Decodes the EISA-compressed form.
    ///
    /// Three five-bit letters biased by `@`, then four hex digits, packed
    /// big-endian — so the little-endian integer the bytecode holds is byte
    /// swapped first. `PNP0C50` is `0x500CD041` in a DSDT and nothing about
    /// that is guessable from looking at it.
    fn from_eisa(compressed: u32) -> HardwareId {
        let swapped = compressed.swap_bytes();
        let letter = |shift: u32| b'@' + ((swapped >> shift) & 0x1F) as u8;
        let hex = |shift: u32| {
            let nibble = ((swapped >> shift) & 0xF) as u8;
            if nibble < 10 {
                b'0' + nibble
            } else {
                b'A' + nibble - 10
            }
        };
        let text = [
            letter(26),
            letter(21),
            letter(16),
            hex(12),
            hex(8),
            hex(4),
            hex(0),
        ];
        HardwareId::from_text(&text)
    }
}

impl core::fmt::Debug for HardwareId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// What an evaluation produced.
///
/// Buffers and strings are ranges into the original bytecode rather than
/// copies: there is no allocator, and everything a caller wants out of them
/// is read once and immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Uninitialised,
    Integer(u64),
    /// A byte buffer, as `code[range]`.
    Buffer(Span),
    /// A null-terminated ASCII string, as `code[range]` without the null.
    Text(Span),
    /// A package, as the range of its encoded element list. Only its
    /// existence is modelled; nothing here needs to index one.
    Package(Span),
}

impl Value {
    pub fn integer(&self) -> Option<u64> {
        match self {
            Value::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn bytes<'a>(&self, code: &'a [u8]) -> Option<&'a [u8]> {
        match self {
            Value::Buffer(span) | Value::Text(span) => span.of(code),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The namespace
// ---------------------------------------------------------------------------

/// A device found in the namespace walk.
#[derive(Debug, Clone, Copy)]
pub struct AcpiDevice {
    pub path: Path,
    pub hid: Option<HardwareId>,
    pub cid: Option<HardwareId>,
    /// The device's own object list, where its methods and names live.
    body: Span,
}

impl AcpiDevice {
    /// Whether either identifier matches `name`.
    ///
    /// Both, because an I2C-HID touchpad names itself by its vendor part in
    /// `_HID` — `ELAN06FA`, which nothing generic can know — and declares
    /// `PNP0C50` in `_CID` to say what protocol it speaks. Matching only
    /// `_HID` finds nothing on any machine you have not seen before.
    pub fn is(&self, name: &str) -> bool {
        self.hid.is_some_and(|id| id.matches(name)) || self.cid.is_some_and(|id| id.matches(name))
    }
}

/// The DSDT, walked.
#[derive(Debug, Clone, Copy)]
pub struct Namespace<'a> {
    code: &'a [u8],
}

/// How deeply [`Namespace::member_conditional`] will follow nested `If` and
/// `Else` blocks looking for a named object.
///
/// Four is past anything real: firmware nests these to pick a hardware
/// configuration, which takes one level and occasionally two. The limit is
/// here so a table that nests without bound cannot turn a lookup into an
/// unbounded recursion on a kernel stack.
const CONDITIONAL_DEPTH: u8 = 4;

/// The fixed 36-byte header every ACPI table starts with.
const TABLE_HEADER: usize = 36;

impl<'a> Namespace<'a> {
    /// Wraps a DSDT or SSDT, skipping its header.
    ///
    /// Returns `None` if the table is too short to hold one, or if its
    /// declared length does not fit — a table whose length field disagrees
    /// with the memory behind it is one this must not walk.
    pub fn new(table: &'a [u8]) -> Option<Namespace<'a>> {
        if table.len() < TABLE_HEADER {
            return None;
        }
        let declared = u32::from_le_bytes([table[4], table[5], table[6], table[7]]) as usize;
        if declared < TABLE_HEADER || declared > table.len() {
            return None;
        }
        Some(Namespace {
            code: &table[..declared],
        })
    }

    /// The bytecode, for resolving a [`Value`]'s ranges.
    pub fn code(&self) -> &'a [u8] {
        self.code
    }

    /// Finds the first device satisfying `wanted`.
    ///
    /// One pass, and it stops at the first match: a namespace is thousands of
    /// objects and there is exactly one touchpad.
    pub fn find_device(&self, wanted: impl Fn(&AcpiDevice) -> bool) -> Option<AcpiDevice> {
        let mut found = None;
        let mut scope = Path::root();
        self.walk(
            Span::new(TABLE_HEADER, self.code.len()),
            &mut scope,
            &mut |device| {
                if found.is_none() && wanted(&device) {
                    found = Some(device);
                }
                found.is_none()
            },
        );
        found
    }

    /// Visits every device, so a caller can count or list them.
    ///
    /// The callback returns whether to keep going.
    pub fn for_each_device(&self, mut visit: impl FnMut(AcpiDevice) -> bool) {
        let mut scope = Path::root();
        self.walk(
            Span::new(TABLE_HEADER, self.code.len()),
            &mut scope,
            &mut visit,
        );
    }

    /// Walks one term list, descending into scopes and devices.
    ///
    /// Everything that is not a scope, a device or a name is *skipped by its
    /// package length* rather than parsed. That is the whole trick: the
    /// encoding is self-delimiting, so an object this does not understand
    /// costs nothing to step over — which is what keeps a namespace walker
    /// from needing the whole language.
    fn walk(
        &self,
        span: Span,
        scope: &mut Path,
        visit: &mut impl FnMut(AcpiDevice) -> bool,
    ) -> bool {
        let mut cursor = Cursor::at(self.code, span.start);
        while cursor.at < span.end {
            let before = cursor.at;
            let Some(opcode) = cursor.byte() else {
                return true;
            };

            match opcode {
                op::SCOPE => {
                    let Some(body) = cursor.package() else {
                        return true;
                    };
                    let Some((name, absolute)) = cursor.name() else {
                        return true;
                    };
                    // A leading backslash means the name is given from the
                    // root, so it *replaces* the enclosing scope rather than
                    // extending it. Discarding that flag — which this did —
                    // turns `Scope (\_SB.PC00.LPCB.EC0)` written inside
                    // `\_SB.PC00.LPCB` into a nine-segment path, which is
                    // both wrong and long enough to hit `MAX_PATH`; the
                    // device inside it was then dropped for having no
                    // nameable path.
                    let saved = *scope;
                    let depth = if absolute {
                        *scope = Path::root();
                        0
                    } else {
                        scope.len()
                    };
                    for segment in name.segments() {
                        scope.push(*segment);
                    }
                    if !self.walk(Span::new(cursor.at, body.end), scope, visit) {
                        return false;
                    }
                    if absolute {
                        *scope = saved;
                    } else {
                        while scope.len() > depth {
                            scope.pop();
                        }
                    }
                    cursor.at = body.end;
                }

                op::EXT_PREFIX => {
                    let Some(extended) = cursor.byte() else {
                        return true;
                    };
                    match extended {
                        op::EXT_DEVICE
                        | op::EXT_POWER_RES
                        | op::EXT_THERMAL_ZONE
                        | op::EXT_PROCESSOR => {
                            let Some(body) = cursor.package() else {
                                return true;
                            };
                            let Some((name, absolute)) = cursor.name() else {
                                return true;
                            };
                            // Power resources and processors carry fixed
                            // fields between the name and the object list;
                            // devices do not.
                            let mut inner = cursor.at;
                            match extended {
                                op::EXT_POWER_RES => inner += 3,
                                op::EXT_PROCESSOR => inner += 6,
                                _ => {}
                            }
                            if inner > body.end {
                                return true;
                            }

                            // As above: an absolute name replaces the scope.
                            let saved = *scope;
                            let depth = if absolute {
                                *scope = Path::root();
                                0
                            } else {
                                scope.len()
                            };
                            let mut full = *scope;
                            let mut nameable = true;
                            for segment in name.segments() {
                                nameable &= scope.push(*segment);
                                full.push(*segment);
                            }

                            if extended == op::EXT_DEVICE && nameable {
                                let device = self.describe(*scope, Span::new(inner, body.end));
                                if !visit(device) {
                                    return false;
                                }
                            }
                            if !self.walk(Span::new(inner, body.end), scope, visit) {
                                return false;
                            }
                            if absolute {
                                *scope = saved;
                            } else {
                                while scope.len() > depth {
                                    scope.pop();
                                }
                            }
                            cursor.at = body.end;
                        }
                        // Everything else extended is a definition this does
                        // not need. Those that carry a package length are
                        // stepped over by it; the rest are fixed-size.
                        op::EXT_OP_REGION => {
                            if cursor.name().is_none() || !skip_region(&mut cursor) {
                                return true;
                            }
                        }
                        op::EXT_FIELD | op::EXT_INDEX_FIELD | op::EXT_BANK_FIELD => {
                            let Some(body) = cursor.package() else {
                                return true;
                            };
                            cursor.at = body.end;
                        }
                        op::EXT_MUTEX => {
                            if cursor.name().is_none() || cursor.byte().is_none() {
                                return true;
                            }
                        }
                        op::EXT_EVENT => {
                            if cursor.name().is_none() {
                                return true;
                            }
                        }
                        // `CreateField`: a buffer, a bit index, a bit length
                        // and a name.
                        op::EXT_CREATE_FIELD => {
                            if !skip_term_arg(&mut cursor)
                                || !skip_term_arg(&mut cursor)
                                || !skip_term_arg(&mut cursor)
                                || cursor.name().is_none()
                            {
                                return true;
                            }
                        }
                        _ => {
                            // An extended opcode with no length to step over.
                            // Continuing would be parsing from the middle of
                            // an object, so the walk of this list stops here
                            // rather than reporting nonsense.
                            return true;
                        }
                    }
                }

                op::METHOD => {
                    let Some(body) = cursor.package() else {
                        return true;
                    };
                    cursor.at = body.end;
                }

                op::NAME => {
                    if cursor.name().is_none() || !skip_data(&mut cursor) {
                        return true;
                    }
                }

                // `Alias` is two names, and `External` a name and two bytes.
                // Neither contains anything, but both appear at the top of
                // real tables — and an opcode this could not step over would
                // end the walk there.
                op::ALIAS => {
                    if cursor.name().is_none() || cursor.name().is_none() {
                        return true;
                    }
                }
                op::EXTERNAL => {
                    if cursor.name().is_none() || cursor.byte().is_none() || cursor.byte().is_none()
                    {
                        return true;
                    }
                }

                // The `CreateXField` family: two term arguments and a name.
                op::CREATE_DWORD_FIELD
                | op::CREATE_WORD_FIELD
                | op::CREATE_BYTE_FIELD
                | op::CREATE_BIT_FIELD
                | op::CREATE_QWORD_FIELD => {
                    if !skip_term_arg(&mut cursor)
                        || !skip_term_arg(&mut cursor)
                        || cursor.name().is_none()
                    {
                        return true;
                    }
                }

                // Control flow can appear in a term list, and the walk goes
                // *into* it rather than over it.
                //
                // An earlier version stepped over these, on the reasonable
                // sounding grounds that a device inside an `If` has a
                // presence depending on firmware state this cannot model.
                // Real firmware settles the argument: one SSDT on the machine
                // this was developed against is thirty kilobytes containing
                // two top-level `If` blocks and a hundred and eight devices,
                // every one of them inside. Skipping the conditionals found
                // none of them. A device reported that a predicate would have
                // excluded is a far smaller error than a hundred devices not
                // reported at all — and `_STA`, which is what actually says
                // whether a device is there, is still consulted.
                //
                // The predicate has to be stepped over before the body: it is
                // a term argument, and it may be a method call, so the same
                // walk everything else uses does it.
                op::IF | op::ELSE | op::WHILE => {
                    let Some(package) = cursor.package() else {
                        return true;
                    };
                    let inner = if opcode == op::ELSE {
                        cursor.at
                    } else {
                        let mut probe = Cursor::at(self.code, cursor.at);
                        if !skip_term_arg(&mut probe) || probe.at > package.end {
                            cursor.at = package.end;
                            continue;
                        }
                        probe.at
                    };
                    if !self.walk(Span::new(inner, package.end), scope, visit) {
                        return false;
                    }
                    cursor.at = package.end;
                }

                op::NOOP => {}

                _ => {
                    // A statement. Stepped over rather than stopped at: the
                    // walk goes inside conditionals now, and their bodies are
                    // full of these with declarations after them.
                    if !skip_statement(&mut cursor, opcode) {
                        // An opcode with no known shape. Continuing would be
                        // parsing from the middle of an object, so this ends
                        // *this* term list rather than desynchronising — the
                        // caller continues after the enclosing package, so an
                        // unparseable region costs its own scope and nothing
                        // outside it.
                        let _ = before;
                        return true;
                    }
                }
            }
        }
        true
    }

    /// Reads a device's `_HID` and `_CID` out of its object list.
    fn describe(&self, path: Path, body: Span) -> AcpiDevice {
        AcpiDevice {
            path,
            hid: self.identifier(&body, b"_HID"),
            cid: self.identifier(&body, b"_CID"),
            body,
        }
    }

    /// Reads one identifier-valued `Name` from an object list.
    fn identifier(&self, body: &Span, want: &[u8; 4]) -> Option<HardwareId> {
        let value = self.name_value(body, want)?;
        match value {
            Value::Integer(compressed) => Some(HardwareId::from_eisa(compressed as u32)),
            Value::Text(span) => Some(HardwareId::from_text(span.of(self.code)?)),
            // A `_CID` may be a package of several. The first is the most
            // specific, which is the one worth reporting.
            Value::Package(span) => {
                let mut cursor = Cursor::at(self.code, span.start);
                // The element count precedes the list.
                cursor.byte()?;
                match parse_data(&mut cursor)? {
                    Value::Integer(compressed) => Some(HardwareId::from_eisa(compressed as u32)),
                    Value::Text(text) => Some(HardwareId::from_text(text.of(self.code)?)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Finds `Name(<want>, ...)` directly inside an object list.
    ///
    /// Not recursive: a device's `_HID` is its own, and one belonging to a
    /// child device would name the child.
    fn name_value(&self, body: &Span, want: &[u8; 4]) -> Option<Value> {
        let mut cursor = Cursor::at(self.code, body.start);
        while cursor.at < body.end {
            let opcode = cursor.byte()?;
            if opcode == op::NAME {
                let (name, _) = cursor.name()?;
                let matched = name.leaf() == Some(*want);
                let value = parse_data_value(&mut cursor)?;
                if matched {
                    return Some(value);
                }
                continue;
            }
            if !skip_object(&mut cursor, opcode) {
                return None;
            }
        }
        None
    }

    /// Locates a method or name inside a device, by its four-character name.
    /// Finds a named object inside a device, descending into conditionals.
    ///
    /// Returns the member and whether it was found inside an `If` or `Else`
    /// rather than at the device's top level. Callers need that distinction:
    /// see [`Namespace::present`].
    ///
    /// # Why conditionals have to be searched at all
    ///
    /// Because firmware puts the answers in them. An Intel LPSS controller is
    /// declared once and configured two ways, and which objects it has
    /// depends on a variable set before the OS ever runs:
    ///
    /// ```asl
    /// Device (I2C5)
    /// {
    ///     If (LEqual (IM05, 0x02)) {            // the ACPI-enumerated form
    ///         Method (_CRS, 0) { Return (I2CH (IC05)) }
    ///         Name (_STA, 0x08)
    ///     }
    ///     If (LOr (LEqual (IM05, One), LEqual (IM05, Zero))) {   // the PCI form
    ///         Method (_ADR, 0) { Return (0x00190001) }
    ///     }
    /// }
    /// ```
    ///
    /// The `_ADR` that says where the controller sits on the PCI bus exists
    /// only inside the second branch. A reader that steps over `If` — which
    /// this one did, on the reasonable-sounding grounds that a conditional
    /// object's presence depends on firmware state it does not model — finds
    /// the device, finds nothing in it, and reports a controller with no
    /// address.
    ///
    /// # And why the distinction is kept
    ///
    /// Because the same example shows the danger. The *first* branch declares
    /// `Name (_STA, 0x08)` — bit 0 clear, meaning **not present**. On a
    /// machine in PCI mode that branch does not apply, but nothing here can
    /// evaluate `IM05` to know that. Reading that `_STA` and believing it
    /// would discard a controller that is working. So a conditional answer is
    /// good enough for data (`_ADR`, `_CRS`, `_HID`) and not good enough for
    /// a claim about whether the device exists.
    fn member_conditional(&self, device: &AcpiDevice, want: &[u8; 4]) -> Option<(Member, bool)> {
        self.find_member(device.body, want, CONDITIONAL_DEPTH)
    }

    fn member(&self, device: &AcpiDevice, want: &[u8; 4]) -> Option<Member> {
        self.member_conditional(device, want)
            .map(|(member, _)| member)
    }

    /// One level of [`member_conditional`]'s search: this term list, then the
    /// conditionals in it.
    fn find_member(&self, span: Span, want: &[u8; 4], depth: u8) -> Option<(Member, bool)> {
        if let Some(member) = self.member_at_level(span, want) {
            return Some((member, false));
        }
        if depth == 0 {
            return None;
        }

        let mut cursor = Cursor::at(self.code, span.start);
        while cursor.at < span.end {
            let opcode = cursor.byte()?;
            match opcode {
                op::IF | op::ELSE => {
                    let package = cursor.package()?;
                    // An `If`'s body starts after its predicate; an `Else`
                    // has none. Stepping over the predicate needs the same
                    // term-argument walk everything else uses, method calls
                    // and all.
                    let inner = if opcode == op::IF {
                        let mut probe = Cursor::at(self.code, cursor.at);
                        if !skip_term_arg(&mut probe) || probe.at > package.end {
                            cursor.at = package.end;
                            continue;
                        }
                        probe.at
                    } else {
                        cursor.at
                    };
                    if let Some((member, _)) =
                        self.find_member(Span::new(inner, package.end), want, depth - 1)
                    {
                        return Some((member, true));
                    }
                    cursor.at = package.end;
                }
                op::NAME => {
                    cursor.name()?;
                    if !skip_data(&mut cursor) {
                        return None;
                    }
                }
                _ => {
                    if !skip_object(&mut cursor, opcode) && !skip_statement(&mut cursor, opcode) {
                        return None;
                    }
                }
            }
        }
        None
    }

    /// Looks for a named object in one term list, without descending.
    fn member_at_level(&self, span: Span, want: &[u8; 4]) -> Option<Member> {
        let mut cursor = Cursor::at(self.code, span.start);
        while cursor.at < span.end {
            let opcode = cursor.byte()?;
            match opcode {
                op::NAME => {
                    let (name, _) = cursor.name()?;
                    let start = cursor.at;
                    let matched = name.leaf() == Some(*want);
                    if !skip_data(&mut cursor) {
                        return None;
                    }
                    if matched {
                        return Some(Member::Name(start));
                    }
                }
                op::METHOD => {
                    let package = cursor.package()?;
                    let (name, _) = cursor.name()?;
                    let flags = cursor.byte()?;
                    if name.leaf() == Some(*want) {
                        return Some(Member::Method {
                            body: Span::new(cursor.at, package.end),
                            arguments: (flags & 0x07) as usize,
                        });
                    }
                    cursor.at = package.end;
                }
                _ => {
                    if !skip_object(&mut cursor, opcode) {
                        return None;
                    }
                }
            }
        }
        None
    }
}

/// Where a device's named object lives.
#[derive(Debug, Clone, Copy)]
enum Member {
    /// A `Name`, with the offset of its data.
    Name(usize),
    /// A `Method`, with its body and how many arguments it takes.
    Method { body: Span, arguments: usize },
}

/// Steps over an `OpRegion`'s region space, offset and length.
fn skip_region(cursor: &mut Cursor<'_>) -> bool {
    // RegionSpace is one byte; the offset and length are TermArgs.
    //
    // They are *usually* integer constants, and an earlier version of this
    // skipped them as plain data on that assumption. Real firmware disagrees:
    //
    // ```asl
    // OperationRegion (SANV, SystemMemory, SANB, SANL)
    // ```
    //
    // — where `SANB` and `SANL` are names defined a few lines above. A
    // NameString is a perfectly ordinary TermArg, and reading one as data
    // consumed the wrong number of bytes and desynchronised the walk from
    // that point on. Every device in the table came after it.
    cursor.byte().is_some() && skip_term_arg(cursor) && skip_term_arg(cursor)
}

/// Steps over one data object without interpreting it.
fn skip_data(cursor: &mut Cursor<'_>) -> bool {
    parse_data(cursor).is_some()
}

/// Steps over one term argument.
///
/// Wider than [`skip_data`]: a term argument may also be a name, an argument
/// or a local. Anything beyond that — an expression — is refused, because
/// stepping over one needs to know its operand count and getting that wrong
/// resumes parsing in the middle of an object.
fn skip_term_arg(cursor: &mut Cursor<'_>) -> bool {
    let Some(opcode) = cursor.peek() else {
        return false;
    };
    match opcode {
        op::LOCAL0..=op::LOCAL7 | op::ARG0..=op::ARG6 => {
            cursor.at += 1;
            true
        }

        // An extended expression: the two-byte opcodes. `CondRefOf` is the
        // one that matters, because firmware writes whole conditional blocks
        // as `If (CondRefOf (\\DTFS))` — one SSDT on the machine this was
        // developed against wraps a hundred and eight devices in exactly
        // that, and a predicate this could not step over hid every one.
        op::EXT_PREFIX => {
            let Some(extended) = cursor.code.get(cursor.at + 1).copied() else {
                return false;
            };
            let Some((args, target)) = extended_arity(extended) else {
                return false;
            };
            cursor.at += 2;
            for _ in 0..args {
                if !skip_term_arg(cursor) {
                    return false;
                }
            }
            // `Acquire` ends in a sixteen-bit timeout rather than a target.
            if extended == op::EXT_ACQUIRE {
                return cursor.byte().is_some() && cursor.byte().is_some();
            }
            !target || skip_target(cursor)
        }

        // An expression. A TermArg is allowed to be one, and firmware takes
        // the offer — this table declares a region as
        //
        // ```asl
        // OperationRegion (CPWR, SystemMemory, Add (\_SB.PC00.GMHB, 0x5000), 0x1000)
        // ```
        //
        // Stepping over one needs its shape but not its meaning, and the
        // shapes fall into three groups: how many term arguments, and whether
        // a target name follows them. Getting the count right is all that
        // matters; a wrong one desynchronises everything after it.
        _ if arity(opcode).is_some() => {
            cursor.at += 1;
            let (args, target) = arity(opcode).unwrap_or((0, false));
            for _ in 0..args {
                if !skip_term_arg(cursor) {
                    return false;
                }
            }
            !target || skip_target(cursor)
        }

        // A name string begins with a root or parent marker, a prefix, or a
        // leading name character. It comes last: a bare name is what is left
        // once every opcode has been ruled out.
        //
        // And it may be a *call*, which looks identical — see
        // [`method_arity`]. A name that names a method is followed by that
        // method's arguments, and they have to be stepped over too or the
        // parse continues from the middle of one.
        b'\\' | b'^' | b'A'..=b'Z' | b'_' | 0x2E | 0x2F => {
            let code = cursor.code;
            let Some((name, _)) = cursor.name() else {
                return false;
            };
            let arity = name.leaf().and_then(|leaf| method_arity(code, leaf));
            for _ in 0..arity.unwrap_or(0) {
                if !skip_term_arg(cursor) {
                    return false;
                }
            }
            true
        }
        _ => skip_data(cursor),
    }
}

/// How many term arguments an expression opcode takes, and whether a target
/// follows them.
///
/// `None` for anything that is not an expression, which is what makes the
/// arm in [`skip_term_arg`] able to test for one.
fn arity(opcode: u8) -> Option<(u8, bool)> {
    Some(match opcode {
        // Two arguments and a place to put the answer.
        op::ADD
        | op::SUBTRACT
        | op::MULTIPLY
        | op::SHIFT_LEFT
        | op::SHIFT_RIGHT
        | op::AND
        | op::NAND
        | op::OR
        | op::NOR
        | op::XOR
        | op::CONCAT
        | op::INDEX => (2, true),
        // Two arguments, no target: the logical operators yield a value.
        op::LAND | op::LOR | op::LEQUAL | op::LGREATER | op::LLESS => (2, false),
        // One argument and a target.
        op::TO_BUFFER
        | op::TO_INTEGER
        | op::TO_HEX_STRING
        | op::TO_DECIMAL_STRING
        | op::FIND_SET_LEFT_BIT
        | op::FIND_SET_RIGHT_BIT
        | op::STORE => (1, true),
        // One argument, no target.
        op::LNOT | op::SIZE_OF | op::DEREF_OF | op::REF_OF | op::INCREMENT | op::DECREMENT => {
            (1, false)
        }
        _ => return None,
    })
}

/// Steps over one object in a term list, given its opcode.
///
/// Everything except `Name`, which every caller wants the value of and so
/// handles itself. Containers — `Scope`, `Device`, and the rest — are stepped
/// *over* rather than descended into; a caller that wants to go inside one
/// does so before calling this.
///
/// This exists because there were two copies of it. The namespace walk had
/// one and the identifier lookup had another, and the second was missing five
/// opcodes the first handled. The consequence was silent and specific: a
/// device whose body opened with
///
/// ```asl
/// CreateWordField (SBFB, 0x11, I2CN)
/// Name (_HID, "XXXX0000")
/// Name (_CID, "PNP0C50")
/// ```
///
/// was found and reported as present with no identifier at all, because the
/// lookup gave up at the `CreateWordField` and never reached the two names
/// after it. One copy cannot drift from itself.
///
/// Returns false when the opcode carries no length to step over, which ends
/// the caller's term list rather than desynchronising it.
fn skip_object(cursor: &mut Cursor<'_>, opcode: u8) -> bool {
    match opcode {
        op::METHOD | op::SCOPE | op::IF | op::ELSE | op::WHILE => match cursor.package() {
            Some(package) => {
                cursor.at = package.end;
                true
            }
            None => false,
        },

        op::ALIAS => cursor.name().is_some() && cursor.name().is_some(),
        op::EXTERNAL => {
            cursor.name().is_some() && cursor.byte().is_some() && cursor.byte().is_some()
        }

        op::CREATE_DWORD_FIELD
        | op::CREATE_WORD_FIELD
        | op::CREATE_BYTE_FIELD
        | op::CREATE_BIT_FIELD
        | op::CREATE_QWORD_FIELD => {
            skip_term_arg(cursor) && skip_term_arg(cursor) && cursor.name().is_some()
        }

        op::NOOP => true,

        op::EXT_PREFIX => {
            let Some(extended) = cursor.byte() else {
                return false;
            };
            match extended {
                op::EXT_DEVICE
                | op::EXT_POWER_RES
                | op::EXT_THERMAL_ZONE
                | op::EXT_PROCESSOR
                | op::EXT_FIELD
                | op::EXT_INDEX_FIELD
                | op::EXT_BANK_FIELD => match cursor.package() {
                    Some(package) => {
                        cursor.at = package.end;
                        true
                    }
                    None => false,
                },
                op::EXT_OP_REGION => cursor.name().is_some() && skip_region(cursor),
                op::EXT_MUTEX => cursor.name().is_some() && cursor.byte().is_some(),
                op::EXT_EVENT => cursor.name().is_some(),
                op::EXT_CREATE_FIELD => {
                    skip_term_arg(cursor)
                        && skip_term_arg(cursor)
                        && skip_term_arg(cursor)
                        && cursor.name().is_some()
                }
                _ => false,
            }
        }

        _ => false,
    }
}

/// How many arguments the method with this leaf name declares, if the table
/// declares one.
///
/// AML gives a method call no syntax of its own. `I2CM (I2CX, BADR, SPED)`
/// assembles to the name `I2CM` followed immediately by three term arguments
/// and nothing to mark where they end — so a reader that does not already
/// know `I2CM` takes three cannot tell the call from a bare reference, and
/// stops the parse dead at the first one. Real firmware is full of them:
///
/// ```asl
/// Method (_CRS, 0, NotSerialized)
/// {
///     Return (ConcatenateResTemplate (I2CM (I2CX, BADR, SPED), SBFG))
/// }
/// ```
///
/// A real interpreter answers this by building the whole namespace up front
/// and looking the name up in it. That is more machinery than is wanted here,
/// and a table of every method in a 600 KiB DSDT is tens of kilobytes that a
/// kernel with a small stack should not be holding. So the answer is looked
/// up when it is needed instead: a linear scan for a `Method` declaration
/// with this leaf name. Calls appear only inside the handful of objects this
/// reader evaluates, so the scan runs a few times per boot rather than once
/// per opcode.
///
/// Names are matched on their last segment only, because that is all a call
/// site necessarily carries. Two methods in different scopes may share a leaf
/// and declare different arities; when the scan finds a disagreement it
/// returns `None`, so an ambiguous name is treated as a plain reference
/// rather than as a call with a guessed argument count. Guessing there would
/// desynchronise the parse, which is the failure this exists to prevent.
///
/// The scan is validated rather than trusted: a candidate `0x14` is only
/// believed if a package length, a name and a flags byte follow it and the
/// package fits the table. A byte of data that happens to be `0x14` will
/// almost never satisfy all three, and one that does costs an arity for a
/// name nothing calls.
fn method_arity(code: &[u8], leaf: NameSeg) -> Option<u8> {
    let mut found: Option<u8> = None;
    let mut at = TABLE_HEADER;

    while at < code.len() {
        if code[at] != op::METHOD {
            at += 1;
            continue;
        }
        at += 1;

        let mut cursor = Cursor::at(code, at);
        let Some(package) = cursor.package() else {
            continue;
        };
        if package.end > code.len() || package.end <= cursor.at {
            continue;
        }
        let Some((name, _)) = cursor.name() else {
            continue;
        };
        let Some(flags) = cursor.byte() else {
            continue;
        };
        if cursor.at > package.end || name.leaf() != Some(leaf) {
            continue;
        }

        // Bits 2:0 are the argument count; the rest is the sync level and
        // the serialised flag, neither of which changes the call's shape.
        let args = flags & 0x07;
        match found {
            None => found = Some(args),
            Some(seen) if seen == args => {}
            // Two methods, same leaf, different arities. There is no way to
            // tell which a call site meant, so neither is offered.
            Some(_) => return None,
        }
    }
    found
}

/// How many term arguments a two-byte opcode takes, and whether a target
/// follows.
///
/// `None` for anything that is not an expression — a `Device`, a `Field`, a
/// `Mutex` — which is what lets [`skip_term_arg`] tell "this is an extended
/// expression" from "this is a definition and has no business here".
fn extended_arity(extended: u8) -> Option<(u8, bool)> {
    Some(match extended {
        // A name and somewhere to put the reference to it.
        op::EXT_COND_REF_OF => (1, true),
        // One argument and a target.
        op::EXT_FROM_BCD | op::EXT_TO_BCD => (1, true),
        // A mutex and a timeout; the timeout is two raw bytes, not a term
        // argument, so the caller finishes this one itself.
        op::EXT_ACQUIRE => (1, false),
        // An event and a term argument.
        op::EXT_WAIT => (2, false),
        // Six arguments and a target.
        op::EXT_LOAD_TABLE => (6, true),
        // Nullary: they name a value rather than compute one.
        op::EXT_REVISION | op::EXT_DEBUG | op::EXT_TIMER => (0, false),
        _ => return None,
    })
}

/// Steps over a statement — something a method does rather than declares.
///
/// The opcode has already been consumed by the caller.
///
/// This became necessary the moment the namespace walk started descending
/// into `If` blocks. A term list at the top of a table holds definitions and
/// nothing else; a term list inside a conditional holds whatever the firmware
/// author wrote, `Store` and `Notify` and `Release` alongside the `Device`
/// declarations. The walk used to end its list at the first of those, on the
/// grounds that a statement carries no length to step over — true of the
/// opcode alone, and not true once its operands are understood. Ending there
/// lost every declaration after it: one `Store` in the middle of a
/// conditional hid the `Device (PXSX)` that followed.
///
/// Returns false for an opcode with no known shape, which puts the caller
/// back where it was: end this list rather than desynchronise.
fn skip_statement(cursor: &mut Cursor<'_>, opcode: u8) -> bool {
    match opcode {
        op::BREAK | op::NOOP | op::CONTINUE => true,
        op::RETURN => skip_term_arg(cursor),
        op::NOTIFY => skip_term_arg(cursor) && skip_term_arg(cursor),
        op::EXT_PREFIX => {
            let Some(extended) = cursor.byte() else {
                return false;
            };
            match extended {
                // One operand: an object to act on, or a duration.
                op::EXT_STALL
                | op::EXT_SLEEP
                | op::EXT_SIGNAL
                | op::EXT_RESET
                | op::EXT_RELEASE
                | op::EXT_UNLOAD => skip_term_arg(cursor),
                // A table to load, and somewhere to put its handle.
                op::EXT_LOAD => skip_term_arg(cursor) && skip_term_arg(cursor),
                // A type byte, a 32-bit code, and an argument.
                op::EXT_FATAL => {
                    if cursor.byte().is_none() {
                        return false;
                    }
                    for _ in 0..4 {
                        if cursor.byte().is_none() {
                            return false;
                        }
                    }
                    skip_term_arg(cursor)
                }
                _ => false,
            }
        }
        _ => {
            // Everything else with a known shape: the assignments and the
            // arithmetic, which are statements when their result is thrown
            // away and expressions when it is not. The shape is the same.
            let Some((args, target)) = arity(opcode) else {
                return false;
            };
            for _ in 0..args {
                if !skip_term_arg(cursor) {
                    return false;
                }
            }
            !target || skip_target(cursor)
        }
    }
}

/// Steps over a Target: either a name to store into, or `Zero` meaning/// Steps over a Target: either a name to store into, or `Zero` meaning
/// "discard the result", which is how firmware writes an expression used only
/// for its value.
fn skip_target(cursor: &mut Cursor<'_>) -> bool {
    match cursor.peek() {
        None => false,
        Some(op::ZERO) => {
            cursor.at += 1;
            true
        }
        Some(op::LOCAL0..=op::LOCAL7) | Some(op::ARG0..=op::ARG6) => {
            cursor.at += 1;
            true
        }
        Some(_) => skip_term_arg(cursor),
    }
}

/// The same, keeping the value.
fn parse_data_value(cursor: &mut Cursor<'_>) -> Option<Value> {
    parse_data(cursor)
}

/// Reads one DataObject: a constant, a buffer, a string or a package.
fn parse_data(cursor: &mut Cursor<'_>) -> Option<Value> {
    let opcode = cursor.byte()?;
    match opcode {
        op::ZERO => Some(Value::Integer(0)),
        op::ONE => Some(Value::Integer(1)),
        op::ONES => Some(Value::Integer(u64::MAX)),
        op::BYTE_PREFIX => cursor.integer(1).map(Value::Integer),
        op::WORD_PREFIX => cursor.integer(2).map(Value::Integer),
        op::DWORD_PREFIX => cursor.integer(4).map(Value::Integer),
        op::QWORD_PREFIX => cursor.integer(8).map(Value::Integer),
        op::STRING_PREFIX => {
            let start = cursor.at;
            loop {
                match cursor.byte()? {
                    0 => break,
                    _ => continue,
                }
            }
            Some(Value::Text(Span::new(start, cursor.at - 1)))
        }
        op::BUFFER => {
            let package = cursor.package()?;
            // The declared size is a TermArg; the bytes are whatever is left
            // of the package after it. The *package* is authoritative, which
            // is what makes a buffer whose declared size is a method call
            // readable anyway.
            skip_data(cursor).then_some(())?;
            let contents = Span::new(cursor.at, package.end);
            // Past the contents, not into them. Leaving the cursor at the
            // first byte of the buffer makes the next read parse a resource
            // template as opcodes, and every object after it is lost — which
            // is how a `_DSM` sitting after a `_CRS` becomes invisible.
            cursor.at = package.end;
            Some(Value::Buffer(contents))
        }
        op::PACKAGE | op::VAR_PACKAGE => {
            let package = cursor.package()?;
            let start = cursor.at;
            cursor.at = package.end;
            Some(Value::Package(Span::new(start, package.end)))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The interpreter
// ---------------------------------------------------------------------------

/// What a statement did.
#[derive(Debug, Clone, Copy)]
enum Flow {
    Continue,
    Return(Value),
    Break,
}

/// Runs the subset of AML that device methods are written in.
///
/// Deliberately small. The methods this has to execute — `_CRS` when it is a
/// method rather than a name, `_DSM`, `_STA` — are written to a recognisable
/// shape: compare an argument, return a constant or a buffer. Anything
/// outside that shape, and in particular anything that reads an operation
/// region (which would mean talking to an embedded controller), makes
/// evaluation fail rather than return a plausible number.
///
/// That failure is the point. A `_DSM` this misread would hand back a wrong
/// register address, and a wrong address on an I2C bus is not a wrong answer
/// — it is a transaction with whatever else is on that bus.
#[derive(Debug)]
pub struct Interpreter<'a> {
    code: &'a [u8],
    args: [Value; 7],
    locals: [Value; 8],
    /// Bounds the work. A `While` whose condition this evaluates wrongly
    /// would otherwise spin forever in a kernel with no way out.
    budget: u32,
}

/// How many opcodes one evaluation may execute.
///
/// Generous for the methods this runs — a `_DSM` is tens of opcodes — and far
/// below anything that would be a visible pause.
const BUDGET: u32 = 200_000;

impl<'a> Interpreter<'a> {
    pub fn new(code: &'a [u8]) -> Interpreter<'a> {
        Interpreter {
            code,
            args: [Value::Uninitialised; 7],
            locals: [Value::Uninitialised; 8],
            budget: BUDGET,
        }
    }

    /// Sets the arguments a method will see as `Arg0`..`Arg6`.
    pub fn with_args(mut self, args: &[Value]) -> Interpreter<'a> {
        for (slot, value) in self.args.iter_mut().zip(args) {
            *slot = *value;
        }
        self
    }

    fn spend(&mut self) -> Option<()> {
        self.budget = self.budget.checked_sub(1)?;
        Some(())
    }

    /// Runs a term list, returning what it returned.
    fn run(&mut self, span: Span) -> Option<Flow> {
        let mut cursor = Cursor::at(self.code, span.start);
        while cursor.at < span.end {
            self.spend()?;
            match self.statement(&mut cursor, span.end)? {
                Flow::Continue => {}
                other => return Some(other),
            }
        }
        Some(Flow::Continue)
    }

    /// Executes one statement.
    fn statement(&mut self, cursor: &mut Cursor<'a>, end: usize) -> Option<Flow> {
        let opcode = cursor.peek()?;
        match opcode {
            op::IF => {
                cursor.at += 1;
                let package = cursor.package()?;
                let condition = self.eval(cursor)?;
                let taken = truthy(&condition);
                let body = Span::new(cursor.at, package.end);
                cursor.at = package.end;

                // An `Else` may follow, and it is a separate opcode with its
                // own package rather than part of the `If`.
                let mut otherwise = None;
                if cursor.at < end && cursor.peek() == Some(op::ELSE) {
                    cursor.at += 1;
                    let alternative = cursor.package()?;
                    otherwise = Some(Span::new(cursor.at, alternative.end));
                    cursor.at = alternative.end;
                }

                if taken {
                    self.run(body)
                } else if let Some(alternative) = otherwise {
                    self.run(alternative)
                } else {
                    Some(Flow::Continue)
                }
            }

            op::ELSE => {
                // An `Else` with no `If` before it: the bytecode is not what
                // this thinks it is, so stop rather than execute the body.
                None
            }

            op::WHILE => {
                cursor.at += 1;
                let package = cursor.package()?;
                let test = cursor.at;
                loop {
                    self.spend()?;
                    let mut condition = Cursor::at(self.code, test);
                    if !truthy(&self.eval(&mut condition)?) {
                        break;
                    }
                    match self.run(Span::new(condition.at, package.end))? {
                        Flow::Continue => {}
                        Flow::Break => break,
                        Flow::Return(value) => return Some(Flow::Return(value)),
                    }
                }
                cursor.at = package.end;
                Some(Flow::Continue)
            }

            op::RETURN => {
                cursor.at += 1;
                Some(Flow::Return(self.eval(cursor)?))
            }

            op::BREAK => {
                cursor.at += 1;
                Some(Flow::Break)
            }

            op::NOOP => {
                cursor.at += 1;
                Some(Flow::Continue)
            }

            // A `Name` inside a method body is a local definition. Nothing
            // this evaluates reads one back, so it is stepped over.
            op::NAME => {
                cursor.at += 1;
                cursor.name()?;
                skip_data(cursor).then_some(Flow::Continue)
            }

            // Anything else is an expression evaluated for its effect —
            // `Store`, an increment, a method call. Its value is discarded.
            _ => {
                self.eval(cursor)?;
                Some(Flow::Continue)
            }
        }
    }

    /// Evaluates one TermArg.
    fn eval(&mut self, cursor: &mut Cursor<'a>) -> Option<Value> {
        self.spend()?;
        let opcode = cursor.peek()?;

        // Constants and containers.
        if matches!(
            opcode,
            op::ZERO
                | op::ONE
                | op::ONES
                | op::BYTE_PREFIX
                | op::WORD_PREFIX
                | op::DWORD_PREFIX
                | op::QWORD_PREFIX
                | op::STRING_PREFIX
                | op::BUFFER
                | op::PACKAGE
                | op::VAR_PACKAGE
        ) {
            return parse_data(cursor);
        }

        cursor.at += 1;
        match opcode {
            op::ARG0..=op::ARG6 => Some(self.args[(opcode - op::ARG0) as usize]),
            op::LOCAL0..=op::LOCAL7 => Some(self.locals[(opcode - op::LOCAL0) as usize]),

            op::STORE => {
                let value = self.eval(cursor)?;
                self.store(cursor, value)?;
                Some(value)
            }

            op::LNOT => {
                let value = self.eval(cursor)?;
                Some(boolean(!truthy(&value)))
            }
            op::LAND => {
                let left = self.eval(cursor)?;
                let right = self.eval(cursor)?;
                Some(boolean(truthy(&left) && truthy(&right)))
            }
            op::LOR => {
                let left = self.eval(cursor)?;
                let right = self.eval(cursor)?;
                Some(boolean(truthy(&left) || truthy(&right)))
            }
            op::LEQUAL => {
                let left = self.eval(cursor)?;
                let right = self.eval(cursor)?;
                Some(boolean(self.equal(&left, &right)))
            }
            op::LGREATER => {
                let left = self.eval(cursor)?.integer()?;
                let right = self.eval(cursor)?.integer()?;
                Some(boolean(left > right))
            }
            op::LLESS => {
                let left = self.eval(cursor)?.integer()?;
                let right = self.eval(cursor)?.integer()?;
                Some(boolean(left < right))
            }

            op::ADD
            | op::SUBTRACT
            | op::AND
            | op::OR
            | op::XOR
            | op::NAND
            | op::NOR
            | op::SHIFT_LEFT
            | op::SHIFT_RIGHT => {
                let left = self.eval(cursor)?.integer()?;
                let right = self.eval(cursor)?.integer()?;
                let value = match opcode {
                    op::ADD => left.wrapping_add(right),
                    op::SUBTRACT => left.wrapping_sub(right),
                    op::AND => left & right,
                    op::NAND => !(left & right),
                    op::OR => left | right,
                    op::NOR => !(left | right),
                    op::XOR => left ^ right,
                    op::SHIFT_LEFT => left.checked_shl(right as u32).unwrap_or(0),
                    _ => left.checked_shr(right as u32).unwrap_or(0),
                };
                // Each of these takes an optional target as its third operand.
                self.store(cursor, Value::Integer(value))?;
                Some(Value::Integer(value))
            }

            op::INCREMENT | op::DECREMENT => {
                let mut probe = *cursor;
                let current = self.eval(&mut probe)?.integer()?;
                let value = if opcode == op::INCREMENT {
                    current.wrapping_add(1)
                } else {
                    current.wrapping_sub(1)
                };
                self.store(cursor, Value::Integer(value))?;
                Some(Value::Integer(value))
            }

            op::TO_INTEGER => {
                let value = self.eval(cursor)?;
                let integer = match value {
                    Value::Integer(v) => v,
                    Value::Buffer(span) => {
                        let bytes = span.of(self.code)?;
                        let mut out = 0u64;
                        for (i, &byte) in bytes.iter().take(8).enumerate() {
                            out |= (byte as u64) << (8 * i);
                        }
                        out
                    }
                    _ => return None,
                };
                self.store(cursor, Value::Integer(integer))?;
                Some(Value::Integer(integer))
            }

            op::TO_BUFFER => {
                let value = self.eval(cursor)?;
                self.store(cursor, value)?;
                Some(value)
            }

            op::SIZE_OF => {
                let value = self.eval(cursor)?;
                match value {
                    Value::Buffer(span) | Value::Text(span) => {
                        Some(Value::Integer(span.len() as u64))
                    }
                    _ => None,
                }
            }

            // `DerefOf` and `Index` reach into packages. Nothing this runs
            // needs to, and implementing them halfway would mean returning
            // the wrong element rather than no element.
            op::DEREF_OF | op::INDEX => None,

            _ => None,
        }
    }

    /// Writes to a target: a local, an argument, or nowhere.
    ///
    /// The "nowhere" case is deliberate. A `Store` whose destination is a
    /// named object outside this method changes state this does not model,
    /// and the value is discarded — but the *statement* succeeds, because a
    /// `_DSM` that caches its answer in a global is still a `_DSM` whose
    /// `Return` is correct. A destination that is neither a local, an
    /// argument nor a name is refused.
    fn store(&mut self, cursor: &mut Cursor<'a>, value: Value) -> Option<()> {
        let opcode = cursor.peek()?;
        match opcode {
            // ZeroOp as a target means "discard", which is how the optional
            // third operand of an arithmetic op is encoded when unused.
            op::ZERO => {
                cursor.at += 1;
                Some(())
            }
            op::LOCAL0..=op::LOCAL7 => {
                cursor.at += 1;
                self.locals[(opcode - op::LOCAL0) as usize] = value;
                Some(())
            }
            op::ARG0..=op::ARG6 => {
                cursor.at += 1;
                self.args[(opcode - op::ARG0) as usize] = value;
                Some(())
            }
            // A name: stepped over, its write not modelled.
            b'\\' | b'^' | b'A'..=b'Z' | b'_' | 0x2E | 0x2F => {
                cursor.name()?;
                Some(())
            }
            _ => None,
        }
    }

    /// Compares two values the way `LEqual` does.
    fn equal(&self, left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Integer(a), Value::Integer(b)) => a == b,
            // Buffers compare by content, which is how a `_DSM` recognises
            // its UUID: `LEqual(Arg0, ToUUID("3cdff6f7-..."))` is a
            // sixteen-byte memcmp and nothing else.
            (Value::Buffer(a), Value::Buffer(b)) | (Value::Text(a), Value::Text(b)) => {
                match (a.of(self.code), b.of(self.code)) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Integer(v) => *v != 0,
        Value::Uninitialised => false,
        _ => true,
    }
}

fn boolean(value: bool) -> Value {
    Value::Integer(if value { u64::MAX } else { 0 })
}

impl<'a> Namespace<'a> {
    /// Evaluates one of a device's members by name.
    ///
    /// A `Name` is read directly; a `Method` is executed with `args`. Returns
    /// `None` when the member is absent, when it takes a different number of
    /// arguments than were supplied, or when its body uses something outside
    /// the supported subset.
    pub fn evaluate(&self, device: &AcpiDevice, name: &[u8; 4], args: &[Value]) -> Option<Value> {
        self.evaluate_member_with(self.member(device, name)?, args)
    }

    /// Reads a member that takes no arguments.
    fn evaluate_member(&self, member: Member) -> Option<Value> {
        self.evaluate_member_with(member, &[])
    }

    fn evaluate_member_with(&self, member: Member, args: &[Value]) -> Option<Value> {
        match member {
            Member::Name(at) => {
                let mut cursor = Cursor::at(self.code, at);
                parse_data(&mut cursor)
            }
            Member::Method { body, arguments } => {
                if arguments != args.len() {
                    return None;
                }
                let mut interpreter = Interpreter::new(self.code).with_args(args);
                match interpreter.run(body)? {
                    Flow::Return(value) => Some(value),
                    _ => None,
                }
            }
        }
    }

    /// Whether the device reports itself present and enabled.
    ///
    /// A missing `_STA` means present — that is what the specification says,
    /// and it is the common case. An `_STA` this cannot evaluate is also
    /// treated as present: refusing to drive a device because its status
    /// method was too complicated to read would be a worse failure than
    /// trying and getting no answer on the bus.
    pub fn present(&self, device: &AcpiDevice) -> bool {
        // Only an unconditional `_STA` counts. One inside an `If` describes
        // the device as it would be in a hardware configuration this cannot
        // evaluate the predicate for, and firmware writes exactly that: an
        // Intel LPSS controller declares `Name (_STA, 0x08)` — not present —
        // in the branch that applies when it is enumerated through ACPI
        // instead of PCI. Believing it on a machine in PCI mode discards a
        // controller that is working. Absent, or present only conditionally,
        // means present, which is also what ACPI says about a device with no
        // `_STA` at all.
        let Some((member, conditional)) = self.member_conditional(device, b"_STA") else {
            return true;
        };
        if conditional {
            return true;
        }
        match self.evaluate_member(member) {
            // Bit 0 present, bit 1 enabled. A device that is present but
            // not enabled has no resources assigned.
            Some(Value::Integer(status)) => status & 0b11 == 0b11,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Resource templates
// ---------------------------------------------------------------------------

/// An `I2cSerialBus` descriptor out of a `_CRS`.
///
/// This is the whole reason an AML reader is here: it is the only place the
/// slave address of an I2C device is written down. An I2C bus has no
/// enumeration — a master can only address a device it already knows about —
/// so without this the touchpad is unreachable no matter how good the
/// controller driver is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I2cSerialBus {
    /// The address to talk to. Seven bits unless `ten_bit`.
    pub slave_address: u16,
    /// The bus clock the device was designed for, in hertz. Typically
    /// 400 kHz for a touchpad.
    pub connection_speed: u32,
    pub ten_bit: bool,
    /// The controller this hangs off, by namespace path — `\_SB.PC00.I2C5`
    /// or a relative form of it.
    pub controller: Path,
}

/// A `GpioInt` descriptor: which pin the device raises to say it has data.
///
/// Recorded and reported, not used. Servicing it would need a GPIO
/// controller driver and an interrupt controller, and this kernel has
/// neither by design — the input register is polled instead. Knowing the pin
/// exists is still worth something: it is what says the device is
/// interrupt-driven rather than broken when a poll comes back empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioInterrupt {
    pub pin: u16,
    pub controller: Path,
}

/// A fixed-location memory window out of a `_CRS`.
///
/// A GPIO controller declares one of these per *community* — the hardware
/// block that owns a contiguous run of pads. Both the base and the length
/// matter, and the length is the interesting half: it is the only statement
/// anywhere of how many pads a community has that does not require knowing
/// which SoC this is. See [`Namespace::gpio_communities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u32,
    /// False for a `Memory32Fixed` marked read-only. Recorded because a
    /// read-only window is a ROM or a mapped table, never a register file,
    /// and skipping those keeps the community list honest.
    pub writable: bool,
}

/// One entry of a resource template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    I2c(I2cSerialBus),
    Gpio(GpioInterrupt),
    Memory(MemoryRegion),
    /// A descriptor of a type this does not decode. Reported so a caller can
    /// tell "no I2C descriptor" from "nothing parsed at all".
    Other {
        large: bool,
        kind: u8,
    },
}

/// Large-descriptor type: a serial bus connection.
const LARGE_SERIAL_BUS: u8 = 0x0E;
/// Large-descriptor type: a GPIO connection.
const LARGE_GPIO: u8 = 0x0C;
/// Large-descriptor type: a 32-bit fixed-location memory range.
const LARGE_MEMORY32_FIXED: u8 = 0x06;
/// Large-descriptor type: a QWord address space. Used with a memory type it
/// is how a controller above 4 GiB states its window.
const LARGE_QWORD_SPACE: u8 = 0x0A;
/// Large-descriptor type: a DWord address space, the 32-bit sibling of the
/// above. Some firmware writes a controller's window this way instead of as
/// a `Memory32Fixed`, and there is no telling which until it is read.
const LARGE_DWORD_SPACE: u8 = 0x07;
/// Address-space resource type 0 is memory; 1 is I/O, 2 is bus number.
const ADDRESS_SPACE_MEMORY: u8 = 0x00;
/// Serial bus type 1 is I2C.
const SERIAL_BUS_I2C: u8 = 0x01;
/// Small-descriptor type 0x0F is the end tag.
const SMALL_END_TAG: u8 = 0x0F;

/// Walks a resource template, yielding one descriptor at a time.
///
/// A template is a byte buffer of self-describing records — a small
/// descriptor carries its length in the low three bits of its tag, a large
/// one in the two bytes after it — so an unrecognised entry costs nothing to
/// step over. That is the same property the namespace walk relies on, and it
/// is why neither needs to understand everything.
pub fn resources(buffer: &[u8]) -> impl Iterator<Item = Resource> + '_ {
    let mut at = 0usize;
    core::iter::from_fn(move || {
        let tag = *buffer.get(at)?;

        if tag & 0x80 == 0 {
            // Small descriptor: type in bits 6:3, length in bits 2:0.
            let kind = (tag >> 3) & 0x0F;
            let length = (tag & 0x07) as usize;
            if kind == SMALL_END_TAG {
                return None;
            }
            at = at.checked_add(1 + length)?;
            if at > buffer.len() {
                return None;
            }
            return Some(Resource::Other { large: false, kind });
        }

        // Large descriptor: type in bits 6:0, then a 16-bit length.
        let kind = tag & 0x7F;
        let length = u16::from_le_bytes([*buffer.get(at + 1)?, *buffer.get(at + 2)?]) as usize;
        let body_at = at + 3;
        let end = body_at.checked_add(length)?;
        if end > buffer.len() {
            return None;
        }
        let body = &buffer[body_at..end];
        let whole = &buffer[at..end];
        at = end;

        let decoded = match kind {
            LARGE_SERIAL_BUS => parse_i2c_serial_bus(body).map(Resource::I2c),
            LARGE_GPIO => parse_gpio(whole).map(Resource::Gpio),
            LARGE_MEMORY32_FIXED => parse_memory32_fixed(body).map(Resource::Memory),
            LARGE_QWORD_SPACE => parse_address_space(body, 8).map(Resource::Memory),
            LARGE_DWORD_SPACE => parse_address_space(body, 4).map(Resource::Memory),
            _ => None,
        };
        Some(decoded.unwrap_or(Resource::Other { large: true, kind }))
    })
}

/// Decodes the body of a serial-bus descriptor, if it is an I2C one.
///
/// Layout, from the ACPI specification's `I2cSerialBus` macro:
///
/// | Offset | Field |
/// |---|---|
/// | 0 | Revision ID |
/// | 1 | Resource source index |
/// | 2 | Serial bus type — 1 is I2C |
/// | 3 | General flags |
/// | 4 | Type-specific flags (2) |
/// | 6 | Type-specific revision ID |
/// | 7 | Type data length (2) |
/// | 9 | Connection speed (4), slave address (2) |
///
/// The resource source — the controller's name, as text — follows the
/// type-specific data, at an offset the descriptor states rather than one
/// that can be assumed: vendor data may sit between them.
fn parse_i2c_serial_bus(body: &[u8]) -> Option<I2cSerialBus> {
    if body.len() < 9 || *body.get(2)? != SERIAL_BUS_I2C {
        return None;
    }
    let type_specific_flags = u16::from_le_bytes([*body.get(4)?, *body.get(5)?]);
    let type_data_length = u16::from_le_bytes([*body.get(7)?, *body.get(8)?]) as usize;

    let connection_speed = u32::from_le_bytes([
        *body.get(9)?,
        *body.get(10)?,
        *body.get(11)?,
        *body.get(12)?,
    ]);
    let slave_address = u16::from_le_bytes([*body.get(13)?, *body.get(14)?]);

    // Bit 0 of the type-specific flags selects ten-bit addressing.
    let ten_bit = type_specific_flags & 0x0001 != 0;

    let source_at = 9usize.checked_add(type_data_length)?;
    let controller = body
        .get(source_at..)
        .and_then(parse_path)
        .unwrap_or(Path::root());

    Some(I2cSerialBus {
        slave_address,
        connection_speed,
        ten_bit,
        controller,
    })
}

/// Decodes a GPIO connection descriptor.
///
/// Its offsets are from the start of the *whole* descriptor, three-byte
/// header included, which is why this takes the whole record where the serial
/// bus one takes the body.
fn parse_gpio(whole: &[u8]) -> Option<GpioInterrupt> {
    // 0..3 header, 3 revision, 4 connection type, 5..7 general flags,
    // 7..9 interrupt/io flags, 9 pin configuration, 10..12 drive strength,
    // 12..14 debounce, 14..16 pin table offset, 16 resource source index,
    // 17..19 resource source name offset.
    if whole.len() < 19 {
        return None;
    }
    // Connection type 0 is an interrupt; 1 is plain I/O, which no device
    // signals data with.
    if whole[4] != 0 {
        return None;
    }
    let pin_table_offset = u16::from_le_bytes([whole[14], whole[15]]) as usize;
    let source_offset = u16::from_le_bytes([whole[17], whole[18]]) as usize;

    let pin = whole
        .get(pin_table_offset..pin_table_offset + 2)
        .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))?;
    let controller = whole
        .get(source_offset..)
        .and_then(parse_path)
        .unwrap_or(Path::root());

    Some(GpioInterrupt { pin, controller })
}

/// Decodes a `Memory32Fixed` descriptor.
///
/// | Offset | Field |
/// |---|---|
/// | 0 | Information — bit 0 is the write status |
/// | 1 | Base address (4) |
/// | 5 | Range length (4) |
///
/// A zero length is a descriptor firmware left in place but disabled. It is
/// rejected here rather than passed on as a window of no size, which every
/// caller would then have to check for.
fn parse_memory32_fixed(body: &[u8]) -> Option<MemoryRegion> {
    if body.len() < 9 {
        return None;
    }
    let base = u32::from_le_bytes([body[1], body[2], body[3], body[4]]) as u64;
    let length = u32::from_le_bytes([body[5], body[6], body[7], body[8]]);
    if length == 0 {
        return None;
    }
    Some(MemoryRegion {
        base,
        length,
        writable: body[0] & 0x01 != 0,
    })
}

/// Decodes a DWord or QWord address-space descriptor, if it describes memory.
///
/// The two differ only in how wide their five address fields are, so `width`
/// selects between them and the rest is shared:
///
/// | Offset | Field |
/// |---|---|
/// | 0 | Resource type — 0 is memory |
/// | 1 | General flags |
/// | 2 | Type-specific flags — bit 0 is the write status |
/// | 3 | Granularity, minimum, maximum, translation offset, length |
///
/// The *minimum* is the base: firmware that writes a controller's window this
/// way always fixes it, so minimum and maximum describe one place. The length
/// is taken from the address-length field rather than computed from the pair,
/// because a descriptor may state a length shorter than the range it is
/// allowed to be placed within.
fn parse_address_space(body: &[u8], width: usize) -> Option<MemoryRegion> {
    if body.len() < 3 + width * 5 || body[0] != ADDRESS_SPACE_MEMORY {
        return None;
    }
    let field = |index: usize| -> u64 {
        let at = 3 + index * width;
        let mut value = 0u64;
        for (shift, byte) in body[at..at + width].iter().enumerate() {
            value |= (*byte as u64) << (shift * 8);
        }
        value
    };
    let base = field(1);
    let length = field(4);
    if length == 0 || length > u32::MAX as u64 {
        return None;
    }
    Some(MemoryRegion {
        base,
        length: length as u32,
        writable: body[2] & 0x01 != 0,
    })
}

// ---------------------------------------------------------------------------
// The two questions this exists to answer
// ---------------------------------------------------------------------------

/// The UUID an I2C-HID device's `_DSM` answers to.
///
/// From the HID over I2C specification. Written here in the byte order AML
/// stores it — `ToUUID` reverses the first three fields — so it can be
/// compared against a buffer out of the bytecode directly.
pub const I2C_HID_DSM_UUID: [u8; 16] = [
    0xF7, 0xF6, 0xDF, 0x3C, 0x67, 0x42, 0x55, 0x45, 0xAD, 0x05, 0xB3, 0x0A, 0x3D, 0x89, 0x38, 0xDE,
];

/// The compatible ID every I2C-HID device declares.
pub const I2C_HID_CID: &str = "PNP0C50";

/// The two descriptors that matter, however they were obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceResources {
    pub bus: I2cSerialBus,
    pub interrupt: Option<GpioInterrupt>,
}

/// Where a device's resources were read from.
///
/// Reported rather than hidden: `_CRS` is what the firmware would actually
/// have returned, and a declared buffer is the reader's best reading of what
/// it would have chosen. They are usually the same descriptor. When they are
/// not, whoever is looking at the screen should be told which one this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// `_CRS` was evaluated.
    Crs,
    /// `_CRS` could not be evaluated; the device's own declared buffers were
    /// read instead.
    DeclaredBuffers,
}

/// Everything needed to talk to an I2C-HID device.
#[derive(Debug, Clone, Copy)]
pub struct I2cHidDevice {
    pub path: Path,
    pub hid: Option<HardwareId>,
    pub bus: I2cSerialBus,
    /// The register the HID descriptor is at, from `_DSM` function 1, or
    /// `None` when `_DSM` could not be evaluated. See
    /// [`I2cHidDevice::DESCRIPTOR_REGISTER_CANDIDATES`].
    pub descriptor_register: Option<u16>,
    /// The pin the device raises when it has a report, if it declared one.
    pub interrupt: Option<GpioInterrupt>,
    /// Whether the two above came from `_CRS` or from declared buffers.
    pub provenance: Provenance,
}

impl I2cHidDevice {
    /// Registers to try for the HID descriptor when `_DSM` will not say.
    ///
    /// Not a guess so much as a short list to test. The descriptor is thirty
    /// bytes beginning with its own length and a version, so a read at the
    /// wrong register is recognisable as wrong almost every time — which
    /// turns "which register?" from a question needing firmware cooperation
    /// into one answerable by asking the device twice.
    ///
    /// The two values are the ones the HID over I2C specification uses in its
    /// own examples, and between them they cover the overwhelming majority of
    /// shipped devices.
    pub const DESCRIPTOR_REGISTER_CANDIDATES: [u16; 2] = [0x0001, 0x0020];
}

impl<'a> Namespace<'a> {
    /// Finds an I2C-HID device and reads everything needed to reach it.
    ///
    /// Three steps, and each can fail on its own:
    ///
    /// 1. Find a device declaring `PNP0C50` in `_HID` or `_CID`.
    /// 2. Read its `_CRS` for the `I2cSerialBus` — the slave address and
    ///    which controller it is on.
    /// 3. Call `_DSM` function 1 for the HID descriptor register.
    ///
    /// The `_DSM` is the only part that has to *execute* firmware, and the
    /// specification fixes what it returns: a single integer. Where it cannot
    /// be evaluated this returns `None` rather than assuming the common value
    /// — several vendors use a different one, and reading the wrong register
    /// gets a plausible-looking buffer of the wrong thing.
    /// The memory windows a GPIO controller declares, in `_CRS` order.
    ///
    /// An Intel-style GPIO controller is not one register block but several:
    /// each *community* owns a contiguous run of pads and gets its own window,
    /// and `_CRS` lists them in ascending pad order. That ordering is the
    /// whole point — it is what lets a pin number be resolved to a window
    /// without a table of which SoC this is. See `gpio::Controller` in the
    /// bare-metal crate, which does the resolving.
    ///
    /// Returns how many were written. Windows past `out`'s length are
    /// dropped rather than reported, because a caller that cannot hold them
    /// cannot address their pads either.
    /// Visits every buffer a device declares with `Name`, in order.
    ///
    /// The escape hatch for firmware whose `_CRS` this cannot evaluate. A
    /// method like
    ///
    /// ```asl
    /// Method (_CRS, 0, NotSerialized)
    /// {
    ///     If (LLess (OSYS, 0x07DC)) { Return (SBFI) }
    ///     Return (ConcatenateResTemplate (I2CM (I2CX, BADR, SPED), SBFG))
    /// }
    /// ```
    ///
    /// picks between resource templates the device also declares as plain
    /// named buffers — `SBFB`, `SBFG`, `SBFI` here — and running it would
    /// need globals, vendor helper methods and buffer-field writes that this
    /// reader does not implement and should not pretend to.
    ///
    /// What it *can* do is read the buffers. They are the same descriptors
    /// the method would have returned, minus the runtime selection between
    /// them, so a device that declares exactly one I2C descriptor across all
    /// of them is unambiguous however `_CRS` would have chosen. That is the
    /// common case and it is checked rather than assumed: see
    /// [`Namespace::declared_resources`].
    ///
    /// Stops early if `visit` returns false.
    pub fn for_each_declared_buffer(
        &self,
        device: &AcpiDevice,
        mut visit: impl FnMut(&[u8]) -> bool,
    ) {
        let mut cursor = Cursor::at(self.code, device.body.start);
        while cursor.at < device.body.end {
            let Some(opcode) = cursor.byte() else { return };
            if opcode != op::NAME {
                // Anything else is stepped over by the same routine the
                // identifier lookup uses, so the two cannot drift apart.
                if !skip_object(&mut cursor, opcode) {
                    return;
                }
                continue;
            }
            if cursor.name().is_none() {
                return;
            }
            let Some(value) = parse_data_value(&mut cursor) else {
                return;
            };
            if let Value::Buffer(span) = value {
                let Some(bytes) = span.of(self.code) else {
                    return;
                };
                if !visit(bytes) {
                    return;
                }
            }
        }
    }

    /// The I2C and GPIO descriptors a device declares, wherever they are.
    ///
    /// Tries `_CRS` first, and falls back to the device's declared buffers
    /// when it cannot be evaluated. The second value says which happened, so
    /// nothing downstream has to guess how much to trust the answer.
    ///
    /// The fallback refuses ambiguity: if the buffers hold two *different*
    /// I2C descriptors, there is no way to know which `_CRS` would have
    /// returned and this returns `None` rather than picking one. Identical
    /// duplicates are fine — firmware often repeats the same descriptor in
    /// several templates that differ only in their interrupt half.
    pub fn declared_resources(&self, device: &AcpiDevice) -> Option<(DeviceResources, Provenance)> {
        if let Some(template) = self
            .evaluate(device, b"_CRS", &[])
            .and_then(|crs| crs.bytes(self.code))
        {
            if let Some(bus) = resources(template).find_map(|entry| match entry {
                Resource::I2c(bus) => Some(bus),
                _ => None,
            }) {
                let interrupt = resources(template).find_map(|entry| match entry {
                    Resource::Gpio(gpio) => Some(gpio),
                    _ => None,
                });
                return Some((DeviceResources { bus, interrupt }, Provenance::Crs));
            }
        }

        let mut bus: Option<I2cSerialBus> = None;
        let mut interrupt: Option<GpioInterrupt> = None;
        let mut conflicting = false;
        self.for_each_declared_buffer(device, |buffer| {
            for entry in resources(buffer) {
                match entry {
                    Resource::I2c(found) => match bus {
                        None => bus = Some(found),
                        Some(seen) if seen == found => {}
                        Some(_) => conflicting = true,
                    },
                    // The first interrupt wins. Unlike the bus descriptor
                    // there is nothing to cross-check a second one against,
                    // and templates that differ only in their interrupt half
                    // are common, so a disagreement here is not evidence of
                    // anything.
                    Resource::Gpio(found) if interrupt.is_none() => interrupt = Some(found),
                    _ => {}
                }
            }
            !conflicting
        });

        if conflicting {
            return None;
        }
        Some((
            DeviceResources {
                bus: bus?,
                interrupt,
            },
            Provenance::DeclaredBuffers,
        ))
    }

    pub fn gpio_communities(&self, controller: &Path, out: &mut [MemoryRegion]) -> usize {
        let mut device = None;
        self.for_each_device(|candidate| {
            if candidate.path.ends_with(controller) {
                device = Some(candidate);
                return false;
            }
            true
        });
        let Some(device) = device else {
            return 0;
        };
        if !self.present(&device) {
            return 0;
        }
        let Some(crs) = self.evaluate(&device, b"_CRS", &[]) else {
            return 0;
        };
        let Some(template) = crs.bytes(self.code) else {
            return 0;
        };

        let mut written = 0usize;
        for resource in resources(template) {
            if written == out.len() {
                break;
            }
            // Read-only windows are skipped: a pad configuration register
            // file is written to, so anything the firmware marked read-only
            // is some other thing that happens to be in the same `_CRS`.
            if let Resource::Memory(region) = resource {
                if region.writable {
                    out[written] = region;
                    written += 1;
                }
            }
        }
        written
    }

    /// How many present I2C-HID devices this table declares.
    ///
    /// Needed to index across tables: the namespace is spread over the DSDT
    /// and the SSDTs, so "the second touchpad" may be the first one in the
    /// third table, and a caller walking the tables in order has to know how
    /// many each contributed.
    pub fn i2c_hid_count(&self) -> usize {
        let mut count = 0usize;
        self.for_each_device(|device| {
            if device.is(I2C_HID_CID) && self.present(&device) {
                count += 1;
            }
            true
        });
        count
    }

    pub fn find_i2c_hid(&self, scratch: &mut [u8]) -> Option<I2cHidDevice> {
        self.find_i2c_hid_nth(0, scratch)
    }

    /// The `index`th I2C-HID device, counting from zero.
    ///
    /// A notebook routinely has two: the touchpad, and the keyboard where it
    /// is not on the 8042. They are separate namespace nodes with separate
    /// addresses, so finding only the first finds whichever the firmware
    /// happened to declare first — which on some machines is the keyboard and
    /// on others the touchpad.
    pub fn find_i2c_hid_nth(&self, index: usize, scratch: &mut [u8]) -> Option<I2cHidDevice> {
        let mut seen = 0usize;
        let mut chosen = None;
        self.for_each_device(|device| {
            if device.is(I2C_HID_CID) && self.present(&device) {
                if seen == index {
                    chosen = Some(device);
                    return false;
                }
                seen += 1;
            }
            true
        });
        let device = chosen?;

        // `_CRS` when it can be evaluated, the device's declared buffers
        // when it cannot. Real firmware writes `_CRS` as a method over
        // vendor helpers this reader does not implement, so the fallback is
        // the common path rather than a rare one — and `provenance` says
        // which was used.
        let (found, provenance) = self.declared_resources(&device)?;

        // `_DSM` function 1 is where the HID descriptor's register lives.
        // The same firmware that hides `_CRS` behind helper methods hides
        // this one too, so a device that will not answer reports `None` and
        // leaves the choice to the driver, which can try the two values the
        // specification's own examples use and keep whichever produces a
        // descriptor that validates. Guessing here, with no way to check the
        // guess, would be worse than saying nothing.
        let descriptor_register = self.i2c_hid_descriptor_register(&device, scratch);

        Some(I2cHidDevice {
            path: device.path,
            hid: device.hid,
            bus: found.bus,
            descriptor_register,
            interrupt: found.interrupt,
            provenance,
        })
    }

    /// Calls `_DSM(uuid, 1, 1, {})` and takes the register out of it.
    ///
    /// The UUID has to be a [`Value::Buffer`], and a buffer is a range into
    /// the bytecode — so the caller's scratch space stands in for a piece of
    /// bytecode that does not exist. It is appended to a copy of the code
    /// only in the sense that the interpreter is handed a range describing
    /// it; see below.
    fn i2c_hid_descriptor_register(&self, device: &AcpiDevice, scratch: &mut [u8]) -> Option<u16> {
        // The interpreter compares buffers by their bytes, and both operands
        // must live in the same slice. The UUID does not appear in the
        // bytecode at a known offset, so it is found *in* the bytecode: a
        // `_DSM` that answers to it necessarily contains it as a literal.
        let at = find_bytes(self.code, &I2C_HID_DSM_UUID)?;
        let uuid = Value::Buffer(Span::new(at, at + I2C_HID_DSM_UUID.len()));
        let _ = scratch;

        let result = self.evaluate(
            device,
            b"_DSM",
            &[
                uuid,
                // Revision 1, function 1 — "HID descriptor address" — and an
                // empty package of arguments.
                Value::Integer(1),
                Value::Integer(1),
                Value::Package(Span::new(0, 0)),
            ],
        )?;
        match result {
            Value::Integer(register) => u16::try_from(register).ok(),
            _ => None,
        }
    }
}

/// Finds the first occurrence of `needle` in `haystack`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes a PkgLength for a body of `body` bytes.
    ///
    /// The encoded length counts its own bytes, which is the detail that
    /// makes this worth a function: getting it wrong by one puts every
    /// subsequent object at the wrong offset.
    fn package_length(body: usize) -> Vec<u8> {
        // Try the one-byte form first: six bits of length including this
        // byte, so up to 63 total.
        if body < 0x3F {
            return vec![(body + 1) as u8];
        }
        // Two-byte form: four low bits here, eight in the next.
        let total = body + 2;
        if total <= 0x0FFF {
            return vec![0x40 | (total & 0x0F) as u8, (total >> 4) as u8];
        }
        let total = body + 3;
        vec![
            0x80 | (total & 0x0F) as u8,
            (total >> 4) as u8,
            (total >> 12) as u8,
        ]
    }

    /// Wraps `body` in `opcode` + PkgLength.
    fn packaged(opcode: &[u8], body: Vec<u8>) -> Vec<u8> {
        let mut out = opcode.to_vec();
        out.extend_from_slice(&package_length(body.len()));
        out.extend_from_slice(&body);
        out
    }

    fn name_seg(name: &str) -> [u8; 4] {
        let mut segment = [b'_'; 4];
        for (slot, byte) in segment.iter_mut().zip(name.bytes()) {
            *slot = byte;
        }
        segment
    }

    /// `I2cSerialBus(address, ..., "controller")` as the assembler emits it.
    fn i2c_descriptor(address: u16, speed: u32, controller: &str) -> Vec<u8> {
        let mut body = vec![
            0x01, // revision
            0x00, // resource source index
            0x01, // serial bus type: I2C
            0x00, // general flags
            0x00, 0x00, // type-specific flags
            0x01, // type-specific revision
            0x06, 0x00, // type data length: speed + address
        ];
        body.extend_from_slice(&speed.to_le_bytes());
        body.extend_from_slice(&address.to_le_bytes());
        body.extend_from_slice(controller.as_bytes());
        body.push(0);

        let mut out = vec![0x80 | LARGE_SERIAL_BUS];
        out.extend_from_slice(&(body.len() as u16).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// `GpioInt(...)`, whose offsets are from the start of the descriptor.
    fn gpio_descriptor(pin: u16, controller: &str) -> Vec<u8> {
        // Header is three bytes; the fixed fields run to offset 23.
        const FIXED_END: usize = 23;
        let pin_table_offset = FIXED_END;
        let source_offset = FIXED_END + 2;

        let mut out = vec![0x80 | LARGE_GPIO, 0, 0];
        out.push(0x01); // revision
        out.push(0x00); // connection type: interrupt
        out.extend_from_slice(&0u16.to_le_bytes()); // general flags
        out.extend_from_slice(&0u16.to_le_bytes()); // interrupt flags
        out.push(0x00); // pin configuration
        out.extend_from_slice(&0u16.to_le_bytes()); // drive strength
        out.extend_from_slice(&0u16.to_le_bytes()); // debounce
        out.extend_from_slice(&(pin_table_offset as u16).to_le_bytes());
        out.push(0x00); // resource source index
        out.extend_from_slice(&(source_offset as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // vendor data offset
        out.extend_from_slice(&0u16.to_le_bytes()); // vendor data length
        assert_eq!(out.len(), FIXED_END);

        out.extend_from_slice(&pin.to_le_bytes());
        out.extend_from_slice(controller.as_bytes());
        out.push(0);

        let length = (out.len() - 3) as u16;
        out[1..3].copy_from_slice(&length.to_le_bytes());
        out
    }

    fn end_tag() -> Vec<u8> {
        // Small descriptor, type 0x0F, length 1: the tag then a checksum
        // byte, which a zero means "not computed".
        vec![(SMALL_END_TAG << 3) | 1, 0x00]
    }

    /// `Name(_CRS, ResourceTemplate() { ... })`.
    fn crs(descriptors: &[Vec<u8>]) -> Vec<u8> {
        let mut template = Vec::new();
        for descriptor in descriptors {
            template.extend_from_slice(descriptor);
        }
        template.extend_from_slice(&end_tag());

        // BufferOp: PkgLength, then the declared size as a TermArg, then the
        // bytes.
        let mut body = vec![op::WORD_PREFIX];
        body.extend_from_slice(&(template.len() as u16).to_le_bytes());
        body.extend_from_slice(&template);

        let mut out = vec![op::NAME];
        out.extend_from_slice(&name_seg("_CRS"));
        out.extend_from_slice(&packaged(&[op::BUFFER], body));
        out
    }

    /// The `_DSM` shape every I2C-HID device uses:
    ///
    /// ```text
    /// Method (_DSM, 4) {
    ///     If (LEqual(Arg0, ToUUID("3cdff6f7-..."))) {
    ///         If (LEqual(Arg2, One)) { Return (register) }
    ///     }
    ///     Return (Zero)
    /// }
    /// ```
    fn dsm(register: u16) -> Vec<u8> {
        // The UUID literal, as a Buffer.
        let mut uuid_buffer = vec![op::BYTE_PREFIX, 16];
        uuid_buffer.extend_from_slice(&I2C_HID_DSM_UUID);
        let uuid_buffer = packaged(&[op::BUFFER], uuid_buffer);

        // Return (register)
        let mut ret = vec![op::RETURN, op::WORD_PREFIX];
        ret.extend_from_slice(&register.to_le_bytes());

        // If (LEqual(Arg2, One)) { Return (register) }
        let mut inner = vec![op::LEQUAL, op::ARG0 + 2, op::ONE];
        inner.extend_from_slice(&ret);
        let inner = packaged(&[op::IF], inner);

        // If (LEqual(Arg0, uuid)) { ...inner... }
        let mut outer = vec![op::LEQUAL, op::ARG0];
        outer.extend_from_slice(&uuid_buffer);
        outer.extend_from_slice(&inner);
        let outer = packaged(&[op::IF], outer);

        let mut body = Vec::new();
        body.extend_from_slice(&name_seg("_DSM"));
        // Method flags: four arguments, not serialised.
        body.push(0x04);
        body.extend_from_slice(&outer);
        body.extend_from_slice(&[op::RETURN, op::ZERO]);

        packaged(&[op::METHOD], body)
    }

    /// The whole synthetic table: the namespace shape a real laptop has.
    fn table(dsm_register: u16, address: u16) -> Vec<u8> {
        let mut touchpad = Vec::new();
        touchpad.extend_from_slice(&name_seg("TPD0"));

        // Name(_HID, "ELAN06FA") — a vendor string, as the real device does.
        touchpad.push(op::NAME);
        touchpad.extend_from_slice(&name_seg("_HID"));
        touchpad.push(op::STRING_PREFIX);
        touchpad.extend_from_slice(b"ELAN06FA\0");

        // Name(_CID, EisaId("PNP0C50")) — the protocol it speaks.
        touchpad.push(op::NAME);
        touchpad.extend_from_slice(&name_seg("_CID"));
        touchpad.push(op::DWORD_PREFIX);
        touchpad.extend_from_slice(&0x500C_D041u32.to_le_bytes());

        touchpad.extend_from_slice(&crs(&[
            i2c_descriptor(address, 400_000, "\\_SB.PC00.I2C5"),
            gpio_descriptor(0x2A, "\\_SB.PC00.GPI0"),
        ]));
        touchpad.extend_from_slice(&dsm(dsm_register));

        let touchpad = packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], touchpad);

        let mut i2c5 = Vec::new();
        i2c5.extend_from_slice(&name_seg("I2C5"));
        i2c5.push(op::NAME);
        i2c5.extend_from_slice(&name_seg("_HID"));
        i2c5.push(op::STRING_PREFIX);
        i2c5.extend_from_slice(b"INTC1006\0");
        i2c5.extend_from_slice(&touchpad);
        let i2c5 = packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], i2c5);

        let mut pc00 = Vec::new();
        pc00.extend_from_slice(&name_seg("PC00"));
        pc00.extend_from_slice(&i2c5);
        let pc00 = packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], pc00);

        let mut scope = Vec::new();
        scope.extend_from_slice(&name_seg("_SB"));
        scope.extend_from_slice(&pc00);
        let scope = packaged(&[op::SCOPE], scope);

        let mut out = vec![0u8; TABLE_HEADER];
        out[..4].copy_from_slice(b"DSDT");
        out.extend_from_slice(&scope);
        let length = out.len() as u32;
        out[4..8].copy_from_slice(&length.to_le_bytes());
        out
    }

    #[test]
    fn eisa_ids_decode_to_their_written_form() {
        // The number a DSDT actually contains for PNP0C50. Nothing about the
        // packing is guessable, which is exactly why it is pinned here.
        assert_eq!(HardwareId::from_eisa(0x500C_D041).as_str(), "PNP0C50");
        assert_eq!(HardwareId::from_eisa(0x0303_D041).as_str(), "PNP0303");
        assert_eq!(HardwareId::from_eisa(0x080A_D041).as_str(), "PNP0A08");
    }

    #[test]
    fn a_path_renders_without_its_padding() {
        let mut path = Path::root();
        path.push(name_seg("_SB"));
        path.push(name_seg("PC00"));
        path.push(name_seg("I2C5"));
        let mut buf = [0u8; 64];
        let used = path.render(&mut buf);
        assert_eq!(&buf[..used], b"\\_SB.PC00.I2C5");
    }

    #[test]
    fn a_resource_source_string_parses_to_the_same_path() {
        let parsed = parse_path(b"\\_SB.PC00.I2C5").expect("path");
        let mut built = Path::root();
        built.push(name_seg("_SB"));
        built.push(name_seg("PC00"));
        built.push(name_seg("I2C5"));
        assert_eq!(parsed, built);
        assert!(built.ends_with(&parse_path(b"I2C5").unwrap()));
    }

    #[test]
    fn the_walk_finds_a_device_by_its_compatible_id() {
        let table = table(0x0020, 0x15);
        let namespace = Namespace::new(&table).expect("namespace");

        let device = namespace
            .find_device(|device| device.is(I2C_HID_CID))
            .expect("a PNP0C50 device");

        let mut buf = [0u8; 64];
        let used = device.path.render(&mut buf);
        assert_eq!(&buf[..used], b"\\_SB.PC00.I2C5.TPD0");
        assert_eq!(device.hid.expect("_HID").as_str(), "ELAN06FA");
        assert_eq!(device.cid.expect("_CID").as_str(), "PNP0C50");
    }

    #[test]
    fn every_device_in_the_table_is_visited() {
        let table = table(0x0020, 0x15);
        let namespace = Namespace::new(&table).expect("namespace");
        let mut count = 0;
        namespace.for_each_device(|_| {
            count += 1;
            true
        });
        // PC00, I2C5 and TPD0.
        assert_eq!(count, 3);
    }

    #[test]
    fn crs_yields_the_slave_address_and_the_controller() {
        let table = table(0x0020, 0x2C);
        let namespace = Namespace::new(&table).expect("namespace");
        let device = namespace
            .find_device(|device| device.is(I2C_HID_CID))
            .expect("device");

        let value = namespace.evaluate(&device, b"_CRS", &[]).expect("_CRS");
        let template = value.bytes(namespace.code()).expect("buffer");

        let bus = resources(template)
            .find_map(|resource| match resource {
                Resource::I2c(bus) => Some(bus),
                _ => None,
            })
            .expect("an I2cSerialBus descriptor");

        assert_eq!(bus.slave_address, 0x2C);
        assert_eq!(bus.connection_speed, 400_000);
        assert!(!bus.ten_bit);
        assert!(bus.controller.ends_with(&parse_path(b"I2C5").unwrap()));

        let gpio = resources(template)
            .find_map(|resource| match resource {
                Resource::Gpio(gpio) => Some(gpio),
                _ => None,
            })
            .expect("a GpioInt descriptor");
        assert_eq!(gpio.pin, 0x2A);
    }

    #[test]
    fn dsm_runs_and_returns_the_descriptor_register() {
        let table = table(0x0020, 0x15);
        let namespace = Namespace::new(&table).expect("namespace");
        let mut scratch = [0u8; 32];
        let device = namespace
            .find_i2c_hid(&mut scratch)
            .expect("i2c-hid device");

        assert_eq!(device.descriptor_register, Some(0x0020));
        assert_eq!(device.bus.slave_address, 0x15);
        assert_eq!(device.interrupt.expect("gpio").pin, 0x2A);
    }

    #[test]
    fn a_different_register_is_read_rather_than_assumed() {
        // 0x20 is the common value and 0x01 is also in the wild. A reader
        // that defaulted instead of evaluating would pass the test above and
        // fail this one — which is the failure that matters, because the
        // wrong register returns a plausible buffer of the wrong thing.
        let table = table(0x0001, 0x15);
        let namespace = Namespace::new(&table).expect("namespace");
        let mut scratch = [0u8; 32];
        let device = namespace.find_i2c_hid(&mut scratch).expect("device");
        assert_eq!(device.descriptor_register, Some(0x0001));
    }

    #[test]
    fn a_dsm_asked_for_the_wrong_function_returns_zero() {
        let table = table(0x0020, 0x15);
        let namespace = Namespace::new(&table).expect("namespace");
        let device = namespace
            .find_device(|device| device.is(I2C_HID_CID))
            .expect("device");

        let at = find_bytes(namespace.code(), &I2C_HID_DSM_UUID).expect("uuid literal");
        let uuid = Value::Buffer(Span::new(at, at + 16));
        // Function 0 is "which functions are supported", not the register.
        let result = namespace.evaluate(
            &device,
            b"_DSM",
            &[
                uuid,
                Value::Integer(1),
                Value::Integer(0),
                Value::Package(Span::new(0, 0)),
            ],
        );
        assert_eq!(result, Some(Value::Integer(0)));
    }

    #[test]
    fn a_dsm_with_a_foreign_uuid_returns_zero() {
        let table = table(0x0020, 0x15);
        let namespace = Namespace::new(&table).expect("namespace");
        let device = namespace
            .find_device(|device| device.is(I2C_HID_CID))
            .expect("device");

        // Some other UUID: point at sixteen bytes of the table that are not
        // the one the method tests for.
        let other = Value::Buffer(Span::new(0, 16));
        let result = namespace.evaluate(
            &device,
            b"_DSM",
            &[
                other,
                Value::Integer(1),
                Value::Integer(1),
                Value::Package(Span::new(0, 0)),
            ],
        );
        assert_eq!(result, Some(Value::Integer(0)));
    }

    /// The same table, with the definitions a vendor's firmware scatters
    /// through a scope placed *before* the device.
    ///
    /// This is the failure that would not show up in a synthetic test written
    /// only for the happy path: the walk stops at the first opcode it cannot
    /// step over, so an `Alias` or a `CreateByteField` ahead of the touchpad
    /// makes the touchpad invisible. A vendor DSDT is half a megabyte of
    /// machine-generated bytecode and has plenty of both.
    #[test]
    fn definitions_the_walk_does_not_care_about_are_stepped_over() {
        let mut clutter = Vec::new();

        // Alias(SRC_, DST_)
        clutter.push(op::ALIAS);
        clutter.extend_from_slice(&name_seg("SRC"));
        clutter.extend_from_slice(&name_seg("DST"));

        // External(EXT_, DeviceObj, 0)
        clutter.push(op::EXTERNAL);
        clutter.extend_from_slice(&name_seg("EXT"));
        clutter.extend_from_slice(&[0x06, 0x00]);

        // CreateByteField(BUF_, 0x02, FLD_)
        clutter.push(op::CREATE_BYTE_FIELD);
        clutter.extend_from_slice(&name_seg("BUF"));
        clutter.extend_from_slice(&[op::BYTE_PREFIX, 0x02]);
        clutter.extend_from_slice(&name_seg("FLD"));

        // Mutex(MTX_, 0)
        clutter.extend_from_slice(&[op::EXT_PREFIX, op::EXT_MUTEX]);
        clutter.extend_from_slice(&name_seg("MTX"));
        clutter.push(0x00);

        // OperationRegion(RGN_, SystemMemory, 0x1000, 0x100)
        clutter.extend_from_slice(&[op::EXT_PREFIX, op::EXT_OP_REGION]);
        clutter.extend_from_slice(&name_seg("RGN"));
        clutter.push(0x00);
        clutter.extend_from_slice(&[op::WORD_PREFIX, 0x00, 0x10]);
        clutter.extend_from_slice(&[op::WORD_PREFIX, 0x00, 0x01]);

        // If (One) { Name(IGN_, Zero) } — a device inside one would depend on
        // firmware state, so the whole thing is skipped by its length.
        let mut body = vec![op::ONE, op::NAME];
        body.extend_from_slice(&name_seg("IGN"));
        body.push(op::ZERO);
        clutter.extend_from_slice(&packaged(&[op::IF], body));

        let full = table(0x0020, 0x15);
        let namespace = Namespace::new(&full).expect("namespace");
        let baseline = namespace.find_i2c_hid(&mut [0u8; 32]).expect("device");

        // Splice the clutter in just after the `_SB` scope's name, which is
        // where a vendor's own definitions sit.
        let marker = name_seg("_SB");
        let at = find_bytes(&full, &marker).expect("the _SB segment") + 4;
        let mut cluttered = Vec::new();
        cluttered.extend_from_slice(&full[..at]);
        cluttered.extend_from_slice(&clutter);
        cluttered.extend_from_slice(&full[at..]);

        // Both the scope's package length and the table's length grow.
        let grown = clutter.len();
        let scope_at = find_bytes(&cluttered, &[op::SCOPE]).expect("the scope opcode");
        widen_package(&mut cluttered, scope_at + 1, grown);
        let length = (cluttered.len() as u32).to_le_bytes();
        cluttered[4..8].copy_from_slice(&length);

        let namespace = Namespace::new(&cluttered).expect("namespace");
        let found = namespace
            .find_i2c_hid(&mut [0u8; 32])
            .expect("the touchpad, past the clutter");
        assert_eq!(found.bus.slave_address, baseline.bus.slave_address);
        assert_eq!(found.descriptor_register, baseline.descriptor_register);
    }

    /// Adds `extra` to the PkgLength at `at`, keeping its encoded width.
    ///
    /// Only correct while the length stays inside the two-byte form, which is
    /// all the test above needs.
    fn widen_package(bytes: &mut [u8], at: usize, extra: usize) {
        let lead = bytes[at];
        assert_eq!(
            lead >> 6,
            1,
            "the test table's scope uses the two-byte form"
        );
        let total = (lead & 0x0F) as usize | ((bytes[at + 1] as usize) << 4);
        let total = total + extra;
        assert!(total <= 0x0FFF, "the widened package needs a third byte");
        bytes[at] = 0x40 | (total & 0x0F) as u8;
        bytes[at + 1] = (total >> 4) as u8;
    }

    #[test]
    fn a_truncated_table_is_refused_rather_than_walked() {
        let table = table(0x0020, 0x15);
        // A length field claiming more than the memory behind it: exactly
        // what a corrupt or misread table looks like, and following it would
        // walk off the end.
        let mut lying = table.clone();
        let length = (lying.len() as u32 + 4096).to_le_bytes();
        lying[4..8].copy_from_slice(&length);
        assert!(Namespace::new(&lying).is_none());

        assert!(Namespace::new(&table[..20]).is_none());
    }

    #[test]
    fn a_walk_over_truncated_bytecode_stops_instead_of_reading_past_it() {
        let full = table(0x0020, 0x15);
        // Every prefix of the table, with the length field made consistent,
        // must terminate without panicking. Firmware is not always well
        // formed, and a walk that runs off the end is a page fault in a
        // kernel with no handler.
        for cut in TABLE_HEADER..full.len() {
            let mut partial = full[..cut].to_vec();
            let length = (partial.len() as u32).to_le_bytes();
            partial[4..8].copy_from_slice(&length);
            let Some(namespace) = Namespace::new(&partial) else {
                continue;
            };
            let mut scratch = [0u8; 32];
            let _ = namespace.find_i2c_hid(&mut scratch);
            namespace.for_each_device(|_| true);
        }
    }

    /// The real thing, when it is available.
    ///
    /// A synthetic table proves the reader understands the encoding. It does
    /// not prove it survives a *vendor's* DSDT, which is half a megabyte of
    /// machine-generated bytecode with operation regions, mutexes, and
    /// methods that call other methods — and which is what this has to walk
    /// on the machine it runs on.
    ///
    /// The table is root-readable, so this skips when it cannot be opened
    /// rather than failing on a machine where it is not there. Copy it out to
    /// exercise this:
    ///
    /// ```console
    /// $ sudo cp /sys/firmware/acpi/tables/DSDT /tmp/dsdt.aml
    /// $ sudo chmod a+r /tmp/dsdt.aml
    /// ```
    #[test]
    #[cfg(feature = "std")]
    fn the_real_firmware_table_walks_and_finds_its_touchpad() {
        let candidates = [
            "/tmp/acpi/DSDT",
            "/tmp/dsdt.aml",
            "/sys/firmware/acpi/tables/DSDT",
        ];
        let Some(table) = candidates.iter().find_map(|path| std::fs::read(path).ok()) else {
            eprintln!("no readable DSDT; skipping (see this test's documentation)");
            return;
        };

        let namespace = Namespace::new(&table).expect("a DSDT with a sane length");

        let mut devices = 0;
        namespace.for_each_device(|_| {
            devices += 1;
            true
        });
        assert!(
            devices > 10,
            "only {devices} devices found in {} bytes of DSDT; the walk stopped early",
            table.len()
        );

        let Some(touchpad) = namespace.find_device(|device| device.is(I2C_HID_CID)) else {
            eprintln!("no PNP0C50 device in this firmware; nothing further to check");
            return;
        };

        let mut buf = [0u8; 64];
        let used = touchpad.path.render(&mut buf);
        let path = std::str::from_utf8(&buf[..used]).unwrap();
        eprintln!(
            "i2c-hid device: {path} hid={:?} cid={:?}",
            touchpad.hid, touchpad.cid
        );

        let mut scratch = [0u8; 32];
        match namespace.find_i2c_hid(&mut scratch) {
            Some(device) => {
                eprintln!(
                    "  slave 0x{:02x} at {} Hz, descriptor register {:?}, gpio {:?}, from {:?}",
                    device.bus.slave_address,
                    device.bus.connection_speed,
                    device.descriptor_register,
                    device.interrupt.map(|gpio| gpio.pin),
                    device.provenance,
                );
                assert!(
                    device.bus.slave_address != 0 && device.bus.slave_address < 0x80,
                    "slave address 0x{:02x} is not a seven-bit address",
                    device.bus.slave_address
                );
                assert_ne!(
                    device.bus.connection_speed, 0,
                    "a bus speed of zero cannot be programmed into a divider"
                );
                assert_ne!(device.descriptor_register, Some(0));
            }
            None => panic!(
                "found {path} but neither its _CRS nor its declared buffers \
                 yielded an I2C descriptor"
            ),
        }
    }

    /// A `Memory32Fixed` is what a GPIO controller states its communities
    /// with, and both halves matter: the base to reach the pads, the length
    /// to know how many there are.
    #[test]
    fn a_memory32_fixed_yields_base_and_length() {
        // Tag 0x86, length 9, then: information, base, length.
        let template = [
            0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0xC5, 0x00, 0x00, 0x10, 0x00, 0x00, 0x79, 0x00,
        ];
        let found: Vec<_> = resources(&template).collect();
        assert_eq!(
            found,
            [Resource::Memory(MemoryRegion {
                base: 0x00C5_0000,
                length: 0x1000,
                writable: true,
            })]
        );
    }

    /// A read-only window is not a register file. Decoded, but flagged, so
    /// `gpio_communities` can drop it.
    #[test]
    fn a_read_only_memory_window_is_marked_as_such() {
        let template = [
            0x86, 0x09, 0x00, 0x00, 0x00, 0x00, 0x0E, 0x00, 0x00, 0x02, 0x00, 0x00, 0x79, 0x00,
        ];
        let found: Vec<_> = resources(&template).collect();
        assert_eq!(
            found,
            [Resource::Memory(MemoryRegion {
                base: 0x000E_0000,
                length: 0x200,
                writable: false,
            })]
        );
    }

    /// A zero-length window is a descriptor firmware disabled in place.
    /// Rejecting it here means no caller has to check for a region of no
    /// size.
    #[test]
    fn a_zero_length_memory_window_is_rejected() {
        let template = [
            0x86, 0x09, 0x00, 0x01, 0x00, 0x00, 0xC5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x79, 0x00,
        ];
        let found: Vec<_> = resources(&template).collect();
        assert_eq!(
            found,
            [Resource::Other {
                large: true,
                kind: 0x06
            }]
        );
    }

    /// Some firmware writes a controller's window as a QWord address space
    /// rather than a `Memory32Fixed`. The minimum is the base and the
    /// address-length field is the length; the maximum is not consulted.
    #[test]
    fn a_qword_memory_space_yields_the_same_shape() {
        let mut template = vec![0x8A, 0x2B, 0x00];
        template.push(0x00); // resource type: memory
        template.push(0x00); // general flags
        template.push(0x01); // type-specific flags: writable
        for value in [
            0u64,
            0x0000_0000_C500_0000,
            0x0000_0000_C500_0FFF,
            0,
            0x1000,
        ] {
            template.extend_from_slice(&value.to_le_bytes());
        }
        template.extend_from_slice(&[0x79, 0x00]);

        let found: Vec<_> = resources(&template).collect();
        assert_eq!(
            found,
            [Resource::Memory(MemoryRegion {
                base: 0xC500_0000,
                length: 0x1000,
                writable: true,
            })]
        );
    }

    /// An address space describing I/O ports is not memory and must not be
    /// mistaken for a register window.
    #[test]
    fn an_io_address_space_is_not_decoded_as_memory() {
        let mut template = vec![0x87, 0x17, 0x00];
        template.push(0x01); // resource type: I/O
        template.push(0x00);
        template.push(0x03);
        for value in [0u32, 0x1000, 0x10FF, 0, 0x100] {
            template.extend_from_slice(&value.to_le_bytes());
        }
        template.extend_from_slice(&[0x79, 0x00]);

        let found: Vec<_> = resources(&template).collect();
        assert_eq!(
            found,
            [Resource::Other {
                large: true,
                kind: 0x07
            }]
        );
    }

    /// Wraps a body as a DSDT, the way [`table`] does, so a hand-built
    /// fragment can be walked.
    fn as_dsdt(body: Vec<u8>) -> Vec<u8> {
        let mut out = vec![0u8; TABLE_HEADER];
        out[..4].copy_from_slice(b"DSDT");
        out.extend_from_slice(&body);
        let length = out.len() as u32;
        out[4..8].copy_from_slice(&length.to_le_bytes());
        out
    }

    /// A method taking `args` arguments, whose body returns zero.
    fn method(name: &str, args: u8) -> Vec<u8> {
        let mut body = name_seg(name).to_vec();
        body.push(args);
        body.push(op::RETURN);
        body.push(op::ZERO);
        packaged(&[op::METHOD], body)
    }

    /// A method call in a term-argument position has no syntax of its own:
    /// the name is followed straight by its arguments, with nothing to mark
    /// where they end. A reader that does not know the arity parses the first
    /// argument as the next object and desynchronises from there.
    #[test]
    fn a_method_call_consumes_its_declared_arguments() {
        let code = as_dsdt(method("I2CM", 3));
        assert_eq!(method_arity(&code, name_seg("I2CM")), Some(3));
        assert_eq!(method_arity(&code, name_seg("NONE")), None);
    }

    /// Two methods sharing a leaf and disagreeing about arity give no answer.
    /// Picking one would desynchronise every parse that met the other, which
    /// is worse than treating the name as a plain reference.
    #[test]
    fn an_ambiguous_method_leaf_yields_no_arity() {
        let mut body = method("AMBG", 1);
        body.extend_from_slice(&method("AMBG", 3));
        let code = as_dsdt(body);
        assert_eq!(method_arity(&code, name_seg("AMBG")), None);
    }

    /// An `OperationRegion` whose offset is a name rather than a constant.
    /// Real firmware writes these, and reading the name as data desynchronised
    /// the walk from that point to the end of the table — which on the machine
    /// this was found on meant every device in it.
    #[test]
    fn a_region_with_a_named_offset_does_not_derail_the_walk() {
        let mut body = Vec::new();
        body.push(op::EXT_PREFIX);
        body.push(op::EXT_OP_REGION);
        body.extend_from_slice(&name_seg("SANV"));
        body.push(0x00);
        body.extend_from_slice(&name_seg("SANB"));
        body.extend_from_slice(&name_seg("SANL"));

        let mut device = name_seg("TPD0").to_vec();
        device.push(op::NAME);
        device.extend_from_slice(&name_seg("_CID"));
        device.push(op::STRING_PREFIX);
        device.extend_from_slice(b"PNP0C50\0");
        body.extend_from_slice(&packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], device));

        let code = as_dsdt(body);
        let namespace = Namespace::new(&code).expect("namespace");
        assert!(
            namespace
                .find_device(|device| device.is(I2C_HID_CID))
                .is_some(),
            "the device after the operation region was never reached"
        );
    }

    /// The identifier lookup has to step over everything the walk does. It
    /// once did not, and a `CreateWordField` standing between the device's
    /// opening brace and its `_CID` made the device report no identifier at
    /// all — found, present, and matching nothing.
    #[test]
    fn an_identifier_after_a_create_field_is_still_found() {
        let mut device = name_seg("TPD0").to_vec();

        device.push(op::NAME);
        device.extend_from_slice(&name_seg("SBFB"));
        device.push(op::BUFFER);
        let mut buffer = vec![op::BYTE_PREFIX, 0x02, 0x00, 0x00];
        let mut with_length = package_length(buffer.len());
        with_length.append(&mut buffer);
        device.extend_from_slice(&with_length);

        device.push(op::CREATE_WORD_FIELD);
        device.extend_from_slice(&name_seg("SBFB"));
        device.push(op::BYTE_PREFIX);
        device.push(0x00);
        device.extend_from_slice(&name_seg("I2CN"));

        device.push(op::NAME);
        device.extend_from_slice(&name_seg("_CID"));
        device.push(op::STRING_PREFIX);
        device.extend_from_slice(b"PNP0C50\0");

        let code = as_dsdt(packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], device));
        let namespace = Namespace::new(&code).expect("namespace");
        let device = namespace
            .find_device(|device| device.is(I2C_HID_CID))
            .expect("the _CID after the CreateWordField was not reached");
        assert_eq!(
            device.cid.map(|id| id.as_str().to_owned()),
            Some("PNP0C50".to_owned())
        );
    }

    /// When `_CRS` cannot be evaluated, the descriptors the device declares
    /// as plain buffers are read instead — and the answer says so, because
    /// a declared buffer is a default the firmware may have meant to patch.
    #[test]
    fn declared_buffers_stand_in_for_an_unevaluable_crs() {
        let mut device = name_seg("TPD0").to_vec();
        device.push(op::NAME);
        device.extend_from_slice(&name_seg("_CID"));
        device.push(op::STRING_PREFIX);
        device.extend_from_slice(b"PNP0C50\0");

        // Name (SBFB, ResourceTemplate () { I2cSerialBus (...), GpioInt (...) })
        let mut template = i2c_descriptor(0x2C, 400_000, "\\_SB.PC00.I2C5");
        template.extend_from_slice(&gpio_descriptor(0x2A, "\\_SB.GPI0"));
        template.extend_from_slice(&end_tag());
        device.push(op::NAME);
        device.extend_from_slice(&name_seg("SBFB"));
        device.push(op::BUFFER);
        let mut buffer = vec![op::WORD_PREFIX];
        buffer.extend_from_slice(&(template.len() as u16).to_le_bytes());
        buffer.extend_from_slice(&template);
        let mut with_length = package_length(buffer.len());
        with_length.append(&mut buffer);
        device.extend_from_slice(&with_length);

        let code = as_dsdt(packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], device));
        let namespace = Namespace::new(&code).expect("namespace");
        let found = namespace
            .find_device(|device| device.is(I2C_HID_CID))
            .expect("device");
        let (resources, provenance) = namespace
            .declared_resources(&found)
            .expect("the declared buffer was not read");

        assert_eq!(provenance, Provenance::DeclaredBuffers);
        assert_eq!(resources.bus.slave_address, 0x2C);
        assert_eq!(resources.interrupt.expect("gpio").pin, 0x2A);
    }

    /// The namespace is the DSDT *plus* the SSDTs, and firmware splits it.
    ///
    /// On the machine this was developed against, `Device (TPD0)` — the
    /// touchpad — is in the DSDT, while the `Device (I2C5)` it hangs off,
    /// carrying the `_ADR` that says where on the PCI bus the controller is,
    /// is in one of sixteen SSDTs. A reader that looks only at the DSDT finds
    /// the touchpad, resolves its controller by name, and then finds nothing
    /// anywhere declaring that name — which is exactly the failure this
    /// checks has been fixed.
    ///
    /// Skips when the tables are not readable. To supply them:
    ///
    /// ```console
    /// $ sudo mkdir -p /tmp/acpi && sudo cp /sys/firmware/acpi/tables/{DSDT,SSDT*} /tmp/acpi/
    /// $ sudo chmod -R a+r /tmp/acpi
    /// ```
    #[test]
    fn the_controller_is_found_across_the_dsdt_and_the_ssdts() {
        let Ok(dsdt) = std::fs::read("/tmp/acpi/DSDT") else {
            eprintln!("no /tmp/acpi/DSDT; skipping (see this test's documentation)");
            return;
        };
        let namespace = Namespace::new(&dsdt).expect("a DSDT with a sane length");

        let mut scratch = [0u8; 32];
        let Some(device) = namespace.find_i2c_hid(&mut scratch) else {
            eprintln!("no PNP0C50 device in the DSDT; nothing further to check");
            return;
        };

        let mut rendered = [0u8; 64];
        let used = device.bus.controller.render(&mut rendered);
        let wanted = std::str::from_utf8(&rendered[..used]).unwrap().to_owned();

        // Every table, DSDT first, the way the driver searches.
        let mut tables = vec![("DSDT".to_owned(), dsdt.clone())];
        let mut entries: Vec<_> = std::fs::read_dir("/tmp/acpi")
            .expect("the dump directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("SSDT"))
            })
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if let Ok(bytes) = std::fs::read(&path) {
                tables.push((name, bytes));
            }
        }

        let mut found = None;
        for (name, bytes) in &tables {
            let Some(table) = Namespace::new(bytes) else {
                continue;
            };
            let Some(controller) =
                table.find_device(|candidate| candidate.path.ends_with(&device.bus.controller))
            else {
                continue;
            };
            if let Some(Value::Integer(address)) = table.evaluate(&controller, b"_ADR", &[]) {
                found = Some((name.clone(), address));
                break;
            }
        }

        let (table, address) = found.unwrap_or_else(|| {
            panic!(
                "the controller {wanted} was declared with an _ADR in none of the \
                 {} tables read",
                tables.len()
            )
        });
        eprintln!(
            "controller {wanted}: _ADR 0x{address:08x} -> {:02x}.{}, from {table}",
            (address >> 16) & 0x1F,
            address & 0x07
        );
        assert_ne!(address, 0, "an _ADR of zero addresses the host bridge");
    }

    /// Everything the parser can be asked, asked of every table this machine
    /// has, in one sweep.
    ///
    /// The point is coverage against firmware nobody wrote for a test. Every
    /// DSDT and SSDT differs from every other — vendors, chipsets and BIOS
    /// revisions all change the shape — so a reader that handles one machine
    /// has been shown very little. This walks each table, renders every
    /// device's path, reads every identifier, evaluates every `_STA` and
    /// `_CRS` it can, and parses every resource template that comes back.
    ///
    /// Nothing here asserts a *value*; the values belong to whoever's machine
    /// this ran on. What it asserts is that none of it derails: that the
    /// walk reaches devices in the tables that have them, that a path always
    /// renders, and that a resource template either parses or is rejected
    /// rather than read past.
    ///
    /// Skips when the tables are not readable. To supply them:
    ///
    /// ```console
    /// $ sudo mkdir -p /tmp/acpi && sudo cp /sys/firmware/acpi/tables/{DSDT,SSDT*} /tmp/acpi/
    /// $ sudo chmod -R a+r /tmp/acpi
    /// ```
    #[test]
    fn every_table_this_machine_has_walks_without_derailing() {
        let Ok(entries) = std::fs::read_dir("/tmp/acpi") else {
            eprintln!("no /tmp/acpi; skipping (see this test's documentation)");
            return;
        };
        let mut tables: Vec<_> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        tables.sort();

        let mut total_devices = 0usize;
        let mut total_identified = 0usize;
        let mut total_resources = 0usize;
        let mut walked = 0usize;

        for path in &tables {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Some(namespace) = Namespace::new(&bytes) else {
                // A table whose length field does not fit its bytes is
                // refused, which is the correct outcome, not a failure.
                eprintln!("{name}: refused ({} bytes)", bytes.len());
                continue;
            };
            walked += 1;

            let mut devices = 0usize;
            let mut identified = 0usize;
            let mut templates = 0usize;
            namespace.for_each_device(|device| {
                devices += 1;

                let mut rendered = [0u8; 64];
                let used = device.path.render(&mut rendered);
                assert!(used > 0, "{name}: a device rendered an empty path");
                assert!(
                    std::str::from_utf8(&rendered[..used]).is_ok(),
                    "{name}: a device path is not valid UTF-8"
                );

                if device.hid.is_some() || device.cid.is_some() {
                    identified += 1;
                }

                // Presence must answer without panicking, whatever the
                // `_STA` turns out to be or where it was declared.
                let _ = namespace.present(&device);

                // Every resource template that evaluates gets walked. A
                // descriptor this does not decode comes back as
                // `Resource::Other`, which is the whole point of the
                // self-describing walk: an unknown entry costs nothing to
                // step over.
                if let Some(template) = namespace
                    .evaluate(&device, b"_CRS", &[])
                    .and_then(|crs| crs.bytes(namespace.code()))
                {
                    let count = resources(template).count();
                    if count > 0 {
                        templates += 1;
                    }
                }
                true
            });

            eprintln!(
                "{name}: {} bytes, {devices} devices, {identified} identified, \
                 {templates} resource templates",
                bytes.len()
            );
            total_devices += devices;
            total_identified += identified;
            total_resources += templates;
        }

        eprintln!(
            "{walked} tables walked: {total_devices} devices, {total_identified} identified, \
             {total_resources} resource templates"
        );
        assert!(walked > 0, "no table in /tmp/acpi could be walked");
        assert!(
            total_devices > 10,
            "only {total_devices} devices across {walked} tables; the walk is stopping early"
        );
    }

    /// A `Scope` named from the root replaces the enclosing scope; it does
    /// not extend it.
    ///
    /// Firmware writes fully-qualified scopes inside other scopes all the
    /// time. Appending instead of replacing produced a path with the prefix
    /// repeated — and, deep enough, one longer than [`MAX_PATH`], at which
    /// point the device inside it was dropped for having no nameable path.
    #[test]
    fn an_absolute_scope_replaces_the_enclosing_one() {
        let mut inner = name_seg("EC0_").to_vec();
        let mut device = name_seg("VPC0").to_vec();
        device.push(op::NAME);
        device.extend_from_slice(&name_seg("_CID"));
        device.push(op::STRING_PREFIX);
        device.extend_from_slice(b"PNP0C50\0");
        inner.extend_from_slice(&packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], device));
        // Scope (\EC0) — absolute, written inside another scope.
        let mut absolute = vec![b'\\'];
        absolute.extend_from_slice(&inner);
        let absolute = packaged(&[op::SCOPE], absolute);

        let mut outer = name_seg("_SB_").to_vec();
        outer.extend_from_slice(&absolute);
        let code = as_dsdt(packaged(&[op::SCOPE], outer));

        let namespace = Namespace::new(&code).expect("namespace");
        let device = namespace
            .find_device(|device| device.is(I2C_HID_CID))
            .expect("the device inside the absolute scope was not reached");

        let mut rendered = [0u8; 64];
        let used = device.path.render(&mut rendered);
        assert_eq!(
            std::str::from_utf8(&rendered[..used]).unwrap(),
            "\\EC0.VPC0",
            "the absolute scope was appended to the enclosing one"
        );
    }

    /// A statement inside a conditional must not end the walk of the list it
    /// is in. One `Store` between two declarations used to hide the second.
    #[test]
    fn a_statement_does_not_hide_the_declarations_after_it() {
        let mut body = vec![
            // Store (One, Local0)
            op::STORE,
            op::ONE,
            op::LOCAL0,
            // Notify (Zero, Zero)
            op::NOTIFY,
            op::ZERO,
            op::ZERO,
        ];
        // Device (TPD0) { Name (_CID, "PNP0C50") }
        let mut device = name_seg("TPD0").to_vec();
        device.push(op::NAME);
        device.extend_from_slice(&name_seg("_CID"));
        device.push(op::STRING_PREFIX);
        device.extend_from_slice(b"PNP0C50\0");
        body.extend_from_slice(&packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], device));

        let code = as_dsdt(body);
        let namespace = Namespace::new(&code).expect("namespace");
        assert!(
            namespace
                .find_device(|device| device.is(I2C_HID_CID))
                .is_some(),
            "the device after the statements was not reached"
        );
    }

    /// Devices inside an `If` are walked, not skipped. One SSDT on the
    /// machine this was developed against wraps a hundred and eight devices
    /// in two conditionals; skipping them found none.
    #[test]
    fn devices_inside_a_conditional_are_still_found() {
        let mut device = name_seg("TPD0").to_vec();
        device.push(op::NAME);
        device.extend_from_slice(&name_seg("_CID"));
        device.push(op::STRING_PREFIX);
        device.extend_from_slice(b"PNP0C50\0");
        let device = packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], device);

        // If (CondRefOf (\DTFS)) { Device (TPD0) { ... } } — the predicate is
        // a two-byte opcode, which is what real firmware uses here.
        let mut conditional = vec![op::EXT_PREFIX, op::EXT_COND_REF_OF, b'\\'];
        conditional.extend_from_slice(&name_seg("DTFS"));
        conditional.push(op::ZERO);
        conditional.extend_from_slice(&device);
        let code = as_dsdt(packaged(&[op::IF], conditional));

        let namespace = Namespace::new(&code).expect("namespace");
        assert!(
            namespace
                .find_device(|device| device.is(I2C_HID_CID))
                .is_some(),
            "the device inside the conditional was not reached"
        );
    }

    /// An `_ADR` declared inside an `If` is readable, and an `_STA` declared
    /// inside one is not believed.
    ///
    /// Both halves come from the same real device: an Intel LPSS controller
    /// declares `_ADR` in its PCI-mode branch and `Name (_STA, 0x08)` — not
    /// present — in its ACPI-mode branch. The address has to be found; the
    /// status has to be ignored, because nothing here can evaluate the
    /// predicate that chooses between them.
    #[test]
    fn a_conditional_adr_is_read_and_a_conditional_sta_is_not_believed() {
        let mut acpi_branch = vec![op::LEQUAL];
        acpi_branch.extend_from_slice(&name_seg("IM05"));
        acpi_branch.push(op::BYTE_PREFIX);
        acpi_branch.push(0x02);
        acpi_branch.push(op::NAME);
        acpi_branch.extend_from_slice(&name_seg("_STA"));
        acpi_branch.push(op::BYTE_PREFIX);
        acpi_branch.push(0x08);

        let mut pci_branch = vec![op::LEQUAL];
        pci_branch.extend_from_slice(&name_seg("IM05"));
        pci_branch.push(op::ONE);
        pci_branch.push(op::NAME);
        pci_branch.extend_from_slice(&name_seg("_ADR"));
        pci_branch.push(op::DWORD_PREFIX);
        pci_branch.extend_from_slice(&0x0019_0001u32.to_le_bytes());

        let mut device = name_seg("I2C5").to_vec();
        device.extend_from_slice(&packaged(&[op::IF], acpi_branch));
        device.extend_from_slice(&packaged(&[op::IF], pci_branch));

        let code = as_dsdt(packaged(&[op::EXT_PREFIX, op::EXT_DEVICE], device));
        let namespace = Namespace::new(&code).expect("namespace");
        let found = namespace
            .find_device(|candidate| candidate.path.leaf() == Some(name_seg("I2C5")))
            .expect("device");

        assert_eq!(
            namespace.evaluate(&found, b"_ADR", &[]),
            Some(Value::Integer(0x0019_0001)),
            "the _ADR inside the conditional was not read"
        );
        assert!(
            namespace.present(&found),
            "a _STA of 0x08 inside a branch this cannot evaluate was believed"
        );
    }
}
