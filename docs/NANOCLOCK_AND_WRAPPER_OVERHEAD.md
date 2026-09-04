# Nanosecond clock, route selection, and wrapper overhead

This patch adds a library-level nanosecond clock API plus CLI and GUI views.

## Core C API

- `nc_unix_time_ns()` returns UTC wall-clock nanoseconds.
- `nc_nanoclock_snapshot()` returns wall-clock, monotonic, process, thread, raw
  counter, selected-route, SIMD-route, and overhead values in one ABI-stable
  struct.
- `nc_clock_select_best_route()` chooses the strongest available route. It
  prefers the best available SIMD counter family first (`AVX-512 VNNI`,
  `AVX-512`, `AVX2`, `AVX`, `SSE*`, `MMX`, `SME`, `SVE2`, `SVE`, `NEON`) and
  falls back to `RDTSCP+LFENCE`, `CNTVCT_EL0+ISB`, or monotonic time.
- x64 snapshots include raw `RDTSC`, `LFENCE+RDTSC`, `MFENCE+LFENCE+RDTSC`, and
  `RDTSCP+LFENCE`.
- ARM64 snapshots include `CNTFRQ_EL0`, `CNTVCT_EL0`, `CNTVCT_EL0+ISB`, and
  optional `PMCCNTR_EL0`.

## ARM64 PMCCNTR_EL0

`PMCCNTR_EL0` often traps unless EL0 PMU access is enabled by the OS/kernel.
The assembler symbol `nc_arm64_pmccntr_el0()` is present. Automatic GUI/CLI
snapshots call it only when `NANOCHRONO_USE_PMCCNTR_EL0=1` is set, so the UI
stays safe on systems where user-mode PMU access is disabled.

## CLI

- `--ns-clock` shows a live clock snapshot every 250 ms.
- `--ns-clock-once` prints one snapshot.
- `--wrapper-overhead` prints native wrapper baselines.
- `--asm-simd` runs low-level SIMD probes across available families.

## GUI

The title bar now has a clock on the left side of `NanoChrono`. Press `C` or
click the `DIGITAL`/`ANALOG` button to switch clock designs. The main display
also shows UTC nanoseconds, selected route, raw counter values, selected SIMD
family, and Python `ctypes` wrapper-overhead hint.

## Language wrappers

Python, Node.js, and Lua wrappers now expose language-side overhead helpers.
Python `ctypes` wraps `nc_empty_call()` with `nc_now_cycles()` to measure real
interpreter + FFI overhead rather than only the native C baseline.
