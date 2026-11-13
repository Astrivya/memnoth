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

use std::time::SystemTime;

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

/// Probes a recorded child-process identity to detect orphaning.
///
/// Record the identity at spawn with [`OrphanProbe::record`], then call
/// [`OrphanProbe::probe`] later. The probe re-obtains the identity of the
/// recorded process and reports [`ProbeOutcome::Orphaned`] when the process
/// is gone or any component of the identity has diverged.
pub struct OrphanProbe {
    recorded: Option<ProcessIdentity>,
}

impl OrphanProbe {
    /// Creates a probe that has not yet recorded a child process.
    pub fn new() -> Self {
        Self { recorded: None }
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
    #[cfg(unix)]
    return current_process_identity_unix(pid);
    #[cfg(not(unix))]
    let _ = pid;
    None
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
}
