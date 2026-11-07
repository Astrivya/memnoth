//! Bespoke windows for verb families, one module per family.
//!
//! `popover::spec_for` keeps the specs for the cloud console verbs and the KG
//! rows it has always drawn, then asks here, then falls back to the generic
//! window over a verb's own fields. Splitting by family is what keeps a spec file
//! readable as the verb surface grows: a new `vault …` list is an arm in
//! `vault.rs`, not a thousand more lines in one matcher.
//!
//! A module returns `None` for a command it does not own, and a spec with no rows
//! is filtered out by the caller — an empty window is worse than the sentence the
//! verb printed.

use serde_json::Value;

use crate::tui::overlays::popover::PopoverSpec;

pub(crate) mod kg;
pub(crate) mod local;
pub(crate) mod ops;
pub(crate) mod projects;
pub(crate) mod vault;

/// The bespoke spec for this command, if its family has one.
pub fn spec(command: &str, data: &Value) -> Option<PopoverSpec> {
    kg::spec(command, data)
        .or_else(|| projects::spec(command, data))
        .or_else(|| vault::spec(command, data))
        .or_else(|| local::spec(command, data))
        .or_else(|| ops::spec(command, data))
}
