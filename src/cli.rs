//! The command surface of `memnoth`.
//!
//! One binary answers to every verb the product has, and the shape here is the
//! contract the rest of the crate is written against:
//!
//! ```text
//! memnoth                              # the session (TUI)
//! memnoth --resume payments            # a named session, resumed
//! memnoth vault init|status|asof|history|contradictions|remember|recall
//! memnoth kg index|status|impact|search
//! memnoth mcp serve|doctor|add|call|selftest
//! memnoth doctor
//! ```
//!
//! `--root`, `--vault`, `--namespace`, `--json` and `--quiet` are global so they
//! can be written after the verb (`memnoth kg index --root ../other`, which is
//! how `SKILL.md` tells agents to call it) or before it.
//!
//! A typo in a profile name is refused here, by [`crate::commands`], before a
//! server is started: the profile decides which tools a host is told about, so
//! falling back to `full` on a typo would silently widen the surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// The product in one binary: session, vault, code graph, MCP server.
#[derive(Debug, Parser)]
#[command(
    name = "memnoth",
    version,
    about = "Terminal agent, bi-temporal vault, code graph and MCP server",
    long_about = "One binary for the whole local product.\n\n\
                  With no verb it opens the session against the current directory: the \
                  transcript, the vault and what the graph knows, in one screen. Every verb \
                  answers as text for a person, or as JSON with --json for a program — the \
                  same shapes the MCP tools return, so a script and an agent read one \
                  representation rather than two that can drift.",
    after_help = "Run `memnoth doctor` when something is not wired: it reports the \
                  vault, the graph, the MCP server binary it can find and the clients \
                  whose config it can read."
)]
pub struct Cli {
    /// Workspace to read. Defaults to the current directory.
    #[arg(long, global = true, value_name = "DIR")]
    pub root: Option<PathBuf>,

    /// Vault database. Defaults to `~/.memnoth/vault.db`.
    #[arg(long, global = true, value_name = "PATH")]
    pub vault: Option<PathBuf>,

    /// Vault namespace (`default`, or a name that selects `vault-<name>.db`).
    #[arg(long, global = true, value_name = "NAME", default_value = "default")]
    pub namespace: String,

    /// Machine-readable output: one JSON envelope on stdout.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress progress lines; keep the answer.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Resume the named session instead of starting a new one.
    #[arg(long, global = true, value_name = "NAME")]
    pub resume: Option<String>,

    #[command(subcommand)]
    pub verb: Option<Verb>,
}

/// A top-level verb. Without one, the CLI opens the session.
#[derive(Debug, Subcommand)]
pub enum Verb {
    /// Open the session (the default when no verb is given).
    Session(SessionArgs),
    /// Create, read, and correct the temporal vault.
    Vault(VaultArgs),
    /// Build and query the code graph.
    Kg(KgArgs),
    /// Serve the graph over MCP, and register clients for it.
    Mcp(McpArgs),
    /// List available models from the cloud.
    Models(ModelsArgs),
    /// Show usage and costs, with local reconciliation.
    Usage(UsageArgs),
    /// Show usage statistics with local reconciliation.
    Stats(StatsArgs),
    /// Authentication: login, logout, status, whoami.
    Auth(AuthArgs),
    /// Team operations: join an organization via invite code.
    Teams(TeamsArgs),
    /// Configuration: get, set, list, edit.
    Config(ConfigArgs),
    /// Cloud operations: sync, profile, billing, credits.
    Cloud(CloudArgs),
    /// Check for and apply updates.
    Update(UpdateArgs),
    /// Install memnoth (shows installer instructions).
    Install(InstallArgs),
    /// Uninstall memnoth.
    Uninstall(UninstallArgs),
    /// First-run setup: vault, code graph, auth, MCP, provider, config.
    Setup(SetupArgs),
    /// Shell completion: generate or install completions.
    Completion(CompletionArgs),
    /// Report what is wired: paths, versions, profiles, clients.
    Doctor(DoctorArgs),
    /// Record, show and verify what an authorised agent changed.
    Receipt(ReceiptArgs),
    /// Show, verify and export the hash-chained memory audit trail.
    Audit(AuditArgs),
    /// Show telemetry status and toggle collection.
    Telemetry(TelemetryArgs),
    /// Privacy: show what leaves machine, toggle telemetry, set metadata-only default.
    Privacy(PrivacyArgs),
    /// Billing: invoices, plan, limits.
    Billing(BillingCommands),
    /// Cloud projects: list, create, rename, delete.
    Projects(ProjectsArgs),
    /// API keys for this org: list, create, revoke.
    Keys(KeysArgs),
    /// The organization: show, rename, members, invites.
    Org(OrgArgs),
    /// Open a deep-link to the memnoth cloud console.
    Open(OpenArgs),
    /// Skill management: add, list, remove, run, edit.
    Skill(SkillArgs),
    /// Set UI theme.
    SetTheme(crate::tui::app::Theme),
    /// Set screen reader mode.
    SetScreenReader(crate::tui::app::ScreenReaderMode),
    /// Send feedback to memnoth cloud.
    Feedback(FeedbackArgs),
    /// Configure model provider: add, list, remove, test connection.
    Connect(ConnectArgs),
    /// Automation: git hooks, GitHub Actions and CI indexing.
    Automation(AutomationArgs),
    /// Git worktrees: create, list, clean and diff.
    Worktree(WorktreeArgs),
    /// Fan a task out across isolated worktrees.
    Batch(BatchCmdArgs),
    /// Run one command in parallel across worktrees.
    Parallel(ParallelArgs),
    /// Generate command documentation from the registry.
    Docs(DocsArgs),
    /// Serve the graph and vault over a local HTTP API.
    Serve(ServeArgs),
    /// Watch files and reindex incrementally.
    Watch(WatchArgs),
}
/// `mn connect` — configure model provider with API key.
#[derive(Debug, Args)]
pub struct ConnectArgs {
    #[command(subcommand)]
    pub action: ConnectCommand,
}

/// Connect subcommands.
#[derive(Debug, Subcommand)]
pub enum ConnectCommand {
    /// Add or update a provider configuration.
    #[command(name = "add")]
    Add(ConnectAddArgs),
    /// List configured providers.
    #[command(name = "list")]
    List(ConnectListArgs),
    /// Remove a provider configuration.
    #[command(name = "remove")]
    Remove(ConnectRemoveArgs),
    /// Test connection to a provider.
    #[command(name = "test")]
    Test(ConnectTestArgs),
}

/// Arguments for `mn connect add`.
#[derive(Debug, Args)]
pub struct ConnectAddArgs {
    /// Provider name (e.g., anthropic, openai, openrouter).
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// API key for the provider.
    #[arg(long, value_name = "KEY")]
    pub api_key: Option<String>,
    /// Base URL for the provider (for custom endpoints/gateways).
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,
    /// Model ID to use with this provider.
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
    /// Run in non-interactive mode (requires --provider and --api-key).
    #[arg(long)]
    pub non_interactive: bool,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn connect list`.
#[derive(Debug, Args)]
pub struct ConnectListArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn connect remove <name>`.
#[derive(Debug, Args)]
pub struct ConnectRemoveArgs {
    /// Provider name to remove.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn connect test`.
#[derive(Debug, Args)]
pub struct ConnectTestArgs {
    /// Provider name to test (defaults to current configured provider).
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}
/// `memnoth doctor` — report what is wired.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Apply fixes for detected issues (vault init, kg index, config defaults, PATH, completions, auth).
    #[arg(long)]
    pub fix: bool,

    /// Non-interactive: apply all fixes without prompting.
    #[arg(long, requires = "fix")]
    pub yes: bool,

    /// Write a redacted support bundle to this path.
    ///
    /// The bundle carries counts and versions, never a path, a credential, a
    /// prompt or vault content. It is safe to attach to a bug report.
    #[arg(long, value_name = "PATH")]
    pub bundle: Option<String>,
}

/// `mn audit …` — the governance trail.
#[derive(Debug, Args)]
pub struct AuditArgs {
    #[command(subcommand)]
    pub action: AuditCommand,
}

/// Audit subcommands.
#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    /// List the most recent events.
    Show(AuditShowArgs),
    /// Verify the hash chain and exit non-zero on the first break.
    Verify(AuditVerifyArgs),
    /// Write the trail unchanged as JSONL.
    Export(AuditExportArgs),
}

/// Arguments for `mn audit show`.
#[derive(Debug, Args)]
pub struct AuditShowArgs {
    /// Maximum events to print; 0 prints all.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// Print the events as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn audit verify`.
#[derive(Debug, Args)]
pub struct AuditVerifyArgs {
    /// Print the verdict as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn audit export`.
#[derive(Debug, Args)]
pub struct AuditExportArgs {
    /// Destination file; without it the JSONL goes to stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<String>,
    /// Print the export summary as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn receipt …` — the change receipt.
#[derive(Debug, Args)]
pub struct ReceiptArgs {
    #[command(subcommand)]
    pub action: ReceiptCommand,
}

/// Receipt subcommands.
#[derive(Debug, Subcommand)]
pub enum ReceiptCommand {
    /// Measure `HEAD` (or --commit) and write its receipt.
    Write(ReceiptWriteArgs),
    /// Print the receipt for a commit.
    Show(ReceiptShowArgs),
    /// Verify a receipt against the commit it describes.
    Verify(ReceiptVerifyArgs),
}

/// Arguments for `mn receipt write`.
#[derive(Debug, Args)]
pub struct ReceiptWriteArgs {
    /// Revision to measure; defaults to HEAD.
    #[arg(long, value_name = "REV")]
    pub commit: Option<String>,
    /// Print the receipt as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn receipt show`.
#[derive(Debug, Args)]
pub struct ReceiptShowArgs {
    /// Revision to show; defaults to HEAD.
    #[arg(long, value_name = "REV")]
    pub commit: Option<String>,
    /// Print the receipt as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn receipt verify`.
#[derive(Debug, Args)]
pub struct ReceiptVerifyArgs {
    /// Revision to verify; defaults to HEAD.
    #[arg(long, value_name = "REV")]
    pub commit: Option<String>,
    /// Print the verification as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}
/// `memnoth session`.
#[derive(Debug, Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub action: SessionCommand,
}

/// `memnoth session ...` subcommands.
#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Start a new session (or resume an existing one with --resume).
    Start(StartArgs),
    /// Resume an existing named session.
    Resume(ResumeArgs),
    /// List all saved sessions.
    List(ListArgs),
    /// Show status of a session (current or named).
    Status(StatusArgs),
    /// Stop (save and close) a session.
    Stop(StopArgs),
    /// Set reasoning effort budget for the session.
    Effort(EffortArgs),
    /// Deterministically replay a session from its transcript.
    Replay(ReplayArgs),
    /// Attach session for handoff to another machine.
    Attach(AttachArgs),
    /// Hand off session to another machine (pair with attach).
    Handoff(HandoffArgs),
    /// Run a command under the policy engine (sandbox).
    Exec(ExecArgs),
}

/// `memnoth session start` — start a new session.
#[derive(Debug, Args)]
pub struct StartArgs {
    /// Session name. Defaults to `session`; `--resume` loads it instead.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Resume an existing session instead of starting fresh.
    #[arg(long, value_name = "NAME")]
    pub resume: Option<String>,
    /// Model ID to record in the session header.
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
}

/// `memnoth session resume` — resume an existing session.
#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// Session name to resume.
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// `memnoth session list` — list all sessions.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session status` — show session status.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Session name (defaults to current session).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}
/// `memnoth session stop` — stop and save a session.
#[derive(Debug, Args)]
pub struct StopArgs {
    /// Session name (defaults to current session).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Force stop without saving.
    #[arg(long)]
    pub force: bool,
}

/// `memnoth session run` — run a scripted session non-interactively.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Path to the script file (one user prompt per line, or JSON array).
    #[arg(value_name = "SCRIPT")]
    pub script: String,
    /// Session name (defaults to "session").
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Model to use.
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
    /// Output format: text, json, markdown.
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub format: String,
    /// Maximum turns to run.
    #[arg(long, value_name = "N")]
    pub max_turns: Option<usize>,
    /// Stop on first error.
    #[arg(long)]
    pub stop_on_error: bool,
}

/// `memnoth session fork` — fork at a turn to try a direction.
#[derive(Debug, Args)]
pub struct ForkArgs {
    /// Session name (defaults to current session).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Turn index to fork from (0 = latest turn).
    #[arg(long, value_name = "N")]
    pub turn: Option<usize>,
    /// Label for the checkpoint/fork.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session branch` — branch from a checkpoint.
#[derive(Debug, Args)]
pub struct BranchArgs {
    /// Session name (defaults to current session).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Checkpoint label or index to branch from.
    #[arg(long, value_name = "LABEL")]
    pub checkpoint: String,
    /// Name for the new branch session.
    #[arg(long = "branch", value_name = "NAME")]
    pub branch_name: Option<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session plan` — enter plan mode (approve before edits).
#[derive(Debug, Args)]
pub struct PlanArgs {
    /// Session name (defaults to current session).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Enable/disable plan mode.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub enable: bool,
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub disable: bool,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session background` — manage long-running background tasks.
#[derive(Debug, Args)]
pub struct BackgroundArgs {
    /// Session name (defaults to current session).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    #[command(subcommand)]
    pub action: BackgroundAction,
}

/// Background task actions.
#[derive(Debug, Subcommand)]
pub enum BackgroundAction {
    /// Start a background task.
    Start(BackgroundStartArgs),
    /// List background tasks.
    List(BackgroundListArgs),
    /// Show status of a background task.
    Status(BackgroundStatusArgs),
    /// Cancel a background task.
    Cancel(BackgroundCancelArgs),
    /// Wait for a background task to complete.
    Wait(BackgroundWaitArgs),
}

/// `memnoth session background start` — start a background task.
#[derive(Debug, Args)]
pub struct BackgroundStartArgs {
    /// Task description/prompt.
    #[arg(value_name = "TASK")]
    pub task: String,
    /// Working directory (defaults to session's workspace).
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<String>,
    /// Model to use.
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
}

/// `memnoth session background list` — list background tasks.
#[derive(Debug, Args)]
pub struct BackgroundListArgs {
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session background status` — show background task status.
#[derive(Debug, Args)]
pub struct BackgroundStatusArgs {
    /// Task ID.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session background cancel` — cancel a background task.
#[derive(Debug, Args)]
pub struct BackgroundCancelArgs {
    /// Task ID.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// `memnoth session background wait` — wait for background task.
#[derive(Debug, Args)]
pub struct BackgroundWaitArgs {
    /// Task ID.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Timeout in seconds.
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,
}

/// `memnoth session batch` — fan out a task across worktrees.
#[derive(Debug, Args)]
pub struct SessionBatchArgs {
    /// Task description.
    #[arg(value_name = "TASK")]
    pub task: String,
    /// Number of worktrees to fan out to.
    #[arg(long, value_name = "N", default_value = "4")]
    pub workers: usize,
    /// Model to use for each worker.
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
    /// Working directory (workspace root).
    #[arg(long, value_name = "DIR")]
    pub root: Option<String>,
    /// Output format: text, json.
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub format: String,
}

/// `memnoth session effort` — set reasoning effort budget.
#[derive(Debug, Args)]
pub struct EffortArgs {
    /// Session name (defaults to current session).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Effort level: low, medium, high, max.
    #[arg(value_name = "LEVEL")]
    pub level: String,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `mn replay <session>` — deterministically replay a session.
#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// Session name to replay.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Turn to start replay from (0 = beginning).
    #[arg(long, value_name = "N", default_value = "0")]
    pub from_turn: usize,
    /// Maximum turns to replay.
    #[arg(long, value_name = "N")]
    pub max_turns: Option<usize>,
    /// Model to use for replay (defaults to session's model).
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
    /// Output format: text, json.
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub format: String,
    /// Output file (defaults to stdout).
    #[arg(long, value_name = "PATH")]
    pub output: Option<String>,
    /// Show diff between original and replayed.
    #[arg(long)]
    pub diff: bool,
}

/// `memnoth session attach` — prepare session for handoff.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Session name to attach.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Output bundle file (defaults to <name>.memnoth-bundle).
    #[arg(long, value_name = "PATH")]
    pub output: Option<String>,
    /// Include vault snapshot.
    #[arg(long)]
    pub include_vault: bool,
    /// Include code graph snapshot.
    #[arg(long)]
    pub include_kg: bool,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session handoff` — receive session on another machine.
#[derive(Debug, Args)]
pub struct HandoffArgs {
    /// Bundle file to receive.
    #[arg(value_name = "BUNDLE")]
    pub bundle: String,
    /// New session name (defaults to original name).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    /// Target workspace directory.
    #[arg(long, value_name = "DIR")]
    pub workspace: Option<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `memnoth session exec` — run command under policy engine.
#[derive(Debug, Args)]
pub struct ExecArgs {
    /// Command to execute.
    #[arg(value_name = "CMD", num_args = 1..)]
    pub cmd: Vec<String>,
    /// Session name for context (defaults to current).
    #[arg(long, value_name = "NAME")]
    pub session: Option<String>,
    /// Policy profile: strict, balanced, permissive.
    #[arg(long, value_name = "PROFILE", default_value = "balanced")]
    pub policy: String,
    /// Allow network access.
    #[arg(long)]
    pub network: bool,
    /// Allow filesystem writes outside workspace.
    #[arg(long)]
    pub write_outside: bool,
    /// Timeout in seconds.
    #[arg(long, value_name = "SECONDS", default_value = "300")]
    pub timeout: u64,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}
/// `mn teams …` — team operations.
#[derive(Debug, Args)]
pub struct TeamsArgs {
    #[command(subcommand)]
    pub action: TeamsCommand,
}

/// Team subcommands.
#[derive(Debug, Subcommand)]
pub enum TeamsCommand {
    /// Accept an invite code and join an organization.
    Join(TeamsJoinArgs),
    /// List the org's teams.
    #[command(name = "list")]
    List(TeamsListArgs),
    /// Create a team inside the org.
    #[command(name = "create")]
    Create(TeamsCreateArgs),
    /// Show one team: its people and the projects it reaches.
    #[command(name = "members")]
    Members(TeamsMembersArgs),
    /// Claim a task from the shared board.
    Claim(TeamClaimArgs),
    /// Release a claimed task.
    Release(TeamReleaseArgs),
    /// List tasks on the shared board.
    Tasks(TeamTasksArgs),
    /// Show messages in your inbox.
    Inbox(TeamInboxArgs),
    /// Show team presence (active sessions).
    Presence(TeamPresenceArgs),
    /// Lock a file path for exclusive editing.
    Lock(TeamLockArgs),
    /// Hand off a task to another team member.
    Handoff(TeamHandoffArgs),
    /// Request a review for a task.
    Review(TeamReviewArgs),
    /// Run an audit of team activity.
    Audit(TeamAuditArgs),
}

/// Arguments for `mn teams join`.
#[derive(Debug, Args)]
pub struct TeamsJoinArgs {
    /// The invite code (XXXX-XXXX format, hyphen optional).
    #[arg(value_name = "CODE")]
    pub code: String,
}

/// Arguments for `mn teams list`.
#[derive(Debug, Args)]
pub struct TeamsListArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn teams create`.
#[derive(Debug, Args)]
pub struct TeamsCreateArgs {
    /// Team name.
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn teams members`.
#[derive(Debug, Args)]
pub struct TeamsMembersArgs {
    /// Team id, as `mn teams list` reports it.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn team claim`.
#[derive(Debug, Args)]
pub struct TeamClaimArgs {
    /// The task ID to claim.
    #[arg(value_name = "TASK_ID")]
    pub task_id: String,
}

/// Arguments for `mn team release`.
#[derive(Debug, Args)]
pub struct TeamReleaseArgs {
    /// The task ID to release.
    #[arg(value_name = "TASK_ID")]
    pub task_id: String,
}

/// Arguments for `mn team tasks`.
#[derive(Debug, Args)]
pub struct TeamTasksArgs {
    /// Show only pending tasks.
    #[arg(long)]
    pub pending: bool,
    /// Show only in-progress tasks.
    #[arg(long)]
    pub in_progress: bool,
    /// Show only done tasks.
    #[arg(long)]
    pub done: bool,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn team inbox`.
#[derive(Debug, Args)]
pub struct TeamInboxArgs {
    /// Include broadcast messages.
    #[arg(long)]
    pub broadcast: bool,
    /// Acknowledge all messages after reading.
    #[arg(long)]
    pub ack: bool,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn team presence`.
#[derive(Debug, Args)]
pub struct TeamPresenceArgs {
    /// Show stale sessions only.
    #[arg(long)]
    pub stale: bool,
    /// Sync presence with cloud.
    #[arg(long)]
    pub sync: bool,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn team lock`.
#[derive(Debug, Args)]
pub struct TeamLockArgs {
    /// The file path glob to lock.
    #[arg(value_name = "PATH")]
    pub path: String,
    /// TTL in seconds (default: 1800 = 30 min).
    #[arg(long, default_value = "1800")]
    pub ttl: u64,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn team handoff`.
#[derive(Debug, Args)]
pub struct TeamHandoffArgs {
    /// The task ID to hand off.
    #[arg(value_name = "TASK_ID")]
    pub task_id: String,
    /// The target session/user to hand off to.
    #[arg(value_name = "TARGET")]
    pub target: String,
    /// Message to include with handoff.
    #[arg(long)]
    pub message: Option<String>,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn team review`.
#[derive(Debug, Args)]
pub struct TeamReviewArgs {
    /// The task ID to request review for.
    #[arg(value_name = "TASK_ID")]
    pub task_id: String,
    /// The reviewer session/user.
    #[arg(value_name = "REVIEWER")]
    pub reviewer: String,
    /// Review message.
    #[arg(long)]
    pub message: Option<String>,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn team audit`.
#[derive(Debug, Args)]
pub struct TeamAuditArgs {
    /// Time range for audit (e.g., "24h", "7d", "30d").
    #[arg(long, default_value = "24h")]
    pub range: String,
    /// Filter by session ID.
    #[arg(long)]
    pub session: Option<String>,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn config …` — configuration commands.
#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigCommand,
}

/// `mn setup` — interactive setup wizard.
#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Skip specific steps (comma-separated: vault,graph,auth,mcp,provider,config).
    #[arg(long, value_delimiter = ',', value_name = "STEPS")]
    pub skip: Vec<String>,
    /// Non-interactive mode: accept defaults, don't prompt.
    #[arg(long)]
    pub yes: bool,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}
/// Configuration subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Get a configuration value.
    Get(ConfigGetArgs),
    /// Set a configuration value.
    Set(ConfigSetArgs),
    /// List all configuration values with their sources.
    List(ConfigListArgs),
    /// Open the configuration file in $EDITOR.
    Edit(ConfigEditArgs),
}

/// Arguments for `mn config get`.
#[derive(Debug, Args)]
pub struct ConfigGetArgs {
    /// Configuration key to get.
    #[arg(value_name = "KEY")]
    pub key: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn config set`.
#[derive(Debug, Args)]
pub struct ConfigSetArgs {
    /// Configuration key to set.
    #[arg(value_name = "KEY")]
    pub key: String,

    /// Configuration value to set.
    #[arg(value_name = "VALUE")]
    pub value: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn config list`.
#[derive(Debug, Args)]
pub struct ConfigListArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn config edit`.
#[derive(Debug, Args)]
pub struct ConfigEditArgs {
    /// Output as JSON (prints path instead of opening editor).
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn models` — list available models from the cloud.
/// `mn model use <id>` — switch default model (persisted).
#[derive(Debug, Args)]
pub struct ModelsArgs {
    #[command(subcommand)]
    pub action: ModelsCommand,
}

/// Models subcommands.
#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// List available models from the cloud.
    #[command(name = "list")]
    List(ModelsListArgs),
    /// Set default model (persisted to config).
    #[command(name = "use")]
    Use(ModelUseArgs),
}

/// Arguments for `mn models list`.
#[derive(Debug, Args)]
pub struct ModelsListArgs {
    /// Filter by provider name.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,
    /// Only show models from configured providers (have API keys).
    #[arg(long)]
    pub connected: bool,
    /// Output as JSON with full metadata.
    #[arg(long, global = true)]
    pub json: bool,
    /// Output format: table, wide (all columns), or json.
    #[arg(long, value_name = "FORMAT", default_value = "table")]
    pub format: ModelsFormat,
    /// Compare two models side-by-side (comma-separated IDs).
    #[arg(long, value_name = "ID1,ID2")]
    pub compare: Option<String>,
}

/// Output format for models list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ModelsFormat {
    Table,
    Wide,
    Json,
}

/// Arguments for `mn model use <id>`.
#[derive(Debug, Args)]
pub struct ModelUseArgs {
    /// Model ID to set as default.
    #[arg(value_name = "ID")]
    pub model_id: String,
}

/// `mn telemetry` — show telemetry status and toggle.
#[derive(Debug, Args)]
pub struct TelemetryArgs {
    #[command(subcommand)]
    pub action: TelemetryCommand,
}

/// Telemetry subcommands.
#[derive(Debug, Subcommand)]
pub enum TelemetryCommand {
    /// Show telemetry status (what's collected, current state).
    #[command(name = "status")]
    Status(TelemetryStatusArgs),
    /// Enable telemetry collection.
    #[command(name = "on")]
    On(TelemetryOnArgs),
    /// Disable telemetry collection.
    #[command(name = "off")]
    Off(TelemetryOffArgs),
}

/// Arguments for `mn telemetry status`.
#[derive(Debug, Args)]
pub struct TelemetryStatusArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn telemetry on`.
#[derive(Debug, Args)]
pub struct TelemetryOnArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn telemetry off`.
#[derive(Debug, Args)]
pub struct TelemetryOffArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn privacy` — show what leaves machine, toggle telemetry, set metadata-only default.
#[derive(Debug, Args)]
pub struct PrivacyArgs {
    #[command(subcommand)]
    pub action: PrivacyCommand,
}

#[derive(Debug, Subcommand)]
pub enum PrivacyCommand {
    /// Show privacy status and what data leaves the machine.
    #[command(name = "status")]
    Status(PrivacyStatusArgs),
    /// Enable telemetry collection (default: on).
    #[command(name = "on")]
    On(PrivacyOnArgs),
    /// Disable telemetry collection (no telemetry data leaves machine).
    #[command(name = "off")]
    Off(PrivacyOffArgs),
    /// Set privacy mode: metadata-only, balanced, open.
    #[command(name = "mode")]
    Mode(PrivacyModeArgs),
    /// Show what data would be sent in a sync.
    #[command(name = "show-data")]
    ShowData(PrivacyShowDataArgs),
}

/// Arguments for `mn privacy status`.
#[derive(Debug, Args)]
pub struct PrivacyStatusArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn privacy on`.
#[derive(Debug, Args)]
pub struct PrivacyOnArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn privacy off`.
#[derive(Debug, Args)]
pub struct PrivacyOffArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn privacy mode`.
#[derive(Debug, Args)]
pub struct PrivacyModeArgs {
    /// Privacy mode: metadata-only, balanced, open.
    #[arg(value_name = "MODE")]
    pub mode: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn privacy show-data`.
#[derive(Debug, Args)]
pub struct PrivacyShowDataArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn billing` — invoices, plan, limits.
#[derive(Debug, Args)]
pub struct BillingCommands {
    #[command(subcommand)]
    pub action: BillingCommand,
}

/// Billing subcommands.
#[derive(Debug, Subcommand)]
pub enum BillingCommand {
    /// Show billing history / invoices link.
    #[command(name = "invoices")]
    Invoices(BillingInvoicesArgs),
    /// Show current plan and quotas.
    #[command(name = "plan")]
    Plan(BillingPlanArgs),
    /// Show rate limits and quotas.
    #[command(name = "limits")]
    Limits(BillingLimitsArgs),
}

/// Arguments for `mn invoices`.
#[derive(Debug, Args)]
pub struct BillingInvoicesArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn plan`.
#[derive(Debug, Args)]
pub struct BillingPlanArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn limits`.
#[derive(Debug, Args)]
pub struct BillingLimitsArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn projects` — the org's projects, as the console lists them.
#[derive(Debug, Args)]
pub struct ProjectsArgs {
    #[command(subcommand)]
    pub action: ProjectsCommand,
}

/// Project subcommands.
#[derive(Debug, Subcommand)]
pub enum ProjectsCommand {
    /// List the projects this credential can see.
    #[command(name = "list")]
    List(ProjectsListArgs),
    /// Create a project.
    #[command(name = "create")]
    Create(ProjectsCreateArgs),
    /// Rename a project.
    #[command(name = "rename")]
    Rename(ProjectsRenameArgs),
    /// Delete a project. Its sessions are kept, unassigned.
    #[command(name = "delete")]
    Delete(ProjectsDeleteArgs),
    /// File every session from this machine under a project.
    #[command(name = "use")]
    Use(ProjectsUseArgs),
    /// List the sessions no project has claimed yet.
    #[command(name = "unassigned")]
    Unassigned(ProjectsUnassignedArgs),
    /// File one already-recorded session under the current project.
    #[command(name = "file")]
    File(ProjectsFileArgs),
}

/// Arguments for `mn projects list`.
#[derive(Debug, Args)]
pub struct ProjectsListArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn projects create`.
#[derive(Debug, Args)]
pub struct ProjectsCreateArgs {
    /// Project name.
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Who can see it without a grant: `org` (default) or `restricted`.
    #[arg(long, value_name = "SCOPE")]
    pub visibility: Option<String>,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn projects rename`.
#[derive(Debug, Args)]
pub struct ProjectsRenameArgs {
    /// Project id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// The new name.
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn projects delete`.
#[derive(Debug, Args)]
pub struct ProjectsDeleteArgs {
    /// Project id.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Delete without asking for confirmation.
    #[arg(long)]
    pub yes: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn projects use`.
#[derive(Debug, Args)]
pub struct ProjectsUseArgs {
    /// Project id, as `mn projects list` prints it. Omit it to print the current
    /// choice.
    #[arg(value_name = "ID")]
    pub id: Option<String>,

    /// Stop filing sessions under a project; they report as unassigned again.
    #[arg(long)]
    pub clear: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn projects unassigned`.
#[derive(Debug, Args)]
pub struct ProjectsUnassignedArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn projects file`.
#[derive(Debug, Args)]
pub struct ProjectsFileArgs {
    /// Session id, as `mn projects unassigned` prints it.
    #[arg(value_name = "SESSION")]
    pub session: String,

    /// File into this project instead of the one `mn projects use` stored.
    #[arg(long, value_name = "ID")]
    pub project: Option<String>,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn keys` — the org's API keys. Never their secrets, except once on create.
#[derive(Debug, Args)]
pub struct KeysArgs {
    #[command(subcommand)]
    pub action: KeysCommand,
}

/// Key subcommands.
#[derive(Debug, Subcommand)]
pub enum KeysCommand {
    /// List keys in this org, including revoked ones.
    #[command(name = "list")]
    List(KeysListArgs),
    /// Mint a key. The secret is printed once and never stored.
    #[command(name = "create")]
    Create(KeysCreateArgs),
    /// Revoke one key by id.
    #[command(name = "revoke")]
    Revoke(KeysRevokeArgs),
    /// Revoke every live key in the org (owner or admin).
    #[command(name = "revoke-all")]
    RevokeAll(KeysRevokeAllArgs),
}

/// Arguments for `mn keys list`.
#[derive(Debug, Args)]
pub struct KeysListArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn keys create`.
#[derive(Debug, Args)]
pub struct KeysCreateArgs {
    /// A label so the key can be recognised later.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// The key's authority (`read` | `write` | `admin`); the server decides the default.
    #[arg(long, value_name = "SCOPE")]
    pub scope: Option<String>,

    /// Bind the key to one project id instead of the whole org.
    #[arg(long, value_name = "ID")]
    pub project: Option<String>,

    /// Stop working after this many days.
    #[arg(long, value_name = "DAYS")]
    pub expires_in_days: Option<i64>,

    /// Output as JSON. The secret is in the envelope, so do not pipe it to a log.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn keys revoke`.
#[derive(Debug, Args)]
pub struct KeysRevokeArgs {
    /// Key id, as `mn keys list` reports it.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn keys revoke-all`.
#[derive(Debug, Args)]
pub struct KeysRevokeAllArgs {
    /// Revoke every key without asking for confirmation.
    #[arg(long)]
    pub yes: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn org` — the tenant's identity, its roster and its pending invites.
#[derive(Debug, Args)]
pub struct OrgArgs {
    #[command(subcommand)]
    pub action: OrgCommand,
}

/// Org subcommands.
#[derive(Debug, Subcommand)]
pub enum OrgCommand {
    /// Show the org: name, plan, your role, and its counts.
    #[command(name = "show")]
    Show(OrgShowArgs),
    /// Rename the org (owner or admin).
    #[command(name = "rename")]
    Rename(OrgRenameArgs),
    /// List the org's members.
    #[command(name = "members")]
    Members(OrgMembersArgs),
    /// Add an existing account to the org by email.
    #[command(name = "add-member")]
    AddMember(OrgAddMemberArgs),
    /// Change a member's role: owner, admin or member.
    #[command(name = "set-role")]
    SetRole(OrgSetRoleArgs),
    /// Remove a member, their project grants and their keys.
    #[command(name = "remove-member")]
    RemoveMember(OrgRemoveMemberArgs),
    /// List pending invites.
    #[command(name = "invites")]
    Invites(OrgInvitesArgs),
    /// Invite someone by email; the link and code are shown once.
    #[command(name = "invite")]
    Invite(OrgInviteArgs),
    /// Withdraw a pending invite.
    #[command(name = "withdraw")]
    Withdraw(OrgWithdrawArgs),
}

/// Arguments for `mn org show`.
#[derive(Debug, Args)]
pub struct OrgShowArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org rename`.
#[derive(Debug, Args)]
pub struct OrgRenameArgs {
    /// The new display name.
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org members`.
#[derive(Debug, Args)]
pub struct OrgMembersArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org add-member`.
#[derive(Debug, Args)]
pub struct OrgAddMemberArgs {
    /// The address of an account that already exists.
    #[arg(value_name = "EMAIL")]
    pub email: String,

    /// Role to grant: owner, admin or member (default: member).
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org set-role`.
#[derive(Debug, Args)]
pub struct OrgSetRoleArgs {
    /// The member's user id, as `mn org members` reports it.
    #[arg(value_name = "USER_ID")]
    pub user_id: String,

    /// The new role: owner, admin or member.
    #[arg(value_name = "ROLE")]
    pub role: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org remove-member`.
#[derive(Debug, Args)]
pub struct OrgRemoveMemberArgs {
    /// The member's user id.
    #[arg(value_name = "USER_ID")]
    pub user_id: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org invites`.
#[derive(Debug, Args)]
pub struct OrgInvitesArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org invite`.
#[derive(Debug, Args)]
pub struct OrgInviteArgs {
    /// The invitee's email address.
    #[arg(value_name = "EMAIL")]
    pub email: String,

    /// Role the invite carries: admin or member (default: member).
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,

    /// Output as JSON. The link and code are in the envelope.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn org withdraw`.
#[derive(Debug, Args)]
pub struct OrgWithdrawArgs {
    /// Invite id, as `mn org invites` reports it.
    #[arg(value_name = "ID")]
    pub id: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn usage` — show usage and costs, with local reconciliation.
#[derive(Debug, Args)]
pub struct UsageArgs {
    /// Group by: day, model, project.
    #[arg(long, value_name = "KIND")]
    pub by: Option<String>,

    /// Since date (YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// Time range: 7d, 30d, 90d, 365d.
    #[arg(long, value_name = "RANGE", default_value = "30d")]
    pub range: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn stats` — show usage statistics with local reconciliation.
#[derive(Debug, Args)]
pub struct StatsArgs {
    /// Group by: day, model, project.
    #[arg(long, value_name = "KIND")]
    pub by: Option<String>,

    /// Since date (YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// Time range: 7d, 30d, 90d, 365d.
    #[arg(long, value_name = "RANGE", default_value = "30d")]
    pub range: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn completions …` — shell completion commands.
#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[command(subcommand)]
    pub action: CompletionCommand,
}

/// Completion subcommands.
#[derive(Debug, Subcommand)]
pub enum CompletionCommand {
    /// Print shell completions to stdout.
    #[command(name = "show")]
    Show(CompletionShowArgs),
    /// Install shell completions to the standard location.
    #[command(name = "install")]
    Install(CompletionInstallArgs),
}

/// Arguments for `mn completions show`.
#[derive(Debug, Args)]
pub struct CompletionShowArgs {
    /// Shell to generate completions for (bash, zsh, fish, powershell).
    #[arg(long, short = 's', value_name = "SHELL", default_value = "bash")]
    pub shell: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn completions install`.
#[derive(Debug, Args)]
pub struct CompletionInstallArgs {
    /// Shell to install completions for (bash, zsh, fish, powershell).
    /// Auto-detected from SHELL environment variable if not specified.
    #[arg(long, short = 's', value_name = "SHELL")]
    pub shell: Option<String>,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn cloud …` — cloud operations.
#[derive(Debug, Args)]
pub struct CloudArgs {
    #[command(subcommand)]
    pub action: CloudCommand,
}

/// Cloud subcommands.
#[derive(Debug, Subcommand)]
pub enum CloudCommand {
    /// Sync local sessions to the cloud (POST /api/v1/sessions).
    #[command(name = "sync")]
    Sync(SyncArgs),
    /// Manage cloud profile configuration.
    #[command(name = "profile")]
    Profile(ProfileArgs),
    /// Show billing information (credits + pricing).
    #[command(name = "billing")]
    Billing(BillingArgs),
    /// Show credits balance, grants, and expiry.
    #[command(name = "credits")]
    Credits(CreditsArgs),
}

/// Arguments for `mn cloud sync`.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Perform a dry run without sending data.
    #[arg(long)]
    pub dry_run: bool,

    /// Continuous sync: watch for changes and sync automatically.
    #[arg(long)]
    pub watch: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn cloud profile`.
#[derive(Debug, Args)]
pub struct ProfileArgs {
    #[command(subcommand)]
    pub action: ProfileCommand,
}

/// Profile subcommands.
#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Show current cloud profile.
    Show(ProfileShowArgs),
    /// Set cloud profile value.
    Set(ProfileSetArgs),
}

/// Arguments for `mn cloud profile show`.
#[derive(Debug, Args)]
pub struct ProfileShowArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn cloud profile set`.
#[derive(Debug, Args)]
pub struct ProfileSetArgs {
    /// Profile key to set (e.g., base_url).
    #[arg(value_name = "KEY")]
    pub key: String,

    /// Profile value to set.
    #[arg(value_name = "VALUE")]
    pub value: String,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn cloud billing`.
#[derive(Debug, Args)]
pub struct BillingArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn cloud credits`.
#[derive(Debug, Args)]
pub struct CreditsArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}
/// `mn update` — check for and apply updates.
#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Only check for updates, don't apply.
    #[arg(long)]
    pub check: bool,

    /// Release channel to check/use.
    #[arg(long, value_name = "CHANNEL")]
    pub channel: Option<String>,

    /// Specific version to install (e.g., "0.2.0").
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,

    /// Install without verifying the release signature (dangerous). The
    /// artifact checksum is still checked, because a corrupted download is a
    /// different failure from an unsigned one.
    #[arg(long)]
    pub no_verify: bool,
}

/// `mn install` — install memnoth (shows installer instructions).
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Force reinstall even if already installed.
    #[arg(long)]
    pub force: bool,
}

/// `mn uninstall` — uninstall memnoth.
#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Keep configuration and state files.
    #[arg(long)]
    pub keep_state: bool,
}

/// `mn auth …` — authentication commands.
#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub action: AuthCommand,
}

/// Authentication subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Start device authorization flow (RFC 8628).
    Login(AuthLoginArgs),
    /// Revoke current token and clear local credentials.
    Logout(AuthLogoutArgs),
    /// Show authentication status: who, org, scopes, expiry.
    Status(AuthStatusArgs),
    /// Print identity one-liner.
    Whoami(WhoamiArgs),
}

/// Arguments for `mn auth login`.
#[derive(Debug, Args)]
pub struct AuthLoginArgs {
    /// Never offer to open a browser: print the link and wait for approval.
    ///
    /// The default already opens nothing. `mn auth login` prints the link and
    /// then waits for a key — Enter opens it, `c` copies it, `q` cancels — so a
    /// browser never appears unasked. This flag is for scripts and headless
    /// machines, where there is nobody to press one.
    #[arg(long)]
    pub no_browser: bool,

    /// Use a pre-existing API token (CI/paste path), skipping device flow.
    #[arg(long, value_name = "TOKEN")]
    pub with_token: Option<String>,

    /// Client name to register (default: hostname-based).
    #[arg(long, value_name = "NAME")]
    pub client_name: Option<String>,

    /// Requested tier (optional).
    #[arg(long, value_name = "TIER")]
    pub tier: Option<String>,
}

/// Arguments for `mn auth logout`.
#[derive(Debug, Args)]
pub struct AuthLogoutArgs {}

/// Arguments for `mn auth status`.
#[derive(Debug, Args)]
pub struct AuthStatusArgs {}

/// Arguments for `mn auth whoami`.
#[derive(Debug, Args)]
pub struct WhoamiArgs {}

/// `memnoth vault …`.
#[derive(Debug, Args)]
pub struct VaultArgs {
    #[command(subcommand)]
    pub action: VaultCommand,
}

/// The temporal vault: facts that carry the date they became true.
#[derive(Debug, Subcommand)]
pub enum VaultCommand {
    /// Create the vault (idempotent; safe to run once per machine).
    Init,
    /// What the vault holds now: namespaces, counts, path, size.
    Status,
    /// What the vault believed at an instant.
    ///
    /// `--at` takes RFC 3339 (`2026-09-01T00:00:00Z`), a date
    /// (`2026-09-01`, midnight UTC), or epoch milliseconds. With `--name`
    /// the answer is one entity's state at that instant; without it, every
    /// entity live at that instant, which is the shape `vault asof` is for.
    ///
    /// The name is `asof`, not clap's derived `as-of`: the documented verb is
    /// one word, and a shell script that has to discover the difference is a
    /// broken promise rather than a naming preference.
    #[command(name = "asof")]
    AsOf {
        /// The instant to reconstruct. Defaults to now.
        #[arg(long, value_name = "WHEN")]
        at: Option<String>,
        /// Restrict the answer to one entity.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Maximum entities to print when no `--name` is given.
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// The full chain of versions for one entity, with the reasons they changed.
    History {
        /// Entity name.
        name: String,
    },
    /// Beliefs that were explicitly contradicted, both sources kept.
    ///
    /// `disputed` is the same verb under the name the team docs use; both
    /// spellings are supported so a script never has to discover the alias.
    #[command(visible_alias = "disputed")]
    Contradictions {
        /// Restrict to one entity. Without it, the vault-wide audit.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
    /// Record a fact, decision, lesson or correction.
    Remember {
        /// Entity the fact is about (created if absent).
        name: String,
        /// Observation kind: `fact`, `decision`, `lesson`, `gotcha`, `correction`.
        #[arg(long, default_value = "fact")]
        kind: String,
        /// Confidence, 0.0–1.0.
        #[arg(long)]
        confidence: Option<f64>,
        /// Entity kind when the entity has to be created.
        #[arg(long, default_value = "concept")]
        entity_kind: String,
        /// The fact itself.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Who is recording this. Defaults to `MEMNOTH_USER`, then the OS user.
        ///
        /// Stored as provenance `{"user":…,"agent":"cli"}` so a shared vault
        /// can answer "who decided this" rather than only "an agent did".
        #[arg(long, value_name = "USER")]
        author: Option<String>,
    },
    /// Propose a belief without asserting it (the unattended write path).
    ///
    /// A proposal is visible in `vault proposals` and to the team, but it is
    /// **not** a belief: recall, search and asof cannot see it, and nothing
    /// accepts it on your behalf. It expires after 14 days.
    Propose {
        /// Entity the belief is about (created when the proposal is accepted).
        name: String,
        /// Observation kind: `fact`, `decision`, `lesson`, `gotcha`, `correction`.
        #[arg(long, default_value = "fact")]
        kind: String,
        /// Confidence, 0.0–1.0.
        #[arg(long)]
        confidence: Option<f64>,
        /// Entity kind used if acceptance has to create the entity.
        #[arg(long, default_value = "concept")]
        entity_kind: String,
        /// The belief itself.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Who is proposing this. Defaults to `MEMNOTH_USER`, then the OS user.
        #[arg(long, value_name = "USER")]
        author: Option<String>,
    },
    /// Beliefs waiting for a decision; expired ones are shown, never accepted.
    Proposals {
        /// Only this status: `proposed`, `accepted`, `rejected` or `expired`.
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Accept a proposal: it becomes a belief with its proposer's attribution.
    Approve {
        /// Proposal id from `vault proposals`.
        id: i64,
        /// Who is accepting it. Defaults to `MEMNOTH_USER`, then the OS user.
        #[arg(long, value_name = "USER")]
        by: Option<String>,
    },
    /// Reject a proposal, keeping the reason.
    Reject {
        /// Proposal id from `vault proposals`.
        id: i64,
        /// Why it is rejected; required, because "no" without a reason is
        /// exactly the context a team loses.
        #[arg(long, required = true)]
        reason: String,
        /// Who is rejecting it. Defaults to `MEMNOTH_USER`, then the OS user.
        #[arg(long, value_name = "USER")]
        by: Option<String>,
    },
    /// Search the vault (FTS + graph, temporal windows understood).
    Recall {
        /// What to look for.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// How many entities to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Answer as of this instant instead of now.
        #[arg(long, value_name = "DATE")]
        at: Option<String>,
    },

    /// Retract a belief (mark as forgotten with a reason).
    Forget {
        /// Entity name to forget.
        name: String,
        /// Reason for forgetting.
        #[arg(long, required = true)]
        reason: String,
        /// Apply immediately (default: dry run shows what would be forgotten).
        #[arg(long)]
        apply: bool,
    },
    /// Correct/supersede a belief with new content.
    Correct {
        /// Entity name to correct.
        name: String,
        /// New entity name (optional rename).
        #[arg(long)]
        new_name: Option<String>,
        /// New summary (optional).
        #[arg(long)]
        summary: Option<String>,
        /// New confidence 0.0–1.0 (optional).
        #[arg(long)]
        confidence: Option<f64>,
        /// Reason for the correction.
        #[arg(long, required = true)]
        reason: String,
    },
    /// Merge two entities (dedupe), keeping one as canonical.
    Merge {
        /// Source entity name to merge (will be closed).
        from: String,
        /// Target entity name to merge into (kept as canonical).
        into: String,
        /// Reason for the merge.
        #[arg(long, required = true)]
        reason: String,
    },
    /// Show near-duplicate entity candidates.
    Duplicates {
        /// Check as of this instant instead of now.
        #[arg(long, value_name = "WHEN")]
        at: Option<String>,
        /// Show JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show most-referenced entities (top by reference count).
    Top {
        /// How many to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Filter by entity kind.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
        /// Check as of this instant instead of now.
        #[arg(long, value_name = "WHEN")]
        at: Option<String>,
    },
    /// Chronological feed of vault activity.
    Timeline {
        /// How many entries to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Show from this instant backwards.
        #[arg(long, value_name = "WHEN")]
        at: Option<String>,
        /// Filter by entity kind.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
        /// Include observations in timeline.
        #[arg(long)]
        include_observations: bool,
    },
    /// Explain why an entity is believed (sources, observations).
    Explain {
        /// Entity name.
        name: String,
        /// Check as of this instant instead of now.
        #[arg(long, value_name = "WHEN")]
        at: Option<String>,
    },
    /// Show entity relations (graph neighborhood).
    Graph {
        /// Entity name.
        name: String,
        /// How many hops to traverse.
        #[arg(long, default_value_t = 2)]
        depth: usize,
        /// Check as of this instant instead of now.
        #[arg(long, value_name = "WHEN")]
        at: Option<String>,
        /// Show JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Create a relation between two entities.
    Link {
        /// Source entity name.
        source: String,
        /// Target entity name.
        target: String,
        /// Relation kind (e.g., depends_on, implements, uses).
        kind: String,
        /// Confidence 0.0–1.0.
        #[arg(long, default_value_t = 1.0)]
        confidence: f64,
        /// Reason for linking.
        #[arg(long, required = true)]
        reason: String,
    },
    /// Remove a relation between two entities.
    Unlink {
        /// Source entity name.
        source: String,
        /// Target entity name.
        target: String,
        /// Relation kind to remove.
        kind: String,
        /// Reason for unlinking.
        #[arg(long, required = true)]
        reason: String,
    },
    /// Search with semantic embeddings.
    Search {
        /// Query text.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// How many entities to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Use semantic (embedding) search.
        #[arg(long)]
        semantic: bool,
        /// Answer as of this instant instead of now.
        #[arg(long, value_name = "WHEN")]
        at: Option<String>,
    },
    /// Export vault to JSON or Markdown.
    Export {
        /// Output file (stdout if omitted).
        #[arg(short, long, value_name = "FILE")]
        output: Option<String>,
        /// Export format: json, markdown.
        #[arg(long, default_value = "json")]
        format: String,
        /// Include superseded entities.
        #[arg(long)]
        include_history: bool,
        /// Filter by entity kind.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
    },
    /// Import vault from JSON or Markdown.
    Import {
        /// Input file (stdin if omitted).
        #[arg(short, long, value_name = "FILE")]
        input: Option<String>,
        /// Import format: json, markdown.
        #[arg(long, default_value = "json")]
        format: String,
        /// Dry run: validate without writing.
        #[arg(long)]
        dry_run: bool,
        /// Conflict strategy: skip, overwrite, merge.
        #[arg(long, default_value = "skip")]
        on_conflict: String,
    },
    /// Garbage collect: close stale observations.
    Gc {
        /// Observations to keep per entity (newest).
        #[arg(long, default_value_t = 20)]
        keep: usize,
        /// Apply changes (default: dry run).
        #[arg(long)]
        apply: bool,
    },
    /// Prune superseded entities beyond retention.
    Prune {
        /// Keep entities modified within this many days.
        #[arg(long, default_value_t = 365)]
        keep_days: u32,
        /// Apply changes (default: dry run).
        #[arg(long)]
        apply: bool,
    },
    /// Rebuild search indexes (FTS, embeddings).
    Reindex {
        /// Rebuild embeddings.
        #[arg(long)]
        embeddings: bool,
        /// Full rebuild (drop and recreate).
        #[arg(long)]
        full: bool,
    },
    /// Vault statistics (counts, growth, size).
    Stats {
        /// Output JSON.
        #[arg(long)]
        json: bool,
        /// Show growth over time (buckets).
        #[arg(long)]
        growth: bool,
    },
    /// Create a vault snapshot backup.
    Backup {
        /// Output file.
        #[arg(short, long, value_name = "FILE")]
        output: String,
        /// Include WAL/shm files for point-in-time recovery.
        #[arg(long)]
        include_wal: bool,
        /// Compress backup (gzip).
        #[arg(long)]
        compress: bool,
    },
    /// Restore vault from backup.
    Restore {
        /// Backup file.
        #[arg(short, long, value_name = "FILE")]
        input: String,
        /// Verify integrity before restore.
        #[arg(long, default_value_t = true)]
        verify: bool,
        /// Force restore even if vault exists.
        #[arg(long)]
        force: bool,
    },
    /// Audit trail: chronological feed of vault activity with author, timestamp, action.
    Audit {
        /// How many entries to show (newest first).
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Filter by entity kind.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
        /// Show JSON output.
        #[arg(long)]
        json: bool,
    },
}

/// `memnoth kg …`.
#[derive(Debug, Args)]
pub struct KgArgs {
    #[command(subcommand)]
    pub action: KgCommand,
}

/// The code graph.
#[derive(Debug, Subcommand)]
pub enum KgCommand {
    /// Build or update the graph from the workspace.
    Index {
        /// Discard the previous graph and rebuild from scratch.
        #[arg(long)]
        full: bool,
        /// Structure only: skip embeddings (search stays lexical until a later run).
        #[arg(long)]
        no_embed: bool,
    },
    /// What was built, when, and from where.
    Status,
    /// What depends on a symbol: definitions, references, tests, blast radius.
    Impact {
        /// Symbol name (`memnoth_engine::MemoryEngine`, `MemoryEngine`, or `open_vault`).
        symbol: String,
        /// How many hops of references to follow.
        ///
        /// 1 is "who mentions this symbol". 2 adds a hop through symbols
        /// declared in those files, which is a weak signal: on a real component
        /// it turned 2 mentions into 380 rows. Ask for it deliberately.
        #[arg(long, default_value_t = 1)]
        depth: u32,
    },
    /// Find symbols by name.
    Search {
        /// Name fragment.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Maximum symbols.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Explore a symbol: definition, snippet, references, callers, callees.
    Explore {
        /// Symbol name to explore.
        symbol: String,
    },
    /// Show references to a symbol.
    Refs {
        /// Symbol name.
        symbol: String,
        /// Maximum references to show.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Show callers of a symbol (functions that call it).
    Callers {
        /// Symbol name.
        symbol: String,
        /// Maximum callers to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show callees of a symbol (functions it calls).
    Callees {
        /// Symbol name.
        symbol: String,
        /// Maximum callees to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show definition of a symbol with snippet.
    Def {
        /// Symbol name.
        symbol: String,
    },
    /// Show impact of unstaged/staged changes.
    DiffImpact {
        /// Show impact of staged changes instead of unstaged.
        #[arg(long)]
        staged: bool,
    },
    /// Shortest call path between two symbols.
    Callpath {
        /// Source symbol.
        from: String,
        /// Target symbol.
        to: String,
    },
    /// Structural outline of a file.
    Outline {
        /// File path.
        file: String,
    },
    /// Symbol tree for a directory.
    Tree {
        /// Directory path.
        dir: String,
    },
    /// Graph-aware diff review.
    Review {
        /// Review staged changes instead of unstaged.
        #[arg(long)]
        staged: bool,
        /// Review what this branch adds on top of a revision (`main`, a tag, a
        /// sha) instead of the working tree. Refused together with `--staged`:
        /// the two ask different questions.
        #[arg(long, value_name = "REV")]
        base: Option<String>,
    },
    /// Tests that cover a symbol.
    TestsFor {
        /// Symbol name.
        symbol: String,
    },
    /// CODEOWNERS + git blame for a path.
    Owners {
        /// File or directory path.
        path: String,
    },
    /// Unreferenced exports (dead code).
    DeadCode,
    /// Import cycles.
    Cycles,
    /// Churn × complexity hotspots.
    Hotspots,
    /// Unused public exports (API hygiene).
    UnusedExports,
    /// Import graph for a file.
    Deps {
        /// File path.
        file: String,
    },
    /// Watch for changes and reindex incrementally.
    Watch,
    /// Graph DSL query.
    Query {
        /// Query expression.
        expr: String,
    },
    /// Local HTTP/JSON API server.
    Serve {
        /// Port to listen on.
        #[arg(long, default_value_t = 3030)]
        port: u16,
    },
}

/// `memnoth mcp …`.
#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: McpCommand,
}

/// The MCP server and the clients that talk to it.
#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Serve the graph over MCP (stdio by default, HTTP with `--http`).
    Serve {
        /// Serve over loopback HTTP instead of stdio.
        #[arg(long)]
        http: bool,
        /// Vault to serve. Defaults to this CLI's resolved vault.
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Tool profile: micro, memory, coord, code, full, agent.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
    },
    /// What is registered, and where — profile, tool counts, client configs.
    Doctor,
    /// Write a client's MCP config for this server.
    Add {
        /// Clients to write: claude, cursor, codex, zed. Comma-separated, or
        /// repeat the flag. Omit to detect every client on this machine.
        #[arg(long, value_delimiter = ',', value_name = "CLIENT")]
        clients: Vec<String>,
        /// Tool profile to register. Defaults to `full`.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
        /// Server endpoint. Defaults to the configured URL.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Bearer token for the client's header. Defaults to `$MEMNOTH_MCP_TOKEN`.
        /// Never written to memnoth's own config.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
        /// Show what would be written without touching a file.
        #[arg(long)]
        dry_run: bool,
    },
    /// Call one MCP tool without a transport in front of it.
    Call {
        /// Tool name (`kg_status`, `vault_asof`, …).
        tool: String,
        /// Arguments as a JSON object. Defaults to `{}`.
        #[arg(long, value_name = "JSON", default_value = "{}")]
        args: String,
        /// Profile that gates the call.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
    },
    /// Run the server's own initialize/tools/list/resources/list probe.
    Selftest,
    /// Remove a client's MCP registration.
    Remove {
        /// Clients to remove: claude, cursor, codex, zed. Comma-separated, or
        /// repeat the flag. Omit to remove from every detected client.
        #[arg(long, value_delimiter = ',', value_name = "CLIENT")]
        clients: Vec<String>,
        /// Also remove the token from the client's config.
        #[arg(long)]
        token: bool,
        /// Show what would be removed without touching a file.
        #[arg(long)]
        dry_run: bool,
    },
    /// List all registered MCP clients.
    List,
    /// Run tools/list and a real call to verify the server works.
    Test {
        /// Tool to call for the real test (default: vault_status).
        #[arg(long, value_name = "TOOL")]
        tool: Option<String>,
        /// Profile to use for the test.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
    },
    /// Mint an MCP bearer token from the cloud for this account.
    Token {
        /// Output as JSON.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Show MCP server logs (agent events, lifecycle).
    Logs {
        /// Session to show logs for (default: all sessions).
        #[arg(long, value_name = "SESSION")]
        session: Option<String>,
        /// Number of events to show from the end.
        #[arg(long, value_name = "N", default_value = "50")]
        tail: usize,
        /// Output as JSON.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Manage MCP tool profile.
    Profile {
        #[command(subcommand)]
        action: McpProfileCommand,
    },
    /// Show what tools each profile advertises.
    Exposes {
        /// Output as JSON.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Restart the MCP server.
    Restart {
        /// Server name (for HTTP servers).
        #[arg(long, value_name = "NAME")]
        server: Option<String>,
        /// Force restart even if not running.
        #[arg(long)]
        force: bool,
    },
}

/// MCP profile subcommands.
#[derive(Debug, Subcommand)]
pub enum McpProfileCommand {
    /// Show the current active profile.
    Show {
        /// Output as JSON.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Set the active MCP profile.
    Set {
        /// Profile name: micro, memory, coord, code, full, agent.
        profile: String,
    },
}
/// `mn automation ...` — automation and CI integration.
#[derive(Debug, Args)]
pub struct AutomationArgs {
    #[command(subcommand)]
    pub action: AutomationCommand,
}

/// Automation subcommands.
#[derive(Debug, Subcommand)]
pub enum AutomationCommand {
    /// Install git hooks (pre-commit for kg index).
    #[command(name = "hooks")]
    Hook(HookArgs),
    /// Generate GitHub Actions workflow.
    #[command(name = "action")]
    Action(ActionArgs),
    /// CI environment detection and indexing.
    #[command(name = "ci")]
    Ci(CiArgs),
}

/// `mn hooks ...` — git hooks management.
#[derive(Debug, Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub action: HookCommand,
}

/// Hook subcommands.
#[derive(Debug, Subcommand)]
pub enum HookCommand {
    /// Install pre-commit hook for kg index.
    #[command(name = "install")]
    Install(HookInstallArgs),
}

/// Arguments for `mn hooks install`.
#[derive(Debug, Args)]
pub struct HookInstallArgs {
    /// Overwrite existing hook.
    #[arg(long)]
    pub force: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn action ...` — GitHub Actions workflow generation.
#[derive(Debug, Args)]
pub struct ActionArgs {
    #[command(subcommand)]
    pub action: ActionCommand,
}

/// Action subcommands.
#[derive(Debug, Subcommand)]
pub enum ActionCommand {
    /// Generate GitHub Actions workflow for CI.
    #[command(name = "init")]
    Init(ActionInitArgs),
}

/// Arguments for `mn action init`.
#[derive(Debug, Args)]
pub struct ActionInitArgs {
    /// Project name (defaults to directory name).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Include kg index step in workflow.
    #[arg(long, default_value = "true")]
    pub kg_index: bool,

    /// Include cloud sync step in workflow.
    #[arg(long, default_value = "true")]
    pub sync: bool,

    /// Overwrite existing workflow.
    #[arg(long)]
    pub force: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn ci ...` — CI environment commands.
#[derive(Debug, Args)]
pub struct CiArgs {
    #[command(subcommand)]
    pub action: CiCommand,
}

/// CI subcommands.
#[derive(Debug, Subcommand)]
pub enum CiCommand {
    /// Detect CI environment.
    #[command(name = "detect")]
    Detect(CiDetectArgs),
    /// Index code graph in CI.
    #[command(name = "index")]
    Index(CiIndexArgs),
}

/// Arguments for `mn ci detect`.
#[derive(Debug, Args)]
pub struct CiDetectArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn ci index`.
#[derive(Debug, Args)]
pub struct CiIndexArgs {
    /// Full re-index (ignore incremental).
    #[arg(long)]
    pub full: bool,

    /// Skip embedding generation.
    #[arg(long)]
    pub no_embed: bool,

    /// Also sync to cloud after indexing.
    #[arg(long)]
    pub sync: bool,

    /// Force run even outside CI.
    #[arg(long)]
    pub force: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn watch` — watch for file changes and incrementally update the code graph and vault.
#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Port to serve file watch events (optional, for editor integration).
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// Debounce interval in milliseconds.
    #[arg(long, default_value_t = 500)]
    pub debounce: u64,

    /// Also watch vault files for changes.
    ///
    /// Not plain `--vault`: that is the global vault *path*, and two `vault`
    /// arguments in one command tree made clap panic on every `memnoth watch`
    /// before it ran a line.
    #[arg(long = "vault-changes")]
    pub vault_changes: bool,

    /// Run once and exit (no continuous watch).
    #[arg(long)]
    pub once: bool,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn serve` — serve a local HTTP API for editor integration.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Port to listen on.
    #[arg(long, short = 'p', default_value_t = 3030)]
    pub port: u16,

    /// Host to bind to (default: 127.0.0.1).
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Enable CORS for all origins.
    #[arg(long)]
    pub cors: bool,

    /// Bearer token every request must present (default: `MEMNOTH_SERVE_TOKEN`,
    /// then `~/.memnoth/serve.token`, created on first use).
    #[arg(long, value_name = "SECRET")]
    pub token: Option<String>,

    /// Path to serve the kg index from (default: workspace .memnoth/graph.db).
    #[arg(long, value_name = "PATH")]
    pub graph_path: Option<String>,

    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn open` — open a deep-link to the memnoth cloud console.
#[derive(Debug, Args)]
pub struct OpenArgs {
    /// Console page to open: dashboard, projects, sessions, settings, usage, org, teams, api-keys, workspaces.
    #[arg(value_name = "PAGE", default_value = "dashboard")]
    pub page: String,

    /// Project ID (for projects page).
    #[arg(long, value_name = "ID")]
    pub project_id: Option<String>,

    /// Session ID (for sessions page).
    #[arg(long, value_name = "ID")]
    pub session_id: Option<String>,

    /// Don't open the browser automatically.
    #[arg(long)]
    pub no_browser: bool,

    /// Output as JSON (prints URL instead of opening).
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn worktree ...` — Git worktree management.
#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    pub action: WorktreeCommand,
}

/// Worktree subcommands.
#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// Create a new worktree.
    #[command(name = "create")]
    Create(WorktreeCreateArgs),
    /// List all worktrees.
    #[command(name = "list")]
    List(WorktreeListArgs),
    /// Remove a worktree.
    #[command(name = "clean")]
    Clean(WorktreeCleanArgs),
    /// Diff between worktrees.
    #[command(name = "diff")]
    Diff(WorktreeDiffArgs),
}

/// Arguments for `mn worktree create`.
#[derive(Debug, Args)]
pub struct WorktreeCreateArgs {
    /// Worktree name (directory name under .memnoth/worktrees/).
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Branch to create/check out in the worktree.
    #[arg(long, short = 'b', value_name = "BRANCH")]
    pub branch: Option<String>,
    /// Base branch/commit to create from (default: HEAD).
    #[arg(long, value_name = "BASE")]
    pub base: Option<String>,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn worktree list`.
#[derive(Debug, Args)]
pub struct WorktreeListArgs {
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn worktree clean`.
#[derive(Debug, Args)]
pub struct WorktreeCleanArgs {
    /// Worktree name to remove.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Force removal even with uncommitted changes.
    #[arg(long, short = 'f')]
    pub force: bool,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Arguments for `mn worktree diff`.
#[derive(Debug, Args)]
pub struct WorktreeDiffArgs {
    /// Source worktree/branch name.
    #[arg(value_name = "FROM")]
    pub from: String,
    /// Target worktree/branch name (default: main worktree).
    #[arg(value_name = "TO")]
    pub to: Option<String>,
    /// Paths to diff (default: all).
    #[arg(value_name = "PATHS")]
    pub paths: Vec<String>,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Skill subcommands.
#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    /// Add a new skill from a SKILL.md file.
    #[command(name = "add")]
    Add(SkillAddArgs),
    /// List all installed skills.
    #[command(name = "list")]
    List(SkillListArgs),
    /// Remove a skill by name.
    #[command(name = "remove")]
    Remove(SkillRemoveArgs),
    /// Run a skill with arguments.
    #[command(name = "run")]
    Run(SkillRunArgs),
    /// Edit a skill's SKILL.md file.
    #[command(name = "edit")]
    Edit(SkillEditArgs),
}

/// Arguments for `mn skill add`.
#[derive(Debug, Args)]
pub struct SkillAddArgs {
    /// Path to the SKILL.md file to add.
    #[arg(value_name = "PATH")]
    pub path: String,
    /// Override the skill name (uses frontmatter name by default).
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
}

/// Arguments for `mn skill list`.
#[derive(Debug, Args)]
pub struct SkillListArgs {
    /// Show detailed information for each skill.
    #[arg(long, short)]
    pub detail: bool,
    /// Filter by tag.
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `mn skill remove`.
#[derive(Debug, Args)]
pub struct SkillRemoveArgs {
    /// Name of the skill to remove.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Force removal without confirmation.
    #[arg(long, short)]
    pub force: bool,
}

/// Arguments for `mn skill run`.
#[derive(Debug, Args)]
pub struct SkillRunArgs {
    /// Name of the skill to run.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Arguments to pass to the skill (KEY=VALUE format).
    #[arg(value_name = "KEY=VALUE", trailing_var_arg = true)]
    pub args: Vec<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `mn skill edit`.
#[derive(Debug, Args)]
pub struct SkillEditArgs {
    /// Name of the skill to edit.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Editor to use (defaults to $EDITOR or vi).
    #[arg(long, short, value_name = "EDITOR")]
    pub editor: Option<String>,
}

/// `mn skill ...` — Skill management.
#[derive(Debug, Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub action: SkillCommand,
}

/// `mn batch <task>` — fan out a task across isolated worktrees.
#[derive(Debug, Args)]
pub struct BatchCmdArgs {
    /// Task description.
    #[arg(value_name = "TASK")]
    pub task: String,
    /// Number of workers to spawn.
    #[arg(long, short = 'w', value_name = "N", default_value = "4")]
    pub workers: usize,
    /// Command to run in each worker (e.g., "cargo test").
    #[arg(long, value_name = "CMD", value_delimiter = ' ', num_args = 1..)]
    pub command: Vec<String>,
    /// Path glob for claims (e.g., "src/**/*.rs").
    #[arg(long, value_name = "GLOB")]
    pub claim_glob: Option<String>,
    /// Claim TTL in seconds.
    #[arg(long, value_name = "SECS", default_value = "1800")]
    pub claim_ttl: u64,
    /// Working directory relative to worktree root.
    #[arg(long, value_name = "DIR")]
    pub workdir: Option<String>,
    /// Output format: text, json.
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub format: String,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn parallel <cmd> <targets>` — run a command in parallel across worktree targets.
#[derive(Debug, Args)]
pub struct ParallelArgs {
    /// Command to execute.
    #[arg(value_name = "CMD")]
    pub command: String,
    /// Target worktree names or branches (empty = all non-main worktrees).
    #[arg(value_name = "TARGETS")]
    pub targets: Vec<String>,
    /// Arguments to pass to the command.
    #[arg(value_name = "ARGS", last = true)]
    pub args: Vec<String>,
    /// Maximum concurrent executions.
    #[arg(long, short = 'j', value_name = "N", default_value = "4")]
    pub max_concurrent: usize,
    /// Output as JSON.
    #[arg(long, global = true)]
    pub json: bool,
}

/// `mn docs` — generate Markdown documentation from the command registry.
#[derive(Debug, Args)]
pub struct DocsArgs {
    /// Output file path. If not provided, writes to stdout.
    #[arg(long, short, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

/// `mn feedback ...` — send feedback to memnoth cloud.
#[derive(Debug, Args)]
pub struct FeedbackArgs {
    #[command(subcommand)]
    pub action: FeedbackCommand,
}

/// Feedback subcommands.
#[derive(Debug, Subcommand)]
pub enum FeedbackCommand {
    /// Send feedback to the memnoth team.
    Send(FeedbackSendArgs),
}

/// Arguments for `mn feedback send <message>`.
#[derive(Debug, Args)]
pub struct FeedbackSendArgs {
    /// Feedback message.
    #[arg(value_name = "MESSAGE")]
    pub message: String,
    /// Feedback category.
    #[arg(long, value_name = "CATEGORY", default_value = "general")]
    pub category: String,
    /// Include session/vault context with feedback.
    #[arg(long)]
    pub include_context: bool,
}
// `mn set-theme` and `mn set-screen-reader` take the TUI's own enums, so that
// the CLI and the `/theme` slash command cannot disagree about the spelling.
// clap needs a variant type to be `Args`, and these two are the only enums in
// the crate that are neither — so the argument is defined once, here, and the
// parse rules stay in `tui::app` where the rest of the product reads them.
impl clap::Args for crate::tui::app::Theme {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        cmd.arg(
            clap::Arg::new("theme")
                .value_name("THEME")
                .required(true)
                .help("default, high-contrast or no-color"),
        )
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        Self::augment_args(cmd)
    }
}

impl clap::FromArgMatches for crate::tui::app::Theme {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let value = matches.get_one::<String>("theme").ok_or_else(|| {
            clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                "a theme name is required".to_string(),
            )
        })?;
        Self::parse(value).map_err(|message| {
            clap::Error::raw(clap::error::ErrorKind::InvalidValue, message.to_string())
        })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = <Self as clap::FromArgMatches>::from_arg_matches(matches)?;
        Ok(())
    }
}

impl clap::Args for crate::tui::app::ScreenReaderMode {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        cmd.arg(
            clap::Arg::new("mode")
                .value_name("MODE")
                .required(true)
                .help("off or plain"),
        )
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        Self::augment_args(cmd)
    }
}

impl clap::FromArgMatches for crate::tui::app::ScreenReaderMode {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let value = matches.get_one::<String>("mode").ok_or_else(|| {
            clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                "a screen reader mode is required".to_string(),
            )
        })?;
        Self::parse(value).map_err(|message| {
            clap::Error::raw(clap::error::ErrorKind::InvalidValue, message.to_string())
        })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = <Self as clap::FromArgMatches>::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Cli {
    /// The session name this invocation refers to.
    pub fn session_name(&self) -> String {
        match &self.resume {
            Some(name) => name.clone(),
            None => match &self.verb {
                Some(Verb::Session(args)) => match &args.action {
                    SessionCommand::Start(start) => {
                        start.name.clone().unwrap_or_else(|| "session".into())
                    }
                    SessionCommand::Resume(resume) => resume.name.clone(),
                    SessionCommand::Status(status) => {
                        status.name.clone().unwrap_or_else(|| "session".into())
                    }
                    SessionCommand::Stop(stop) => {
                        stop.name.clone().unwrap_or_else(|| "session".into())
                    }
                    // List, Effort, Replay, Attach, Handoff and Exec all act on
                    // `--resume` or on the default session, never on a name of
                    // their own.
                    _ => "session".into(),
                },
                _ => "session".into(),
            },
        }
    }
}
