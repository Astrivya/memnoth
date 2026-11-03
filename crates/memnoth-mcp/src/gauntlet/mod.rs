//! The Memory Gauntlet — shared foundations for every benchmark outside LoCoMo/LongMemEval.
//!
//! Why this module exists, in one paragraph. Nine of the eleven benchmarks in the
//! gauntlet score *memory as changing state* — forgetting, updating, ordering,
//! causality, preference evolution — rather than recall of a static haystack. The
//! previous two lanes optimized LoCoMo and LongMemEval and discovered their classes
//! by failing them, which is why the first honest full-500 LongMemEval run (70.0%)
//! arrived *after* a week of tuning. This module is the answer to that process
//! failure: the scoring instruments and the ingest shapes for the rest of the
//! gauntlet are written and tested **before** any of those benchmarks is run.
//!
//! Nothing here calls a model or touches the network. It is deliberately pure
//! logic so that it can be unit-tested to exhaustion, because a metric bug is
//! invisible in the output — it produces a plausible number that is simply wrong.
//! (We have already paid for that lesson once: the LongMemEval loader silently
//! dropped 32 integer golds, removing the hardest 6.4% of the set and *raising* our
//! apparent score.)
//!
//! ## What is covered
//!
//! | section | serves |
//! |---|---|
//! | [`nugget`] — BEAM's 0/0.5/1.0 rubric clamp | BEAM 100K/1M/10M |
//! | [`kendall`] — τ-b over event orderings | BEAM `event_ordering` |
//! | [`temporal`] — duration/interval arithmetic | BEAM, LongMemEval temporal |
//! | [`reconcile`] — belief-at-T with a logged reason | FAMA/Memora, BEAM `contradiction_resolution`, MemoryAgentBench selective forgetting |
//! | [`verbatim`] — raw spans with provenance | LongMemEval `single-session-assistant`, HaluMem, FAMA |
//! | [`loaders`] — BEAM / PersonaMem / MemoryArena / HaluMem ingest shapes | the gauntlet |

// This module was one file and is now a tree. `dead_code` was already allowed here
// because the instruments are consumed selectively by the bench harnesses, and the
// re-exports below inherit the same property: a given build reaches only some of
// them, so the rest read as unused imports. They are kept because `crate::gauntlet::X`
// must keep resolving exactly as it did before the split.
#![allow(dead_code, unused_imports)]

// Shared prelude. Re-exported (rather than left private) so each submodule can say
// `use super::*;` and keep exactly the names it had when this was a single file.
pub(crate) use anyhow::{Context, Result};
pub(crate) use serde_json::Value;
pub(crate) use std::collections::{BTreeMap, HashMap};

// Split by concern; the doc table above maps each module to the benchmarks it serves.
mod agentic;
mod citations;
mod helpers;
mod intrinsic;
mod kendall;
mod loaders;
mod nugget;
mod reconcile;
mod temporal;
mod verbatim;

pub use agentic::*;
pub use citations::*;
pub use helpers::*;
pub use intrinsic::*;
pub use kendall::*;
pub use loaders::*;
pub use nugget::*;
pub use reconcile::*;
pub use temporal::*;
pub use verbatim::*;

#[cfg(test)]
mod tests;
