//! The compiled prompt: four layers, a stable prefix, a mutable tail, and a receipt.
//!
//! Prompt caching is a **byte-exact prefix match** (tools, then system, then messages). Any
//! change anywhere in the prefix invalidates every cached token after it, so the layout is not a
//! formatting preference — it is the difference between paying for a delta and re-paying for the
//! whole conversation.
//!
//! Therefore:
//!
//! * [`Layer::Global`], [`Layer::Project`] and [`Layer::Session`] render into [`Assembly::prefix`].
//!   Identical inputs must produce identical bytes, forever; no clocks, no counters, no map
//!   iteration order.
//! * [`Layer::Turn`] renders into [`Assembly::tail`]. Retrieved context, tool results and the
//!   recent turns live here, because they legitimately change between requests.
//! * A mid-session update (a file changed, a decision was superseded) belongs in the tail — never
//!   as an edit to the prefix.
//!
//! The receipt is how that rule is enforced rather than trusted: [`prefix_hash`] covers only the
//! prefix, so a test can assert "the tail changed and the prefix did not".

use crate::budget::{Budget, Slot, estimate_tokens};
use crate::stable::{StableHasher, hex};

/// The four layers, in cache order. The declaration order is the rendering order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// Tool stubs, identity, policy. Shared by every project on this machine.
    Global,
    /// Project rules, pinned decisions, ownership. Shared by every session in the project.
    Project,
    /// Task card, state snapshot, ledger. Shared by every turn of one session.
    Session,
    /// Retrieved context, tool results, recent turns. Changes per request.
    Turn,
}

impl Layer {
    /// Index into [`PieceReceipt::layer`] and [`Receipt::layer_tokens`] (layer ordinal 0-3).
    pub fn index(self) -> usize {
        match self {
            Layer::Global => 0,
            Layer::Project => 1,
            Layer::Session => 2,
            Layer::Turn => 3,
        }
    }

    fn is_prefix(self) -> bool {
        !matches!(self, Layer::Turn)
    }
}

/// One rendered block. `slot` is a stable name (`"identity"`, `"rules"`, `"task"`), used as the
/// heading and as half of the piece hash — renaming a slot is a prefix change and must be treated
/// as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub layer: Layer,
    pub slot: &'static str,
    pub text: String,
}

/// What a receipt records about one piece: where it went, what it cost, and its identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PieceReceipt {
    pub layer: u8,
    pub slot: &'static str,
    pub tokens: usize,
    pub hash: u64,
}

/// The accounting for one assembly. Emitted before the request, so a turn can be audited without
/// replaying it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// Per-layer token accounting, one entry per layer in declaration order
    /// (Global, Project, Session, Turn). Sums to `total_tokens`.
    pub layer_tokens: [usize; 4],
    pub total_tokens: usize,
    /// Identity of the cacheable prefix alone. Two turns of one session must report the same
    /// number here; a change means a cache miss, whether or not the text looks different.
    pub prefix_hash: u64,
    /// Identity of the whole rendered prompt (prefix + tail).
    pub hash: u64,
    pub pieces: Vec<PieceReceipt>,
}

/// The compiled prompt and its accounting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assembly {
    /// [`Layer::Global`] + [`Layer::Project`] + [`Layer::Session`], in that order.
    pub prefix: String,
    /// [`Layer::Turn`], rendered the same way but allowed to change every request.
    pub tail: String,
    pub receipt: Receipt,
}

/// Render the pieces and account for them.
///
/// Ordering contract: layers in declaration order, insertion order within a layer. The caller
/// sorts its inputs; this function never reorders, because a stable order that the assembler
/// decides for itself would silently change when a `HashMap` did.
pub fn assemble(pieces: Vec<Piece>, slots: &[Slot], budget: &Budget) -> Assembly {
    // Map slot name -> max_tokens for fast lookup.
    let slot_caps: Vec<(&str, usize)> = slots.iter().map(|s| (s.name, s.max_tokens)).collect();

    let mut ordered: Vec<Piece> = Vec::with_capacity(pieces.len());
    for layer in [Layer::Global, Layer::Project, Layer::Session, Layer::Turn] {
        ordered.extend(pieces.iter().filter(|piece| piece.layer == layer).cloned());
    }

    let mut prefix = String::new();
    let mut tail = String::new();
    let mut layer_tokens_used = [0usize; 4];
    let mut slot_used = vec![0usize; slots.len()];
    let mut receipts = Vec::with_capacity(ordered.len());
    let mut whole = StableHasher::new();
    let mut prefix_only = StableHasher::new();

    for piece in &ordered {
        let tokens = estimate_tokens(&piece.text);

        // Find the slot index for this piece's slot name.
        let slot_index = slot_caps.iter().position(|(name, _)| *name == piece.slot);

        // Enforce per-slot ceiling: skip pieces that would exceed their slot cap.
        if let Some(index) = slot_index {
            let cap = slot_caps[index].1;
            if slot_used[index] + tokens > cap {
                continue; // Drop this piece: slot ceiling would be exceeded.
            }
            slot_used[index] += tokens;
        }

        // Enforce overall budget ceiling.
        let used_so_far: usize = slot_used.iter().sum();
        if used_so_far + tokens > budget.total {
            continue; // Drop this piece: overall budget would be exceeded.
        }

        let mut piece_hasher = StableHasher::new();
        piece_hasher.write(piece.slot);
        piece_hasher.write(&piece.text);
        let hash = piece_hasher.finish();

        whole.write(piece.slot);
        whole.write(&piece.text);
        if piece.layer.is_prefix() {
            prefix_only.write(piece.slot);
            prefix_only.write(&piece.text);
        }

        let slot_index = slot_caps
            .iter()
            .position(|(name, _)| *name == piece.slot)
            .expect("piece slot must be registered in default_slots");
        slot_used[slot_index] += tokens;
        layer_tokens_used[piece.layer.index()] += tokens;
        receipts.push(PieceReceipt {
            layer: piece.layer.index() as u8,
            slot: piece.slot,
            tokens,
            hash,
        });

        let target = if piece.layer.is_prefix() {
            &mut prefix
        } else {
            &mut tail
        };
        target.push_str("## ");
        target.push_str(piece.slot);
        target.push('\n');
        target.push_str(&piece.text);
        target.push_str("\n\n");
    }

    let total_tokens = layer_tokens_used.iter().sum();
    Assembly {
        prefix,
        tail,
        receipt: Receipt {
            layer_tokens: layer_tokens_used,
            total_tokens,
            prefix_hash: prefix_only.finish(),
            hash: whole.finish(),
            pieces: receipts,
        },
    }
}

/// Identity of the cacheable prefix alone.
///
/// Takes the pieces rather than the rendered string so a caller can compare "what is about to be
/// sent" with "what was sent last turn" before rendering. `Turn` pieces are ignored.
pub fn prefix_hash(prefix: &[Piece]) -> u64 {
    let mut hasher = StableHasher::new();
    for piece in prefix.iter().filter(|piece| piece.layer.is_prefix()) {
        hasher.write(piece.slot);
        hasher.write(&piece.text);
    }
    hasher.finish()
}
/// Per-slot token accounting for an assembly.
///
/// `Receipt::layer_tokens` holds one entry per layer (Global, Project, Session, Turn).
/// `Receipt::pieces` holds per-slot breakdown; `slot_tokens` returns the per-`slot`
/// breakdown from [`Receipt::pieces`].
pub fn slot_tokens(receipt: &Receipt) -> Vec<(&'static str, usize)> {
    let mut out = Vec::with_capacity(receipt.pieces.len());
    for piece in &receipt.pieces {
        out.push((piece.slot, piece.tokens));
    }
    out
}
pub fn hash_label(hash: u64) -> String {
    hex(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{default_budget, default_slots};

    fn test_budget() -> (Vec<Slot>, Budget) {
        let slots = default_slots();
        let total: usize = slots.iter().map(|s| s.max_tokens).sum();
        (slots, default_budget(total))
    }

    fn global() -> Piece {
        Piece {
            layer: Layer::Global,
            slot: "identity",
            text: "You are memnoth.".to_string(),
        }
    }

    fn project(text: &str) -> Piece {
        Piece {
            layer: Layer::Project,
            slot: "policy",
            text: text.to_string(),
        }
    }

    fn turn(text: &str) -> Piece {
        Piece {
            layer: Layer::Turn,
            slot: "memory",
            text: text.to_string(),
        }
    }

    #[test]
    fn turn_content_lands_in_the_tail_and_never_in_the_prefix() {
        let (slots, budget) = test_budget();
        let assembly = assemble(
            vec![global(), project("no unwraps"), turn("lease.rs:41")],
            &slots,
            &budget,
        );
        assert!(assembly.prefix.contains("no unwraps"));
        assert!(!assembly.prefix.contains("lease.rs:41"));
        assert!(assembly.tail.contains("lease.rs:41"));
        assert!(!assembly.tail.contains("no unwraps"));
    }

    #[test]
    fn the_prefix_hash_ignores_the_turn_layer() {
        let base = vec![global(), project("no unwraps")];
        let mut with_turn = base.clone();
        with_turn.push(turn("a tool result that arrived just now"));
        assert_eq!(prefix_hash(&base), prefix_hash(&with_turn));
    }

    #[test]
    fn a_project_change_changes_every_hash() {
        let (slots, budget) = test_budget();
        let a = assemble(vec![global(), project("no unwraps")], &slots, &budget);
        let b = assemble(vec![global(), project("no unwraps, ever")], &slots, &budget);
        assert_ne!(a.receipt.hash, b.receipt.hash);
        assert_ne!(a.receipt.prefix_hash, b.receipt.prefix_hash);
        assert_ne!(a.prefix, b.prefix);
    }

    #[test]
    fn the_receipt_prefix_hash_agrees_with_prefix_hash_of_the_pieces() {
        let (slots, budget) = test_budget();
        let pieces = vec![global(), project("rules"), turn("tail")];
        let receipt = assemble(pieces.clone(), &slots, &budget).receipt;
        assert_eq!(receipt.prefix_hash, prefix_hash(&pieces));
    }

    #[test]
    fn a_turn_change_leaves_the_receipt_prefix_hash_alone() {
        let (slots, budget) = test_budget();
        let a = assemble(vec![global(), turn("first result")], &slots, &budget);
        let b = assemble(
            vec![global(), turn("a completely different result")],
            &slots,
            &budget,
        );
        assert_eq!(a.receipt.prefix_hash, b.receipt.prefix_hash);
        assert_ne!(a.receipt.hash, b.receipt.hash);
    }

    #[test]
    fn identical_inputs_produce_identical_bytes_and_hashes() {
        let (slots, budget) = test_budget();
        let pieces = vec![global(), project("rules"), turn("tail")];
        let a = assemble(pieces.clone(), &slots, &budget);
        let b = assemble(pieces, &slots, &budget);
        assert_eq!(a.prefix, b.prefix);
        assert_eq!(a.tail, b.tail);
        assert_eq!(a.receipt, b.receipt);
    }

    #[test]
    fn layers_render_in_cache_order_regardless_of_input_order() {
        let (slots, budget) = test_budget();
        let piece = |layer, slot, text: &str| Piece {
            layer,
            slot,
            text: text.to_string(),
        };
        let assembly = assemble(
            vec![
                piece(Layer::Turn, "memory", "TAIL"),
                piece(Layer::Project, "policy", "PROJECT"),
                piece(Layer::Global, "identity", "GLOBAL"),
            ],
            &slots,
            &budget,
        );
        let global_at = assembly.prefix.find("GLOBAL").unwrap();
        let project_at = assembly.prefix.find("PROJECT").unwrap();
        assert!(
            global_at < project_at,
            "Global must precede Project: {}",
            assembly.prefix
        );
        assert!(assembly.tail.starts_with("## memory"));
    }

    #[test]
    fn layer_tokens_sum_to_the_total() {
        let (slots, budget) = test_budget();
        let assembly = assemble(
            vec![global(), project("a longer rule text here"), turn("tail")],
            &slots,
            &budget,
        );
        assert_eq!(
            assembly.receipt.layer_tokens.iter().sum::<usize>(),
            assembly.receipt.total_tokens
        );
        assert_eq!(assembly.receipt.pieces.len(), 3);
        // Global (identity) + Project (policy) + Turn (memory)
        assert!(assembly.receipt.layer_tokens[0] > 0);
        assert!(assembly.receipt.layer_tokens[1] > 0);
        assert!(assembly.receipt.layer_tokens[3] > 0);
    }

    #[test]
    fn an_empty_assembly_is_empty_not_malformed() {
        let (slots, budget) = test_budget();
        let assembly = assemble(vec![], &slots, &budget);
        assert!(assembly.prefix.is_empty());
        assert!(assembly.tail.is_empty());
        assert_eq!(assembly.receipt.total_tokens, 0);
        assert!(assembly.receipt.pieces.is_empty());
        assert_eq!(assembly.receipt.layer_tokens, [0, 0, 0, 0]);
    }

    #[test]
    fn the_receipt_names_every_piece_with_its_layer() {
        let (slots, budget) = test_budget();
        let assembly = assemble(vec![global()], &slots, &budget);
        let piece = &assembly.receipt.pieces[0];
        assert_eq!(piece.layer, 0);
        assert_eq!(piece.slot, "identity");
        assert_eq!(piece.tokens, estimate_tokens("You are memnoth."));
        assert_eq!(piece.hash, {
            let mut h = StableHasher::new();
            h.write("identity");
            h.write("You are memnoth.");
            h.finish()
        });
    }
}
