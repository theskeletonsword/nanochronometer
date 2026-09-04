# NanoChronometer 0.5.6 build fix

Fixes the MinGW GUI link failure:

```text
undefined reference to `nc_bench_kernel_mmx'
undefined reference to `nc_bench_kernel_asm_legacy'
```

The GUI benchmark dispatcher in `bench_kernels.c` calls two legacy GUI-only ASM kernels. Their implementation already existed in:

```text
asm/x64/windows/bench_kernels_asm.asm
```

but that file was not part of the `nanochrono_gui` target. `CMakeLists.txt` now appends it when building the Windows x64 GUI, so MinGW/Ninja links `nanochrono_gui.exe`.
