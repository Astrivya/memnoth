//! Fixed provenance shape for beliefs proposed into the shared memory bus.
//!
//! Every proposal carries exactly six fields — `{run_id, agent, user, turn,
//! file, line}`. The shape is a contract (VLT-03): nothing in the write path
//! may add or drop a field, and a belief that cannot show all six is not
//! attributable and must not be served by recall.
//!
//! This file is loaded as `workflow::provenance` (see the `#[path]` declaration
//! in `workflow.rs`) so the crate root needs no extra module declaration.

use serde::{Deserialize, Serialize};

/// The six field names of the fixed provenance shape, in contract order.
pub const PROVENANCE_KEYS: [&str; 6] = ["run_id", "agent", "user", "turn", "file", "line"];

/// Provenance of one proposed belief: the run, agent, person, turn and source
/// location that produced it.
///
/// The six fields are the whole contract — keep them in sync with
/// [`PROVENANCE_KEYS`] and with `has_full_provenance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The run this belief was produced in.
    pub run_id: String,
    /// The agent that produced it.
    pub agent: String,
    /// The person the run acts for; a child never invents this.
    pub user: String,
    /// The turn within the run.
    pub turn: u64,
    /// The file the belief references or was recorded against.
    pub file: String,
    /// The line within the file.
    pub line: usize,
}

impl Provenance {
    /// Build provenance for a belief. `user` defaults to `"local"`; a caller
    /// that knows the person overrides it with [`Self::with_user`].
    pub fn new(run_id: &str, agent: &str, turn: u64, file: &str, line: usize) -> Self {
        Self {
            run_id: run_id.to_string(),
            agent: agent.to_string(),
            user: "local".to_string(),
            turn,
            file: file.to_string(),
            line,
        }
    }

    /// Name the person this run acts for. Blank input leaves the default
    /// untouched, so a child cannot erase attribution by passing an empty name.
    pub fn with_user(mut self, user: &str) -> Self {
        let user = user.trim();
        if !user.is_empty() {
            self.user = user.to_string();
        }
        self
    }

    /// Render as the JSON string stored on a proposal or vault row.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Stable, human-readable source label: `agent@run_id file:line`.
    ///
    /// Used by contradiction reports so both sides of a disagreement name the
    /// child and the exact location it was reading.
    pub fn source(&self) -> String {
        format!("{}@{} {}:{}", self.agent, self.run_id, self.file, self.line)
    }

    /// True when every textual field is populated (run, agent, user, file).
    pub fn is_complete(&self) -> bool {
        !self.run_id.trim().is_empty()
            && !self.agent.trim().is_empty()
            && !self.user.trim().is_empty()
            && !self.file.trim().is_empty()
    }
}

/// True when `json` carries the complete fixed provenance shape.
///
/// Accepts the integer fields as any JSON number representable as `u64`; the
/// four text fields must be non-empty strings. Used as a write-path guard so an
/// unattributable belief never becomes a proposal, and as a read-path check so
/// no belief reaches recall without provenance.
pub fn has_full_provenance(json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    PROVENANCE_KEYS.iter().all(|key| match *key {
        "turn" | "line" => value[*key].as_u64().is_some(),
        _ => value[*key]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_shape_is_fixed_and_complete() {
        let prov = Provenance::new("run-1", "child-a", 3, "src/lib.rs", 42);
        assert!(prov.is_complete());
        assert!(has_full_provenance(&prov.to_json()));
        assert_eq!(prov.source(), "child-a@run-1 src/lib.rs:42");
        assert_eq!(
            prov.clone().with_user("priya").user,
            "priya",
            "a child can be told who it acts for"
        );
        assert_eq!(
            prov.clone().with_user("  ").user,
            "local",
            "blank input cannot erase attribution"
        );
    }

    #[test]
    fn incomplete_provenance_is_rejected() {
        assert!(
            !has_full_provenance("{}"),
            "an empty provenance object is unattributable"
        );
        assert!(!has_full_provenance("not json"));
        let mut value = serde_json::to_value(Provenance::new("r", "a", 1, "f.rs", 1)).unwrap();
        value.as_object_mut().unwrap().remove("line");
        assert!(
            !has_full_provenance(&value.to_string()),
            "a dropped field breaks the contract"
        );
    }
}
