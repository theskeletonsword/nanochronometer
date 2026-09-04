# NanoChronometer unified dispatch model

NanoChronometer is intentionally built like OpenSSL/FFmpeg: one library per OS and CPU architecture, not one library per instruction family.

Examples:

- Windows x64: `nanochrono.dll` + import/static `nanochrono.lib`
- MinGW x64: `libnanochrono.dll.a` / `libnanochrono.a` + `nanochrono.dll`
- Linux x64: `libnanochrono.so` / `libnanochrono.a`
- Windows ARM64: ARM64 `nanochrono.dll` / `nanochrono.lib`
- Linux ARM64: ARM64 `libnanochrono.so` / `libnanochrono.a`

An x64 `.a` or `.lib` is never interchangeable with an ARM64 `.a` or `.lib`. The unification is within each OS/architecture build: AES, SHA, PCLMULQDQ, CRC32, SSE/AVX/AVX512, NEON/SVE/SVE2/SME are exposed through one stable public API and selected at runtime.

## Runtime safety

Public measurement calls first check CPU capability:

- unsupported x64 instruction family -> `NC_ERR_UNSUPPORTED`
- unsupported ARM64 family -> `NC_ERR_UNSUPPORTED`
- linked crypto backend missing -> `NC_ERR_CRYPTO_BACKEND`

This prevents accidental `illegal instruction` crashes when a CPU does not support AES-NI, SHA-NI, AVX512, SVE, etc.

## New instruction-family APIs

- `nc_instruction_family_available()`
- `nc_measure_instruction_family_cycles()`
- `nc_measure_aesenc_cycles()`
- `nc_measure_sha256msg_cycles()`
- `nc_measure_pclmul_cycles()`
- `nc_measure_vector_add_cycles()`
- `nc_measure_vector_xor_cycles()`
- `nc_measure_cache_probe_cycles()`
- `nc_measure_branch_mispredict_cycles()`

## New low-level ASM symbols

x64 `.asm`:

- `nc_asm_aesenc_128_loop`
- `nc_asm_aesdec_128_loop`
- `nc_asm_pclmul_loop`
- `nc_asm_sha256rnds2_loop`
- `nc_asm_crc32_loop`
- `nc_asm_vector_add_u64_loop`
- `nc_asm_vector_xor_loop`
- `nc_asm_branch_probe_loop`

ARM64 `.S`:

- `nc_arm64_neon_aes_loop`
- `nc_arm64_neon_sha256_loop`
- `nc_arm64_neon_pmull_loop`
- `nc_arm64_neon_vector_xor_loop`
- `nc_arm64_sve_vector_xor_loop`

## Crypto timing APIs

These are wrappers for timing crypto operations, not replacements for OpenSSL/libsodium:

- `nc_crypto_backend_mask()`
- `nc_crypto_rand_cycles()`
- `nc_crypto_sha256_cycles()`
- `nc_crypto_hmac_sha256_cycles()`
- `nc_crypto_aead_chacha20poly1305_cycles()`

x64 externals use OpenSSL/libsodium. ARM64 externals use BoringSSL/libsodium; OpenSSL is intentionally not included under ARM64.
