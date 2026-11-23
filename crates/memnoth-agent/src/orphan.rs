//! Orphan recovery for delegated child processes.
//!
//! A child process is identified at spawn by the triple
//! `(pid, boot_id, start_time)`. A later probe re-reads the identity of the
//! recorded process and reports [`ProbeOutcome::Orphaned`] when the process
//! is gone or when any component of the triple has diverged.
//!
//! Keying on the triple — rather than the pid alone — is what makes the
//! check sound: pids are recycled, and the boot id changes across reboots,
//! so a pid that matches but belongs to a different process (or a different
//! boot) is correctly reported as orphaned instead of being mistaken for
//! the original child.
//!
//! The standard library is used; no external process-introspection crates.
//! [`std::process`] supplies the pid, [`std::time`] supplies the start
//! instant, and on Linux [`std::fs`] is used solely to re-read `/proc` when
//! refreshing a recorded identity.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use anyhow::Result;

use crate::runtime::{AgentRun, orphan_candidates};

/// Clock ticks per second on Linux (`_SC_CLK_TCK`); 100 is the common value.
///
/// Unix-only in production; kept for tests on other platforms.
#[cfg(any(unix, test))]
const CLK_TCK: u64 = 100;

/// The identity of a process at a moment in time: `(pid, boot_id, start_time)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// The process id.
    pub pid: u32,
    /// The OS boot id; changes on every reboot.
    pub boot_id: String,
    /// The process creation time.
    pub start_time: SystemTime,
}

/// The result of probing a recorded child-process identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The process is alive and its `(pid, boot_id, start_time)` still matches.
    Alive,
    /// The process is gone, or its identity no longer matches the recording:
    /// the child is an orphan.
    Orphaned,
}

/// The durable triple used for adopt-or-orphan reconciliation (F-08).
///
/// A pid alone is not identity: pids are recycled, and the boot id changes
/// across reboots. All three components must match before a run may be
/// adopted; anything less is an orphan, never "sometimes it resumes".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity {
    /// The process id recorded at spawn.
    pub pid: u32,
    /// Process start time in clock ticks since boot (`/proc/<pid>/stat` field 22).
    pub process_start_time: u64,
    /// The OS boot id recorded at spawn.
    pub boot_id: String,
}

/// The deterministic adopt-or-orphan decision.
///
/// `Alive` means the observed triple is exactly the recorded one. A missing
/// record, a missing observation, or a single diverged component is
/// `Orphaned`. This is a pure function so the decision is testable on any
/// platform and identical across runs.
pub fn reconcile(recorded: Option<&RunIdentity>, observed: Option<&RunIdentity>) -> ProbeOutcome {
    match (recorded, observed) {
        (Some(recorded), Some(observed)) if recorded == observed => ProbeOutcome::Alive,
        _ => ProbeOutcome::Orphaned,
    }
}

/// Probes a recorded child-process identity to detect orphaning.
///
/// Record the identity at spawn with [`OrphanProbe::record`], then call
/// [`OrphanProbe::probe`] later. The probe re-obtains the identity of the
/// recorded process and reports [`ProbeOutcome::Orphaned`] when the process
/// is gone or any component of the identity has diverged.
pub struct OrphanProbe {
    recorded: Option<ProcessIdentity>,
    recorded_run: Option<RunIdentity>,
}

impl OrphanProbe {
    /// Creates a probe that has not yet recorded a child process.
    pub fn new() -> Self {
        Self {
            recorded: None,
            recorded_run: None,
        }
    }

    /// Records the identity of a just-spawned child process.
    pub fn record(&mut self, pid: u32, boot_id: impl Into<String>, start_time: SystemTime) {
        self.recorded = Some(ProcessIdentity {
            pid,
            boot_id: boot_id.into(),
            start_time,
        });
    }

    /// Returns the recorded identity, if any.
    pub fn recorded(&self) -> Option<&ProcessIdentity> {
        self.recorded.as_ref()
    }

    /// Re-obtains the identity of the recorded process and compares it.
    ///
    /// Returns [`ProbeOutcome::Orphaned`] when no identity was recorded, the
    /// process can no longer be located, or its `(pid, boot_id, start_time)`
    /// no longer matches the recording.
    pub fn probe(&self) -> ProbeOutcome {
        let recorded = match self.recorded.as_ref() {
            Some(r) => r,
            None => return ProbeOutcome::Orphaned,
        };
        match current_process_identity(recorded.pid) {
            None => ProbeOutcome::Orphaned,
            Some(current) if current == *recorded => ProbeOutcome::Alive,
            _ => ProbeOutcome::Orphaned,
        }
    }

    /// Record the exact `(pid, process_start_time, boot_id)` triple for a run.
    pub fn record_run(&mut self, identity: RunIdentity) {
        self.recorded_run = Some(identity);
    }

    /// The recorded run identity, when one was given.
    pub fn recorded_run(&self) -> Option<&RunIdentity> {
        self.recorded_run.as_ref()
    }

    /// Re-read the process and reconcile the recorded triple (F-08).
    ///
    /// On platforms without `/proc` the observation is `None`, so the decision
    /// is deterministically `Orphaned` rather than a guess that resumes a run
    /// whose process cannot be proven.
    pub fn probe_run(&self) -> ProbeOutcome {
        let recorded = self.recorded_run.as_ref();
        let observed = recorded.and_then(|identity| current_run_identity(identity.pid));
        reconcile(recorded, observed.as_ref())
    }
}

impl Default for OrphanProbe {
    fn default() -> Self {
        Self::new()
    }
}

/// Re-obtains the current identity of the process with `pid`, or `None` if
/// it cannot be located (gone, or re-acquisition not supported on this
/// platform).
fn current_process_identity(pid: u32) -> Option<ProcessIdentity> {
    // Each platform's arm is the whole body, not a `return` with a fallback
    // after it: on unix that fallback was unreachable code.
    #[cfg(unix)]
    {
        current_process_identity_unix(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

#[cfg(unix)]
fn current_process_identity_unix(pid: u32) -> Option<ProcessIdentity> {
    use std::fs;
    use std::time::{Duration, UNIX_EPOCH};

    let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()?
        .trim()
        .to_string();
    if boot_id.is_empty() {
        return None;
    }

    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let starttime = parse_starttime(&stat)?;
    let btime = parse_btime()?;
    let start_time =
        UNIX_EPOCH + Duration::from_secs(btime) + Duration::from_secs(starttime / CLK_TCK);

    Some(ProcessIdentity {
        pid,
        boot_id,
        start_time,
    })
}

/// Parses the process start time (clock ticks since boot) from a
/// `/proc/{pid}/stat` contents.
///
/// The commname field (field 2) is parenthesized and may contain spaces or
/// parentheses, so the cut is taken at the final `)`; `starttime` is field
/// 22 overall, i.e. index 19 after the commname.
#[cfg(any(unix, test))]
fn parse_starttime(stat: &str) -> Option<u64> {
    let rest = stat.rsplit_once(')').map(|(_, tail)| tail)?.trim_start();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let starttime = fields.get(19)?;
    starttime.parse::<u64>().ok()
}

/// Parses the boot time (seconds since the Unix epoch) from `/proc/stat`.
#[cfg(any(unix, test))]
fn parse_btime() -> Option<u64> {
    use std::fs;

    let content = fs::read_to_string("/proc/stat").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse::<u64>().ok();
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Restart recovery: adopt only a proven identity, orphan everything else
// ---------------------------------------------------------------------------

/// The outcome of reconciling a session's run journal with live processes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoverReport {
    /// Runs whose recorded process identity still matched; left running.
    pub adopted: Vec<String>,
    /// Runs whose identity could not be proven; now terminally `orphaned`.
    pub orphaned: Vec<String>,
}

/// Reconcile every live run in `journal_path` with observed process identities.
///
/// A run is adopted only when the observed `(pid, process_start_time, boot_id)`
/// is exactly the recorded triple. Every other live run gets a terminal
/// `orphaned` edge appended to the journal, so a restart can never report a
/// run as completed when its process vanished mid-flight (P10-14 / F-08).
///
/// `identities` holds the triple recorded at each run's spawn. `observer`
/// re-reads a live process for a recorded triple; returning `None` means the
/// process is gone or the platform cannot prove identity.
pub fn recover(
    journal_path: impl AsRef<Path>,
    identities: &BTreeMap<String, Option<RunIdentity>>,
    observer: impl Fn(&RunIdentity) -> Option<RunIdentity>,
) -> Result<RecoverReport> {
    let path = journal_path.as_ref();
    let mut report = RecoverReport::default();
    for run in orphan_candidates(path)? {
        let recorded = identities.get(&run).and_then(Option::as_ref);
        let observed = recorded.and_then(&observer);
        match reconcile(recorded, observed.as_ref()) {
            ProbeOutcome::Alive => report.adopted.push(run),
            ProbeOutcome::Orphaned => {
                let mut agent = AgentRun::attach(&run, path)?;
                agent.orphan("process identity unproven at restart")?;
                report.orphaned.push(run);
            }
        }
    }
    Ok(report)
}

/// Re-read the `(pid, process_start_time, boot_id)` of a live process.
///
/// On unix this is taken from `/proc`; elsewhere no identity can be proven and
/// the result is `None`, which [`reconcile`] turns into `Orphaned`.
pub fn current_run_identity(pid: u32) -> Option<RunIdentity> {
    #[cfg(unix)]
    {
        use std::fs;

        let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()?
            .trim()
            .to_string();
        if boot_id.is_empty() {
            return None;
        }
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let process_start_time = parse_starttime(&stat)?;
        Some(RunIdentity {
            pid,
            process_start_time,
            boot_id,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_orphaned_when_nothing_recorded() {
        let probe = OrphanProbe::new();
        assert_eq!(probe.probe(), ProbeOutcome::Orphaned);
    }

    #[test]
    fn parse_starttime_handles_parenthesized_comm() {
        // comm `(foo)bar` with a space and paren, then normal fields.
        let stat = "123 (foo)bar) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 4294967295";
        assert_eq!(parse_starttime(stat), Some(4294967295));
    }

    #[test]
    fn parse_starttime_simple() {
        let stat = "123 (comm) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345";
        let ticks = parse_starttime(stat).expect("field 22 parses");
        assert_eq!(ticks, 12345);
        // The tick count converts to whole seconds via CLK_TCK.
        assert_eq!(ticks / CLK_TCK, 123);
    }

    #[test]
    fn parse_btime_reads_proc_stat_when_present() {
        // On unix `/proc/stat` exists and carries a positive btime;
        // elsewhere the helper reports None rather than guessing.
        match parse_btime() {
            Some(btime) => assert!(btime > 0),
            None => assert!(!std::path::Path::new("/proc/stat").exists()),
        }
    }

    fn identity(pid: u32, ticks: u64, boot: &str) -> RunIdentity {
        RunIdentity {
            pid,
            process_start_time: ticks,
            boot_id: boot.to_string(),
        }
    }

    #[test]
    fn a_matching_triple_is_adopted_and_a_diverged_one_is_orphaned() {
        let recorded = identity(42, 12_345, "boot-a");
        assert_eq!(
            reconcile(Some(&recorded), Some(&identity(42, 12_345, "boot-a"))),
            ProbeOutcome::Alive
        );
        // Same pid, different process: pid alone is not identity.
        assert_eq!(
            reconcile(Some(&recorded), Some(&identity(42, 99_999, "boot-a"))),
            ProbeOutcome::Orphaned
        );
        // Same pid and ticks, new boot: also an orphan.
        assert_eq!(
            reconcile(Some(&recorded), Some(&identity(42, 12_345, "boot-b"))),
            ProbeOutcome::Orphaned
        );
        assert_eq!(reconcile(Some(&recorded), None), ProbeOutcome::Orphaned);
        assert_eq!(reconcile(None, None), ProbeOutcome::Orphaned);
    }

    #[test]
    fn recovery_marks_unproven_live_runs_orphaned_and_adopts_proven_ones() {
        let path = std::env::temp_dir().join(format!(
            "memnoth-agent-recover-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut adopted = AgentRun::enqueue("adopted", None, None, "worker", &path).unwrap();
        adopted.start().unwrap();
        let mut lost = AgentRun::enqueue("lost", None, None, "worker", &path).unwrap();
        lost.start().unwrap();

        let recorded_adopted = identity(100, 500, "boot-a");
        let recorded_lost = identity(200, 600, "boot-a");
        let mut identities = BTreeMap::new();
        identities.insert("adopted".to_string(), Some(recorded_adopted.clone()));
        identities.insert("lost".to_string(), Some(recorded_lost.clone()));

        let report = recover(&path, &identities, |recorded| {
            // Only `adopted`'s process is still the one recorded.
            if recorded.pid == 100 {
                Some(recorded.clone())
            } else {
                None
            }
        })
        .unwrap();

        assert_eq!(report.adopted, vec!["adopted"]);
        assert_eq!(report.orphaned, vec!["lost"]);
        use crate::runtime::{AgentPhase, last_phases};
        let phases = last_phases(&path).unwrap();
        assert_eq!(phases.get("adopted"), Some(&AgentPhase::Running));
        assert_eq!(phases.get("lost"), Some(&AgentPhase::Orphaned));
    }
}
