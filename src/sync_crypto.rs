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
//!
//! ## Epoch-keyed plaintext cache
//!
//! [`PlaintextCache`] is an in-process cache for data that has already been
//! decrypted locally. Every entry is keyed by `(project, org, revision epoch)`
//! plus an entry id. A revocation advances the epoch for a `(project, org)`
//! scope, dropping and zeroizing all plaintext for that scope; an epoch the
//! process has not been told is legitimate is refused before any plaintext is
//! stored or served (fail closed).
//!
//! This is a local primitive only. It does **not** implement hosted semantic
//! search or remote revocation, and nothing here proves that another device has
//! applied a bump: that device is revoked only when it pulls the new epoch.
//! Evicted plaintext is not copied into an index or snapshot, so offline it is
//! physically unrecallable from this cache.

use std::collections::{BTreeMap, BTreeSet};
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

/// A stable salt for one project graph. The dedicated domain and both
/// identifiers keep graph keys separate from the org vault key and from other
/// projects that share the same recovery phrase.
pub fn project_graph_salt(org: &str, project: &str) -> OrgSalt {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"memnoth-project-graph-sync-salt-v1:");
    hasher.update(org.trim().to_ascii_lowercase().as_bytes());
    hasher.update(b"\0");
    hasher.update(project.trim().to_ascii_lowercase().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; SALT_BYTES];
    bytes.copy_from_slice(&digest.as_bytes()[..SALT_BYTES]);
    OrgSalt::from_bytes(bytes)
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

/// A revision epoch: one generation of decrypted data for a `(project, org)`
/// scope. Larger values supersede smaller ones.
///
/// The value is an opaque counter chosen by a local revocation or a pulled
/// manifest. The cache never invents an epoch from plaintext.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RevisionEpoch(u64);

impl RevisionEpoch {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// The epoch after this one. Saturating: an epoch must never wrap back to
    /// zero, because a wrapped epoch could be mistaken for an older, revoked
    /// generation.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl fmt::Display for RevisionEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The identity of one cache scope: the `(project, org)` pair whose plaintext
/// is invalidated together.
///
/// Both identifiers are trimmed and ASCII-lowercased, matching the salts from
/// [`OrgSalt::for_org`] and [`project_graph_salt`], so the same logical scope
/// cannot be addressed under two spellings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId {
    project: String,
    org: String,
}

impl ScopeId {
    pub fn new(project: &str, org: &str) -> Self {
        Self {
            project: project.trim().to_ascii_lowercase(),
            org: org.trim().to_ascii_lowercase(),
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn org(&self) -> &str {
        &self.org
    }
}

/// Why a plaintext entry could not be stored or served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// The `(project, org)` scope has no recognized epoch at all. The caller
    /// cannot know what is current, so the request is refused.
    UnknownScope,
    /// The presented epoch is not recognized for the scope (stale, foreign or
    /// revoked). Storing or serving plaintext under it would risk revoked data.
    UnknownEpoch {
        scope: ScopeId,
        epoch: RevisionEpoch,
    },
    /// Authentication of an envelope failed while filling the cache.
    Crypto(CryptoError),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownScope => f.write_str("the cache scope has no recognized revision epoch"),
            Self::UnknownEpoch { scope, epoch } => write!(
                f,
                "revision epoch {epoch} is not recognized for {}/{}",
                scope.project, scope.org
            ),
            Self::Crypto(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Crypto(error) => Some(error),
            _ => None,
        }
    }
}

/// Full primary key of a cached plaintext entry: `(project, org, epoch, entry)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EntryKey {
    scope: ScopeId,
    epoch: RevisionEpoch,
    entry: String,
}

/// An in-process cache of decrypted local data, invalidated by revision epoch.
///
/// Entries are held in zeroizing buffers and dropped (zeroized) on revocation.
/// There is no index, snapshot or search structure: evicted plaintext cannot be
/// reconstructed from this cache, and no remote service holds a copy of it.
#[derive(Debug, Default)]
pub struct PlaintextCache {
    /// Epochs the local process has been told are legitimate for a scope. An
    /// epoch outside this set is uncertain and is refused before any plaintext
    /// is touched. More than one epoch can be recognized while two devices are
    /// still converging; a bump collapses the set to a single new epoch.
    recognized: BTreeMap<ScopeId, BTreeSet<RevisionEpoch>>,
    entries: BTreeMap<EntryKey, Zeroizing<Vec<u8>>>,
}

impl PlaintextCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Recognize `epoch` as a legitimate revision for `scope`. This is the local
    /// act of learning which generation is authoritative (e.g. after a
    /// successful pull). It never decrypts or stores plaintext by itself.
    pub fn recognize_epoch(&mut self, scope: &ScopeId, epoch: RevisionEpoch) {
        self.recognized
            .entry(scope.clone())
            .or_default()
            .insert(epoch);
    }

    /// Whether `epoch` is currently recognized for `scope`.
    pub fn is_epoch_known(&self, scope: &ScopeId, epoch: RevisionEpoch) -> bool {
        self.recognized
            .get(scope)
            .is_some_and(|epochs| epochs.contains(&epoch))
    }

    /// Refuse an epoch the process has not recognized for the scope.
    ///
    /// Callers that do their own decryption can use this before touching
    /// plaintext; every cache method applies the same check internally.
    pub fn require_epoch(&self, scope: &ScopeId, epoch: RevisionEpoch) -> Result<(), CacheError> {
        if self.is_epoch_known(scope, epoch) {
            Ok(())
        } else if self.recognized.contains_key(scope) {
            Err(CacheError::UnknownEpoch {
                scope: scope.clone(),
                epoch,
            })
        } else {
            Err(CacheError::UnknownScope)
        }
    }

    /// Store decrypted `plaintext` under an exact `(scope, epoch, entry)` key.
    /// Refuses any epoch that is not currently recognized for the scope.
    pub fn put(
        &mut self,
        scope: &ScopeId,
        epoch: RevisionEpoch,
        entry: &str,
        plaintext: Zeroizing<Vec<u8>>,
    ) -> Result<(), CacheError> {
        self.require_epoch(scope, epoch)?;
        self.entries.insert(
            EntryKey {
                scope: scope.clone(),
                epoch,
                entry: entry.to_string(),
            },
            plaintext,
        );
        Ok(())
    }

    /// Borrow cached plaintext for an exact key. `Ok(None)` is a miss for a
    /// *known* epoch; any uncertain epoch is refused before the lookup.
    pub fn get(
        &self,
        scope: &ScopeId,
        epoch: RevisionEpoch,
        entry: &str,
    ) -> Result<Option<&Zeroizing<Vec<u8>>>, CacheError> {
        self.require_epoch(scope, epoch)?;
        Ok(self.entries.get(&EntryKey {
            scope: scope.clone(),
            epoch,
            entry: entry.to_string(),
        }))
    }

    /// Decrypt `envelope` under `key` only when `epoch` is recognized for
    /// `scope`, then cache the plaintext under `entry`. Later calls for the
    /// same key are served from the cache.
    ///
    /// The crypto is exactly the module's existing XChaCha20-Poly1305
    /// [`decrypt`]; this method changes neither key derivation nor the wire
    /// format. The epoch check runs first, so an uncertain epoch never reaches
    /// the cipher, the cache, or the caller.
    pub fn decrypt_cached(
        &mut self,
        key: &OrgKey,
        scope: &ScopeId,
        epoch: RevisionEpoch,
        entry: &str,
        envelope: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CacheError> {
        self.require_epoch(scope, epoch)?;
        let cache_key = EntryKey {
            scope: scope.clone(),
            epoch,
            entry: entry.to_string(),
        };
        if let Some(plaintext) = self.entries.get(&cache_key) {
            return Ok(plaintext.clone());
        }
        let plaintext = decrypt(key, envelope).map_err(CacheError::Crypto)?;
        self.entries.insert(cache_key, plaintext.clone());
        Ok(plaintext)
    }

    /// Revoke every cached plaintext for `scope` by advancing its epoch.
    ///
    /// The new epoch becomes the only recognized epoch for the scope; older
    /// epochs become uncertain immediately (they fail closed), and every
    /// cached entry under the scope is dropped and zeroized, whichever epoch it
    /// was stored under. Eviction is local: other devices are revoked only once
    /// they pull the new epoch.
    pub fn bump_epoch(&mut self, scope: &ScopeId) -> Result<RevisionEpoch, CacheError> {
        let current = self
            .recognized
            .get(scope)
            .and_then(|epochs| epochs.iter().max().copied())
            .ok_or(CacheError::UnknownScope)?;
        let next = current.next();
        self.recognized
            .insert(scope.clone(), BTreeSet::from([next]));
        self.entries.retain(|key, _| &key.scope != scope);
        Ok(next)
    }

    /// Drop every cached plaintext for `scope` without changing its epoch.
    ///
    /// Use this when cached data must be revoked now but no new epoch has been
    /// established yet. Recognized epochs are left untouched, so the scope can
    /// be repopulated from a fresh pull under the same epoch.
    pub fn invalidate_scope(&mut self, scope: &ScopeId) {
        self.entries.retain(|key, _| &key.scope != scope);
    }

    /// Number of cached plaintext entries. Metadata only; no plaintext leaks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no plaintext entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_graph_salts_are_domain_and_project_separated() {
        let project = project_graph_salt("org-a", "project-a");
        assert_ne!(project, OrgSalt::for_org("org-a"));
        assert_ne!(project, project_graph_salt("org-a", "project-b"));
        assert_ne!(project, project_graph_salt("org-b", "project-a"));
        assert_eq!(project, project_graph_salt("ORG-A", "PROJECT-A"));
    }

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
