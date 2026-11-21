//! Verify an assembly receipt against the context that was actually assembled.
//!
//! An assembly receipt is a claim: "this is the stable prefix hash of the pieces I
//! sent, and these are the pieces I left out and why." A claim is only worth as
//! much as the independent check behind it. [`verify`] re-derives the prefix
//! identity from the recorded layer pieces with the *same* function the assembler
//! used ([`memnoth_context::prefix_hash`]) and compares it to the hash the receipt
//! recorded. A mismatch is a failure, never a warning: if the receipt cannot be
//! reproduced, the assembler is the thing to fix, not the model.
//!
//! This module is deliberately pure: no files, no network, no model. It takes a
//! receipt document as JSON and the pieces that were assembled, and returns a
//! verdict. Reading the receipt from disk or stdin is the CLI's job.
//!
//! The document mirrors the real [`memnoth_context::Receipt`] shape:
//! `prefix_hash` / `total_hash` are the `hex(u64)` labels, `pieces` mirrors
//! `PieceReceipt`, and `dropped` mirrors the `(slot, DropReason)` pairs.

use std::collections::HashMap;

use memnoth_context::{Assembly, DropReason, Piece, StableHasher, hex, prefix_hash};
use serde::{Deserialize, Serialize};

/// Schema tag written into receipt documents. A reader refuses a different
/// major version rather than guessing at fields that moved.
pub const SCHEMA: &str = "mn-context-receipt/1";
/// The only major version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

fn default_v() -> u32 {
    SCHEMA_VERSION
}

/// One recorded drop: the slot that was left out and why.
///
/// `reason` is a stable string, not free text: `over_slot`, `over_total` or
/// `unknown_slot` ([`DropReason`]). The design document's `slot_cap` is accepted
/// as an alias for `over_slot` on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedDrop {
    pub slot: String,
    pub reason: String,
}

impl RecordedDrop {
    /// Record a drop from the assembler's own reason enum.
    pub fn from_slot_reason(slot: &str, reason: DropReason) -> Self {
        Self {
            slot: slot.to_string(),
            reason: drop_reason_label(reason).to_string(),
        }
    }

    /// Decode the reason back to the assembler's enum. `None` means the document
    /// named a reason this build does not know, which a caller must treat as a
    /// malformed receipt rather than silently ignoring.
    pub fn drop_reason(&self) -> Option<DropReason> {
        parse_drop_reason(&self.reason)
    }
}

/// Stable string for a [`DropReason`]. These are the values written to JSON.
pub fn drop_reason_label(reason: DropReason) -> &'static str {
    match reason {
        DropReason::OverSlot => "over_slot",
        DropReason::OverTotal => "over_total",
        DropReason::UnknownSlot => "unknown_slot",
    }
}

/// Decode a recorded reason string. `slot_cap` is the alias used in the design
/// document's example receipt.
pub fn parse_drop_reason(label: &str) -> Option<DropReason> {
    match label {
        "over_slot" | "slot_cap" => Some(DropReason::OverSlot),
        "over_total" => Some(DropReason::OverTotal),
        "unknown_slot" | "unknown" => Some(DropReason::UnknownSlot),
        _ => None,
    }
}

/// The per-piece identity recorded by the assembler, mirroring `PieceReceipt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedPiece {
    /// Layer ordinal 0-3 (Global, Project, Session, Turn).
    pub layer: u8,
    pub slot: String,
    pub tokens: usize,
    /// FNV-1a digest of slot + text, as recorded by the assembler.
    pub hash: u64,
}

/// The receipt document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReceipt {
    /// Major schema version; defaults to 1 when absent.
    #[serde(default = "default_v")]
    pub v: u32,
    #[serde(default)]
    pub model: Option<String>,
    /// Identity of the cacheable prefix alone: `hex(u64)` of `prefix_hash`.
    pub prefix_hash: String,
    /// Identity of the whole rendered prompt, when recorded.
    #[serde(default)]
    pub total_hash: Option<String>,
    /// Per-layer token accounting, one entry per layer in cache order.
    #[serde(default)]
    pub layer_tokens: [usize; 4],
    #[serde(default)]
    pub total_tokens: usize,
    /// Per-slot accounting, mirroring `Receipt::pieces`.
    #[serde(default)]
    pub pieces: Vec<RecordedPiece>,
    /// What was left out, in allocation order, each with a reason.
    #[serde(default)]
    pub dropped: Vec<RecordedDrop>,
}

impl ContextReceipt {
    /// Build a receipt document from a real assembler [`Assembly`].
    ///
    /// This is the bridge from the crate's types to the on-disk document: the
    /// hashes become fixed-width hex labels and the drop reasons become stable
    /// strings, so nothing is hand-written twice.
    pub fn from_assembly(assembly: &Assembly, model: Option<&str>) -> Self {
        let receipt = &assembly.receipt;
        Self {
            v: SCHEMA_VERSION,
            model: model.map(str::to_string),
            prefix_hash: hex(receipt.prefix_hash),
            total_hash: Some(hex(receipt.hash)),
            layer_tokens: receipt.layer_tokens,
            total_tokens: receipt.total_tokens,
            pieces: receipt
                .pieces
                .iter()
                .map(|piece| RecordedPiece {
                    layer: piece.layer,
                    slot: piece.slot.to_string(),
                    tokens: piece.tokens,
                    hash: piece.hash,
                })
                .collect(),
            dropped: receipt
                .dropped
                .iter()
                .map(|(slot, reason)| RecordedDrop::from_slot_reason(slot, *reason))
                .collect(),
        }
    }

    /// Serialize this document to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Re-assemble the prefix from the recorded layer pieces and report the
    /// verdict. This is the pure core behind [`verify`]; it takes an
    /// already-parsed document so a caller holding one does not round-trip it.
    ///
    /// The assembler hashes only *admitted* pieces into `prefix_hash` — a piece
    /// it dropped under a slot cap is not part of the prefix. So re-assembly
    /// selects the admitted pieces by the per-piece digests the receipt recorded
    /// in `pieces`, rather than blindly hashing every input. That is what lets a
    /// dropped-piece receipt reproduce its own hash while still reporting what
    /// was left out and why.
    pub fn check(&self, assembled_context: &[Piece]) -> Result<Verification, VerifyError> {
        if self.v != SCHEMA_VERSION {
            return Err(VerifyError::InvalidReceipt(format!(
                "unsupported schema version {}; this build reads {SCHEMA}",
                self.v
            )));
        }
        for drop in &self.dropped {
            if drop.drop_reason().is_none() {
                return Err(VerifyError::InvalidReceipt(format!(
                    "slot '{}' has unknown drop reason '{}'",
                    drop.slot, drop.reason
                )));
            }
        }

        // Every input piece is either admitted (a `pieces` row) or dropped (a
        // `dropped` row). A receipt that cannot account for all of them is not
        // reproducible, so refuse it before hashing anything.
        if !self.pieces.is_empty() {
            let accounted = self.pieces.len() + self.dropped.len();
            if assembled_context.len() != accounted {
                return Err(VerifyError::IncompleteReceipt(format!(
                    "receipt accounts for {accounted} piece(s) but {} were supplied",
                    assembled_context.len()
                )));
            }
        }

        // Multiset of admitted *prefix* digests. Only prefix layers feed the
        // stable hash, so Turn pieces are deliberately exempt: the whole point
        // of a stable prefix is that tail drift does not invalidate it. An
        // input piece is re-admitted only as many times as the receipt records
        // it, so duplicates with identical text cannot smuggle a dropped copy
        // back into the prefix.
        let mut remaining: HashMap<u64, usize> = HashMap::new();
        for recorded_piece in &self.pieces {
            if is_prefix_layer(recorded_piece.layer) {
                *remaining.entry(recorded_piece.hash).or_insert(0) += 1;
            }
        }
        let mut admitted: Vec<Piece> = Vec::new();
        if self.pieces.is_empty() {
            // A hand-written receipt with no per-piece rows has no drop list to
            // apply either; treat every supplied piece as admitted.
            admitted.extend_from_slice(assembled_context);
        } else {
            for piece in assembled_context {
                if !is_prefix_layer(piece.layer.index() as u8) {
                    // Tail content may change every turn; it never reaches the
                    // prefix hash, so there is nothing to reconcile.
                    admitted.push(piece.clone());
                    continue;
                }
                let digest = piece_digest(piece);
                if let Some(count) = remaining.get_mut(&digest)
                    && *count > 0
                {
                    *count -= 1;
                    admitted.push(piece.clone());
                }
                // A prefix piece with no recorded digest was dropped by the
                // budget; it is excluded from re-assembly and reported from the
                // receipt's `dropped` rows.
            }
            let missing = remaining.values().sum::<usize>();
            if missing > 0 {
                return Err(VerifyError::IncompleteReceipt(format!(
                    "{missing} recorded admitted prefix piece(s) are missing from the supplied context"
                )));
            }
        }

        let recorded = parse_hash(&self.prefix_hash)?;
        let recomputed = prefix_hash(&admitted);
        if recorded != recomputed {
            return Err(VerifyError::PrefixMismatch {
                recorded: self.prefix_hash.clone(),
                recomputed: hex(recomputed),
            });
        }
        Ok(Verification {
            ok: true,
            schema: SCHEMA.to_string(),
            recorded_prefix_hash: self.prefix_hash.clone(),
            recomputed_prefix_hash: hex(recomputed),
            prefix_pieces: admitted
                .iter()
                .filter(|piece| piece.layer != memnoth_context::Layer::Turn)
                .count(),
            dropped: self.dropped.clone(),
        })
    }
}

/// Whether a layer ordinal belongs to the cacheable prefix (all but Turn).
pub fn is_prefix_layer(layer: u8) -> bool {
    layer < memnoth_context::Layer::Turn.index() as u8
}

/// Identity of one input piece, computed exactly as the assembler computes
/// `PieceReceipt::hash`: slot bytes, then text bytes, each FNV-1a separated.
pub fn piece_digest(piece: &Piece) -> u64 {
    let mut hasher = StableHasher::new();
    hasher.write(piece.slot);
    hasher.write(&piece.text);
    hasher.finish()
}

/// The verdict of a verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verification {
    /// True only when the re-derived prefix hash matched the recorded one.
    /// A caller must fail closed when this is false.
    pub ok: bool,
    /// Schema of the receipt that was checked.
    pub schema: String,
    /// The prefix hash the receipt claimed.
    pub recorded_prefix_hash: String,
    /// The prefix hash re-derived from the recorded layer pieces.
    pub recomputed_prefix_hash: String,
    /// How many of the assembled pieces belong to the cacheable prefix.
    pub prefix_pieces: usize,
    /// What the receipt recorded as dropped, with reasons.
    pub dropped: Vec<RecordedDrop>,
}

/// Why verification did not produce a passing verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The document could not be parsed or named an unsupported schema.
    InvalidReceipt(String),
    /// The receipt had no prefix hash to check.
    MissingPrefixHash,
    /// The prefix hash was not a hex (or decimal) `u64`.
    MalformedPrefixHash(String),
    /// The receipt's admitted + dropped rows did not match the supplied pieces,
    /// or a recorded admitted piece was absent. Fail closed: it cannot reproduce.
    IncompleteReceipt(String),
    /// The re-derived prefix hash differs from the recorded one. Fail closed.
    PrefixMismatch {
        recorded: String,
        recomputed: String,
    },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidReceipt(msg) => write!(f, "invalid context receipt: {msg}"),
            Self::MissingPrefixHash => write!(f, "context receipt has no prefix_hash"),
            Self::MalformedPrefixHash(value) => {
                write!(f, "context receipt prefix_hash is not a u64: {value}")
            }
            Self::IncompleteReceipt(msg) => {
                write!(f, "context receipt cannot be re-assembled: {msg}")
            }
            Self::PrefixMismatch {
                recorded,
                recomputed,
            } => write!(
                f,
                "prefix hash mismatch: receipt recorded {recorded}, re-assembly produced {recomputed}"
            ),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Parse a hash label. Receipts store the 16-character lowercase hex produced
/// by [`memnoth_context::hex`]; a `0x` prefix or a decimal `u64` are accepted
/// for hand-written receipts.
pub fn parse_hash(label: &str) -> Result<u64, VerifyError> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Err(VerifyError::MissingPrefixHash);
    }
    let body = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(body, 16)
        .or_else(|_| body.parse::<u64>())
        .map_err(|_| VerifyError::MalformedPrefixHash(label.to_string()))
}

/// Verify a receipt document against the pieces that were actually assembled.
///
/// `receipt_json` is the document emitted before the request; `assembled_context`
/// is the recorded layer pieces the assembler consumed. The prefix identity is
/// re-derived from the admitted pieces with the assembler's own `prefix_hash`,
/// so a tampered hash or a changed prefix rule fails closed here. Piece order
/// is intentionally irrelevant: `prefix_hash` canonicalises it.
///
/// Returns `Ok` only on a match. A parse error, a malformed hash, or a hash
/// mismatch is an `Err` — a verification that could not reproduce the receipt
/// is never reported as a pass.
pub fn verify(
    receipt_json: &str,
    assembled_context: &[Piece],
) -> Result<Verification, VerifyError> {
    let receipt: ContextReceipt = serde_json::from_str(receipt_json)
        .map_err(|err| VerifyError::InvalidReceipt(err.to_string()))?;
    receipt.check(assembled_context)
}

/// Human-readable verdict, with one line per recorded drop.
pub fn render(verdict: &Verification) -> String {
    let mut out = format!(
        "{} — prefix {} ({} prefix piece(s))",
        if verdict.ok { "OK" } else { "FAILED" },
        verdict.recomputed_prefix_hash,
        verdict.prefix_pieces
    );
    if verdict.dropped.is_empty() {
        out.push_str("\n  dropped: none");
    } else {
        out.push_str(&format!("\n  dropped: {} piece(s)", verdict.dropped.len()));
        for drop in &verdict.dropped {
            out.push_str(&format!("\n    - {} ({})", drop.slot, drop.reason));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use memnoth_context::{
        Budget, Layer, Slot, assemble, budget::default_budget, budget::default_slots,
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

    #[test]
    fn a_matching_receipt_verifies() {
        let (assembly, pieces) = assembled();
        let json = ContextReceipt::from_assembly(&assembly, Some("unit-test"))
            .to_json()
            .unwrap();
        let verdict = verify(&json, &pieces).expect("a truthful receipt must verify");
        assert!(verdict.ok);
        assert_eq!(
            verdict.recorded_prefix_hash,
            hex(assembly.receipt.prefix_hash)
        );
        assert_eq!(verdict.recomputed_prefix_hash, verdict.recorded_prefix_hash);
        assert_eq!(verdict.prefix_pieces, 2, "Global + Project, not Turn");
    }

    #[test]
    fn a_tampered_prefix_fails_closed() {
        let (assembly, pieces) = assembled();
        let mut receipt = ContextReceipt::from_assembly(&assembly, None);
        // Flip one hex nibble of the recorded prefix hash. The text is intact;
        // the claim about it is not.
        let mut tampered = receipt.prefix_hash.into_bytes();
        tampered[0] = if tampered[0] == b'0' { b'1' } else { b'0' };
        receipt.prefix_hash = String::from_utf8(tampered).unwrap();
        let json = receipt.to_json().unwrap();

        let err = verify(&json, &pieces).expect_err("a tampered prefix must not verify");
        assert!(matches!(err, VerifyError::PrefixMismatch { .. }));
        assert!(err.to_string().contains("mismatch"));
    }

    #[test]
    fn a_dropped_piece_reason_is_reported() {
        // One slot, one cap: the second piece is dropped as over_slot.
        let slots: Vec<Slot> = vec![Slot {
            name: "identity",
            max_tokens: 10,
        }];
        let budget: Budget = Budget { total: 100 };
        let pieces = vec![
            Piece {
                layer: Layer::Global,
                slot: "identity",
                text: "a".repeat(40), // 10 tokens, fills the slot
            },
            Piece {
                layer: Layer::Global,
                slot: "identity",
                text: "b".repeat(40), // 10 more, over the slot cap
            },
        ];
        let assembly = assemble(pieces.clone(), &slots, &budget);
        let receipt = ContextReceipt::from_assembly(&assembly, None);
        assert_eq!(receipt.dropped.len(), 1);
        assert_eq!(receipt.dropped[0].slot, "identity");
        assert_eq!(receipt.dropped[0].reason, "over_slot");
        assert_eq!(receipt.dropped[0].drop_reason(), Some(DropReason::OverSlot));

        let verdict = verify(&receipt.to_json().unwrap(), &pieces).unwrap();
        assert!(verdict.ok);
        assert_eq!(verdict.dropped, receipt.dropped);
        assert!(render(&verdict).contains("identity (over_slot)"));
    }

    #[test]
    fn a_malformed_hash_fails_closed() {
        let (assembly, pieces) = assembled();
        let mut receipt = ContextReceipt::from_assembly(&assembly, None);
        receipt.prefix_hash = "not-a-hash".to_string();
        let err = verify(&receipt.to_json().unwrap(), &pieces).unwrap_err();
        assert!(matches!(err, VerifyError::MalformedPrefixHash(_)));
    }

    #[test]
    fn an_unknown_schema_version_is_refused() {
        let (assembly, pieces) = assembled();
        let mut receipt = ContextReceipt::from_assembly(&assembly, None);
        receipt.v = 99;
        let err = verify(&receipt.to_json().unwrap(), &pieces).unwrap_err();
        assert!(matches!(err, VerifyError::InvalidReceipt(_)));
    }
}
