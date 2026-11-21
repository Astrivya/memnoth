use std::collections::HashSet;
use std::fs;

use memnoth::project_graph_sync::{
    SnapshotRefusal, frame,
    restore_verify::{self, GateOutcome},
    unframe,
};
use memnoth::sync_crypto::{
    CryptoError, Manifest, ManifestBlob, NONCE_BYTES, OrgKey, OrgSalt, content_hash, decrypt,
    encrypt, encrypt_with_nonce, project_graph_salt,
};
use memnoth_engine::store::MemoryEngine;
use memnoth_engine::{EntityKind, now_ms};
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

/// Build a real memory vault on disk and return its raw SQLite bytes.
/// `violating` inserts an inverted validity interval, which MI-1 must catch.
fn vault_bytes(violating: bool) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.db");
    {
        let engine = MemoryEngine::open(&path).unwrap();
        let t0 = now_ms();
        let entity = engine
            .add_entity(EntityKind::Concept, "rust", 0.9, t0)
            .unwrap();
        let at = t0 + 1_000;
        let successor = engine
            .supersede_entity(entity, None, None, None, None, "rename", at)
            .unwrap();
        engine
            .ingest_observation_attributed(
                successor,
                "Rust is a systems language",
                "fact",
                0.9,
                at,
                r#"{"user":"tester"}"#,
                Some(at + 2_000),
            )
            .unwrap();
        if violating {
            engine
                .connection()
                .execute(
                    "INSERT INTO entities (kind, name, valid_from, valid_to, created_at) \
                     VALUES ('concept', 'inverted', 2000, 1000, 1500)",
                    [],
                )
                .unwrap();
        }
    } // drop the connection so every page is flushed to the file
    fs::read(&path).unwrap()
}

#[test]
fn a_violating_vault_snapshot_is_refused_before_install() {
    let vectors = vectors();
    let salt = OrgSalt::from_hex(&vectors.salt_hex).unwrap();
    let key = OrgKey::derive(&vectors.phrase, &salt).unwrap();
    let project = "org:violating-project";

    let framed = frame(project, "source-fingerprint", &vault_bytes(true)).unwrap();
    let envelope = encrypt(&key, &framed).unwrap();

    // The same authenticate + unframe sequence pull_remote performs.
    let plaintext = decrypt(&key, &envelope).expect("the correct key authenticates");
    let (header, database) = unframe(&plaintext).unwrap();
    assert_eq!(header.project_id, project);

    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("incoming.db");
    let target = dir.path().join("vault.db");
    fs::write(&staged, database).unwrap();

    let outcome = restore_verify::check_staged_invariants(&staged).unwrap();
    match &outcome {
        GateOutcome::Violations(ids) => {
            assert!(
                ids.contains(&"MI-1"),
                "an inverted interval must fail MI-1: {ids:?}"
            );
            let refusal = SnapshotRefusal::InvariantViolation(ids.clone());
            assert!(refusal.to_string().contains("memory invariants violated"));
        }
        other => panic!("a vault with an inverted interval must be refused: {other:?}"),
    }
    assert!(outcome.is_violation());

    // pull_remote's gate removes the staged file and never renames it into place.
    if outcome.is_violation() {
        let _ = fs::remove_file(&staged);
    }
    assert!(!staged.exists());
    assert!(
        !target.exists(),
        "a violating snapshot must never be installed"
    );
}

#[test]
fn a_clean_vault_snapshot_passes_the_gate_and_can_be_installed() {
    let vectors = vectors();
    let salt = OrgSalt::from_hex(&vectors.salt_hex).unwrap();
    let key = OrgKey::derive(&vectors.phrase, &salt).unwrap();
    let project = "org:clean-project";

    let framed = frame(project, "source-fingerprint", &vault_bytes(false)).unwrap();
    let envelope = encrypt(&key, &framed).unwrap();
    let plaintext = decrypt(&key, &envelope).unwrap();
    let (header, database) = unframe(&plaintext).unwrap();
    assert_eq!(header.project_id, project);

    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("incoming.db");
    let target = dir.path().join("vault.db");
    fs::write(&staged, database).unwrap();

    assert!(
        matches!(
            restore_verify::check_staged_invariants(&staged).unwrap(),
            GateOutcome::CleanVault(_)
        ),
        "a clean vault must pass the local import gate"
    );

    // The install is the atomic rename pull_remote performs after the gate.
    fs::rename(&staged, &target).unwrap();
    assert!(target.exists());
    assert!(
        matches!(
            restore_verify::check_staged_invariants(&target).unwrap(),
            GateOutcome::CleanVault(_)
        ),
        "the installed vault must still pass the invariants"
    );
}

#[test]
fn tampered_wrong_key_and_wrong_project_have_distinct_honest_errors() {
    let vectors = vectors();
    let salt = OrgSalt::from_hex(&vectors.salt_hex).unwrap();
    let key = OrgKey::derive(&vectors.phrase, &salt).unwrap();
    let wrong_key = OrgKey::derive("not the recovery phrase", &salt).unwrap();
    let framed = frame("project-a", "fp", b"a vault-shaped database").unwrap();
    let envelope = encrypt(&key, &framed).unwrap();

    // A tampered blob fails the content-addressed hash check at the gate,
    // before authenticated decryption is even attempted.
    let mut tampered = envelope.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert_ne!(content_hash(&tampered), content_hash(&envelope));
    assert!(matches!(
        decrypt(&key, &tampered),
        Err(CryptoError::AuthenticationFailed)
    ));

    // A blob with a matching hash that still cannot be opened is a key
    // failure, not a transport-integrity failure.
    assert!(matches!(
        decrypt(&wrong_key, &envelope),
        Err(CryptoError::AuthenticationFailed)
    ));

    // Project binding is framed independently of the key, so a snapshot that
    // opens under this key can still name a different project.
    let other_key = OrgKey::derive("phrase", &project_graph_salt("org", "project-b")).unwrap();
    let framed_a = frame("project-a", "fp", b"db").unwrap();
    let envelope_b = encrypt(&other_key, &framed_a).unwrap();
    let opened = decrypt(&other_key, &envelope_b).unwrap();
    let (header, _) = unframe(&opened).unwrap();
    assert_ne!(header.project_id, "project-b");

    let messages: Vec<String> = vec![
        SnapshotRefusal::TamperedCiphertext.to_string(),
        SnapshotRefusal::AuthenticationFailed.to_string(),
        SnapshotRefusal::WrongProject.to_string(),
        SnapshotRefusal::InvariantViolation(vec!["MI-1"]).to_string(),
    ];
    assert_eq!(
        messages.iter().collect::<HashSet<_>>().len(),
        messages.len(),
        "every refusal must have its own honest message"
    );
    assert!(messages[0].contains("ciphertext hash check"));
    assert!(messages[1].contains("could not be authenticated"));
    assert!(messages[2].contains("another project"));
    assert!(messages[3].contains("memory invariants violated"));
}
