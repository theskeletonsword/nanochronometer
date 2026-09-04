# NanoChrono timer backends

The status bar and `nc_backend_name()` report only the backend used by the nanosecond counter.
Crypto instruction-set extensions are not timer backends.

Valid timer backends:

- legacy scalar ASM
- MMX
- SSE
- SSE2
- SSE3
- SSSE3
- SSE4.1
- SSE4.2
- AVX
- F16C
- FMA
- AVX2
- AVX-VNNI
- AVX-512
- AVX-512 VNNI

AES-NI, PCLMULQDQ, SHA-NI and VAES are crypto acceleration features. They are exposed only as CPU feature flags or via the OpenSSL EVP / libsodium benchmark providers. They must not appear as counter backends.

The GUI CPU/timer panel lists only the timer backends above. The OpenSSL EVP and libsodium panels list crypto operations such as AES-256-GCM, SHA-256 and ChaCha20-Poly1305.

## GUI startup hardening

The GUI no longer executes ISA-specific instructions while creating the timer
context. The per-family `.asm` files are still split by filename and symbol, but
`*_tsc_start`, `*_tsc_end`, and `*_tsc_overhead` use safe TSC primitives only.
This prevents startup crashes from unsupported opcodes.

For emergency fallback:

```bat
dist\nanochrono_gui.exe --safe
```

or:

```bat
set NANOCHRONO_BACKEND=legacy
```

`nc_avx_check` was also corrected so it only executes `XGETBV` after confirming
both CPUID OSXSAVE and AVX bits are set.
