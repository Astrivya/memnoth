//! The context authority: everything that decides what the model sees, and why.
//!
//! Four pure modules, no IO beyond reading instruction files, no model calls:
//!
//! * [`instruct`] — instruction discovery, precedence (`Enterprise > Team > Project > User >
//!   Session`) and the linter that drops rules the formatter already enforces.
//! * [`assemble`] — the four-layer prompt (`Global`, `Project`, `Session`, `Turn`), split into a
//!   byte-stable prefix and a mutable tail, with a receipt that reproduces.
//! * [`budget`] — the per-slot knapsack that fills the tail without overrunning the window.
//! * [`compact`] — selection-first compaction: exact facts are kept verbatim, only narrative may
//!   be summarised, and retention is checked rather than asserted.
//!
//! The design constraints are in `docs/AUDIT-OF-CONTEXT-RESEARCH-2026-10-23.md` §5 (C1–C3, C8):
//! prefix bytes never change mid-session, receipts are deterministic, and a compacted context
//! must still answer exact questions about decisions, paths, symbols and versions.

pub mod assemble;
pub mod budget;
pub mod compact;
pub mod instruct;
pub mod stable;

pub use assemble::{Assembly, Layer, Piece, PieceReceipt, Receipt, assemble, prefix_hash};
pub use budget::{Allocation, Budget, DropReason, Item, Slot, allocate, estimate_tokens};
pub use compact::{Fact, FactKind, Selection, retention_ok, select};
pub use instruct::{
    DropReason as InstructionDropReason, Dropped, InstructionSet, Rule, RuleKind, Scope, load_from,
    merge, parse,
};
pub use stable::{StableHasher, hex};
