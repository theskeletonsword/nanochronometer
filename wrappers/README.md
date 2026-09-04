# NanoChronometer language wrappers

These wrappers intentionally stay thin: each one crosses the language boundary as directly as possible so the toolkit can measure the cost of FFI/DLL/shared-library calls instead of hiding it behind a large framework.

Expected native library names:

- Windows MSVC/MinGW: `nanochrono.dll`
- Linux: `libnanochrono.so`
- macOS, if you build it yourself: `libnanochrono.dylib`

Recommended runtime placement:

- Put the native library next to the executable/script, or
- set `NANOCHRONO_LIB=/absolute/path/to/libnanochrono.so` or `nanochrono.dll`, or
- use the platform loader path: `PATH`, `LD_LIBRARY_PATH`, or `DYLD_LIBRARY_PATH`.

Wrappers included:

- Rust crate using direct C ABI.
- Go package using cgo.
- C#/.NET P/Invoke wrapper.
- Java JNA wrapper.
- Node.js wrapper using `ffi-napi`.
- Zig wrapper using `@cImport`.
- LuaJIT FFI wrapper.

The stable ABI surface used by all wrappers is the C API in `nanochrono.h`.


## Nanosecond clock and wrapper overhead

The core library exports `nc_nanoclock_snapshot()` and `nc_nanoclock_now_ns()`.
Snapshots include UTC wall-clock nanoseconds, monotonic/process/thread time, raw
x64 `RDTSC`, `LFENCE+RDTSC`, `MFENCE+LFENCE+RDTSC`, `RDTSCP+LFENCE`, ARM64
`CNTFRQ_EL0`, `CNTVCT_EL0`, `CNTVCT_EL0+ISB`, optional `PMCCNTR_EL0`, and the
auto-selected best SIMD counter route (`AVX-512`, `AVX2`, `SSE*`, `NEON`,
`SVE*`, `SME`, etc.).

Each wrapper should report two values:

1. `nativeWrapperBaselineCycles`: measured inside the C library.
2. `*CallOverheadCycles`: measured from the language wrapper itself, so it
   includes FFI marshalling/interpreter overhead such as Python `ctypes`,
   Node `ffi-napi`, LuaJIT FFI, JNA, P/Invoke, cgo, Rust FFI, or Zig FFI.

Python `ctypes` support is in `python/nanochronometer/core.py`; run
`python/nanoclock_ctypes_overhead.py` or `wrappers/python/overhead_ctypes.py`.
ARM64 `PMCCNTR_EL0` is opt-in for automatic snapshots because many OS kernels
trap EL0 PMU access by default; set `NANOCHRONO_USE_PMCCNTR_EL0=1` after enabling
PMU user access if you want the clock to read it.
