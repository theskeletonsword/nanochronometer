// SPDX-License-Identifier: Apache-2.0
//! Cryptographic primitives and TLS, backed by the rustls crypto provider.
//!
//! This crate replaces the old OpenSSL/BoringSSL and libsodium backends. There
//! is now one provider — [`ring`], the same one rustls uses — so the
//! AES-256-GCM that encrypts a TLS record and the AES-256-GCM the benchmark
//! panel times are the same code. The C build could not say that: it linked
//! two independent libraries and picked between them at CMake time.
//!
//! What that buys, concretely:
//!
//! * No `externals/openssl`, no `externals/libsodium`, no `SODIUM_STATIC`
//!   handling, no per-platform system import libraries. `cargo build` is the
//!   whole build.
//! * Benchmark numbers are comparable across modes, because they exercise one
//!   implementation rather than two with different assembly backends.
//! * TLS is rustls only. There is no OpenSSL fallback path to audit.
//!
//! # Scope
//!
//! These are measurement and transport primitives. Key management, password
//! hashing and long-term secret storage are deliberately absent — reach for a
//! dedicated crate for those.

use std::sync::Arc;

use ring::{aead, digest, hmac, rand::SecureRandom};

pub mod tls;

/// Size of a SHA-256 digest and an HMAC-SHA-256 tag.
pub const SHA256_LEN: usize = 32;
/// Size of an AEAD authentication tag for both supported ciphers.
pub const TAG_LEN: usize = 16;
/// AES-256 key length.
pub const AES256_KEY_LEN: usize = 32;
/// ChaCha20 key length.
pub const CHACHA20_KEY_LEN: usize = 32;
/// Nonce length for both supported AEADs.
pub const NONCE_LEN: usize = 12;

/// Name of the active crypto provider, for report headers and status lines.
pub const PROVIDER: &str = "rustls/ring";

/// Something went wrong in a primitive.
///
/// Deliberately coarse: distinguishing *why* an AEAD open failed is exactly
/// the kind of detail that turns into a padding oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// A key, nonce or buffer was the wrong length.
    BadLength,
    /// Authentication failed, or the operation could not complete.
    OperationFailed,
    /// The system RNG was unavailable.
    RandomUnavailable,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::BadLength => f.write_str("key, nonce or buffer has the wrong length"),
            CryptoError::OperationFailed => f.write_str("cryptographic operation failed"),
            CryptoError::RandomUnavailable => f.write_str("system random source unavailable"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// The primitives the benchmark modes and the toolkit expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha256,
    HmacSha256,
    /// AES-256-GCM. Hardware-accelerated wherever AES-NI or ARMv8 AES exists.
    Aes256Gcm,
    /// ChaCha20-Poly1305. Constant-time in software on every target.
    ChaCha20Poly1305,
}

impl Algorithm {
    pub const fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "SHA-256",
            Algorithm::HmacSha256 => "HMAC-SHA-256",
            Algorithm::Aes256Gcm => "AES-256-GCM",
            Algorithm::ChaCha20Poly1305 => "ChaCha20-Poly1305",
        }
    }

    /// Bytes of output overhead per operation: a tag for AEADs, a digest for
    /// hashes.
    pub const fn overhead(self) -> usize {
        match self {
            Algorithm::Sha256 | Algorithm::HmacSha256 => SHA256_LEN,
            Algorithm::Aes256Gcm | Algorithm::ChaCha20Poly1305 => TAG_LEN,
        }
    }

    pub const ALL: &'static [Algorithm] = &[
        Algorithm::Sha256,
        Algorithm::HmacSha256,
        Algorithm::Aes256Gcm,
        Algorithm::ChaCha20Poly1305,
    ];
}

/// SHA-256 of `message`.
pub fn sha256(message: &[u8]) -> [u8; SHA256_LEN] {
    let d = digest::digest(&digest::SHA256, message);
    let mut out = [0u8; SHA256_LEN];
    out.copy_from_slice(d.as_ref());
    out
}

/// HMAC-SHA-256 of `message` under `key`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; SHA256_LEN] {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&k, message);
    let mut out = [0u8; SHA256_LEN];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Verifies an HMAC-SHA-256 tag in constant time.
pub fn hmac_sha256_verify(key: &[u8], message: &[u8], tag: &[u8]) -> bool {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::verify(&k, message, tag).is_ok()
}

/// An AEAD sealing key bound to one algorithm.
///
/// Nonces are supplied per call rather than generated internally, because the
/// caller owns the uniqueness requirement — reusing a nonce under the same key
/// destroys confidentiality for both of these ciphers.
pub struct AeadKey {
    key: aead::LessSafeKey,
    algorithm: Algorithm,
}

impl std::fmt::Debug for AeadKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        f.debug_struct("AeadKey")
            .field("algorithm", &self.algorithm)
            .finish_non_exhaustive()
    }
}

impl AeadKey {
    /// Builds a key for `algorithm`. `key` must be exactly 32 bytes.
    pub fn new(algorithm: Algorithm, key: &[u8]) -> Result<AeadKey, CryptoError> {
        let alg = match algorithm {
            Algorithm::Aes256Gcm => &aead::AES_256_GCM,
            Algorithm::ChaCha20Poly1305 => &aead::CHACHA20_POLY1305,
            _ => return Err(CryptoError::BadLength),
        };
        let unbound = aead::UnboundKey::new(alg, key).map_err(|_| CryptoError::BadLength)?;
        Ok(AeadKey {
            key: aead::LessSafeKey::new(unbound),
            algorithm,
        })
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Encrypts `plaintext` in place, appending the authentication tag.
    ///
    /// `buffer` must contain the plaintext on entry; the tag is appended, so
    /// it grows by [`TAG_LEN`].
    pub fn seal(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buffer: &mut Vec<u8>,
    ) -> Result<(), CryptoError> {
        let nonce = aead::Nonce::assume_unique_for_key(*nonce);
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::from(aad), buffer)
            .map_err(|_| CryptoError::OperationFailed)
    }

    /// Decrypts and authenticates in place, returning the plaintext slice.
    ///
    /// On failure the buffer contents are unspecified and must not be used.
    pub fn open<'b>(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        buffer: &'b mut [u8],
    ) -> Result<&'b [u8], CryptoError> {
        let nonce = aead::Nonce::assume_unique_for_key(*nonce);
        self.key
            .open_in_place(nonce, aead::Aad::from(aad), buffer)
            .map(|p| &*p)
            .map_err(|_| CryptoError::OperationFailed)
    }
}

/// Fills `buffer` from the system CSPRNG.
pub fn random_bytes(buffer: &mut [u8]) -> Result<(), CryptoError> {
    let rng = ring::rand::SystemRandom::new();
    rng.fill(buffer).map_err(|_| CryptoError::RandomUnavailable)
}

/// Constant-time equality for secrets.
///
/// Compares every byte regardless of where the first difference is, so the
/// running time reveals only the length.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// The rustls crypto provider these primitives share.
///
/// Installing it as the process default makes every rustls config in the
/// program use `ring` without having to pass it explicitly.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Installs the `ring` provider as the process-wide rustls default.
///
/// Idempotent, and safe to call from several places: a second call is a
/// no-op rather than an error, because rustls only permits one installation
/// and racing to be first is not a failure worth propagating.
pub fn install_default_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NIST FIPS 180-2 test vector for "abc".
    #[test]
    fn sha256_matches_the_published_vector() {
        let digest = sha256(b"abc");
        assert_eq!(
            hex(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// RFC 4231 test case 1.
    #[test]
    fn hmac_sha256_matches_rfc4231() {
        let key = [0x0bu8; 20];
        let tag = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex(&tag),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert!(hmac_sha256_verify(&key, b"Hi There", &tag));
        assert!(!hmac_sha256_verify(&key, b"Hi there", &tag));
    }

    #[test]
    fn aead_round_trips_under_both_ciphers() {
        for algorithm in [Algorithm::Aes256Gcm, Algorithm::ChaCha20Poly1305] {
            let key = AeadKey::new(algorithm, &[0x42u8; 32]).unwrap();
            let nonce = [7u8; NONCE_LEN];
            let aad = b"nanochrono";

            let mut buffer = b"measured payload".to_vec();
            key.seal(&nonce, aad, &mut buffer).unwrap();
            assert_eq!(buffer.len(), b"measured payload".len() + TAG_LEN);

            let plaintext = key.open(&nonce, aad, &mut buffer).unwrap();
            assert_eq!(plaintext, b"measured payload", "{}", algorithm.name());
        }
    }

    #[test]
    fn aead_rejects_a_tampered_tag() {
        let key = AeadKey::new(Algorithm::Aes256Gcm, &[0x42u8; 32]).unwrap();
        let nonce = [7u8; NONCE_LEN];
        let mut buffer = b"payload".to_vec();
        key.seal(&nonce, b"", &mut buffer).unwrap();
        *buffer.last_mut().unwrap() ^= 0x01;
        assert_eq!(
            key.open(&nonce, b"", &mut buffer),
            Err(CryptoError::OperationFailed)
        );
    }

    #[test]
    fn aead_rejects_mismatched_aad() {
        let key = AeadKey::new(Algorithm::ChaCha20Poly1305, &[9u8; 32]).unwrap();
        let nonce = [3u8; NONCE_LEN];
        let mut buffer = b"payload".to_vec();
        key.seal(&nonce, b"context-a", &mut buffer).unwrap();
        assert!(key.open(&nonce, b"context-b", &mut buffer).is_err());
    }

    #[test]
    fn aead_rejects_a_short_key() {
        assert_eq!(
            AeadKey::new(Algorithm::Aes256Gcm, &[0u8; 16]).unwrap_err(),
            CryptoError::BadLength
        );
    }

    #[test]
    fn random_bytes_are_not_all_zero() {
        let mut buffer = [0u8; 64];
        random_bytes(&mut buffer).expect("system RNG must be available");
        assert!(buffer.iter().any(|&b| b != 0));
    }

    #[test]
    fn constant_time_eq_agrees_with_slice_eq() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreu"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
    }

    #[test]
    fn key_debug_does_not_leak_material() {
        let key = AeadKey::new(Algorithm::Aes256Gcm, &[0xABu8; 32]).unwrap();
        let rendered = format!("{key:?}");
        assert!(rendered.contains("Aes256Gcm"));
        assert!(!rendered.contains("171") && !rendered.contains("AB"));
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
