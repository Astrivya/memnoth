//! Focused tests for the standalone context-receipt verifier.
//!
//! The module is included by path so this suite compiles and runs before the
//! CLI wiring lands (another agent owns `lib.rs` and `cli.rs`). Once the module
//! is exported from the library, this file can switch to
//! `use memnoth::context_verify::…` without changing any test body.

#[path = "../src/context_verify.rs"]
mod context_verify;

use context_verify::{
    ContextReceipt, RecordedDrop, VerifyError, drop_reason_label, parse_drop_reason, verify,
};
use memnoth_context::{
    Assembly, Budget, DropReason, Layer, Piece, Slot, assemble, budget::default_budget,
    budget::default_slots, hex, prefix_hash,
};

fn pieces() -> Vec<Piece> {
    vec![
        Piece {
            layer: Layer::Global,
            slot: "identity",
            text: "You are memnoth.".to_string(),
        },
        Piece {
            layer: Layer::Project,
            slot: "policy",
            text: "no unwraps".to_string(),
        },
        Piece {
            layer: Layer::Turn,
            slot: "memory",
            text: "lease.rs:41".to_string(),
        },
    ]
}

fn assembled() -> (Assembly, Vec<Piece>) {
    let pieces = pieces();
    let assembly = assemble(pieces.clone(), &default_slots(), &default_budget(2140));
    (assembly, pieces)
}

/// The receipt is a truthful claim about the context: verify passes, the
/// recorded hash and the re-derived hash agree, and the drop list is reported.
#[test]
fn a_matching_receipt_verifies_and_reports_the_prefix() {
    let (assembly, pieces) = assembled();
    let json = ContextReceipt::from_assembly(&assembly, Some("mn-test"))
        .to_json()
        .unwrap();

    let verdict = verify(&json, &pieces).expect("a matching receipt must verify");
    assert!(verdict.ok);
    assert_eq!(verdict.recorded_prefix_hash, hex(prefix_hash(&pieces)));
    assert_eq!(verdict.recomputed_prefix_hash, verdict.recorded_prefix_hash);
    assert_eq!(verdict.prefix_pieces, 2, "Turn is not part of the prefix");
    assert!(verdict.dropped.is_empty());
    assert!(context_verify::render(&verdict).starts_with("OK —"));
}

/// A tampered prefix — or a changed piece that feeds it — fails closed. The
/// re-derived hash cannot be coerced into matching the recorded claim.
#[test]
fn a_tampered_prefix_fails_closed() {
    let (assembly, pieces) = assembled();
    let mut receipt = ContextReceipt::from_assembly(&assembly, None);
    let mut tampered = receipt.prefix_hash.clone().into_bytes();
    tampered[0] = if tampered[0] == b'0' { b'1' } else { b'0' };
    receipt.prefix_hash = String::from_utf8(tampered).unwrap();

    let err = verify(&receipt.to_json().unwrap(), &pieces).unwrap_err();
    match err {
        VerifyError::PrefixMismatch {
            recorded,
            recomputed,
        } => {
            assert_eq!(recorded, receipt.prefix_hash);
            assert_eq!(recomputed, hex(prefix_hash(&pieces)));
            assert_ne!(recorded, recomputed);
        }
        other => panic!("expected a prefix mismatch, got {other:?}"),
    }
}

/// A changed prefix *piece* also fails closed — the check is over the recorded
/// inputs, not just over the hash string.
#[test]
fn a_changed_prefix_piece_fails_closed() {
    let (assembly, original) = assembled();
    let json = ContextReceipt::from_assembly(&assembly, None)
        .to_json()
        .unwrap();

    let mut altered = original.clone();
    altered[1].text = "no unwraps, ever".to_string(); // a Project rule changed
    let err = verify(&json, &altered).unwrap_err();
    assert!(
        matches!(
            err,
            VerifyError::PrefixMismatch { .. } | VerifyError::IncompleteReceipt(_)
        ),
        "a changed prefix piece must fail closed, got {err:?}"
    );
}

/// Only Turn-layer drift is invisible to the prefix hash: a receipt still
/// verifies when the tail changes, which is the whole point of a stable prefix.
#[test]
fn a_turn_only_change_still_verifies() {
    let (assembly, original) = assembled();
    let json = ContextReceipt::from_assembly(&assembly, None)
        .to_json()
        .unwrap();

    let mut altered = original.clone();
    altered[2].text = "a different tool result entirely".to_string();
    let verdict = verify(&json, &altered).expect("turn drift must not change the prefix");
    assert!(verdict.ok);
}

/// The recorded drop reason is reported, decoded back to the assembler's enum,
/// and rendered into the human output.
#[test]
fn a_dropped_piece_reason_is_reported() {
    let slots: Vec<Slot> = vec![Slot {
        name: "identity",
        max_tokens: 10,
    }];
    let budget = Budget { total: 100 };
    let assembled_pieces = vec![
        Piece {
            layer: Layer::Global,
            slot: "identity",
            text: "a".repeat(40), // fills the 10-token slot exactly
        },
        Piece {
            layer: Layer::Global,
            slot: "identity",
            text: "b".repeat(40), // cannot fit: OverSlot
        },
    ];
    let assembly = assemble(assembled_pieces.clone(), &slots, &budget);
    let receipt = ContextReceipt::from_assembly(&assembly, None);

    assert_eq!(
        receipt.dropped,
        vec![RecordedDrop {
            slot: "identity".to_string(),
            reason: "over_slot".to_string(),
        }]
    );

    let verdict = verify(&receipt.to_json().unwrap(), &assembled_pieces).unwrap();
    assert!(verdict.ok);
    assert_eq!(verdict.dropped.len(), 1);
    assert_eq!(verdict.dropped[0].drop_reason(), Some(DropReason::OverSlot));
    let text = context_verify::render(&verdict);
    assert!(text.contains("identity (over_slot)"), "{text}");
    assert!(text.contains("dropped: 1 piece(s)"), "{text}");
}

/// Drop-reason strings round-trip through the assembler's enum, including the
/// design document's `slot_cap` alias.
#[test]
fn drop_reasons_round_trip() {
    for reason in [
        DropReason::OverSlot,
        DropReason::OverTotal,
        DropReason::UnknownSlot,
    ] {
        assert_eq!(parse_drop_reason(drop_reason_label(reason)), Some(reason));
    }
    assert_eq!(
        parse_drop_reason("slot_cap"),
        Some(DropReason::OverSlot),
        "design-document alias"
    );
    assert_eq!(parse_drop_reason("nonsense"), None);
}

/// A malformed hash or an unknown schema version is a fail-closed error, not a
/// report that quietly passes.
#[test]
fn malformed_documents_fail_closed() {
    let (assembly, pieces) = assembled();

    let mut bad_hash = ContextReceipt::from_assembly(&assembly, None);
    bad_hash.prefix_hash = "zzzz".to_string();
    assert!(matches!(
        verify(&bad_hash.to_json().unwrap(), &pieces),
        Err(VerifyError::MalformedPrefixHash(_))
    ));

    let mut bad_version = ContextReceipt::from_assembly(&assembly, None);
    bad_version.v = 99;
    assert!(matches!(
        verify(&bad_version.to_json().unwrap(), &pieces),
        Err(VerifyError::InvalidReceipt(_))
    ));

    assert!(matches!(
        verify("not json", &pieces),
        Err(VerifyError::InvalidReceipt(_))
    ));
}
