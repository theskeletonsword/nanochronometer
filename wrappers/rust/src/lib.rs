#![allow(non_camel_case_types)]
use core::ffi::{c_char, c_double, c_int, c_uint, c_void};

#[repr(C)]
pub struct nc_ctx_t { _private: [u8; 0] }

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SampleStats {
    pub count: u64,
    pub min_cycles: u64,
    pub max_cycles: u64,
    pub mean_cycles: u64,
    pub median_cycles: u64,
    pub p90_cycles: u64,
    pub p99_cycles: u64,
    pub variance_cycles: f64,
    pub stdev_cycles: f64,
}

pub type VoidCallback = extern "C" fn(*mut c_void);

#[link(name = "nanochrono")]
extern "C" {
    fn nc_create() -> *mut nc_ctx_t;
    fn nc_destroy(ctx: *mut nc_ctx_t);
    fn nc_calibrate(ctx: *mut nc_ctx_t, ms: c_uint) -> c_int;
    fn nc_start(ctx: *mut nc_ctx_t) -> u64;
    fn nc_stop(ctx: *mut nc_ctx_t) -> u64;
    fn nc_now_cycles(ctx: *mut nc_ctx_t) -> u64;
    fn nc_cycles_to_ns(ctx: *mut nc_ctx_t, cycles: u64) -> u64;
    fn nc_elapsed_ns(ctx: *mut nc_ctx_t) -> u64;
    fn nc_measure_overhead_cycles(ctx: *mut nc_ctx_t) -> u64;
    fn nc_measure_ffi_overhead_cycles(ctx: *mut nc_ctx_t, iters: c_uint) -> u64;
    fn nc_measure_callback_min_cycles(ctx: *mut nc_ctx_t, cb: VoidCallback, arg: *mut c_void, iters: c_uint) -> u64;
    fn nc_measure_callback_avg_cycles(ctx: *mut nc_ctx_t, cb: VoidCallback, arg: *mut c_void, iters: c_uint) -> u64;
    fn nc_collect_samples_cycles(ctx: *mut nc_ctx_t, cb: VoidCallback, arg: *mut c_void, samples: *mut u64, count: c_uint) -> c_int;
    fn nc_analyze_samples(samples: *const u64, count: c_uint, out_stats: *mut SampleStats) -> c_int;
    fn nc_welch_t_score(a: *const u64, na: c_uint, b: *const u64, nb: c_uint) -> c_double;
    fn nc_toolkit_version() -> *const c_char;
    fn nc_empty_call() -> u64;
}

pub struct NanoChronometer { raw: *mut nc_ctx_t }
unsafe impl Send for NanoChronometer {}

impl NanoChronometer {
    pub fn new() -> Option<Self> {
        let raw = unsafe { nc_create() };
        if raw.is_null() { None } else { Some(Self { raw }) }
    }
    pub fn calibrate(&mut self, ms: u32) -> bool { unsafe { nc_calibrate(self.raw, ms) != 0 } }
    pub fn start(&mut self) -> u64 { unsafe { nc_start(self.raw) } }
    pub fn stop(&mut self) -> u64 { unsafe { nc_stop(self.raw) } }
    pub fn now_cycles(&self) -> u64 { unsafe { nc_now_cycles(self.raw) } }
    pub fn cycles_to_ns(&self, cycles: u64) -> u64 { unsafe { nc_cycles_to_ns(self.raw, cycles) } }
    pub fn elapsed_ns(&self) -> u64 { unsafe { nc_elapsed_ns(self.raw) } }
    pub fn native_overhead_cycles(&self) -> u64 { unsafe { nc_measure_overhead_cycles(self.raw) } }
    pub fn ffi_overhead_cycles(&self, iterations: u32) -> u64 { unsafe { nc_measure_ffi_overhead_cycles(self.raw, iterations) } }
    pub fn callback_min_cycles(&self, cb: VoidCallback, arg: *mut c_void, iterations: u32) -> u64 {
        unsafe { nc_measure_callback_min_cycles(self.raw, cb, arg, iterations) }
    }
    pub fn callback_avg_cycles(&self, cb: VoidCallback, arg: *mut c_void, iterations: u32) -> u64 {
        unsafe { nc_measure_callback_avg_cycles(self.raw, cb, arg, iterations) }
    }
    pub fn collect_samples(&self, cb: VoidCallback, arg: *mut c_void, count: usize) -> Option<Vec<u64>> {
        let mut samples = vec![0u64; count];
        let ok = unsafe { nc_collect_samples_cycles(self.raw, cb, arg, samples.as_mut_ptr(), count as u32) } != 0;
        ok.then_some(samples)
    }
}
impl Drop for NanoChronometer { fn drop(&mut self) { unsafe { nc_destroy(self.raw) } } }

pub fn analyze_samples(samples: &[u64]) -> Option<SampleStats> {
    let mut s = SampleStats::default();
    let ok = unsafe { nc_analyze_samples(samples.as_ptr(), samples.len() as u32, &mut s) } != 0;
    ok.then_some(s)
}

pub fn welch_t_score(a: &[u64], b: &[u64]) -> f64 {
    unsafe { nc_welch_t_score(a.as_ptr(), a.len() as u32, b.as_ptr(), b.len() as u32) }
}

pub fn empty_call() -> u64 { unsafe { nc_empty_call() } }

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InstructionResult { pub status: c_int, pub family: u32, pub backend: u32, pub cycles: u64, pub ns: u64, pub blocks: u64, pub checksum: u64 }
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SideChannelResult { pub status: c_int, pub cached_cycles: u64, pub flushed_cycles: u64, pub prefetched_cycles: u64, pub threshold_cycles: u64, pub separation_score: f64 }
#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum InstructionFamily { Scalar=0, Aes=1, Sha=2, Pclmulqdq=3, Crc32=4, Sse2=5, Avx2=6, Avx512=7, Neon=8, Sve=9, Sve2=10, Sme=11 }
extern "C" {
    fn nc_instruction_family_available(family: c_uint) -> c_int;
    fn nc_measure_instruction_family_cycles(ctx: *mut nc_ctx_t, family: c_uint, iterations: c_uint, out: *mut InstructionResult) -> u64;
    fn nc_measure_aesenc_cycles(ctx: *mut nc_ctx_t, blocks: c_uint, out: *mut InstructionResult) -> u64;
    fn nc_measure_sha256msg_cycles(ctx: *mut nc_ctx_t, blocks: c_uint, out: *mut InstructionResult) -> u64;
    fn nc_measure_pclmul_cycles(ctx: *mut nc_ctx_t, blocks: c_uint, out: *mut InstructionResult) -> u64;
    fn nc_measure_cache_probe_cycles(ctx: *mut nc_ctx_t, ptr: *const c_void, out: *mut SideChannelResult) -> u64;
    fn nc_crypto_backend_mask() -> c_int;
}
impl NanoChronometer {
    pub fn instruction_available(f: InstructionFamily) -> bool { unsafe { nc_instruction_family_available(f as u32) != 0 } }
    pub fn measure_instruction(&self, f: InstructionFamily, iterations: u32) -> InstructionResult { let mut r=InstructionResult::default(); unsafe{ nc_measure_instruction_family_cycles(self.raw, f as u32, iterations, &mut r); } r }
    pub fn measure_aes(&self, blocks: u32) -> InstructionResult { let mut r=InstructionResult::default(); unsafe{ nc_measure_aesenc_cycles(self.raw, blocks, &mut r); } r }
    pub fn measure_sha_instr(&self, blocks: u32) -> InstructionResult { let mut r=InstructionResult::default(); unsafe{ nc_measure_sha256msg_cycles(self.raw, blocks, &mut r); } r }
    pub fn measure_pclmul(&self, blocks: u32) -> InstructionResult { let mut r=InstructionResult::default(); unsafe{ nc_measure_pclmul_cycles(self.raw, blocks, &mut r); } r }
    pub fn cache_probe(&self, ptr: *const c_void) -> SideChannelResult { let mut r=SideChannelResult::default(); unsafe{ nc_measure_cache_probe_cycles(self.raw, ptr, &mut r); } r }
    pub fn crypto_backend_mask() -> i32 { unsafe { nc_crypto_backend_mask() as i32 } }
}
