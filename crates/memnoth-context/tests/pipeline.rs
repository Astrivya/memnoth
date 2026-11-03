//! The pipeline the runtime will call, exercised end to end without a model or a socket.
//!
//! This is the contract test for the four modules together: instruction precedence produces the
//! project layer, the assembler keeps that layer in a byte-stable prefix, the allocator fills only
//! the tail, and compaction keeps every exact fact verbatim.

use memnoth_context::assemble::{Layer, Piece, assemble, prefix_hash};
use memnoth_context::budget::{Budget, Item, Slot, allocate, default_budget, default_slots};

use memnoth_context::compact::{Fact, FactKind, retention_ok, select};
use memnoth_context::instruct::{Rule, Scope, merge, parse};

fn instruction(text: &str, scope: Scope, source: &str) -> Rule {
    parse(text, scope, source)
        .into_iter()
        .next()
        .expect("one rule")
}

#[test]
fn a_turn_is_assembled_from_instructions_budget_and_memory() {
    // Two scopes, one duplicate: the stronger copy survives and the loser is reported.
    let project = parse(
        "- Never force-push to main\n- Run cargo fmt before committing\n- Keep functions small\n",
        Scope::Project,
        "AGENTS.md",
    );
    let enterprise = vec![instruction(
        "Never force-push to main",
        Scope::Enterprise,
        "enterprise-policy.md",
    )];
    let merged = merge(vec![
        (Scope::Enterprise, enterprise),
        (Scope::Project, project),
    ]);

    assert_eq!(
        merged.rules[0].scope,
        Scope::Enterprise,
        "enterprise is the strongest scope"
    );
    assert!(
        merged
            .dropped
            .iter()
            .any(|dropped| dropped.rule.text.contains("cargo fmt")),
        "a formatter rule is dropped as lint leakage"
    );

    let rules = merged.render();
    let pieces = vec![
        Piece {
            layer: Layer::Global,
            slot: "identity",
            text: "You are memnoth. Ground every claim in a tool result.".into(),
        },
        Piece {
            layer: Layer::Project,
            slot: "policy",
            text: rules,
        },
        Piece {
            layer: Layer::Session,
            slot: "session",
            text: "Objective: fix the lease cache. Done when the test passes.".into(),
        },
        Piece {
            layer: Layer::Turn,
            slot: "memory",
            text: "- D-17: vault writes go through the lease".into(),
        },
    ];

    let slots = default_slots();
    let total: usize = slots.iter().map(|s| s.max_tokens).sum();
    let budget = default_budget(total);
    let stable_prefix = prefix_hash(&pieces);
    let first = assemble(pieces.clone(), &slots, &budget);
    assert!(first.prefix.contains("Never force-push to main"));
    assert!(!first.prefix.contains("vault writes go through the lease"));
    assert!(first.tail.contains("vault writes go through the lease"));
    assert_eq!(
        first.receipt.total_tokens,
        first.receipt.layer_tokens.iter().sum()
    );

    // A tool result arrives. The tail changes; the cacheable prefix does not.
    let mut second_pieces = pieces;
    second_pieces[3].text = "- D-17: vault writes go through the lease\n- test failed: 2".into();
    let second_prefix_after_turn = prefix_hash(&second_pieces);
    let second = assemble(second_pieces, &slots, &budget);
    assert_ne!(second.receipt.hash, first.receipt.hash, "the turn changed");
    assert_eq!(
        second_prefix_after_turn, stable_prefix,
        "the prefix hash must not move when only the turn layer changed"
    );
    assert_eq!(
        second.prefix, first.prefix,
        "the cacheable bytes must be identical"
    );

    // The tail is filled under a ceiling, and every drop is explained.
    let slots = [
        Slot {
            name: "recall",
            max_tokens: 40,
        },
        Slot {
            name: "files",
            max_tokens: 40,
        },
    ];
    let allocation = allocate(
        vec![
            Item::new("recall", "- D-17: vault writes go through the lease", 10),
            Item::new("files", "crates/memnoth-engine/src/store.rs:41", 8),
            Item::new("files", "a".repeat(4_000), 1),
        ],
        &slots,
        &Budget { total: 60 },
    );
    assert_eq!(allocation.kept.len(), 2);
    assert_eq!(allocation.dropped.len(), 1, "the oversized file is dropped");
    assert!(allocation.used <= 60);

    // Compaction keeps the decision byte-for-byte, summarises the story, and proves it.
    let facts = vec![
        Fact::new(
            "D-17",
            FactKind::Decision,
            "Vault writes go through the lease",
        ),
        Fact::new(
            "path:store",
            FactKind::Path,
            "crates/memnoth-engine/src/store.rs",
        ),
        Fact::narrative("n1", "first tried a summary, which lost the path"),
    ];
    let selection = select(&facts, 0);
    assert_eq!(
        selection.summarised.len(),
        1,
        "only narrative may be summarised"
    );
    assert_eq!(retention_ok(&facts, &selection), Ok(()));
}
