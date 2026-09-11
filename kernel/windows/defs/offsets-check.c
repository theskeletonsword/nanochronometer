/* SPDX-License-Identifier: MIT
 *
 * Offset check for the constants embedded in the Rust driver (src/nt.rs).
 * Compiled by the Makefile against the real mingw-w64 DDK headers for BOTH
 * x86_64 and ARM64. If any assertion fails the build aborts, so a mismatch
 * between the Rust mirror and the ABI is caught before linking.
 *
 * x86_64:  wdm.h natively (POINTER_ALIGNMENT pads the IO_STACK_LOCATION
 *          parameter block to offsets 8/24/32).
 * ARM64:   mingw wdm.h has no _M_ARM64 branch, so it must be built with
 *          -D_M_ARM=100 and -defs/armddk-shim in the include path; under that
 *          configuration the parameter block sits at 4/12/16 (the _M_ARM
 *          packing strips the pointer alignment padding).
 */
#include <stddef.h>
#include <ddk/wdm.h>

#define E(type, member, expect) \
    _Static_assert(offsetof(type, member) == (expect), #type "." #member " != " #expect)
#define S(type, expect) _Static_assert(sizeof(type) == (expect), "sizeof(" #type ") != " #expect)

#if defined(_M_AMD64)
/* IO_STACK_LOCATION_
   Parameters.DeviceIoControl start right after Major/Minor/Flags/Control
   re-aligned to pointer alignment on x64. */
E(IO_STACK_LOCATION, Parameters.DeviceIoControl.OutputBufferLength, 8);
E(IO_STACK_LOCATION, Parameters.DeviceIoControl.IoControlCode, 24);
E(IO_STACK_LOCATION, Parameters.DeviceIoControl.Type3InputBuffer, 32);
S(IO_STACK_LOCATION, 72);
#elif defined(_M_ARM) || defined(_M_ARM64) || defined(_ARM64_)
/* ARM64: no pointer-align padding; the parameter block is packed to 4. */
E(IO_STACK_LOCATION, Parameters.DeviceIoControl.OutputBufferLength, 4);
E(IO_STACK_LOCATION, Parameters.DeviceIoControl.IoControlCode, 12);
E(IO_STACK_LOCATION, Parameters.DeviceIoControl.Type3InputBuffer, 16);
S(IO_STACK_LOCATION, 68);
#else
#error "unsupported architecture for offsets-check.c"
#endif

/* IRP fields the driver reads. */
E(IRP, AssociatedIrp.SystemBuffer, 24);
E(IRP, IoStatus.Status, 48);
E(IRP, IoStatus.Information, 56);
E(IRP, Tail.Overlay.CurrentStackLocation, 184);
S(IRP, 208);

/* Called peripheral mirrors. */
E(DRIVER_OBJECT, DriverUnload, 104);
E(DRIVER_OBJECT, MajorFunction, 112);
S(DRIVER_OBJECT, 336);
E(DEVICE_OBJECT, Flags, 48);
E(DEVICE_OBJECT, DeviceExtension, 64);
S(DEVICE_OBJECT, 328);
S(UNICODE_STRING, 16);
S(IO_STATUS_BLOCK, 16);