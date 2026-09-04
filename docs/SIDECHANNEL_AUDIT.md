# NanoChronometer side-channel audit extension

This patch adds defensive timing-measurement support to the NanoChronometer library,
not just GUI/CLI code.

## Library additions

Public declarations live in `nanochrono.h` and implementations in
`src/nanochrono_sidechannel.c`:

- `nc_sidechannel_audit_constant_time()`
  - fixed-vs-random local timing audit
  - Welch t-score
  - fixed/random sample statistics
  - heuristic `likely_timing_leak` flag
- `nc_measure_load_probe_asm_cycles()`
  - local load/cache timing probe wrapper
  - no cross-process target logic
- `nc_stopwatch_ns_*()`
  - nanosecond stopwatch API based on monotonic time
- `nc_benchmark_mode_cycles()`
  - `NC_BENCHMARK_BLACK_BOX_CRYPTO`
  - `NC_BENCHMARK_GRAY_BOX_INTERNAL`
  - `NC_BENCHMARK_WHITE_BOX_ASM`
- `nc_microbench_catalog_*()` and `nc_microbench_run()`
  - enumerates and runs architecture-specific white-box assembly kernels

## Assembly additions

The build now includes generated low-level assembly files:

- `asm/x64/linux/nanochrono_microbench_generated.asm`
- `asm/x64/windows/nanochrono_microbench_generated.asm`
- `asm/arm64/linux/nanochrono_microbench_generated.S`
- `asm/arm64/windows/nanochrono_microbench_generated.S`

Counts:

- x64: 300 exported white-box microbench symbols
- ARM64: 300 exported white-box microbench symbols

The x64 catalog groups functions into SISD, RDTSC, RDTSCP, LFENCE, MFENCE,
prefetch/cache, SSE2, AES-NI, PCLMULQDQ, SHA-NI, and AVX2 families. Optional
families are guarded by `nc_microbench_available()`.

The ARM64 catalog uses architectural equivalents rather than x86-only names:
`CNTVCT_EL0` counters, `ISB`/`DMB`/`DSB` barriers, scalar SISD, memory probes,
branch probes, and NEON SIMD.

## CLI additions

`nanochrono_cli.c` now has:

- `--catalog` to list library microbench entries
- `--whitebox` to run one available ASM microbench
- `--sct-audit` to run a local defensive constant-time timing audit demo

## Safety boundary

The side-channel code is an auditing harness for code and buffers owned by the
caller. It does not include secret-recovery logic, victim orchestration, or
cross-process attack automation.

## Low-level ASM side-channel measurement API

The library now exposes a defensive, local-only ASM measurement layer in addition
to the C statistical audit API. These symbols are available from the static or
dynamic library when the corresponding architecture is built.

Generic wrapper API:

```c
nc_asm_sidechannel_probe(ctx, NC_ASM_SC_PROBE_LOAD_FENCED, ptr, 0, 0, &result);
nc_asm_sidechannel_probe(ctx, NC_ASM_SC_PROBE_FLUSH_RELOAD, ptr, 0, 0, &result); /* x64 */
nc_asm_sidechannel_probe(ctx, NC_ASM_SC_PROBE_PREFETCH_RELOAD, ptr, 0, 0, &result);
nc_asm_sidechannel_cache_audit(ctx, ptr, &load_r, &reload_r, &prefetch_r);
```

Direct x64 ASM entry points return TSC cycles:

- `nc_x64_sc_rdtsc_lfence`
- `nc_x64_sc_rdtscp_lfence`
- `nc_x64_sc_measure_load_cycles`
- `nc_x64_sc_measure_load_lfence_cycles`
- `nc_x64_sc_measure_store_cycles`
- `nc_x64_sc_measure_flush_reload_cycles`
- `nc_x64_sc_measure_prefetch_reload_cycles`
- `nc_x64_sc_measure_branch_cycles`
- `nc_x64_sc_pointer_chase_cycles`
- `nc_x64_sc_barrier_overhead_cycles`

Direct ARM64 ASM entry points return `CNTVCT_EL0` ticks:

- `nc_arm64_sc_cntvct_isb`
- `nc_arm64_sc_measure_load_ticks`
- `nc_arm64_sc_measure_load_isb_ticks`
- `nc_arm64_sc_measure_store_ticks`
- `nc_arm64_sc_measure_prefetch_load_ticks`
- `nc_arm64_sc_measure_branch_ticks`
- `nc_arm64_sc_pointer_chase_ticks`
- `nc_arm64_sc_barrier_overhead_ticks`

ARM64 does not expose x64 `RDTSC`, `RDTSCP`, `LFENCE`, or `CLFLUSH`; the ARM64
implementation uses `CNTVCT_EL0`, `ISB`, `DMB`, `DSB`, and `PRFM` instead.
Crypto benchmarking remains delegated to OpenSSL, BoringSSL, or libsodium and is
not reimplemented in these ASM probes.


## SIMD ASM API coverage

The defensive side-channel/timing API is now implemented not only in scalar or
legacy assembler files, but also in every SIMD family file shipped by the tree.

### x64 families

MMX, SSE, SSE2, SSE3, SSSE3, SSE4.1, SSE4.2, F16C, FMA, AVX, AVX2,
AVX-VNNI, AVX-512 and AVX-512 VNNI each export the same `nc_<family>_sc_*`
entry-point set:

- `*_sc_counter()`
- `*_sc_load_cycles(ptr)`
- `*_sc_store_cycles(ptr, value)`
- `*_sc_vector_load_cycles(ptr)`
- `*_sc_vector_xor_cycles(a, b, out, bytes)`
- `*_sc_barrier_cycles(iterations)`

### ARM64 families

NEON, SVE, SVE2 and SME each export the same `nc_arm64_<family>_sc_*` entry-point
set using `CNTVCT_EL0` ticks:

- `*_sc_counter()`
- `*_sc_load_ticks(ptr)`
- `*_sc_store_ticks(ptr, value)`
- `*_sc_vector_load_ticks(ptr)`
- `*_sc_vector_xor_ticks(a, b, out, bytes)`
- `*_sc_barrier_ticks(iterations)`

The C wrapper `nc_asm_simd_probe()` dispatches to those direct assembler symbols
and checks feature availability before running optional ISA code. Crypto remains
delegated to OpenSSL, BoringSSL or libsodium; these SIMD functions are local
low-level timing probes only.
