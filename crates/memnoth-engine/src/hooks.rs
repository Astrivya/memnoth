//! Shell hooks: the closed event vocabulary, a JSON stdin, and a deadline.
//!
//! `workflow::Hooks` runs in-process callbacks and is pure on purpose. This
//! module is the IO half a project hook needs: one event from the closed list
//! in [`HookPoint`], one JSON document on stdin, and a deadline after which the
//! command is killed rather than left to wedge the turn.
//!
//! A hook that exits non-zero, or that overruns its deadline, is an `Err`: the
//! caller blocks exactly as it does for a blocking callback, so a policy hook
//! cannot half-fail.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::workflow::HookPoint;

/// The default deadline for one hook. A hook that needs longer than this is a
/// workflow step, not a hook.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// One configured shell hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommand {
    /// Where the hook fires; only the five closed points parse.
    pub point: HookPoint,
    /// `*` or a case-insensitive substring of the event subject.
    pub matcher: String,
    /// The command line, run through the platform shell.
    pub command: String,
    /// How long the command may run before it is killed.
    pub timeout: Duration,
}

impl HookCommand {
    /// A hook with an explicit deadline.
    pub fn new(
        point: HookPoint,
        matcher: impl Into<String>,
        command: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            point,
            matcher: matcher.into(),
            command: command.into(),
            timeout,
        }
    }

    /// A hook with [`DEFAULT_HOOK_TIMEOUT`].
    pub fn with_default_timeout(
        point: HookPoint,
        matcher: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self::new(point, matcher, command, DEFAULT_HOOK_TIMEOUT)
    }

    /// Whether this hook fires for `name`, with the same rule the callback
    /// hooks use: `*`/empty matches everything, otherwise a case-insensitive
    /// substring.
    fn matches(&self, name: &str) -> bool {
        let matcher = self.matcher.trim();
        if matcher.is_empty() || matcher == "*" {
            return true;
        }
        name.to_ascii_lowercase()
            .contains(&matcher.to_ascii_lowercase())
    }
}

/// The document a hook receives on stdin.
///
/// `event` is the closed wire name, never a free-form string, so a hook can
/// match on it without guessing the spelling.
pub fn hook_payload(point: HookPoint, name: &str, detail: Option<&str>) -> Value {
    json!({
        "event": point.as_str(),
        "name": name,
        "detail": detail,
    })
}

/// What one completed hook reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOutcome {
    /// The command line that ran.
    pub command: String,
    /// The closed event name it ran for.
    pub event: String,
    /// Its stdout, trimmed.
    pub stdout: String,
    /// Its exit code, or `-1` when the platform reported none.
    pub status: i32,
}

/// Run every command hook matching `(point, name)` in registration order.
///
/// The first failure blocks and is returned: a non-zero exit is a refusal, and
/// a deadline is a refusal too — a hook that cannot answer in time must not be
/// treated as consent.
pub fn run_hooks(
    commands: &[HookCommand],
    point: HookPoint,
    name: &str,
    cwd: &Path,
) -> Result<Vec<HookOutcome>> {
    let mut outcomes = Vec::new();
    for hook in commands
        .iter()
        .filter(|hook| hook.point == point && hook.matches(name))
    {
        outcomes.push(run_one(hook, point, name, cwd)?);
    }
    Ok(outcomes)
}

fn run_one(hook: &HookCommand, point: HookPoint, name: &str, cwd: &Path) -> Result<HookOutcome> {
    let payload = serde_json::to_vec(&hook_payload(point, name, None))?;
    let mut child = shell(&hook.command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot start hook {:?}", hook.command))?;

    if let Some(mut stdin) = child.stdin.take() {
        // A hook is entitled to ignore its input: `exit 3` closes the pipe
        // before the payload is written, and on unix that write is `EPIPE`.
        // Its exit status is the answer, so a broken pipe is not a failure.
        if let Err(error) = stdin.write_all(&payload)
            && error.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(error).with_context(|| format!("cannot write to hook {:?}", hook.command));
        }
        // Dropping stdin closes it, so a hook that reads to EOF sees EOF.
    }

    let deadline = Instant::now() + hook.timeout;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "hook {:?} exceeded {:?} and was killed",
                    hook.command,
                    hook.timeout
                );
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }

    let output = child.wait_with_output()?;
    let status = output.status.code().unwrap_or(-1);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "hook {:?} failed with status {status}: {}",
            hook.command,
            stderr.trim()
        );
    }

    Ok(HookOutcome {
        command: hook.command.clone(),
        event: point.as_str().to_string(),
        stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        status,
    })
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

#[cfg(not(windows))]
fn shell(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXIT_THREE: &str = if cfg!(windows) { "exit /B 3" } else { "exit 3" };
    const EXIT_ZERO: &str = if cfg!(windows) { "exit /B 0" } else { "true" };

    fn here() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn the_payload_names_only_the_closed_event() {
        let payload = hook_payload(HookPoint::PreToolUse, "edit_file", None);
        assert_eq!(payload["event"], "pre_tool_use");
        assert_eq!(payload["name"], "edit_file");
        assert!(payload["detail"].is_null());
    }

    #[test]
    fn a_matcher_star_and_substring_agree_with_the_callback_hooks() {
        let hook = HookCommand::with_default_timeout(HookPoint::PreToolUse, "Edit", EXIT_ZERO);
        assert!(hook.matches("edit_file"));
        assert!(!hook.matches("read_file"));

        let all = HookCommand::with_default_timeout(HookPoint::Stop, "*", EXIT_ZERO);
        assert!(all.matches("anything"));
    }

    #[test]
    fn only_the_matching_point_and_name_run() {
        let commands = vec![
            HookCommand::with_default_timeout(HookPoint::PreToolUse, "*", EXIT_THREE),
            HookCommand::with_default_timeout(HookPoint::PostToolUse, "*", EXIT_ZERO),
            HookCommand::with_default_timeout(HookPoint::PreToolUse, "edit", EXIT_THREE),
        ];
        // The pre-tool hooks would fail if they ran; a post-tool dispatch is
        // silent, which is how a hook learns it is not the one being called.
        let outcomes = run_hooks(&commands, HookPoint::PostToolUse, "edit_file", &here()).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].event, "post_tool_use");
    }

    #[test]
    fn a_failing_hook_blocks_the_action() {
        let hook = HookCommand::with_default_timeout(HookPoint::PreToolUse, "*", EXIT_THREE);
        let error = run_hooks(&[hook], HookPoint::PreToolUse, "edit_file", &here()).unwrap_err();
        assert!(
            error.to_string().contains("failed with status 3"),
            "{error}"
        );
    }

    #[test]
    fn a_passing_hook_reports_its_event_and_status() {
        let hook = HookCommand::with_default_timeout(HookPoint::PostToolUse, "*", EXIT_ZERO);
        let outcomes = run_hooks(&[hook], HookPoint::PostToolUse, "edit_file", &here()).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].event, "post_tool_use");
        assert_eq!(outcomes[0].status, 0);
    }

    #[cfg(unix)]
    #[test]
    fn the_event_arrives_on_stdin() {
        // One byte of stdin is printed and matched: the payload is there before
        // the command runs, not after it exits.
        let hook = HookCommand::with_default_timeout(
            HookPoint::SubagentStart,
            "*",
            "head -c 1 | grep -q .",
        );
        let outcomes = run_hooks(&[hook], HookPoint::SubagentStart, "scout", &here()).unwrap();
        assert_eq!(outcomes.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_past_its_deadline_is_killed() {
        let hook = HookCommand::new(
            HookPoint::Stop,
            "*",
            "sleep 5",
            std::time::Duration::from_millis(100),
        );
        let started = Instant::now();
        let error = run_hooks(&[hook], HookPoint::Stop, "session-1", &here()).unwrap_err();
        assert!(error.to_string().contains("exceeded"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "a past-deadline hook is killed, not awaited"
        );
    }
}
