# GUI main split for Windows builds

This package separates the GUI entry source by target architecture:

- `main_x64.c`: full x64 GUI entry source.
- `main_arm64.c`: ARM64 Windows GUI entry source with runtime DPI-awareness lookup so MinGW/LLVM ARM64 does not fail on an undeclared `SetProcessDpiAwarenessContext`.

`CMakeLists.txt` now picks `main_x64.c` when `NC_ARCH=x64`, `main_arm64.c` when `NC_ARCH=arm64`, and falls back to `main.c` otherwise.

Also fixed `bench_kernels.c` so SHA/x86 intrinsics are not included directly on ARM64.
