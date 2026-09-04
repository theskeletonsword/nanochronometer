// SPDX-License-Identifier: Apache-2.0
//! The C ABI a loaded module is reached through.
//!
//! Only meaningful for the shared object: a kernel linking the static archive
//! calls the Rust API directly and needs none of this. It exists because a
//! symbol resolver looks names up, and Rust's mangled names are not a stable
//! thing to look up — see `docs/BAREMETAL_LIBRARIES.md` for the loader that
//! has to exist on the other side.
//!
//! Every function here is safe to call at ring 0 / EL1 and nowhere else, and
//! several program the PMU. There is no way to express that in a C signature,
//! so it is stated once: **this is a ring 0 interface.**

use crate::pmu::CorePmu;

/// Layout version, so a loader can refuse a module it does not understand.
pub const NC_BM_ABI_VERSION: u32 = 1;

/// What one core's PMU offers, flattened for C.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct nc_bm_pmu_t {
    pub version: u32,
    pub general_counters: u32,
    pub general_width: u32,
    pub fixed_counters: u32,
    pub fixed_width: u32,
    /// 0 uniform, 1 performance, 2 efficiency, 3 unknown.
    pub core_type: u32,
    /// 0 none, 1 fixed, 2 general-purpose.
    pub route: u32,
    pub _pad: u32,
}

/// The ABI version this module was built with.
#[no_mangle]
pub extern "C" fn nc_bm_abi_version() -> u32 {
    NC_BM_ABI_VERSION
}

/// Describes this core's PMU without programming it.
///
/// # Safety
/// Executes `CPUID` or reads `PMCR_EL0`; requires ring 0 / EL1.
#[no_mangle]
pub unsafe extern "C" fn nc_bm_pmu_detect(out: *mut nc_bm_pmu_t) -> i32 {
    let Some(out) = (unsafe { out.as_mut() }) else {
        return -1;
    };
    let pmu = CorePmu::detect();
    *out = flatten(&pmu);
    0
}

/// Programs the counters and proves one advances.
///
/// Returns the route it settled on: 0 none, 1 fixed, 2 general-purpose. Zero
/// means no counter on this core moves, and no measurement should be reported
/// — see `CorePmu::enable`.
///
/// # Safety
/// Writes MSRs or PMU control registers; requires ring 0 / EL1, and must run
/// on the core it is programming.
#[no_mangle]
pub unsafe extern "C" fn nc_bm_pmu_enable(out: *mut nc_bm_pmu_t) -> u32 {
    let mut pmu = CorePmu::detect();
    // SAFETY: forwarded from this function's own contract.
    let route = unsafe { pmu.enable() };
    if let Some(out) = unsafe { out.as_mut() } {
        *out = flatten(&pmu);
    }
    route_code(route)
}

/// Reads the counter `nc_bm_pmu_enable` selected, into `out`.
///
/// Returns 1 on success, 0 if nothing counts on this core.
///
/// # Safety
/// Executes `RDPMC` or reads `PMCCNTR_EL0`; requires ring 0 / EL1.
#[no_mangle]
pub unsafe extern "C" fn nc_bm_pmu_read(pmu: *const nc_bm_pmu_t, out: *mut u64) -> i32 {
    let (Some(flat), Some(out)) = (unsafe { pmu.as_ref() }, unsafe { out.as_mut() }) else {
        return 0;
    };
    // Re-detect rather than trusting the caller's struct to describe *this*
    // core: on a hybrid part a struct filled on another core names counters
    // this one may not have.
    let mut live = CorePmu::detect();
    if route_code(live.route) != flat.route {
        // SAFETY: forwarded from this function's own contract.
        unsafe { live.enable() };
    }
    // SAFETY: as above.
    match unsafe { live.read_cycles() } {
        Some(r) => {
            *out = r.value;
            1
        }
        None => 0,
    }
}

/// The architectural counter, ordered. Nanoseconds are not implied: the unit
/// is counter ticks, and their rate is the platform's.
///
/// # Safety
/// Executes a counter read; requires ring 0 / EL1 on AArch64.
#[no_mangle]
pub unsafe extern "C" fn nc_bm_counter() -> u64 {
    crate::arch::counter_ordered()
}

fn flatten(pmu: &CorePmu) -> nc_bm_pmu_t {
    use nanochrono_core::pmu_leaf::CoreType;
    nc_bm_pmu_t {
        version: pmu.leaf.version as u32,
        general_counters: pmu.leaf.general_counters as u32,
        general_width: pmu.leaf.general_width as u32,
        fixed_counters: pmu.leaf.fixed_counters as u32,
        fixed_width: pmu.leaf.fixed_width as u32,
        core_type: match pmu.core_type {
            CoreType::Uniform => 0,
            CoreType::Performance => 1,
            CoreType::Efficiency => 2,
            CoreType::Unknown(_) => 3,
        },
        route: route_code(pmu.route),
        _pad: 0,
    }
}

fn route_code(route: crate::pmu::CounterRoute) -> u32 {
    use crate::pmu::CounterRoute;
    match route {
        CounterRoute::None => 0,
        CounterRoute::Fixed => 1,
        CounterRoute::General(_) => 2,
    }
}
