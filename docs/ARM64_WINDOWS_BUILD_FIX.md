# ARM64 Windows build fix

This archive was patched to make the MinGW ARM64 Windows target compile further:

- Removed ELF-only `.type` / `.size` directives from `asm/arm64/windows/*.S`, because Windows COFF assembly rejects them.
- Guarded x86-only headers/intrinsics (`cpuid.h`, `immintrin.h`, `xmmintrin.h`, etc.) so they are not included when targeting ARM64.
- Added non-x86 benchmark stubs for x86 benchmark entry points so the GUI/CLI code can link on ARM64 while x86-only kernels remain unavailable through feature detection.

Build from WSL with llvm-mingw in PATH:

```sh
export PATH="/opt/llvm-mingw/bin:$PATH"
scripts/build_mingw_ninja_arm64.sh
```
