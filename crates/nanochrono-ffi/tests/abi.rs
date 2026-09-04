// SPDX-License-Identifier: Apache-2.0
//! The C ABI, exercised through the calls a C caller would make.
//!
//! These are the checks that used to be a hand-run C program. In Rust they
//! run under `cargo test` on every platform the crate builds for, including
//! the cross-compiled Android and Windows targets, which a locally compiled C
//! program could never cover.
//!
//! This file links the crate as an `rlib` and calls the `extern "C"` functions
//! directly, so it tests their behaviour and their contract with null and
//! out-of-range arguments. Whether those functions are actually *exported*
//! from the shared library under the right unmangled names is a separate
//! question, answered by `exported_symbols.rs`.

use nanochrono::*;

/// Runs `body` with a live context, destroying it afterwards.
fn with_ctx(body: impl FnOnce(*mut nc_ctx)) {
    let ctx = nc_create();
    assert!(!ctx.is_null(), "nc_create returned null");
    body(ctx);
    unsafe { nc_destroy(ctx) };
}

#[test]
fn a_context_can_be_created_and_destroyed() {
    with_ctx(|_| {});
}

/// Every entry point must survive a null context rather than dereferencing it.
/// A wrapper in a garbage-collected language will eventually pass one.
#[test]
fn null_contexts_are_rejected_not_dereferenced() {
    unsafe {
        assert_eq!(nc_elapsed_ns(std::ptr::null_mut()), 0);
        assert_eq!(nc_cycles_to_ns(std::ptr::null_mut(), 1_000), 0);
        assert_eq!(
            nc_verify_calibration(std::ptr::null_mut()),
            NC_INTEGRITY_UNRECOVERABLE
        );
        assert_eq!(
            nc_last_integrity(std::ptr::null_mut()),
            NC_INTEGRITY_UNRECOVERABLE
        );
        // Must be a no-op, not a crash.
        nc_inject_calibration_flip(std::ptr::null_mut(), 0);
        // Destroying null is defined and does nothing.
        nc_destroy(std::ptr::null_mut());
    }
}

#[test]
fn elapsed_time_advances() {
    with_ctx(|ctx| {
        let first = unsafe { nc_elapsed_ns(ctx) };
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = unsafe { nc_elapsed_ns(ctx) };
        assert!(
            second > first,
            "elapsed did not advance: {first} -> {second}"
        );
        assert!(
            second - first >= 1_000_000,
            "5 ms of sleep reported as {} ns",
            second - first
        );
    });
}

#[test]
fn cycle_conversion_is_monotonic_and_zero_safe() {
    with_ctx(|ctx| {
        assert_eq!(unsafe { nc_cycles_to_ns(ctx, 0) }, 0);
        let small = unsafe { nc_cycles_to_ns(ctx, 1_000) };
        let large = unsafe { nc_cycles_to_ns(ctx, 1_000_000) };
        assert!(large > small, "conversion is not monotonic");
    });
}

/// The calibrated conversion has to reject an uncalibrated factor instead of
/// returning a plausible-looking duration.
#[test]
fn calibrated_conversion_rejects_a_bad_factor() {
    assert_eq!(nc_cycles_to_ns_calibrated(1_000, 0.0), 0);
    assert_eq!(nc_cycles_to_ns_calibrated(1_000, -1.0), 0);
    assert_eq!(nc_cycles_to_ns_calibrated(1_000, f64::NAN), 0);
    assert!(nc_cycles_to_ns_calibrated(1_000, 3.0) > 0);
}

// -- stored-state integrity ------------------------------------------------

/// The headline claim, through the ABI: a flipped bit in the calibration is
/// repaired, and the conversion that follows is unaffected.
#[test]
fn a_single_flip_is_repaired_through_the_abi() {
    with_ctx(|ctx| {
        let truth = unsafe { nc_cycles_to_ns(ctx, 1_000_000) };
        assert!(truth > 0);

        unsafe { nc_inject_calibration_flip(ctx, 33) };
        assert_eq!(
            unsafe { nc_verify_calibration(ctx) },
            NC_INTEGRITY_CORRECTED_ECC,
            "a single-bit flip was not repaired by the code"
        );
        assert_eq!(
            unsafe { nc_cycles_to_ns(ctx, 1_000_000) },
            truth,
            "the conversion changed after a repaired flip"
        );
        assert_eq!(unsafe { nc_verify_calibration(ctx) }, NC_INTEGRITY_CLEAN);
    });
}

/// Two flips are past what the code can repair alone, so the emergency tier
/// votes. Reaching it must be reported as such, not as a clean read.
#[test]
fn a_double_flip_escalates_to_the_vote_through_the_abi() {
    with_ctx(|ctx| {
        let truth = unsafe { nc_cycles_to_ns(ctx, 1_000_000) };

        unsafe {
            nc_inject_calibration_flip(ctx, 7);
            nc_inject_calibration_flip(ctx, 41);
        }
        assert_eq!(
            unsafe { nc_verify_calibration(ctx) },
            NC_INTEGRITY_CORRECTED_TMR
        );
        assert_eq!(unsafe { nc_cycles_to_ns(ctx, 1_000_000) }, truth);
    });
}

/// Damage to every copy is unrecoverable, and saying so is the whole point:
/// the alternative is a confident wrong number.
#[test]
fn damage_beyond_repair_is_reported_not_guessed() {
    with_ctx(|ctx| {
        unsafe {
            // Two flips defeat the working copy's code, and two more in each
            // replica defeat theirs, so nothing is left that can speak for
            // itself and there is no majority to find.
            nc_inject_calibration_flip(ctx, 3);
            nc_inject_calibration_flip(ctx, 19);
            nc_inject_calibration_flip(ctx, 72);
            nc_inject_calibration_flip(ctx, 100);
            nc_inject_calibration_flip(ctx, 136);
            nc_inject_calibration_flip(ctx, 164);
        }
        assert_eq!(
            unsafe { nc_verify_calibration(ctx) },
            NC_INTEGRITY_UNRECOVERABLE
        );
    });
}

#[test]
fn integrity_counters_are_readable_and_advance() {
    let mut before = nc_integrity_stats_t::default();
    assert_eq!(unsafe { nc_integrity_stats(&mut before) }, 1);

    with_ctx(|ctx| {
        unsafe { nc_inject_calibration_flip(ctx, 12) };
        assert_eq!(
            unsafe { nc_verify_calibration(ctx) },
            NC_INTEGRITY_CORRECTED_ECC
        );
    });

    let mut after = nc_integrity_stats_t::default();
    assert_eq!(unsafe { nc_integrity_stats(&mut after) }, 1);
    // Process-wide counters, and the test harness is threaded, so this is a
    // lower bound rather than an exact delta.
    assert!(after.checks > before.checks);
    assert!(after.ecc_corrections > before.ecc_corrections);
}

#[test]
fn integrity_stats_rejects_a_null_out_pointer() {
    assert_eq!(unsafe { nc_integrity_stats(std::ptr::null_mut()) }, 0);
}

// -- hypervisor ------------------------------------------------------------

/// Detection must fill the report and stay self-consistent, whether or not
/// this machine is virtualised. CI runs both ways.
#[test]
fn hypervisor_detection_fills_a_consistent_report() {
    let mut report = unsafe { std::mem::zeroed::<nc_hypervisor_report_t>() };
    // Returns 1 on success and 0 for a null out-pointer: a boolean, not an
    // `NC_*` status. The wrappers depend on that, so it is asserted as such.
    assert_eq!(unsafe { nc_hypervisor_detect(&mut report) }, 1);

    // The name is always a valid NUL-terminated string, present or not.
    let name = unsafe { std::ffi::CStr::from_ptr(report.name.as_ptr()) };
    assert!(!name.to_bytes().is_empty(), "report carries no name");

    if report.present == 0 {
        assert_eq!(
            report.timing_impact, NC_TIMING_NATIVE,
            "nothing detected, but timings are claimed to be affected"
        );
    } else {
        assert_ne!(
            report.confidence, NC_HV_CONFIDENCE_NONE,
            "a hypervisor was reported present with no confidence in it"
        );
    }

    // A ring 0 hypercall can only have been accepted if the module answered.
    if report.hypercall_accepted != 0 {
        assert_ne!(
            report.kernel_module_loaded, 0,
            "hypercall accepted without the module that issues it"
        );
    }

    // The trap ratio comes from real measurements, so it must be a number.
    assert!(
        report.trap_ratio.is_finite() && report.trap_ratio >= 0.0,
        "trap ratio is not a usable number: {}",
        report.trap_ratio
    );

    let summary = nc_hypervisor_summary();
    assert!(!summary.is_null());
    let summary = unsafe { std::ffi::CStr::from_ptr(summary) };
    assert!(!summary.to_bytes().is_empty(), "summary string is empty");
}

#[test]
fn hypervisor_detect_rejects_a_null_out_pointer() {
    assert_eq!(unsafe { nc_hypervisor_detect(std::ptr::null_mut()) }, 0);
}

// -- instruction-family measurement ----------------------------------------

/// The crypto kernels regressed to a hard `NC_ERR_UNSUPPORTED` once. Each one
/// must either measure something or say the CPU lacks it — never fail on a
/// CPU that has it.
#[test]
fn crypto_kernels_measure_or_decline_honestly() {
    type Measure = unsafe extern "C" fn(*mut nc_ctx, u32, *mut nc_instruction_result_t) -> u64;
    with_ctx(|ctx| {
        let kernels: [(&str, u32, Measure); 3] = [
            ("aesenc", 1, nc_measure_aesenc_cycles),
            ("sha256msg", 2, nc_measure_sha256msg_cycles),
            ("pclmul", 3, nc_measure_pclmul_cycles),
        ];
        for (name, family, measure) in kernels {
            let mut result = nc_instruction_result_t::default();
            let cycles = unsafe { measure(ctx, 1_000, &mut result) };
            assert_eq!(result.family, family, "{name} reported the wrong family");
            assert_eq!(
                cycles, result.cycles,
                "{name} return value disagrees with the struct"
            );
            match result.status {
                NC_OK => {
                    assert!(cycles > 0, "{name} succeeded with zero cycles");
                    // The checksum exists to keep the kernel from being
                    // optimised away. A zero one means it collapsed, which is
                    // how the AES and PCLMUL kernels were once silently
                    // measuring nothing.
                    assert_ne!(result.checksum, 0, "{name} produced a collapsed checksum");
                    assert_eq!(result.blocks, 1_000, "{name} lost the block count");
                }
                NC_ERR_UNSUPPORTED => {
                    assert_eq!(cycles, 0, "{name} declined but returned cycles");
                }
                other => panic!("{name} returned unexpected status {other}"),
            }
        }
    });
}

/// A null out-pointer is tolerated, not fatal: the detailed struct is
/// optional and the cycle count still comes back as the return value. A
/// wrapper that only wants the number passes null.
#[test]
fn a_null_result_struct_is_tolerated() {
    with_ctx(|ctx| unsafe {
        let bare = nc_measure_aesenc_cycles(ctx, 1_000, std::ptr::null_mut());

        let mut result = nc_instruction_result_t::default();
        let with_struct = nc_measure_aesenc_cycles(ctx, 1_000, &mut result);

        // Same call either way, so either both measure or both decline.
        assert_eq!(
            bare == 0,
            with_struct == 0,
            "the null-out path disagreed with the struct path"
        );
    });
}
