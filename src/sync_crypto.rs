//! Client-side cryptography for encrypted sync.
//!
//! The recovery phrase never leaves this process. It is normalised, stretched
//! with Argon2id and a per-org salt, and used as an XChaCha20-Poly1305 key. The
//! server receives only `nonce || ciphertext || tag`, addressed by the BLAKE3
//! digest of those bytes.
//!
//! The manifest is encrypted the same way, but its *plaintext* serialisation is
//! canonical: blob order, clock order and tombstone order are fixed before the
//! bytes are hashed or encrypted. Two devices that describe the same state
//! therefore produce the same manifest hash even when their in-memory vectors
//! were assembled in a different order.
//!
//! Key material lives in [`Zeroizing`] buffers. A wrong phrase decrypts to an
//! authentication error; nothing in this module opens, truncates or rewrites a
//! local file, so the caller can fail closed before it applies anything.

use std::collections::BTreeMap;
use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Bytes in a per-org KDF salt.
pub const SALT_BYTES: usize = 16;
/// Bytes in an XChaCha20 nonce.
pub const NONCE_BYTES: usize = 24;
/// Bytes in the derived org key.
pub const KEY_BYTES: usize = 32;
/// Argon2id memory cost, in KiB (19 MiB).
pub const ARGON2_M_COST_KIB: u32 = 19 * 1024;
/// Argon2id iteration count.
pub const ARGON2_T_COST: u32 = 2;
/// Argon2id parallelism.
pub const ARGON2_P_COST: u32 = 1;

/// A failure that is safe to report to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The phrase contained no non-whitespace characters.
    EmptyPhrase,
    /// Argon2 could not derive a key from the parameters or salt.
    KeyDerivation,
    /// The operating system could not supply salt/nonce randomness.
    Random,
    /// The salt was not exactly [`SALT_BYTES`] bytes of hex.
    InvalidSalt,
    /// The envelope is too short to contain a nonce and authentication tag.
    InvalidEnvelope,
    /// The key is wrong, or the ciphertext/tag was modified.
    AuthenticationFailed,
    /// A decrypted manifest was not valid JSON for [`Manifest`].
    MalformedManifest,
    /// A manifest contained a field that could not be serialised canonically.
    ManifestSerialisation,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyPhrase => "the recovery phrase is empty",
            Self::KeyDerivation => "the recovery phrase could not be stretched into a key",
            Self::Random => "the operating system could not supply randomness",
            Self::InvalidSalt => "the org salt is not 16 bytes of hex",
            Self::InvalidEnvelope => "the encrypted envelope is truncated",
            Self::AuthenticationFailed => {
                "the recovery phrase is wrong or the ciphertext was changed"
            }
            Self::MalformedManifest => "the decrypted manifest is not valid",
            Self::ManifestSerialisation => "the manifest could not be serialised deterministically",
        })
    }
}

impl std::error::Error for CryptoError {}

/// A per-org salt. Salts are not secret; they are stored beside the ciphertext
/// so a second machine can derive the same key from the same phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrgSalt([u8; SALT_BYTES]);

impl OrgSalt {
    pub const fn from_bytes(bytes: [u8; SALT_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn generate() -> Result<Self, CryptoError> {
        let mut bytes = [0_u8; SALT_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| CryptoError::Random)?;
        Ok(Self(bytes))
    }

    pub fn from_hex(hex: &str) -> Result<Self, CryptoError> {
        let decoded = hex::decode(hex).map_err(|_| CryptoError::InvalidSalt)?;
        let bytes: [u8; SALT_BYTES] = decoded.try_into().map_err(|_| CryptoError::InvalidSalt)?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; SALT_BYTES] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// A reproducible per-org salt for machines that share only the phrase and
    /// the org identity. A salt is public; it only has to differ between orgs.
    /// Domain separation keeps this value from colliding with any other use of
    /// the org identifier.
    pub fn for_org(org: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"memnoth-sync-org-salt-v1:");
        hasher.update(org.trim().to_ascii_lowercase().as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; SALT_BYTES];
        bytes.copy_from_slice(&digest.as_bytes()[..SALT_BYTES]);
        Self(bytes)
    }
}

/// The derived key, zeroized when it is dropped.
pub struct OrgKey {
    key: Zeroizing<[u8; KEY_BYTES]>,
}

impl fmt::Debug for OrgKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OrgKey([redacted])")
    }
}

impl OrgKey {
    /// Derive the org key from a recovery phrase and its salt.
    ///
    /// Whitespace is collapsed and ASCII case is folded before derivation, so a
    /// phrase pasted with different spacing or capitalisation opens the same
    /// vault. The phrase itself is held in a zeroizing buffer.
    pub fn derive(phrase: &str, salt: &OrgSalt) -> Result<Self, CryptoError> {
        let normalized = Zeroizing::new(normalize_phrase(phrase));
        if normalized.is_empty() {
            return Err(CryptoError::EmptyPhrase);
        }
        let params = Params::new(
            ARGON2_M_COST_KIB,
            ARGON2_T_COST,
            ARGON2_P_COST,
            Some(KEY_BYTES),
        )
        .map_err(|_| CryptoError::KeyDerivation)?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = [0_u8; KEY_BYTES];
        argon2
            .hash_password_into(normalized.as_bytes(), salt.as_bytes(), &mut key)
            .map_err(|_| CryptoError::KeyDerivation)?;
        Ok(Self {
            key: Zeroizing::new(key),
        })
    }

    fn as_bytes(&self) -> &[u8; KEY_BYTES] {
        &self.key
    }
}

fn normalize_phrase(phrase: &str) -> String {
    phrase
        .split_whitespace()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Encrypt with a fresh OS-generated nonce.
pub fn encrypt(key: &OrgKey, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| CryptoError::Random)?;
    encrypt_with_nonce(key, plaintext, &nonce)
}

/// Encrypt with a caller-supplied nonce.
///
/// This exists for committed test vectors and for resuming a manifest whose
/// ciphertext must be retried byte-for-byte. Production callers use [`encrypt`].
pub fn encrypt_with_nonce(
    key: &OrgKey,
    plaintext: &[u8],
    nonce: &[u8; NONCE_BYTES],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .map_err(|_| CryptoError::KeyDerivation)?;
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(nonce), plaintext)
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    let mut envelope = Vec::with_capacity(NONCE_BYTES + ciphertext.len());
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

/// Decrypt an envelope produced by [`encrypt`] or [`encrypt_with_nonce`].
///
/// The plaintext is returned in a zeroizing buffer. Authentication happens
/// before any caller can read the bytes, so a wrong phrase cannot yield a
/// partial manifest.
pub fn decrypt(key: &OrgKey, envelope: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if envelope.len() < NONCE_BYTES + 16 {
        return Err(CryptoError::InvalidEnvelope);
    }
    let (nonce, ciphertext) = envelope.split_at(NONCE_BYTES);
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .map_err(|_| CryptoError::KeyDerivation)?;
    let plaintext = cipher
        .decrypt(XNonce::from_slice(nonce), ciphertext)
        .map_err(|_| CryptoError::AuthenticationFailed)?;
    Ok(Zeroizing::new(plaintext))
}

/// BLAKE3 in the lowercase hex form the sync wire contract uses.
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// One encrypted object named in a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestBlob {
    pub hash: String,
    pub size: u64,
    /// Per-object vector clock. `BTreeMap` is what makes its serialisation
    /// independent of insertion order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clock: BTreeMap<String, u64>,
}

/// The encrypted sync manifest's plaintext.
///
/// The manifest is versioned by `generation`, names the ciphertext blobs, and
/// carries tombstones for objects that were deleted. It never contains a key;
/// the key is derived from the recovery phrase and the org salt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub org: String,
    pub device: String,
    pub generation: u64,
    pub blobs: Vec<ManifestBlob>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstones: Vec<String>,
}

impl Manifest {
    /// Canonical plaintext bytes: blobs sorted by hash (then clock), tombstones
    /// sorted, and maps serialised through `BTreeMap`.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CryptoError> {
        let mut canonical = self.clone();
        canonical.blobs.sort_by(|left, right| {
            left.hash
                .cmp(&right.hash)
                .then_with(|| left.clock.cmp(&right.clock))
        });
        canonical.tombstones.sort();
        serde_json::to_vec(&canonical).map_err(|_| CryptoError::ManifestSerialisation)
    }

    /// BLAKE3 of the canonical plaintext. This is the stable manifest identity
    /// across retries and across devices that hold the same state.
    pub fn hash(&self) -> Result<String, CryptoError> {
        Ok(content_hash(&self.canonical_bytes()?))
    }

    /// Encrypt the canonical manifest with a fresh nonce.
    pub fn seal(&self, key: &OrgKey) -> Result<Vec<u8>, CryptoError> {
        encrypt(key, &self.canonical_bytes()?)
    }

    /// Encrypt with a nonce the caller already persisted.
    ///
    /// A retry after an interrupted upload re-encrypts the same canonical
    /// manifest with the same nonce, so the ciphertext hash the server already
    /// accepted is unchanged. Fresh uploads use [`Manifest::seal`].
    pub fn seal_with_nonce(
        &self,
        key: &OrgKey,
        nonce: &[u8; NONCE_BYTES],
    ) -> Result<Vec<u8>, CryptoError> {
        encrypt_with_nonce(key, &self.canonical_bytes()?, nonce)
    }

    /// Decrypt and parse a manifest. Authentication runs before parsing.
    pub fn open(key: &OrgKey, envelope: &[u8]) -> Result<Self, CryptoError> {
        let plaintext = decrypt(key, envelope)?;
        serde_json::from_slice(&plaintext).map_err(|_| CryptoError::MalformedManifest)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn test_org_key() -> OrgKey {
        let salt = OrgSalt::for_org("canonical-test-org");
        OrgKey::derive("canonical verify phrase", &salt).expect("key derivation must succeed")
    }

    /// Blobs are sorted by hash, then by clock; the round-tripped JSON must
    /// reflect that order regardless of input order.
    #[test]
    fn canonical_bytes_orders_blobs_by_hash_then_clock() {
        let manifest = Manifest {
            org: "org".to_string(),
            device: "dev".to_string(),
            generation: 1,
            blobs: vec![
                ManifestBlob {
                    hash: "zzz".to_string(),
                    size: 3,
                    clock: BTreeMap::from([("dev".to_string(), 1)]),
                },
                ManifestBlob {
                    hash: "aaa".to_string(),
                    size: 1,
                    clock: BTreeMap::from([("dev".to_string(), 9)]),
                },
                ManifestBlob {
                    hash: "aaa".to_string(),
                    size: 1,
                    clock: BTreeMap::from([("dev".to_string(), 1)]),
                },
            ],
            tombstones: vec![],
        };
        let bytes = manifest.canonical_bytes().expect("canonical serialisation");
        let roundtripped: Manifest = serde_json::from_slice(&bytes).expect("manifest parse");
        let hashes: Vec<&str> = roundtripped.blobs.iter().map(|b| b.hash.as_str()).collect();
        assert_eq!(hashes, vec!["aaa", "aaa", "zzz"]);
        let clocks: Vec<Vec<(String, u64)>> = roundtripped
            .blobs
            .iter()
            .map(|b| b.clock.iter().map(|(k, v)| (k.clone(), *v)).collect())
            .collect();
        // Three blobs in, three clocks out: the two `aaa` blobs order by
        // clock, and `zzz` follows them. The old expectation dropped the last
        // clock, so the assertion could not pass against a correct sort.
        assert_eq!(
            clocks,
            vec![
                vec![("dev".to_string(), 1)],
                vec![("dev".to_string(), 9)],
                vec![("dev".to_string(), 1)],
            ]
        );
    }

    /// Tombstones are sorted lexicographically in canonical form.
    #[test]
    fn canonical_bytes_orders_tombstones() {
        let manifest = Manifest {
            org: "org".to_string(),
            device: "dev".to_string(),
            generation: 1,
            blobs: vec![],
            tombstones: vec!["gamma".to_string(), "alpha".to_string(), "beta".to_string()],
        };
        let bytes = manifest.canonical_bytes().expect("canonical serialisation");
        let roundtripped: Manifest = serde_json::from_slice(&bytes).expect("manifest parse");
        assert_eq!(
            roundtripped.tombstones,
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string(),]
        );
    }

    /// The manifest hash is stable across repeated calls — it depends only on
    /// canonical plaintext, not on any nonce or encryption randomness.
    #[test]
    fn manifest_hash_is_stable_across_calls() {
        let manifest = Manifest {
            org: "org".to_string(),
            device: "dev".to_string(),
            generation: 7,
            blobs: vec![ManifestBlob {
                hash: "object-hash".to_string(),
                size: 42,
                clock: BTreeMap::from([("dev".to_string(), 7)]),
            }],
            tombstones: vec!["deleted-obj".to_string()],
        };
        assert_eq!(manifest.hash().unwrap(), manifest.hash().unwrap());
    }

    /// Retrying an upload with the same nonce produces byte-identical
    /// ciphertext, so the server's content-addressed hash is unchanged.
    #[test]
    fn seal_with_nonce_is_byte_stable_across_retries() {
        let key = test_org_key();
        let nonce = [7u8; NONCE_BYTES];
        let manifest = Manifest {
            org: "org".to_string(),
            device: "dev".to_string(),
            generation: 1,
            blobs: vec![],
            tombstones: vec![],
        };
        let first = manifest.seal_with_nonce(&key, &nonce).expect("seal");
        let second = manifest.seal_with_nonce(&key, &nonce).expect("seal");
        assert_eq!(
            first, second,
            "retry with same nonce must produce identical ciphertext"
        );
        let opened = Manifest::open(&key, &first).expect("open");
        assert_eq!(opened.generation, 1);
    }
}
