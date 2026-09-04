# GUI safe start / illegal instruction hardening

If the GUI does not open on a specific CPU/VM, start it in legacy-safe mode:

```bat
dist\nanochrono_gui.exe --safe
```

or force a backend:

```bat
set NANOCHRONO_BACKEND=legacy
dist\nanochrono_gui.exe
```

Supported forced values:

```text
legacy, mmx, sse, sse2, sse3, ssse3, sse41, sse42,
avx, f16c, fma, avx2, avx-vnni, avx512, avx512vnni
```

Timer backend `.asm` files intentionally do not execute their family-specific
SIMD/VNNI instruction during GUI startup. They use safe `LFENCE/RDTSC/RDTSCP`
entry points. Feature-specific instructions are only for explicit benchmark
kernels after CPUID/XGETBV gating.

Also fixed: `nc_avx_check` no longer uses the parity flag to test OSXSAVE+AVX.
The previous code could execute `XGETBV` even when OSXSAVE was absent, causing
an illegal-instruction crash before the GUI appeared.
