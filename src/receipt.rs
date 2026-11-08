//! `mn-receipt/1`: what changed, who allowed it, and what it can break.
//!
//! A diff answers *what* changed. It cannot answer who asked for it, which
//! person approved the write, which beliefs were in play, or which tests cover
//! the symbols that moved. That chain — belief, author, authorised agent,
//! reviewable change — is the thing a team needs and a diff cannot carry.
//!
//! # The rule that makes a receipt trustworthy
//!
//! Every field is either measured or explicitly `null` with a reason. An empty
//! `impacted_tests` means "the graph was consulted and no test mentions this
//! symbol"; `null` means "the graph was not consulted". Collapsing those two
//! into one value is how a check that cannot see its input reports success.
//!
//! # Where it is written
//!
//! `.memnoth/receipts/<sha>.json`, one file per commit. The commit id does not
//! exist while `pre-commit` runs, so the receipt is written from a `post-commit`
//! hook — a receipt for a commit that may not happen would describe nothing.
//! `mn receipt write` is idempotent; running it again rewrites the same file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::{ReceiptArgs, ReceiptCommand};
use crate::context::AppContext;
use crate::output::Outcome;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The schema name and major version this build writes.
pub const SCHEMA: &str = "mn-receipt/1";

/// The reason `beliefs` is null in this build.
///
/// Stated rather than omitted: a consumer must not read "no beliefs" when the
/// truth is "beliefs are not attributed yet". It disappears when M6 lands.
pub const BELIEFS_UNAVAILABLE: &str = "memory-v1";

/// One change receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    /// Always [`SCHEMA`] for a document this build wrote.
    pub schema: String,
    /// The commit and the files it touched.
    pub change: Change,
    /// Which agent produced it.
    pub agent: Agent,
    /// Who was accountable, and what they allowed.
    pub authority: Authority,
    /// Beliefs in play, or `null` with [`Receipt::beliefs_unavailable`].
    #[serde(default)]
    pub beliefs: Option<Vec<Belief>>,
    /// Why `beliefs` is null.
    #[serde(default)]
    pub beliefs_unavailable: Option<String>,
    /// What the change may affect. Counts are `null` when unmeasured.
    pub impact: Impact,
    /// Policy decisions that shaped the run.
    pub policy: Policy,
}

/// The commit and its files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    /// Full commit id.
    pub commit: String,
    /// The revision the change was taken against; `null` for a root commit.
    pub base: Option<String>,
    /// SHA-256 of the canonical patch, hex encoded.
    pub patch_sha256: String,
    /// One row per file.
    pub files: Vec<FileChange>,
}

/// One file in the change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Repo-relative path (the new path for a rename).
    pub path: String,
    /// Lines added; `0` for a deletion.
    pub added: u64,
    /// Lines removed; `0` for an addition.
    pub removed: u64,
    /// `added`, `modified`, `deleted` or `renamed`.
    pub change_type: String,
}

/// Which agent produced the change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    /// The session the change came from, when one is on disk.
    pub session: Option<String>,
    /// Reserved for delegation runs; `null` until W5.1 lands.
    pub run_id: Option<String>,
    /// The surface: `memnoth`, `ci`, or a named subagent.
    pub agent: String,
    /// The model that answered, when a session records one.
    pub model: Option<String>,
}

/// A belief that was in play.
///
/// Empty until team memory lands; the field exists now so a consumer can be
/// written against the final shape rather than a second schema later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belief {
    pub entity: String,
    pub predicate: Option<String>,
    pub text: String,
    pub recorded_at: Option<i64>,
    pub valid_from: Option<i64>,
    pub supersedes: Option<String>,
}

/// Who was accountable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Authority {
    /// The person at the keyboard.
    pub user: String,
    /// The organisation, when signed in. `null` is "not known", not "none".
    pub org: Option<String>,
    /// The policy document version; `null` until signed policy lands.
    pub policy_version: Option<String>,
    /// `policy` when approvals are enforced, `none` when nothing gates writes.
    pub sandbox: String,
    /// Every approval decision recorded in the session.
    pub approvals: Vec<Approval>,
}

/// One approval decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// The tool that asked.
    pub tool: String,
    /// What it would change.
    pub subject: String,
    /// `true` when allowed, `false` when refused.
    pub approved: bool,
    /// The person who decided. Never the agent.
    pub approved_by: String,
    /// When, in ms since the epoch.
    pub at_ms: i64,
    /// `user`, `no-approver`, or a policy rule.
    pub rule: String,
}

/// What the change may break.
///
/// `None` is "not measured" and always carries a reason in
/// [`Impact::unavailable`]. `Some([])` is "measured, nothing found".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Impact {
    /// Symbols declared in the change that the graph knows.
    pub symbols: Option<usize>,
    /// Mentions of those symbols outside their declaration lines.
    pub references: Option<usize>,
    /// Test files that mention them.
    pub impacted_tests: Option<Vec<String>>,
    /// Why the numbers above are `null`, when they are.
    pub unavailable: Option<String>,
}

/// Policy decisions that shaped the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Tools denied on this machine during the run.
    pub denied: Vec<String>,
    /// The smallest rule that would have allowed a denial. Memnoth's
    /// differentiator: a refusal names how to ask for less.
    pub rule_that_would_allow: Option<String>,
}

/// The result of checking a receipt against its commit.
#[derive(Debug, Clone, Serialize)]
pub struct Verification {
    /// The schema that was verified.
    pub schema: String,
    /// The commit the receipt names.
    pub commit: String,
    /// True only when every check passed.
    pub ok: bool,
    /// One row per check.
    pub checks: Vec<Check>,
}

/// One verification check.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// Whether it held.
    pub ok: bool,
    /// The evidence, in a sentence.
    pub detail: String,
}

/// Run `mn receipt …`.
pub fn run(ctx: &AppContext, args: &ReceiptArgs) -> Result<Outcome> {
    match &args.action {
        ReceiptCommand::Write(args) => write_cmd(ctx, args.commit.as_deref(), args.json),
        ReceiptCommand::Show(args) => show_cmd(ctx, args.commit.as_deref(), args.json),
        ReceiptCommand::Verify(args) => verify_cmd(ctx, args.commit.as_deref(), args.json),
    }
}

/// Where receipts live for this workspace.
pub fn receipts_dir(ctx: &AppContext) -> PathBuf {
    ctx.root.join(".memnoth").join("receipts")
}

/// The file a commit's receipt lives in.
pub fn path_for(ctx: &AppContext, commit: &str) -> PathBuf {
    receipts_dir(ctx).join(format!("{commit}.json"))
}

fn write_cmd(ctx: &AppContext, commit: Option<&str>, as_json: bool) -> Result<Outcome> {
    let receipt = write(ctx, commit)?;
    let path = path_for(ctx, &receipt.change.commit);
    let data = serde_json::to_value(&receipt)?;
    let text = if as_json {
        serde_json::to_string_pretty(&data)?
    } else {
        format!(
            "Wrote {} for {} ({})\n{}",
            SCHEMA,
            &receipt.change.commit[..receipt.change.commit.len().min(12)],
            path.display(),
            render(&receipt)
        )
    };
    Ok(Outcome::new("receipt.write", text, data))
}

fn show_cmd(ctx: &AppContext, commit: Option<&str>, as_json: bool) -> Result<Outcome> {
    let receipt = load(ctx, commit)?;
    let data = serde_json::to_value(&receipt)?;
    let text = if as_json {
        serde_json::to_string_pretty(&data)?
    } else {
        render(&receipt)
    };
    Ok(Outcome::new("receipt.show", text, data))
}

fn verify_cmd(ctx: &AppContext, commit: Option<&str>, as_json: bool) -> Result<Outcome> {
    let verification = verify(ctx, commit)?;
    let data = serde_json::to_value(&verification)?;
    let mut text = format!(
        "{} {}",
        if verification.ok { "OK" } else { "FAILED" },
        &verification.commit[..verification.commit.len().min(12)]
    );
    for check in &verification.checks {
        text.push_str(&format!(
            "\n  {} {} — {}",
            if check.ok { "✓" } else { "✗" },
            check.name,
            check.detail
        ));
    }
    if as_json {
        text = serde_json::to_string_pretty(&data)?;
    }
    let mut outcome = Outcome::new("receipt.verify", text, data);
    if !verification.ok {
        outcome.exit_code = 1;
    }
    Ok(outcome)
}

/// Build a receipt for `commit` (default `HEAD`) and write it.
///
/// The impact section is measured only when `commit` is `HEAD`: the code graph
/// answers for the working revision, so asking it about an older commit would
/// attribute one commit's tests to another. Anything else is `null` with that
/// reason, which is the honest answer rather than an empty list.
pub fn write(ctx: &AppContext, commit: Option<&str>) -> Result<Receipt> {
    let commit = resolve_commit(ctx, commit)?;
    let base = parent_of(ctx, &commit);
    let patch = patch_bytes(ctx, &commit, base.as_deref())?;
    let files = files_in(ctx, &commit, base.as_deref())?;

    let head = git(ctx, &["rev-parse", "HEAD"]).unwrap_or_default();
    let impact = if commit == head {
        match &base {
            Some(base) => measure_impact(ctx, base),
            None => Impact {
                symbols: None,
                references: None,
                impacted_tests: None,
                unavailable: Some("root commit: no base revision to review".to_string()),
            },
        }
    } else {
        Impact {
            symbols: None,
            references: None,
            impacted_tests: None,
            unavailable: Some(format!(
                "the code graph measures HEAD; {commit} is not HEAD"
            )),
        }
    };

    let (session_id, model, approvals) = latest_session(ctx);

    let receipt = Receipt {
        schema: SCHEMA.to_string(),
        change: Change {
            commit: commit.clone(),
            base,
            patch_sha256: sha256_hex(&patch),
            files,
        },
        agent: Agent {
            session: session_id,
            run_id: None,
            agent: "memnoth".to_string(),
            model,
        },
        authority: Authority {
            user: actor_identity(),
            org: None,
            policy_version: None,
            sandbox: "policy".to_string(),
            approvals,
        },
        beliefs: None,
        beliefs_unavailable: Some(BELIEFS_UNAVAILABLE.to_string()),
        impact,
        policy: Policy {
            denied: Vec::new(),
            rule_that_would_allow: None,
        },
    };

    let path = path_for(ctx, &commit);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create receipt dir {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(&receipt)?;
    // Write-then-rename: a half-written receipt would be a document whose hash
    // check fails for a reason that has nothing to do with tampering.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, format!("{json}\n"))
        .with_context(|| format!("write receipt {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)?;
    Ok(receipt)
}

/// Read a receipt for `commit` (default `HEAD`).
pub fn load(ctx: &AppContext, commit: Option<&str>) -> Result<Receipt> {
    let commit = resolve_commit(ctx, commit)?;
    let path = path_for(ctx, &commit);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no receipt for {} at {}. Write one with `mn receipt write` \
             (or reinstall the hook with `mn hooks install --force`).",
            &commit[..commit.len().min(12)],
            path.display()
        )
    })?;
    serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid {SCHEMA} document", path.display()))
}

/// Check a receipt against the repository it claims to describe.
pub fn verify(ctx: &AppContext, commit: Option<&str>) -> Result<Verification> {
    let wanted = resolve_commit(ctx, commit)?;
    let receipt = load(ctx, commit)?;
    let mut checks = Vec::new();

    checks.push(Check {
        name: "schema".to_string(),
        ok: receipt.schema == SCHEMA,
        detail: format!("found `{}`, expected `{SCHEMA}`", receipt.schema),
    });

    let commit_ok = receipt.change.commit == wanted;
    checks.push(Check {
        name: "commit".to_string(),
        ok: commit_ok,
        detail: format!(
            "receipt names {}, file is for {}",
            short(&receipt.change.commit),
            short(&wanted)
        ),
    });

    let exists = git_ok(
        ctx,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{}^{{commit}}", receipt.change.commit),
        ],
    )
    .is_some();
    checks.push(Check {
        name: "commit-exists".to_string(),
        ok: exists,
        detail: if exists {
            "the commit is in this repository".to_string()
        } else {
            "the commit named by the receipt is not in this repository".to_string()
        },
    });

    if exists {
        let patch = patch_bytes(ctx, &receipt.change.commit, receipt.change.base.as_deref())
            .unwrap_or_default();
        let digest = sha256_hex(&patch);
        checks.push(Check {
            name: "patch".to_string(),
            ok: digest == receipt.change.patch_sha256,
            detail: format!("patch sha256 {digest}"),
        });

        let actual = files_in(ctx, &receipt.change.commit, receipt.change.base.as_deref())
            .unwrap_or_default();
        let recorded: BTreeSet<&str> = receipt
            .change
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        let measured: BTreeSet<&str> = actual.iter().map(|file| file.path.as_str()).collect();
        checks.push(Check {
            name: "files".to_string(),
            ok: recorded == measured,
            detail: format!(
                "receipt lists {} file(s), git shows {}",
                recorded.len(),
                measured.len()
            ),
        });
    }

    let unattributed = receipt
        .authority
        .approvals
        .iter()
        .filter(|approval| {
            approval.approved_by.trim().is_empty() || approval.approved_by == "unknown"
        })
        .count();
    checks.push(Check {
        name: "approvals-attributed".to_string(),
        ok: unattributed == 0,
        detail: if receipt.authority.approvals.is_empty() {
            "no approvals were needed for this change".to_string()
        } else if unattributed == 0 {
            format!(
                "{} approval(s), every one names a person",
                receipt.authority.approvals.len()
            )
        } else {
            format!("{unattributed} approval(s) name no person")
        },
    });

    let ok = checks.iter().all(|check| check.ok);
    Ok(Verification {
        schema: receipt.schema,
        commit: receipt.change.commit,
        ok,
        checks,
    })
}

/// Human summary of a receipt.
pub fn render(receipt: &Receipt) -> String {
    let added: u64 = receipt.change.files.iter().map(|file| file.added).sum();
    let removed: u64 = receipt.change.files.iter().map(|file| file.removed).sum();
    let mut out = format!(
        "{} for {}\n  change:    {} file(s), +{added} −{removed}\n  agent:     {}",
        receipt.schema,
        short(&receipt.change.commit),
        receipt.change.files.len(),
        receipt.agent.agent
    );
    if let Some(session) = &receipt.agent.session {
        out.push_str(&format!(" · session {session}"));
    }
    if let Some(model) = &receipt.agent.model {
        out.push_str(&format!(" · {model}"));
    }
    out.push_str(&format!("\n  authority: {}", receipt.authority.user));
    if receipt.authority.approvals.is_empty() {
        out.push_str(" · no approvals needed");
    } else {
        let allowed = receipt
            .authority
            .approvals
            .iter()
            .filter(|approval| approval.approved)
            .count();
        out.push_str(&format!(
            " · {allowed}/{} approved",
            receipt.authority.approvals.len()
        ));
    }
    out.push_str("\n  impact:    ");
    match (&receipt.impact.symbols, &receipt.impact.references) {
        (Some(symbols), Some(references)) => {
            out.push_str(&format!("{symbols} symbol(s), {references} reference(s)"));
            match &receipt.impact.impacted_tests {
                Some(tests) if tests.is_empty() => out.push_str(", no test mentions them"),
                Some(tests) => out.push_str(&format!(", {} test file(s)", tests.len())),
                None => out.push_str(", tests unmeasured"),
            }
        }
        _ => out.push_str(
            receipt
                .impact
                .unavailable
                .as_deref()
                .unwrap_or("not measured"),
        ),
    }
    if receipt.beliefs.is_none()
        && let Some(reason) = &receipt.beliefs_unavailable
    {
        out.push_str(&format!("\n  beliefs:   not available ({reason})"));
    }
    for approval in &receipt.authority.approvals {
        out.push_str(&format!(
            "\n  {} {} by {} at {}",
            if approval.approved { "✓" } else { "✗" },
            approval.subject,
            approval.approved_by,
            approval.at_ms
        ));
    }
    out
}

fn measure_impact(ctx: &AppContext, base: &str) -> Impact {
    match crate::kg::review(ctx, false, Some(base)) {
        Ok(report) => {
            let mut tests: BTreeSet<String> = BTreeSet::new();
            for change in &report.changes {
                tests.extend(change.impacted_tests.iter().cloned());
            }
            Impact {
                symbols: Some(report.summary.symbols_changed),
                references: Some(report.summary.references_affected),
                impacted_tests: Some(tests.into_iter().collect()),
                unavailable: None,
            }
        }
        Err(error) => Impact {
            symbols: None,
            references: None,
            impacted_tests: None,
            unavailable: Some(format!("code graph unavailable: {error}")),
        },
    }
}

fn latest_session(ctx: &AppContext) -> (Option<String>, Option<String>, Vec<Approval>) {
    let store = crate::session::SessionStore::new(&ctx.sessions_dir);
    let Some(newest) = store.list().into_iter().next() else {
        return (None, None, Vec::new());
    };
    let mut session_id = Some(newest.name.clone());
    let mut model = Some(newest.model.clone());
    let mut approvals = Vec::new();
    if let Ok(session) = memnoth_session::Session::load(Path::new(&newest.path)) {
        session_id = Some(session.id.clone());
        model = Some(session.model.clone());
        approvals = session
            .approvals
            .iter()
            .map(|decision| Approval {
                tool: decision.tool.clone(),
                subject: decision.subject.clone(),
                approved: decision.approved,
                approved_by: decision.approved_by.clone(),
                at_ms: decision.at_ms,
                rule: decision.rule.clone(),
            })
            .collect();
    }
    (session_id, model, approvals)
}

fn resolve_commit(ctx: &AppContext, commit: Option<&str>) -> Result<String> {
    let rev = commit.unwrap_or("HEAD").trim();
    if rev.is_empty() {
        bail!("a receipt needs a revision; `HEAD` is the default");
    }
    git(
        ctx,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
    .with_context(|| format!("resolve {rev} to a commit in {}", ctx.root.display()))
}

fn parent_of(ctx: &AppContext, commit: &str) -> Option<String> {
    git_ok(
        ctx,
        &["rev-parse", "--verify", "--quiet", &format!("{commit}^")],
    )
}

fn patch_bytes(ctx: &AppContext, commit: &str, base: Option<&str>) -> Result<Vec<u8>> {
    let args: Vec<&str> = match base {
        Some(base) => vec!["diff", "--binary", "--full-index", base, commit],
        None => vec!["show", "--binary", "--full-index", "--format=", commit],
    };
    git_bytes(ctx, &args)
}

/// Files in the change, with line counts and a truthful change type.
fn files_in(ctx: &AppContext, commit: &str, base: Option<&str>) -> Result<Vec<FileChange>> {
    let status_args: Vec<&str> = match base {
        Some(base) => vec!["diff", "--name-status", "-M", base, commit],
        None => vec!["show", "--name-status", "-M", "--format=", commit],
    };
    let numstat_args: Vec<&str> = match base {
        Some(base) => vec!["diff", "--numstat", "-M", base, commit],
        None => vec!["show", "--numstat", "-M", "--format=", commit],
    };
    let statuses = parse_name_status(&git(ctx, &status_args)?);
    let counts = parse_numstat(&git(ctx, &numstat_args)?);

    Ok(statuses
        .into_iter()
        .map(|(path, change_type)| {
            let (added, removed) = counts.get(&path).copied().unwrap_or((0, 0));
            FileChange {
                path,
                added,
                removed,
                change_type,
            }
        })
        .collect())
}

/// `M\tpath`, `R100\told\tnew` → (path, type). Renames keep the new path.
fn parse_name_status(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut columns = line.split('\t');
        let Some(status) = columns.next() else {
            continue;
        };
        let rest: Vec<&str> = columns.collect();
        let Some(path) = rest.last() else {
            continue;
        };
        let kind = match status.chars().next() {
            Some('A') => "added",
            Some('D') => "deleted",
            Some('R') => "renamed",
            Some('C') => "copied",
            _ => "modified",
        };
        out.push((path.to_string(), kind.to_string()));
    }
    out
}

/// `12\t3\tpath` → (path, (added, removed)). Binary files report `-`.
fn parse_numstat(text: &str) -> BTreeMap<String, (u64, u64)> {
    let mut out = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut columns = line.split('\t');
        let (Some(added), Some(removed), Some(path)) =
            (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        out.insert(
            rename_target(path),
            (added.parse().unwrap_or(0), removed.parse().unwrap_or(0)),
        );
    }
    out
}

/// `src/{a.rs => b.rs}` and `old.rs => new.rs` both name `new` after a rename.
fn rename_target(path: &str) -> String {
    let Some((left, right)) = path.split_once(" => ") else {
        return path.to_string();
    };
    // A rename without braces replaces the whole path, so there is no prefix
    // to keep. With braces, the part before `{` is shared by old and new.
    let prefix = if left.contains('{') {
        left.split('{').next().unwrap_or("")
    } else {
        ""
    };
    let suffix = right.trim_end_matches('}');
    format!("{prefix}{suffix}")
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn actor_identity() -> String {
    for key in ["MEMNOTH_USER", "USERNAME", "USER"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    "unknown".to_string()
}

fn git(ctx: &AppContext, args: &[&str]) -> Result<String> {
    let output = git_bytes(ctx, args)?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

fn git_ok(ctx: &AppContext, args: &[&str]) -> Option<String> {
    git(ctx, args).ok()
}

fn git_bytes(ctx: &AppContext, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(&ctx.root)
        .args(args)
        .output()
        .with_context(|| format!("run `git {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// SHA-256, hex encoded.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_reports_the_new_path() {
        assert_eq!(rename_target("old.rs => new.rs"), "new.rs");
        assert_eq!(rename_target("src/{a.rs => b.rs}"), "src/b.rs");
        assert_eq!(rename_target("plain.rs"), "plain.rs");
    }

    #[test]
    fn deletions_are_not_dropped_and_keep_their_type() {
        let rows =
            parse_name_status("M\tsrc/a.rs\nD\tsrc/b.rs\nA\tCargo.toml\nR100\told.rs\tnew.rs\n");
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1], ("src/b.rs".to_string(), "deleted".to_string()));
        assert_eq!(rows[3], ("new.rs".to_string(), "renamed".to_string()));
    }

    #[test]
    fn binary_files_count_as_zero_lines_rather_than_vanishing() {
        let counts = parse_numstat("-\t-\tlogo.png\n12\t3\tsrc/a.rs\n");
        assert_eq!(counts.get("logo.png"), Some(&(0, 0)));
        assert_eq!(counts.get("src/a.rs"), Some(&(12, 3)));
    }

    #[test]
    fn the_same_bytes_always_hash_the_same() {
        assert_eq!(sha256_hex(b"").len(), 64);
        assert_eq!(sha256_hex(b"a"), sha256_hex(b"a"));
        assert_ne!(sha256_hex(b"a"), sha256_hex(b"b"));
    }
}
