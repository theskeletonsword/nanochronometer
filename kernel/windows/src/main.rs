// SPDX-License-Identifier: MIT
#![no_std]
#![no_main]
//! NanoChronometer Windows kernel driver (WDM), x86_64 + ARM64.
//!
//! Port of `kernel/linux/nanochrono.rs`: a ring-0 hypervisor detector that
//! reports hypervisor presence, hypercall results and a physical-memory demo
//! to user mode. This driver exposes a `\Device\NanoChronometer` device and a
//! single METHOD_BUFFERED IOCTL that returns the `key=value` report.
//!
//! # Targets / toolchain
//!
//! Built with `rustc --target {x86_64,aarch64}-pc-windows-gnullvm` and the
//! clang-based mingw-w64 drivers under
//! `/home/skels/toolchains/windows-crosscompilers/bin/`; linked as a native
//! PE (`--subsystem native --entry DriverEntry`) against a dlltool-generated
//! `ntoskrnl.exe` import library. See the `Makefile`.

use core::ptr;

mod hypercall;
mod mem;
mod nt;
mod report;

use nt::{NTSTATUS, STATUS_SUCCESS};

/// IOCTL implemented by the driver: `FILE_DEVICE_UNKNOWN << 16 | 0x800 | 1` in
/// METHOD_BUFFERED (bit 14-15 = 0b00).
const IOCTL_NANOCHRONO_REPORT: u32 = (nt::FILE_DEVICE_UNKNOWN << 16) | (0x801 << 2) | 0;

/// Widens an ASCII byte string to UTF-16LE (for `RtlInitUnicodeString`), with
/// a trailing NUL, zero-filled to the array length.
const fn widen<const N: usize>(s: &[u8]) -> [u16; N] {
    let mut out = [0u16; N];
    let n = if s.len() >= N - 1 { N - 1 } else { s.len() };
    let mut i = 0;
    while i < n {
        out[i] = s[i] as u16;
        i += 1;
    }
    out
}

/// Device name buffers. `const` (RtlInitUnicodeString only reads the source);
/// the loader guarantees `DriverEntry` runs before anything else touches them.
const DEVICE_NAME_BUF: [u16; 32] = widen::<32>(b"\\Device\\NanoChronometer");
const SYMLINK_NAME_BUF: [u16; 32] = widen::<32>(b"\\DosDevices\\NanoChronometer");

/// The device object handed back by `IoCreateDevice`, needed at unload time.
/// Written once at `DriverEntry`, read once at `DriverUnload` — no concurrent
/// access (load/unload are serialized by the loader).
static mut DEVICE_OBJECT: *mut core::ffi::c_void = core::ptr::null_mut();

/// Terminates the system on a Rust panic. A panicking kernel driver must not
/// fall through; bugchecking is the well-defined failure mode.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // SAFETY: KeBugCheck does not return.
    unsafe { nt::KeBugCheck(0xC0DE_C0DE) }
}

/// Stub for the personality routine that the prebuilt `core` rlib references
/// even under `-C panic=abort` on the gnullvm targets. Never invoked with a
/// panic=abort build; exists only to satisfy the linker.
#[no_mangle]
pub extern "C" fn rust_eh_personality() {}

/// `DriverEntry` — WDM entrypoint called by the I/O manager.
///
/// # Safety
/// Called once by the kernel loader with valid `DRIVER_OBJECT`/registry path.
#[no_mangle]
pub unsafe extern "system" fn DriverEntry(
    driver_object: *mut core::ffi::c_void,
    _registry_path: *const core::ffi::c_void,
) -> NTSTATUS {
    let dev_obj = driver_object.cast::<nt::DriverObject>();
    debug_assert!(!dev_obj.is_null());

    let mut device_name = nt::UnicodeString {
        length: 0,
        maximum_length: 0,
        buffer: ptr::null_mut(),
    };
    let mut symlink_name = nt::UnicodeString {
        length: 0,
        maximum_length: 0,
        buffer: ptr::null_mut(),
    };
    // SAFETY: source buffers are const and only read by RtlInitUnicodeString.
    nt::RtlInitUnicodeString(&mut device_name, DEVICE_NAME_BUF.as_ptr());
    nt::RtlInitUnicodeString(&mut symlink_name, SYMLINK_NAME_BUF.as_ptr());

    // SAFETY: standard WDM libraries invoked with valid objects.
    let mut device: *mut core::ffi::c_void = ptr::null_mut();
    let status = nt::IoCreateDevice(
        dev_obj.cast(),
        0,                     // no extension
        &device_name,
        nt::FILE_DEVICE_UNKNOWN,
        nt::FILE_DEVICE_SECURE_OPEN,
        false as u8,
        &mut device,
    );
    if status != STATUS_SUCCESS {
        // SAFETY: valid device object.
        nt::IoDeleteDevice(device);
        return status;
    }
    DEVICE_OBJECT = device;

    // Clear DO_DEVICE_INITIALIZING so the I/O manager releases the device.
    // SAFETY: `device` was initialized by IoCreateDevice and holds a valid
    // DEVICE_OBJECT whose flags word is at verified offset 48.
    let dev = device.cast::<nt::DeviceObject>();
    (*dev).flags &= !nt::DO_DEVICE_INITIALIZING;

    // Wire the dispatch table: create/close share a completion routine, the
    // report lives on IRP_MJ_DEVICE_CONTROL.
    // SAFETY: `major_function` (offset 112) is an array of 28 function
    // pointers in a valid DRIVER_OBJECT.
    let dispatch = (*dev_obj).major_function.as_mut_ptr();
    dispatch.add(nt::IRP_MJ_CREATE).write(Some(create_close));
    dispatch.add(nt::IRP_MJ_CLOSE).write(Some(create_close));
    dispatch.add(nt::IRP_MJ_DEVICE_CONTROL).write(Some(device_control));

    // SAFETY: valid DRIVER_OBJECT; stored unload is safe because DriverUnload
    // is only invoked after the last handle/IRP to the device is gone.
    (*dev_obj).driver_unload = Some(unload);

    // Expose the user-mode-facing name.
    let status = nt::IoCreateSymbolicLink(&symlink_name, &device_name);
    if status != STATUS_SUCCESS {
        // SAFETY: valid device object.
        nt::IoDeleteDevice(device);
        return status;
    }

    nt::dbg_print("NanoChronometer: driver loaded\r\n");
    STATUS_SUCCESS
}

/// Completes `create`/`close` IRPs with `STATUS_SUCCESS`.
///
/// # Safety
/// Called by the I/O manager with a valid `IRP` whose stack location belongs
/// to this driver.
unsafe extern "system" fn create_close(_device: *mut core::ffi::c_void, irp: *mut nt::Irp) -> NTSTATUS {
    complete_success(irp);
    STATUS_SUCCESS
}

/// Handles `IOCTL_NANOCHRONO_REPORT` (METHOD_BUFFERED): renders the report
/// into the shared `SystemBuffer` and completes with `STATUS_SUCCESS` and the
/// byte count.
///
/// # Safety
/// Called by the I/O manager with a valid METHOD_BUFFERED IRP.
unsafe extern "system" fn device_control(
    _device: *mut core::ffi::c_void,
    irp: *mut nt::Irp,
) -> NTSTATUS {
    // SAFETY: IRP fields at the offsets verified by the C check in the
    // Makefile (see nt.rs module docs).
    let sp = (irp as *const u8).add(nt::IRP_CURRENT_STACK_LOCATION) as *const u8;
    let output_len = *(sp.add(nt::ISL_OUTPUT_BUFFER_LENGTH) as *const u32);
    let ioctl = *(sp.add(nt::ISL_IO_CONTROL_CODE) as *const u32);

    if ioctl != IOCTL_NANOCHRONO_REPORT {
        return complete_with(irp, nt::STATUS_INVALID_DEVICE_REQUEST);
    }

    if output_len < 1 {
        return complete_with(irp, nt::STATUS_BUFFER_TOO_SMALL);
    }

    // METHOD_BUFFERED: SystemBuffer holds output bytes.
    // SAFETY: SystemBuffer at verified offset 24, sized by the I/O manager to
    // the greater of input/output length from user mode.
    let sysbuf = *(irp as *const u8).add(nt::IRP_SYSTEM_BUFFER) as *mut u8;
    if sysbuf.is_null() {
        return complete_with(irp, nt::STATUS_INVALID_PARAMETER);
    }

    let cap = (output_len as usize).min(report::REPORT_CAPACITY);
    let out = core::slice::from_raw_parts_mut(sysbuf, cap);
    let written = report::write_report(out);

    // SAFETY: valid IRP; Information is the count of valid bytes we produced.
    (*irp).io_status().write(nt::IoStatusBlock {
        status: STATUS_SUCCESS,
        information: written,
    });
    nt::dbg_print("NanoChronometer: report served\r\n");
    STATUS_SUCCESS
}

fn complete_success(irp: *mut nt::Irp) {
    // SAFETY: valid IRP.
    unsafe {
        (*irp).io_status().write(nt::IoStatusBlock {
            status: STATUS_SUCCESS,
            information: 0,
        });
    }
}

fn complete_with(irp: *mut nt::Irp, status: NTSTATUS) -> NTSTATUS {
    // SAFETY: valid IRP.
    unsafe {
        (*irp).io_status().write(nt::IoStatusBlock {
            status,
            information: 0,
        });
    }
    status
}

/// Removes the symbolic link and the device.
///
/// # Safety
/// Invoked by the I/O manager after the driver has no outstanding IRPs.
unsafe extern "system" fn unload(_driver: *mut core::ffi::c_void) {
    // SAFETY: buffer was filled at DriverEntry.
    let mut symlink_name = nt::UnicodeString {
        length: 0,
        maximum_length: 0,
        buffer: ptr::null_mut(),
    };
    // SAFETY: source buffer is const and only read by RtlInitUnicodeString.
    nt::RtlInitUnicodeString(&mut symlink_name, SYMLINK_NAME_BUF.as_ptr());
    nt::IoDeleteSymbolicLink(&symlink_name);
    // SAFETY: device object stored at DriverEntry.
    nt::IoDeleteDevice(DEVICE_OBJECT);
    nt::dbg_print("NanoChronometer: driver unloaded\r\n");
}