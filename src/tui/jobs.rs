//! Background jobs for the session rail.
//!
//! A job is a real child process, not a progress bar that never moves. The
//! session starts it with the same shell it uses for `!cmd`, keeps reading its
//! output on two threads, and polls the child on the frame tick. Nothing here
//! blocks the UI: `start` returns as soon as the process exists, and `poll`
//! only calls the non-blocking `try_wait`.
//!
//! The command is not passed through the sandbox rules. That is deliberate and
//! narrow: `/tasks` is for work the person explicitly asked to run in the
//! background, and the rail shows the exact command line next to its output.
//! `!cmd`, which runs in the foreground, still goes through the rule set.

use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// How many output lines one job keeps. Enough to read a failure, bounded so a
/// chatty job cannot grow the session without limit.
const KEPT_LINES: usize = 500;

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    /// The word the rail and `/tasks status` print.
    pub fn label(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Completed => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    /// Whether the job can still be cancelled.
    pub fn is_live(self) -> bool {
        matches!(self, JobStatus::Running)
    }
}

/// One background process and the output collected from it.
#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub command: String,
    pub status: JobStatus,
    pub started_at: String,
    pub exit: Option<i32>,
    lines: Arc<Mutex<VecDeque<String>>>,
    child: Option<Child>,
    readers: Vec<JoinHandle<()>>,
    cancelled: bool,
}

impl Job {
    /// The last `limit` lines, oldest first.
    pub fn tail(&self, limit: usize) -> Vec<String> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let skip = lines.len().saturating_sub(limit);
        lines.iter().skip(skip).cloned().collect()
    }
}

/// The session's jobs, newest last.
#[derive(Debug, Default)]
pub struct Jobs {
    next_id: u32,
    items: Vec<Job>,
}

impl Jobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a command in the workspace root and return its job id.
    pub fn start(&mut self, command: &str, cwd: &Path) -> Result<String> {
        let command = command.trim();
        if command.is_empty() {
            anyhow::bail!("nothing to run");
        }
        self.next_id += 1;
        let id = format!("job-{}", self.next_id);

        let mut builder = if cfg!(target_os = "windows") {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", command]);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", command]);
            cmd
        };
        let mut child = builder
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot start {command}"))?;

        let lines = Arc::new(Mutex::new(VecDeque::new()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(read_lines(stdout, Arc::clone(&lines), false));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(read_lines(stderr, Arc::clone(&lines), true));
        }

        self.items.push(Job {
            id: id.clone(),
            command: command.to_string(),
            status: JobStatus::Running,
            started_at: chrono::Local::now().format("%H:%M:%S").to_string(),
            exit: None,
            lines,
            child: Some(child),
            readers,
            cancelled: false,
        });
        Ok(id)
    }

    /// Reap finished children and release their reader threads.
    pub fn poll(&mut self) {
        for job in &mut self.items {
            // A cancelled job is no longer live but still owns a child that has
            // to be reaped, so the presence of the child decides, not status.
            if !job.status.is_live() && job.child.is_none() {
                continue;
            }
            let Some(child) = job.child.as_mut() else {
                continue;
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    job.exit = status.code();
                    if !job.cancelled {
                        job.status = if status.success() {
                            JobStatus::Completed
                        } else {
                            JobStatus::Failed
                        };
                    }
                    job.child = None;
                    finish_readers(job);
                }
                Ok(None) => {}
                Err(_) => {
                    if !job.cancelled {
                        job.status = JobStatus::Failed;
                    }
                    job.child = None;
                    finish_readers(job);
                }
            }
        }
    }

    /// Kill a running job. Returns whether there was a live job with that id.
    pub fn cancel(&mut self, id: &str) -> Result<bool> {
        let Some(job) = self.items.iter_mut().find(|job| job.id == id) else {
            anyhow::bail!("no job {id}");
        };
        if !job.status.is_live() {
            return Ok(false);
        }
        if let Some(child) = job.child.as_mut() {
            // On Windows the shell is the parent of the real work; killing only
            // the shell leaves the grandchild holding the output pipes open.
            #[cfg(target_os = "windows")]
            {
                let pid = child.id().to_string();
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid, "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            if let Err(error) = child.kill() {
                // `taskkill` above may already have ended it; that is success.
                if error.kind() != std::io::ErrorKind::InvalidInput {
                    return Err(error).with_context(|| format!("cannot stop {}", job.command));
                }
            }
        }
        job.cancelled = true;
        job.status = JobStatus::Cancelled;
        Ok(true)
    }

    pub fn get(&self, id: &str) -> Option<&Job> {
        self.items.iter().find(|job| job.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.items.iter()
    }

    /// Newest first, which is how the rail reads.
    pub fn newest_first(&self) -> impl Iterator<Item = &Job> {
        self.items.iter().rev()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// One line per job for `/tasks status`.
    pub fn status_text(&self) -> String {
        if self.items.is_empty() {
            return "no background jobs — /tasks run <command> starts one".to_string();
        }
        self.items
            .iter()
            .rev()
            .map(|job| {
                format!(
                    "{}  {:<10} {:<9} {}",
                    job.id,
                    job.status.label(),
                    job.started_at,
                    job.command
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn read_lines<R: std::io::Read + Send + 'static>(
    pipe: R,
    lines: Arc<Mutex<VecDeque<String>>>,
    stderr: bool,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            let line = if stderr {
                format!("[err] {line}")
            } else {
                line
            };
            let mut kept = lines.lock().unwrap_or_else(|e| e.into_inner());
            if kept.len() == KEPT_LINES {
                kept.pop_front();
            }
            kept.push_back(line);
        }
    })
}

fn finish_readers(job: &mut Job) {
    for reader in job.readers.drain(..) {
        let _ = reader.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_until_done(jobs: &mut Jobs, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            jobs.poll();
            let status = jobs.get(id).unwrap().status;
            if !status.is_live() {
                return;
            }
            assert!(Instant::now() < deadline, "job did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_job_runs_and_its_output_is_kept() {
        let mut jobs = Jobs::new();
        let id = jobs
            .start("echo memnoth-job", &std::env::temp_dir())
            .unwrap();
        wait_until_done(&mut jobs, &id);

        let job = jobs.get(&id).unwrap();
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.exit, Some(0));
        let output = job.tail(10).join("\n");
        assert!(output.contains("memnoth-job"), "{output:?}");
    }

    #[test]
    fn a_failing_command_is_reported_as_failed() {
        let mut jobs = Jobs::new();
        let command = if cfg!(target_os = "windows") {
            "exit /b 3"
        } else {
            "exit 3"
        };
        let id = jobs.start(command, &std::env::temp_dir()).unwrap();
        wait_until_done(&mut jobs, &id);

        let job = jobs.get(&id).unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.exit, Some(3));
    }

    #[test]
    fn a_running_job_can_be_cancelled() {
        let mut jobs = Jobs::new();
        let command = if cfg!(target_os = "windows") {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        };
        let id = jobs.start(command, &std::env::temp_dir()).unwrap();
        assert!(jobs.cancel(&id).unwrap(), "the job was running");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            jobs.poll();
            let job = jobs.get(&id).unwrap();
            if job.child.is_none() {
                assert_eq!(job.status, JobStatus::Cancelled);
                return;
            }
            assert!(Instant::now() < deadline, "the child was not reaped");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn an_unknown_job_is_an_error_not_a_silent_success() {
        let mut jobs = Jobs::new();
        assert!(jobs.cancel("job-404").is_err());
        assert!(jobs.get("job-404").is_none());
        assert!(jobs.status_text().contains("/tasks run"));
    }
}
