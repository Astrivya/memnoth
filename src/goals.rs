//! Goals and plans: durable per-session state under `$MEMNOTH_HOME/goals/`.
//!
//! A goal is a completion condition that outlives the turn that set it, and a
//! plan is the artifact plan mode works against. Both live in one file per
//! session (`goals/<session>.json`) so `/goal`, `/plan` and the two overlays
//! read the same rows instead of describing a state nothing stored.
//!
//! The evaluator that would check a goal after every turn does not exist yet;
//! this module stores the condition and never claims it was evaluated.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::session::now_ms;

/// One completion condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    /// What has to be true for the session to be done.
    pub condition: String,
    /// When it was set (ms since the epoch).
    pub set_ms: i64,
    /// Whether a person marked the condition met.
    #[serde(default)]
    pub done: bool,
}

/// One plan artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The task the plan is for; empty when plan mode was entered bare.
    pub task: String,
    /// Steps recorded against the task, in order.
    pub steps: Vec<String>,
    /// When the plan was created (ms since the epoch).
    pub created_ms: i64,
}

/// Everything stored for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The active completion condition.
    pub goal: Option<Goal>,
    /// The plan artifact.
    pub plan: Option<Plan>,
}

/// The per-session goal and plan directory.
#[derive(Debug, Clone)]
pub struct GoalStore {
    dir: PathBuf,
}

impl GoalStore {
    /// Point the store at `$MEMNOTH_HOME/goals`.
    pub fn new(home: &Path) -> Self {
        Self {
            dir: home.join("goals"),
        }
    }

    fn path(&self, session: &str) -> PathBuf {
        self.dir.join(format!("{session}.json"))
    }

    /// Read one session's record; a session nothing wrote has an empty one.
    pub fn record(&self, session: &str) -> Result<Record> {
        let path = self.path(session);
        if !path.is_file() {
            return Ok(Record::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
    }

    /// Set (or replace) the goal for `session`.
    pub fn set_goal(&self, session: &str, condition: &str) -> Result<Record> {
        let mut record = self.record(session)?;
        record.goal = Some(Goal {
            condition: condition.trim().to_string(),
            set_ms: now_ms(),
            done: false,
        });
        self.save(session, &record)?;
        Ok(record)
    }

    /// Clear the goal for `session`.
    pub fn clear_goal(&self, session: &str) -> Result<Record> {
        let mut record = self.record(session)?;
        record.goal = None;
        self.save(session, &record)?;
        Ok(record)
    }

    /// Mark the goal met (or reopen it). Refuses when no goal is set: a
    /// completion flag on nothing is a state nobody can explain later.
    pub fn mark_goal(&self, session: &str, done: bool) -> Result<Record> {
        let mut record = self.record(session)?;
        let goal = record.goal.as_mut().ok_or_else(|| {
            anyhow::anyhow!("no goal set for {session} — /goal <condition> sets one")
        })?;
        goal.done = done;
        self.save(session, &record)?;
        Ok(record)
    }

    /// Append one step to the plan, creating the plan on first use.
    pub fn add_step(&self, session: &str, step: &str) -> Result<Record> {
        let mut record = self.ensure_plan(session)?;
        let plan = record
            .plan
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no plan for {session}"))?;
        plan.steps.push(step.trim().to_string());
        self.save(session, &record)?;
        Ok(record)
    }

    /// Read the plan, creating an empty one on first use.
    ///
    /// Plan mode is a place to work, so entering it has to leave a row behind;
    /// the task stays empty until something supplies one.
    pub fn ensure_plan(&self, session: &str) -> Result<Record> {
        let mut record = self.record(session)?;
        if record.plan.is_none() {
            record.plan = Some(Plan {
                task: String::new(),
                steps: Vec::new(),
                created_ms: now_ms(),
            });
            self.save(session, &record)?;
        }
        Ok(record)
    }

    fn save(&self, session: &str, record: &Record) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("cannot create {}", self.dir.display()))?;
        std::fs::write(self.path(session), serde_json::to_string_pretty(record)?)
            .with_context(|| format!("cannot write {}", self.path(session).display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_goal_survives_a_new_store_instance() {
        let home = tempfile::tempdir().unwrap();
        GoalStore::new(home.path())
            .set_goal("payments", "all tests pass")
            .unwrap();

        let record = GoalStore::new(home.path()).record("payments").unwrap();
        assert_eq!(
            record.goal.as_ref().map(|goal| goal.condition.as_str()),
            Some("all tests pass")
        );
    }

    #[test]
    fn clearing_removes_only_the_goal() {
        let home = tempfile::tempdir().unwrap();
        let store = GoalStore::new(home.path());
        store.set_goal("payments", "green build").unwrap();
        store.clear_goal("payments").unwrap();
        assert!(store.record("payments").unwrap().goal.is_none());
    }

    #[test]
    fn entering_plan_mode_leaves_a_row_and_is_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let store = GoalStore::new(home.path());
        let first = store.ensure_plan("payments").unwrap();
        let second = store.ensure_plan("payments").unwrap();
        assert!(first.plan.is_some());
        assert_eq!(first.plan, second.plan, "a second visit keeps the plan");
        assert_eq!(second.plan.unwrap().steps.len(), 0);
    }

    #[test]
    fn steps_append_in_order_and_persist() {
        let home = tempfile::tempdir().unwrap();
        let store = GoalStore::new(home.path());
        store.add_step("payments", "  read login.rs  ").unwrap();
        store.add_step("payments", "add the test").unwrap();
        let record = GoalStore::new(home.path()).record("payments").unwrap();
        assert_eq!(
            record.plan.unwrap().steps,
            vec!["read login.rs".to_string(), "add the test".to_string()]
        );
    }

    #[test]
    fn marking_done_requires_a_goal_and_sticks() {
        let home = tempfile::tempdir().unwrap();
        let store = GoalStore::new(home.path());
        assert!(store.mark_goal("payments", true).is_err());
        store.set_goal("payments", "green build").unwrap();
        store.mark_goal("payments", true).unwrap();
        assert!(
            GoalStore::new(home.path())
                .record("payments")
                .unwrap()
                .goal
                .unwrap()
                .done
        );
    }

    #[test]
    fn sessions_keep_separate_records() {
        let home = tempfile::tempdir().unwrap();
        let store = GoalStore::new(home.path());
        store.set_goal("payments", "green").unwrap();
        assert!(store.record("other").unwrap().goal.is_none());
    }
}
