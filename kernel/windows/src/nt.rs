// SPDX-License-Identifier: MIT

//! FFI surface of the Windows kernel ABI (WDM) that the driver uses, plus the
//! struct mirrors and verified offsets.
//!
//! Every numeric offset in this file was cross-checked by compiling
//! `defs/offsets-check.c` against the toolchain headers for **both** x86_64
//! and ARM64 (the Makefile runs that check before linking). The layout
//! asserted below at compile time is therefore grounded in the actual
//! compiler, not in memory.
//!
//! Notable finding: in the mingw-w64 headers the `IO_STACK_LOCATION`
//! `DeviceIoControl` parameter block starts at different offsets on x64 (8)
//! and ARM64 (4), because `POINTER_ALIGNMENT` is a no-op on ARM64. Those
//! offsets are arch-specific constants, deliberately *not* `#[repr(C)]`
//! mirrors.

use core::ffi::{c_char, c_int, c_void};
use core::mem::offset_of;

pub type NTSTATUS = c_int;

pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023u32 as i32;
pub const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010u32 as i32;
pub const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as i32;

pub const IRP_MJ_CREATE: usize = 0x00;
pub const IRP_MJ_CLOSE: usize = 0x02;
pub const IRP_MJ_DEVICE_CONTROL: usize = 0x0E;

/// `DO_DEVICE_INITIALIZING` in `DEVICE_OBJECT.Flags`.
pub const DO_DEVICE_INITIALIZING: u32 = 0x0000_0080;

/// `FILE_DEVICE_UNKNOWN`.
pub const FILE_DEVICE_UNKNOWN: u32 = 0x0000_0022;

/// `FILE_DEVICE_SECURE_OPEN`.
pub const FILE_DEVICE_SECURE_OPEN: u32 = 0x0000_0100;

/// `NonPagedPoolNx` (POOL_TYPE).
pub const POOL_NON_PAGED_NX: u32 = 512;

/// `MmNonCached` cache type for `MmMapIoSpace`.
pub const MM_NON_CACHED: u32 = 2;

/// Pool tag `'Nano'` (little-endian u32).
pub const POOL_TAG: u32 = u32::from_le_bytes(*b"Nano");

// ---------------------------------------------------------------------------
// Struct mirrors with compile-time offset assertions
// ---------------------------------------------------------------------------

/// LARGE_INTEGER as exported by the kernel ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LargeInteger {
    pub quad_part: i64,
}

/// `UNICODE_STRING` (`ntdef.h`): u16 length, u16 max length, wchar_t*.
#[repr(C)]
pub struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *mut u16,
}

/// IO_STATUS_BLOCK (`wdm.h`): NTSTATUS + ULONG_PTR.
#[repr(C)]
pub struct IoStatusBlock {
    pub status: NTSTATUS,
    pub information: usize,
}

/// The `Tail.Overlay` anonymous struct inside `_IRP`, which carries
/// `CurrentStackLocation` (that is what `IoGetCurrentIrpStackLocation` reads).
#[repr(C)]
pub struct IrpTailOverlay {
    /// union { KDEVICE_QUEUE_ENTRY; PVOID DriverContext[4]; } — 32 bytes.
    _start: [u8; 32],
    thread: *mut c_void,
    auxiliary_buffer: *mut c_void,
    list_entry: [usize; 2],
    pub current_stack_location: *mut c_void,
    original_file_object: *mut c_void,
}

/// The few `_IRP` fields a METHOD_BUFFERED driver touches.
///
/// Layout is `repr(C)`; the two assertions at the bottom certify that the
/// mirrors line up with the C compiler (the C check in the Makefile certifies
/// the compiler line against the header expectations).
#[repr(C)]
pub struct Irp {
    typ: u16,
    size: u16,
    mdl_address: *mut c_void,
    flags: u32,
    /// `AssociatedIrp.SystemBuffer` for METHOD_BUFFERED.
    pub associated_irp: usize,
    thread_list_entry: [usize; 2],
    io_status: IoStatusBlock,
    /// RequestorMode..AllocationFlags (8 × UCHAR).
    _meta: [u8; 8],
    user_iosb: *mut c_void,
    user_event: *mut c_void,
    /// Overlay union (AsynchronousParameters | AllocationSize).
    _overlay: [usize; 2],
    cancel_routine: *mut c_void,
    pub user_buffer: *mut c_void,
    pub tail: IrpTailOverlay,
}

impl Irp {
    /// `&mut IRP.IoStatus` (offset 48).
    pub fn io_status(&mut self) -> *mut IoStatusBlock {
        core::ptr::addr_of_mut!(self.io_status)
    }
}

/// `_DRIVER_OBJECT` — only the fields this driver reads or writes.
#[repr(C)]
pub struct DriverObject {
    pub typ: u16,
    pub size: u16,
    pub device_object: *mut c_void,
    pub flags: u32,
    pub driver_start: *mut c_void,
    pub driver_size: u32,
    pub driver_section: *mut c_void,
    pub driver_extension: *mut c_void,
    pub driver_name: UnicodeString,
    pub hardware_database: *mut c_void,
    pub fast_io_dispatch: *mut c_void,
    pub driver_init: *mut c_void,
    pub driver_start_io: *mut c_void,
    pub driver_unload: Option<unsafe extern "system" fn(*mut c_void) -> ()>,
    pub major_function: [Option<unsafe extern "system" fn(*mut c_void, *mut Irp) -> NTSTATUS>; 28],
}

/// `_DEVICE_OBJECT` — first fields, up to `Flags` (48) and `DeviceExtension`
/// (64), which are the only ones the driver touches.
#[repr(C)]
pub struct DeviceObject {
    pub typ: u16,
    pub size: u16,
    pub reference_count: i32,
    pub driver_object: *mut c_void,
    pub next_device: *mut c_void,
    pub attached_device: *mut c_void,
    pub current_irp: *mut c_void,
    pub timer: *mut c_void,
    pub flags: u32,
    pub characteristics: u32,
    pub vpb: *mut c_void,
    pub device_extension: *mut c_void,
}

// ---------------------------------------------------------------------------
// IO_STACK_LOCATION parameter offsets, verified per architecture.
// `Parameters.DeviceIoControl` { OutputBufferLength, _pad, InputBufferLength,
// _pad, IoControlCode, _pad, Type3InputBuffer }.
// ---------------------------------------------------------------------------

/// Offset of `Parameters.DeviceIoControl.OutputBufferLength` in the current
/// stack location.
#[cfg(target_arch = "x86_64")]
pub const ISL_OUTPUT_BUFFER_LENGTH: usize = 8;
/// Offset of `Parameters.DeviceIoControl.IoControlCode`.
#[cfg(target_arch = "x86_64")]
pub const ISL_IO_CONTROL_CODE: usize = 24;
/// Offset of `Parameters.DeviceIoControl.Type3InputBuffer`. Documented for
/// completeness; the driver currently only reads the length and the ioctl
/// code. Kept asserted (see offsets-check.c) so it cannot rot.
#[allow(dead_code)]
#[cfg(target_arch = "x86_64")]
pub const ISL_TYPE3_INPUT: usize = 32;

#[cfg(target_arch = "aarch64")]
pub const ISL_OUTPUT_BUFFER_LENGTH: usize = 4;
#[cfg(target_arch = "aarch64")]
pub const ISL_IO_CONTROL_CODE: usize = 12;
#[allow(dead_code)]
#[cfg(target_arch = "aarch64")]
pub const ISL_TYPE3_INPUT: usize = 16;

/// `IRP.AssociatedIrp.SystemBuffer` offset (both architectures).
pub const IRP_SYSTEM_BUFFER: usize = 24;
/// Offset of `IRP.Tail.Overlay.CurrentStackLocation` (both architectures).
pub const IRP_CURRENT_STACK_LOCATION: usize = 184;

// ---------------------------------------------------------------------------
// Compile-time certification of the offsets above.
// ---------------------------------------------------------------------------

const _: () = {
    assert!(size_of::<UnicodeString>() == 16);
    assert!(size_of::<IoStatusBlock>() == 16);
    assert!(offset_of!(Irp, associated_irp) == IRP_SYSTEM_BUFFER);
    assert!(offset_of!(Irp, io_status) == 48);
    assert!(offset_of!(Irp, tail) == 120);
    assert!(offset_of!(IrpTailOverlay, current_stack_location) == 64);
    assert!(IRP_CURRENT_STACK_LOCATION == 120 + 64);
    assert!(offset_of!(DriverObject, driver_unload) == 104);
    assert!(offset_of!(DriverObject, major_function) == 112);
    assert!(size_of::<DriverObject>() == 336);
    assert!(offset_of!(DeviceObject, flags) == 48);
    assert!(offset_of!(DeviceObject, device_extension) == 64);
};

// ---------------------------------------------------------------------------
// Kernel imports
// ---------------------------------------------------------------------------

extern "system" {
    pub fn IoCreateDevice(
        driver_object: *mut c_void,
        device_extension_size: u32,
        device_name: *const UnicodeString,
        device_type: u32,
        device_characteristics: u32,
        exclusive: u8,
        device_object: *mut *mut c_void,
    ) -> NTSTATUS;
    pub fn IoDeleteDevice(device_object: *mut c_void);
    pub fn IoCreateSymbolicLink(
        symbolic_link_name: *const UnicodeString,
        device_name: *const UnicodeString,
    ) -> NTSTATUS;
    pub fn IoDeleteSymbolicLink(symbolic_link_name: *const UnicodeString) -> NTSTATUS;
    pub fn RtlInitUnicodeString(destination: *mut UnicodeString, source: *const u16);
    pub fn DbgPrintEx(component: u32, level: u32, format: *const c_char, ...) -> NTSTATUS;
    pub fn ExAllocatePoolWithTag(pool_type: u32, number_of_bytes: usize, tag: u32) -> *mut c_void;
    pub fn ExFreePoolWithTag(pool: *mut c_void, tag: u32);
    pub fn KeIsHypervisorPresent() -> u8;
    #[cfg(target_arch = "x86_64")]
    pub fn KeQueryPerformanceCounter(counter: *mut c_void) -> LargeInteger;
    pub fn KeBugCheck(code: u32) -> !;
    pub fn MmGetPhysicalAddress(base_address: *mut c_void) -> LargeInteger;
    pub fn MmMapIoSpace(physical_address: LargeInteger, number_of_bytes: usize, cache: u32)
        -> *mut c_void;
    pub fn MmUnmapIoSpace(base_address: *mut c_void, number_of_bytes: usize);
}

/// Logs a printf-style line through `DbgPrintEx` (no varargs, one format key).
pub fn dbg_print(format: &str) {
    // SAFETY: `format` is a NUL-terminated byte slice built from a Rust string
    // literal plus a terminating NUL, and DbgPrintEx only reads it.
    unsafe {
        let mut buf = [0u8; 256];
        let bytes = format.as_bytes();
        let n = bytes.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&bytes[..n]);
        DbgPrintEx(0, 0, buf.as_ptr().cast());
    }
}