# ASM stable clock path

This build includes the `/asm` tree again. The stable clock calibration is not only C-level: the raw counter reads used by `nc_clock_read_raw_route()` are backed by exported assembly symbols.

## x64 low-level exported symbols

Implemented in:

- `asm/x64/linux/nanochrono_legacy.asm`
- `asm/x64/windows/nanochrono_legacy.asm`

Key symbols:

- `nc_x64_sc_rdtsc_lfence()`
- `nc_x64_sc_rdtsc_mfence()`
- `nc_x64_sc_rdtscp_lfence(uint32_t *aux_out)`
- `nc_x64_sc_measure_load_cycles()`
- `nc_x64_sc_measure_load_lfence_cycles()`
- `nc_x64_sc_measure_store_cycles()`
- `nc_x64_sc_measure_flush_reload_cycles()`
- `nc_x64_sc_measure_prefetch_reload_cycles()`
- `nc_x64_sc_measure_branch_cycles()`
- `nc_x64_sc_pointer_chase_cycles()`
- `nc_x64_sc_barrier_overhead_cycles()`

The `cycles_per_ns` calibration path can therefore use raw RDTSC/RDTSCP reads without kernel calls on the hot path.

## x64 SIMD timing symbols

Implemented across:

- `nanochrono_mmx.asm`
- `nanochrono_sse.asm`
- `nanochrono_sse2.asm`
- `nanochrono_sse3.asm`
- `nanochrono_ssse3.asm`
- `nanochrono_sse41.asm`
- `nanochrono_sse42.asm`
- `nanochrono_f16c.asm`
- `nanochrono_fma.asm`
- `nanochrono_avx.asm`
- `nanochrono_avx2.asm`
- `nanochrono_avx_vnni.asm`
- `nanochrono_avx512.asm`
- `nanochrono_avx512_vnni.asm`

Each family exports counter/load/store/vector-load/vector-xor/barrier style probes.

## ARM64 low-level exported symbols

Implemented in:

- `asm/arm64/linux/nanochrono_arm64_scalar.S`
- `asm/arm64/windows/nanochrono_arm64_scalar.S`

Key symbols:

- `nc_arm64_cntvct()`
- `nc_arm64_cntfrq()`
- `nc_arm64_pmccntr_el0()`
- `nc_arm64_sc_cntvct_isb()`
- `nc_arm64_sc_measure_load_ticks()`
- `nc_arm64_sc_measure_load_isb_ticks()`
- `nc_arm64_sc_measure_store_ticks()`
- `nc_arm64_sc_measure_prefetch_load_ticks()`
- `nc_arm64_sc_measure_branch_ticks()`
- `nc_arm64_sc_pointer_chase_ticks()`
- `nc_arm64_sc_barrier_overhead_ticks()`

`PMCCNTR_EL0` may be blocked by the OS unless EL0 PMU access is enabled; the framework treats it as an optional route.

## Conversion

The intended HFT/bench flow is:

```text
pin CPU / stabilize frequency
calibrate raw units over a known monotonic interval
cycles_per_ns = elapsed_cycles / elapsed_ns
ns = cycles / cycles_per_ns
```

The C API remains the canonical entry point, but the raw reads are backed by these assembly symbols so the hot path can avoid kernel/API overhead where the platform permits it.
