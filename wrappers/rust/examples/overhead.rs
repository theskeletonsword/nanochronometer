use nanochronometer::{empty_call, NanoChronometer};

extern "C" fn rust_empty(_: *mut core::ffi::c_void) {}

fn main() {
    let mut nc = NanoChronometer::new().expect("load nanochrono");
    nc.calibrate(50);
    println!("native overhead cycles: {}", nc.native_overhead_cycles());
    println!("ffi boundary cycles: {}", nc.ffi_overhead_cycles(10000));
    println!("rust -> C empty_call returned: {}", empty_call());
    println!("C -> Rust callback min cycles: {}", nc.callback_min_cycles(rust_empty, core::ptr::null_mut(), 10000));
    println!("C -> Rust callback avg cycles: {}", nc.callback_avg_cycles(rust_empty, core::ptr::null_mut(), 10000));
}
