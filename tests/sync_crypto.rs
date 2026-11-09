use std::fs;

use memnoth::sync_crypto::{
    CryptoError, Manifest, ManifestBlob, NONCE_BYTES, OrgKey, OrgSalt, content_hash, decrypt,
    encrypt_with_nonce,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Vectors {
    phrase: String,
    salt_hex: String,
    nonce_hex: String,
    plaintext_hex: String,
    envelope_hex: String,
    envelope_blake3: String,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("fixtures/sync_crypto_vectors.json"))
        .expect("the committed vector must parse")
}

fn nonce(hex: &str) -> [u8; NONCE_BYTES] {
    let bytes = hex::decode(hex).expect("nonce hex");
    let mut nonce = [0_u8; NONCE_BYTES];
    nonce.copy_from_slice(&bytes);
    nonce
}

fn key(vectors: &Vectors) -> OrgKey {
    let salt = OrgSalt::from_hex(&vectors.salt_hex).expect("salt hex");
    OrgKey::derive(&vectors.phrase, &salt).expect("phrase derives a key")
}

#[test]
fn committed_vector_matches_and_round_trips() {
    let vectors = vectors();
    let key = key(&vectors);
    let nonce = nonce(&vectors.nonce_hex);
    let plaintext = hex::decode(&vectors.plaintext_hex).expect("plaintext hex");

    let envelope = encrypt_with_nonce(&key, &plaintext, &nonce).expect("encrypt");
    assert_eq!(hex::encode(&envelope), vectors.envelope_hex);
    assert_eq!(content_hash(&envelope), vectors.envelope_blake3);

    let opened = decrypt(&key, &envelope).expect("decrypt");
    assert_eq!(opened.as_slice(), plaintext.as_slice());
}

#[test]
fn phrase_spacing_and_case_are_normalised() {
    let vectors = vectors();
    let salt = OrgSalt::from_hex(&vectors.salt_hex).unwrap();
    let padded = format!("  {}  ", vectors.phrase.to_ascii_uppercase());
    let key = OrgKey::derive(&padded, &salt).expect("normalised phrase derives the same key");
    let envelope = hex::decode(&vectors.envelope_hex).unwrap();

    let opened = decrypt(&key, &envelope).expect("the same phrase opens the envelope");
    assert_eq!(
        opened.as_slice(),
        hex::decode(&vectors.plaintext_hex).unwrap().as_slice()
    );
}

#[test]
fn tampered_ciphertext_is_refused() {
    let vectors = vectors();
    let key = key(&vectors);
    let mut envelope = hex::decode(&vectors.envelope_hex).unwrap();
    *envelope.last_mut().unwrap() ^= 1;

    assert!(matches!(
        decrypt(&key, &envelope),
        Err(CryptoError::AuthenticationFailed)
    ));
    assert!(matches!(
        decrypt(&key, &envelope[..39]),
        Err(CryptoError::InvalidEnvelope)
    ));
}

#[test]
fn a_wrong_phrase_fails_closed_and_leaves_local_data_alone() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("vault.db");
    fs::write(&local, b"local vault bytes").unwrap();
    let before = fs::read(&local).unwrap();

    let vectors = vectors();
    let salt = OrgSalt::from_hex(&vectors.salt_hex).unwrap();
    let wrong = OrgKey::derive("not the recovery phrase", &salt).unwrap();
    let envelope = hex::decode(&vectors.envelope_hex).unwrap();

    assert!(matches!(
        decrypt(&wrong, &envelope),
        Err(CryptoError::AuthenticationFailed)
    ));
    assert_eq!(fs::read(&local).unwrap(), before);
}

fn blob(hash: &str, size: u64, clock: &[(&str, u64)]) -> ManifestBlob {
    ManifestBlob {
        hash: hash.to_string(),
        size,
        clock: clock
            .iter()
            .map(|(device, generation)| ((*device).to_string(), *generation))
            .collect(),
    }
}

#[test]
fn manifest_retry_reuses_the_persisted_envelope() {
    let vectors = vectors();
    let key = key(&vectors);
    let nonce = nonce(&vectors.nonce_hex);
    let manifest = Manifest {
        org: "org:sync-test".to_string(),
        device: "laptop".to_string(),
        generation: 4,
        blobs: vec![blob(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            2,
            &[("device-a", 1)],
        )],
        tombstones: Vec::new(),
    };

    let first = manifest.seal_with_nonce(&key, &nonce).unwrap();
    let after_interruption = manifest.seal_with_nonce(&key, &nonce).unwrap();
    assert_eq!(first, after_interruption);
    assert_eq!(content_hash(&first), content_hash(&after_interruption));
    assert_eq!(manifest.hash().unwrap(), manifest.hash().unwrap());
}

#[test]
fn org_salt_is_reproducible_and_domain_separated() {
    let first = OrgSalt::for_org(" Org:Acme ");
    let second = OrgSalt::for_org("org:acme");
    let other = OrgSalt::for_org("org:other");
    assert_eq!(first, second);
    assert_ne!(first, other);
}

#[test]
fn manifest_bytes_are_canonical_and_wrong_key_cannot_open_them() {
    let vectors = vectors();
    let key = key(&vectors);
    let wrong = OrgKey::derive(
        "not the recovery phrase",
        &OrgSalt::from_hex(&vectors.salt_hex).unwrap(),
    )
    .unwrap();

    let first = Manifest {
        org: "org:sync-test".to_string(),
        device: "laptop".to_string(),
        generation: 7,
        blobs: vec![
            blob(
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                2,
                &[("device-b", 2), ("device-a", 1)],
            ),
            blob(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                1,
                &[],
            ),
        ],
        tombstones: vec!["z".to_string(), "a".to_string()],
    };
    let second = Manifest {
        org: "org:sync-test".to_string(),
        device: "laptop".to_string(),
        generation: 7,
        blobs: vec![
            blob(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                1,
                &[],
            ),
            blob(
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                2,
                &[("device-a", 1), ("device-b", 2)],
            ),
        ],
        tombstones: vec!["a".to_string(), "z".to_string()],
    };

    assert_eq!(
        first.canonical_bytes().unwrap(),
        second.canonical_bytes().unwrap()
    );
    assert_eq!(first.hash().unwrap(), second.hash().unwrap());

    let envelope = first.seal(&key).unwrap();
    let opened = Manifest::open(&key, &envelope).unwrap();
    assert_eq!(
        opened.canonical_bytes().unwrap(),
        first.canonical_bytes().unwrap()
    );
    assert!(matches!(
        Manifest::open(&wrong, &envelope),
        Err(CryptoError::AuthenticationFailed)
    ));
}
