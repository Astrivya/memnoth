//! memnoth code search engine.
//!
//! First-party lexical search for agents: exact grep grouped by file and
//! enclosing symbol, ranked file discovery, structural outline, and relation
//! tracing. Built on ripgrep's `ignore` crate to walk the tree and `globset`
//! for the skip rules; the structural view is a line scanner, not a parser —
//! full tree-sitter extraction belongs to `memnoth-codegraph`.
//!
//! The module layout mirrors the tool surface that consumes it (`cli`, `find`,
//! `outline`, `search`, `smart_dsl`, `smart_engine`, `render`) so the swap is
//! mechanical. Behavior contract, in priority order:
//!
//! 1. **Exact matches are exact.** `grep` never ranks, never drops, never
//!    reorders: same bytes in, same matches out, deterministic order.
//! 2. **Ranked modes are deterministic.** Same tree + same query = same order,
//!    or the ranker is a bug.
//! 3. **No index, no daemon, no network.** Everything reads the working tree.

pub mod cli;
pub mod context;
pub mod find;
pub mod outline;
pub mod render;
pub mod search;
pub mod smart_dsl;
pub mod smart_engine;
pub mod structure;
pub mod workspace;
