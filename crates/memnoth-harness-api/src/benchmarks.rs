//! Ablation matrix benchmarks.
//!
//! Sweeps the harness across five binary ablation axes — memory, subagents, repo
//! map, compute and cache — producing a 32-cell matrix. Each cell is measured
//! against the committed scoreboard JSON, and its lifecycle state is tracked so
//! the harness knows which of the 32 cells have been measured (currently 6).

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Total cells in the ablation matrix: 5 binary axes = 32.
pub const TOTAL_CELLS: usize = 32;

/// Cells with committed scoreboard results.
pub const COMMITTED_CELLS: usize = 6;

/// One ablation axis: component enabled or disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Axis {
    /// Component disabled for this cell.
    Off,
    /// Component enabled for this cell.
    On,
}

/// The five ablation axes of the matrix. The first three are the named contract
/// axes (memory, subagents, repo map); the remaining two are harness-internal
/// axes that complete the 32-cell grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AblationAxis {
    /// Whether the memory vault is consulted.
    Memory,
    /// Whether subagent delegation is available.
    Subagents,
    /// Whether the code graph / repo map is consulted.
    RepoMap,
    /// Whether local compute (embedding / cross-encoder) is available.
    Compute,
    /// Whether result caching is enabled.
    Cache,
}

/// One cell of the ablation matrix: a setting for every axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AblationCell {
    pub memory: Axis,
    pub subagents: Axis,
    pub repo_map: Axis,
    pub compute: Axis,
    pub cache: Axis,
}

impl AblationCell {
    /// Whether every axis is enabled (the full-pipeline reference cell).
    pub fn is_full(self) -> bool {
        self.memory == Axis::On
            && self.subagents == Axis::On
            && self.repo_map == Axis::On
            && self.compute == Axis::On
            && self.cache == Axis::On
    }

    /// Whether every axis is disabled (the bare control cell).
    pub fn is_control(self) -> bool {
        self.memory == Axis::Off
            && self.subagents == Axis::Off
            && self.repo_map == Axis::Off
            && self.compute == Axis::Off
            && self.cache == Axis::Off
    }
}

/// Lifecycle state of one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CellState {
    /// Not yet measured.
    Pending,
    /// Measurement in progress.
    Running,
    /// Measurement complete; result committed to the scoreboard.
    Done,
    /// Measurement failed.
    Failed,
}

/// Committed scoreboard value for one cell, deserialised from committed JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreboardEntry {
    /// The cell this result belongs to.
    pub cell: AblationCell,
    /// The metric that was measured (e.g. "recall@10", "p99_latency_ms").
    pub metric: String,
    /// The measured value.
    pub value: f64,
    /// Path to the committed JSON file this entry was loaded from.
    pub source: String,
}

/// The scoreboard: all committed results indexed by cell.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Scoreboard {
    pub entries: BTreeMap<AblationCell, ScoreboardEntry>,
}

impl Scoreboard {
    /// Load committed scoreboard entries from JSON text.
    ///
    /// The expected JSON shape is an array of objects, each with `cell`
    /// (serialised [`AblationCell`]), `metric`, `value` and `source` fields.
    pub fn from_json(json: &str) -> Result<Self> {
        let entries: Vec<ScoreboardEntry> = serde_json::from_str(json)?;
        Ok(Self {
            entries: entries.into_iter().map(|e| (e.cell, e)).collect(),
        })
    }

    /// The committed entry for a cell, if any.
    pub fn get(&self, cell: &AblationCell) -> Option<&ScoreboardEntry> {
        self.entries.get(cell)
    }

    /// Number of committed cells.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The ablation matrix: cell lifecycle states plus the committed scoreboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AblationMatrix {
    /// State of each cell.
    pub states: BTreeMap<AblationCell, CellState>,
    /// Committed scoreboard entries.
    pub scoreboard: Scoreboard,
}

impl Default for AblationMatrix {
    /// A fresh matrix has every one of the 32 cells present and `Pending`, so
    /// `pending()` reports the unmeasured work rather than an empty grid that
    /// silently claims zero outstanding cells.
    fn default() -> Self {
        Self {
            states: Self::all_cells()
                .into_iter()
                .map(|cell| (cell, CellState::Pending))
                .collect(),
            scoreboard: Scoreboard::default(),
        }
    }
}

impl AblationMatrix {
    /// Every cell of the 32-cell matrix, in ablation order (control first,
    /// then single-axis flips, then the full pipeline last).
    pub fn all_cells() -> Vec<AblationCell> {
        let axes = [Axis::Off, Axis::On];
        let mut out = Vec::with_capacity(TOTAL_CELLS);
        for &m in &axes {
            for &s in &axes {
                for &r in &axes {
                    for &c in &axes {
                        for &k in &axes {
                            out.push(AblationCell {
                                memory: m,
                                subagents: s,
                                repo_map: r,
                                compute: c,
                                cache: k,
                            });
                        }
                    }
                }
            }
        }
        out
    }

    /// Cells whose state is [`CellState::Done`].
    pub fn done(&self) -> Vec<AblationCell> {
        self.states
            .iter()
            .filter_map(|(cell, state)| {
                if *state == CellState::Done {
                    Some(*cell)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Cells still [`CellState::Pending`].
    pub fn pending(&self) -> Vec<AblationCell> {
        self.states
            .iter()
            .filter_map(|(cell, state)| {
                if *state == CellState::Pending {
                    Some(*cell)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Cells whose state is [`CellState::Failed`].
    pub fn failed(&self) -> Vec<AblationCell> {
        self.states
            .iter()
            .filter_map(|(cell, state)| {
                if *state == CellState::Failed {
                    Some(*cell)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Mark a cell's state.
    pub fn set_state(&mut self, cell: AblationCell, state: CellState) {
        self.states.insert(cell, state);
    }

    /// Commit a scoreboard entry for a cell and mark it done.
    pub fn commit(&mut self, entry: ScoreboardEntry) {
        let cell = entry.cell;
        self.scoreboard.entries.insert(cell, entry);
        self.states.insert(cell, CellState::Done);
    }

    /// Number of cells in the done state.
    pub fn done_count(&self) -> usize {
        self.states
            .values()
            .filter(|s| **s == CellState::Done)
            .count()
    }

    /// Number of cells in any non-pending state.
    pub fn measured_count(&self) -> usize {
        self.states
            .values()
            .filter(|s| **s != CellState::Pending)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_has_thirty_two_cells() {
        assert_eq!(AblationMatrix::all_cells().len(), TOTAL_CELLS);
    }

    #[test]
    fn control_and_full_are_distinct() {
        let cells = AblationMatrix::all_cells();
        let control = AblationCell {
            memory: Axis::Off,
            subagents: Axis::Off,
            repo_map: Axis::Off,
            compute: Axis::Off,
            cache: Axis::Off,
        };
        let full = AblationCell {
            memory: Axis::On,
            subagents: Axis::On,
            repo_map: Axis::On,
            compute: Axis::On,
            cache: Axis::On,
        };
        assert!(control.is_control());
        assert!(!control.is_full());
        assert!(full.is_full());
        assert!(!full.is_control());
        assert!(cells.contains(&control));
        assert!(cells.contains(&full));
    }

    #[test]
    fn scoreboard_from_json_round_trips() {
        let json = r#"[
            {"cell":{"memory":"On","subagents":"Off","repo_map":"On","compute":"Off","cache":"On"},"metric":"recall@10","value":0.87,"source":"contract/scoreboard.json"},
            {"cell":{"memory":"Off","subagents":"Off","repo_map":"Off","compute":"Off","cache":"Off"},"metric":"recall@10","value":0.62,"source":"contract/scoreboard.json"}
        ]"#;
        let sb = Scoreboard::from_json(json).unwrap();
        assert_eq!(sb.len(), 2);
    }

    #[test]
    fn matrix_commit_marks_done() {
        let mut m = AblationMatrix::default();
        let cell = AblationCell {
            memory: Axis::On,
            subagents: Axis::Off,
            repo_map: Axis::Off,
            compute: Axis::Off,
            cache: Axis::Off,
        };
        m.commit(ScoreboardEntry {
            cell,
            metric: "recall@10".to_string(),
            value: 0.75,
            source: "contract/scoreboard.json".to_string(),
        });
        assert_eq!(m.done_count(), 1);
        assert_eq!(m.states[&cell], CellState::Done);
        assert!(m.scoreboard.get(&cell).is_some());
    }

    #[test]
    fn pending_starts_empty_for_default_matrix() {
        let m = AblationMatrix::default();
        assert_eq!(m.pending().len(), TOTAL_CELLS);
    }
}
