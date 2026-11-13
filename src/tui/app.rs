//! Session state and the line parser — the pure half of the TUI.
//!
//! Nothing here touches a terminal, which is what makes it testable: the parser
//! turns one typed line into the same [`Verb`] the CLI exposes, the transcript
//! wraps and scrolls deterministically, and only [`crate::tui`] knows what a
//! `Frame` is.
//!
//! The parser is deliberately small. It is not a shell: it splits on
//! whitespace, understands `--flag value` / `--flag=value`, and refuses
//! everything else by name. A line that does not parse becomes an error entry in
//! the transcript, never a panic and never a silent no-op.
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::cli::{
    ActionArgs, ActionCommand, ActionInitArgs, AgentListArgs, AgentResultArgs, AgentRunArgs,
    AgentUnpackArgs, AgentsArgs, AgentsCommand, AskArgs, AuditArgs, AuditCommand, AuditExportArgs,
    AuditShowArgs, AuditVerifyArgs, AuthArgs, AuthCommand, AuthLoginArgs, AuthLogoutArgs,
    AuthStatusArgs, AutomationArgs, AutomationCommand, BatchCmdArgs, BillingCommand,
    BillingCommands, BillingInvoicesArgs, BillingLimitsArgs, BillingPlanArgs, BtwArgs,
    CheckpointArgs, CiArgs, CiCommand, CiDetectArgs, CiIndexArgs, CloudArgs, CloudCommand,
    CompactArgs, CompletionArgs, CompletionCommand, CompletionInstallArgs, CompletionShowArgs,
    CompletionUninstallArgs, ConfigArgs, ConfigCommand, ConfigEditArgs, ConfigExplainArgs,
    ConfigGetArgs, ConfigListArgs, ConfigSetArgs, CreditsArgs, DelegateArgs, DocsArgs, DoctorArgs,
    DriftArgs, ExportArgs, GoalArgs, HandoffTopArgs, HookArgs, HookCommand, HookInstallArgs,
    ImpactArgs, InstallArgs, KeymapArgs, KeysArgs, KeysCommand, KeysListArgs, KeysRevokeAllArgs,
    KeysRevokeArgs, KgArgs, KgCommand, LineageArgs, ListArgs, McpArgs, McpCommand, MergeArgs,
    OpenArgs, OrgAddMemberArgs, OrgArgs, OrgCommand, OrgInvitesArgs, OrgMembersArgs,
    OrgRemoveMemberArgs, OrgRenameArgs, OrgSetRoleArgs, OrgShowArgs, OrgWithdrawArgs, ParallelArgs,
    PlanArgs, PrivacyArgs, PrivacyCommand, PrivacyModeArgs, PrivacyOffArgs, PrivacyOnArgs,
    PrivacyShowDataArgs, PrivacyStatusArgs, ProfileArgs, ProfileCommand, ProfileSetArgs,
    ProfileShowArgs, ProjectsArgs, ProjectsCommand, ProjectsCreateArgs, ProjectsDeleteArgs,
    ProjectsFileArgs, ProjectsListArgs, ProjectsRenameArgs, ProjectsUnassignedArgs,
    ProjectsUseArgs, ReceiptArgs, ReceiptCommand, ReceiptShowArgs, ReceiptVerifyArgs,
    ReceiptWriteArgs, RenameArgs, ResumeArgs, SchemaArgs, ServeArgs, SessionArgs, SessionCommand,
    SetupArgs, SimulateArgs, SkillAddArgs, SkillArgs, SkillCommand, SkillEditArgs, SkillListArgs,
    SkillRemoveArgs, SkillRunArgs, StartArgs, StatusArgs, StopArgs, SyncArgs, TeamAuditArgs,
    TeamClaimArgs, TeamHandoffArgs, TeamInboxArgs, TeamLockArgs, TeamPresenceArgs, TeamReleaseArgs,
    TeamReviewArgs, TeamTasksArgs, TeamsArgs, TeamsCommand, TeamsCreateArgs, TeamsDisputedArgs,
    TeamsEnforceArgs, TeamsImportExportArgs, TeamsJoinArgs, TeamsListArgs, TeamsMembersArgs,
    TeamsNamespaceArgs, TeamsProvenanceArgs, TelemetryArgs, TelemetryCommand, TelemetryOffArgs,
    TelemetryOnArgs, TelemetryStatusArgs, UninstallArgs, VaultArgs, VaultCommand, Verb,
    VersionArgs, WatchArgs, WhoamiArgs, WhyArgs, WorktreeArgs, WorktreeCleanArgs, WorktreeCommand,
    WorktreeCreateArgs, WorktreeDiffArgs, WorktreeListArgs,
};
use crate::tui::jobs::{JobStatus, Jobs};
use crate::tui::panes::{DiffPane, TasksPane, TranscriptPane};
use memnoth_sandbox::rules::RuleDecision;
use memnoth_session::{Role, Session};
use serde_json::Value;
use walkdir::WalkDir;

/// Theme variants for accessibility
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Default,
    HighContrast,
    NoColor,
}

/// Screen reader mode for accessibility
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenReaderMode {
    #[default]
    Off,
    Plain, // Plain text output for mn ask
}

impl Theme {
    /// The spelling `mn set-theme` and `[ui] theme` accept, and the one saved.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::HighContrast => "high-contrast",
            Self::NoColor => "no-color",
        }
    }

    /// Parse a theme name, naming the valid set on a miss.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "default" => Ok(Self::Default),
            "high-contrast" | "highcontrast" => Ok(Self::HighContrast),
            "no-color" | "nocolor" => Ok(Self::NoColor),
            other => Err(format!(
                "unknown theme `{other}`; use default, high-contrast or no-color"
            )),
        }
    }
}

impl ScreenReaderMode {
    /// The spelling `mn set-screen-reader` and `[ui] screen_reader` accept.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Plain => "plain",
        }
    }

    /// Parse a screen-reader mode, naming the valid set on a miss.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "off" => Ok(Self::Off),
            "plain" => Ok(Self::Plain),
            other => Err(format!(
                "unknown screen reader mode `{other}`; use off or plain"
            )),
        }
    }
}

/// Fuzzy path completion state for @file references.
#[derive(Debug, Default)]
pub struct CompletionState {
    /// The @file prefix being completed (e.g., "@src/").
    pub prefix: String,
    /// Matched paths for the current prefix.
    pub matches: Vec<PathBuf>,
    /// Index of the currently selected match.
    pub index: usize,
    /// Whether completion is active (Tab was pressed).
    pub active: bool,
    /// Cursor position when completion started.
    pub cursor_at_start: usize,
    /// Workspace root for finding matches.
    pub workspace_root: PathBuf,
}
/// A shell command (`!cmd`) pending sandbox policy decision.
#[derive(Debug, Clone)]
pub struct PendingShell {
    /// The full command line after `!`.
    pub command: String,
    /// The sandbox policy decision.
    pub decision: RuleDecision,
    /// Whether yes starts a background job (`/tasks run`) rather than running
    /// it in the foreground (`!cmd`). One box, two destinations: a person who
    /// approved a command should not be asked again because it moved to the
    /// rail.
    pub background: bool,
}

impl PendingShell {
    /// The text the confirmation box shows: what would run, why it is asking,
    /// and — when the policy flagged it — that it can destroy work. The keys
    /// are spelled out because this box is the only thing on screen.
    pub fn prompt(&self) -> String {
        let (reason, rule, destructive) = match &self.decision {
            RuleDecision::Ask {
                reason,
                rule,
                destructive,
                ..
            } => (reason.as_str(), rule.as_str(), *destructive),
            RuleDecision::Allow { rule } => ("allowed by policy", rule.as_str(), false),
            RuleDecision::Deny { reason, rule } => (reason.as_str(), rule.as_str(), true),
        };
        let mut text = String::new();
        if destructive {
            text.push_str("destructive — this can remove or overwrite work\n\n");
        }
        text.push_str("run this command?\n\n  ");
        text.push_str(&self.command);
        // The reason usually names the rule that fired; printing the id twice
        // ("rule builtin:68: …  [rule: builtin:68]") reads like a defect.
        text.push_str(&format!("\n\n{reason}"));
        if !reason.contains(rule) {
            text.push_str(&format!("  [rule: {rule}]"));
        }
        if self.background {
            text.push_str("\n\nthis starts a background job (/tasks logs to read it)");
        }
        text.push_str("\n\ny run · n or esc cancel");
        text
    }
}

/// Observation kinds `remember` accepts, mirrored from the vault command so a
/// `/vault remember x --decision …` line reads naturally in the TUI.
const OBSERVATION_KINDS: [&str; 5] = ["fact", "decision", "lesson", "gotcha", "correction"];

/// What a typed line means.
///
/// What `/tasks` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TasksCommand {
    /// No argument: show or hide the rail.
    Show,
    /// `/tasks run <command>`: start a background process.
    Run(String),
    /// `/tasks status`: one line per job.
    Status,
    /// `/tasks logs [id]`: print a job's tail, or the selected job's.
    Logs(Option<String>),
    /// `/tasks cancel <id>`: stop a running job.
    Cancel(String),
}

/// `Verb` deliberately has no `PartialEq` (it is a clap surface, not a value
/// type), so this enum does not derive one either; tests match on the variant
/// they expect.
#[derive(Debug)]
pub enum Parsed {
    /// Nothing to do (blank line).
    Empty,
    /// `/new [name]`: a fresh conversation, in this process.
    NewChat(Option<String>),
    /// Leave the session.
    Quit,
    /// Open the help overlay (the command list and the keybindings).
    Help,
    /// Clear the transcript, and with it show the brief again.
    Clear,
    /// `/home`: the brief against the existing transcript.
    Home,
    /// `/sessions` (alias `/resume`): the picker — every conversation on disk,
    /// with a title and a preview, and the one you pick opens here.
    Sessions,
    /// `/models` (and its `/model` alias): browse, search, and switch models.
    ModelsOverlay,
    /// `/tasks`: the background-task rail and the jobs behind it.
    Tasks(TasksCommand),
    /// `/connect`: open provider connections overlay.
    ConnectOverlay,
    /// A real verb, dispatched through [`crate::commands::execute`].
    Run(Verb),
    /// Plain text: a model turn when one is configured, the stores otherwise.
    Ask(Vec<String>),
    /// `/search`: the two stores, even when a model is configured.
    Search(Vec<String>),
    /// `!cmd`: shell passthrough with sandbox policy checking.
    Shell(String),
    /// `/init`: generate AGENTS.md.
    Init,
    /// `/memory`: the vault as a hub.
    Memory,
    /// `/feedback`: open feedback overlay.
    FeedbackOverlay,
    /// `/search` with no argument: open the search overlay.
    SearchOverlay,
    /// `/context`: open the context overlay.
    ContextOverlay,
    /// `/diff`: open the diff overlay.
    DiffOverlay,
    /// `/rewind`: open the checkpoint picker.
    RewindOverlay,
    /// `/checkpoints`: the rows `/checkpoint` wrote, and the one to restore.
    CheckpointsOverlay,
    /// `/palette`: the same command list as ctrl-k, opened by name.
    PaletteOverlay,
    /// `/onboarding`: the four-move capability card, replayed with the current
    /// state and without changing the first-run flag.
    Onboarding,
    /// `/commands`: the whole registry, grouped, with no row limit.
    CommandsOverlay,
    /// `/teams members` with no id: the id only exists in another command's
    /// output, so ask for it with the list instead of a uuid to copy.
    TeamsOverlay,
    /// `/keys create`: a form, because the secret is shown once and a session
    /// writes every answer to its transcript on disk.
    KeyCreate {
        name: Option<String>,
        scope: Option<String>,
        project: Option<String>,
        expires_in_days: Option<i64>,
    },
    /// `/org invite`: the same, for the link and the code.
    Invite {
        email: Option<String>,
        role: Option<String>,
    },
    /// A true statement that is not an error: e.g. a long-running server that
    /// belongs in its own terminal. Shown as an info entry, not a failure.
    Info(String),
    /// `/login`: start the cloud device flow on a worker thread.
    Login,
    /// `/logout`: forget this machine's cloud credentials.
    Logout,
    /// `/permissions`: open the permissions overlay.
    PermissionsOverlay,
    /// A line this parser could not read; the message is the transcript entry.
    /// A known command used wrongly: the string is the usage line.
    Usage(String),
    Unknown(String),
}

/// Who a transcript entry is from, which decides its glyph and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRole {
    /// What the person typed.
    User,
    /// What a verb answered.
    Answer,
    /// A failure, shown as the failure it is.
    Error,
    /// A note from the session itself (`/help`, `/sessions`).
    Info,
    /// A tool call the turn has started and not finished: a row with a caret.
    Tool,
    /// A tool call whose result landed. The same row, resolved in place.
    ToolDone,
    /// The model's reasoning for one turn, collapsed to a header row until
    /// expanded with `ctrl-r`. Never persisted: it is not part of the answer.
    Thinking,
}

/// What a home fact is worth: the colour, and whether the eye should go there.
///
/// This is the closed vocabulary of §11 of the design: a state is one of five
/// words, and `missing` and `stale` are the only ones that ask for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HomeState {
    /// Present and usable; drawn as a plain fact.
    #[default]
    Ready,
    /// Absent, so `next` carries the command that builds it. Yellow.
    Missing,
    /// Present but behind; yellow, because `next` can catch it up.
    Stale,
}

/// One label/value row of the brief: `vault   412 beliefs · 18 closed`.
#[derive(Debug, Clone)]
pub struct Fact {
    /// The word in the left column. Fixed width, so the values line up.
    pub label: &'static str,
    /// What is true right now, with units.
    pub value: String,
    /// Whether that truth is a problem.
    pub state: HomeState,
}

/// One row of `next`.
///
/// A step without a command is the invitation to type in the prompt. It is a
/// row in the brief but not an affordance: the cursor skips it, so nothing on
/// screen looks clickable and does nothing when you press enter.
#[derive(Debug, Clone)]
pub struct Step {
    /// The command, as it would be typed, with the slash.
    pub command: String,
    /// One clause about the answer it gives.
    pub description: String,
    /// Whether `enter` has something to run.
    pub runnable: bool,
}

/// The startup screen: what this session opened, what is missing, what to do
/// next — then out of the way.
///
/// It is structured rather than a block of text so the layout can decide what
/// each part looks like: the same values read as a wall of prose in a narrow
/// terminal and as a table when the columns line up.
#[derive(Debug, Clone, Default)]
pub struct Home {
    /// `solo · not signed in · session "payments" (new)`.
    pub identity: String,
    /// The fact grid, in reading order: what is here, what is not.
    pub facts: Vec<Fact>,
    /// The shortest path to something useful, most-needed first.
    pub steps: Vec<Step>,
    /// The last line of `next`: the invitation to type in the prompt, shown only
    /// when a model is configured. It is a row, not an affordance.
    pub invitation: Option<String>,
    /// How many turns the session file behind this brief already holds.
    ///
    /// The brief is drawn over history on every launch, so the count is what
    /// makes `esc` a specific promise — "show those" — instead of a key that
    /// might close the screen.
    pub hidden_turns: usize,
    /// The vault's own timeline, newest first, already aged (`2h ago`).
    pub recent: Vec<String>,
}

/// One entry in the transcript. Wrapped lines still belong to one entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Who it is from.
    pub role: EntryRole,
    /// The text, possibly multi-line.
    pub text: String,
    /// When the line was produced, in milliseconds since the Unix epoch.
    ///
    /// Recorded at the moment it happened — the turn's own `at_ms` when a
    /// session is resumed, the clock when the line is written. It is never
    /// stamped while drawing: a render-time clock would make every row read as
    /// the moment the screen last repainted, which is a fact about the terminal
    /// rather than about the conversation.
    pub at_ms: i64,
    /// Still streaming: the last visual line carries the caret glyph.
    pub live: bool,
    /// A thinking block's expanded state. Meaningless for other roles.
    pub expanded: bool,
    /// The turn's pace, stamped when the answer settles: `17s · 5.5 tps ·
    /// ↑24k ↓8`. Rendered as one dim line under the answer; never persisted,
    /// because the receipt already carries the same numbers in the file.
    pub stats: Option<String>,
}

/// The stats line for one settled turn, in the shape every coding agent
/// converged on: how long it took, how fast the answer was written, how much
/// context moved, what it cost, and how many tools it ran. Tokens read 812,
/// 1.8k, 128k — k is 1,000, never 1,024. The rate is end-to-end (output over
/// wall time), so a tool-bound turn reads as slow; that is the truth about
/// the turn, not a hidden detail.
pub fn turn_stats(
    elapsed_ms: i64,
    input: u64,
    output: u64,
    cost_micros: Option<u64>,
    tools: usize,
) -> Option<String> {
    if input == 0 && output == 0 {
        return None;
    }
    let elapsed = elapsed_ms.max(0) as f64 / 1000.0;
    let age = if elapsed < 10.0 {
        format!("{elapsed:.1}s")
    } else if elapsed < 60.0 {
        format!("{}s", elapsed.round() as u64)
    } else {
        format!(
            "{}m {:02}s",
            (elapsed / 60.0) as u64,
            (elapsed % 60.0) as u64
        )
    };
    let mut parts = vec![age];
    if output > 0 && elapsed > 0.0 {
        let rate = output as f64 / elapsed;
        if rate.fract() < 0.05 || rate >= 100.0 {
            parts.push(format!("{} tps", rate.round() as u64));
        } else {
            parts.push(format!("{rate:.1} tps"));
        }
    }
    fn short(tokens: u64) -> String {
        if tokens < 1_000 {
            tokens.to_string()
        } else if tokens < 1_000_000 {
            let kilos = tokens as f64 / 1000.0;
            if kilos.fract() < 0.05 {
                format!("{}k", kilos.round() as u64)
            } else {
                format!("{kilos:.1}k")
            }
        } else {
            let megas = tokens as f64 / 1_000_000.0;
            if megas.fract() < 0.05 {
                format!("{}M", megas.round() as u64)
            } else {
                format!("{megas:.1}M")
            }
        }
    }
    parts.push(format!("↑{} ↓{}", short(input), short(output)));
    // Order is the sentence the other agents read: how long, how fast, how
    // much. A fourth slot exists for one extra, and only one: cost beats
    // the tool count, because money is the number a person acts on and a
    // tool count is colour. More than that is a log line, not a pace line.
    if let Some(micros) = cost_micros.filter(|micros| *micros > 0) {
        let dollars = micros as f64 / 1_000_000.0;
        let cost = if dollars >= 1.0 {
            format!("${dollars:.2}")
        } else if dollars >= 0.1 {
            format!("${dollars:.3}")
        } else {
            format!("${dollars:.4}")
        };
        parts.push(cost);
    } else if tools > 0 {
        parts.push(format!(
            "{} {}",
            tools,
            if tools == 1 { "tool" } else { "tools" }
        ));
    }
    debug_assert!(parts.len() <= 4);
    Some(parts.join(" · "))
}
/// Something the header says needs a person, and the line that fixes it.
///
/// The pair travels together so no key has to guess: the header renders
/// `label · fix`, and `ctrl-g` runs `fix` through the prompt exactly as if it had
/// been typed. A label with no command would be a riddle, and a command with no
/// label would be a key that does something unexplained.
#[derive(Debug, Clone)]
pub struct Attention {
    /// What is wrong, e.g. `graph not indexed`.
    pub label: String,
    /// The command that resolves it, e.g. `/kg index`.
    pub fix: String,
}

/// A session the person asked to open, waiting for the loop that owns the file.
///
/// The screen may ask — `/new`, or a row in the sessions picker — but it cannot
/// perform: the `Session` and its single writer live in the run loop, and two
/// writers on one conversation is how a transcript forks. So the request parks
/// here and the loop takes it before drawing the next frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSwitch {
    /// A conversation that does not exist yet, under the name given.
    New(String),
    /// An existing conversation, by the name `--resume` would take.
    Open(String),
    /// Rename a conversation. The file name is the identity, so this can only be
    /// done by whoever holds the session it names.
    Rename { from: String, to: String },
}

/// The session screen's state.
#[derive(Debug)]
pub struct App {
    /// Session name (`--resume`, or `session`).
    pub name: String,
    /// Whether the file already existed when the session opened.
    pub resumed: bool,
    /// A session to open as soon as the loop can swap the file it records.
    pub pending_session: Option<SessionSwitch>,
    /// A line typed on the brief, waiting for the fresh conversation the loop
    /// opens for it. The brief can stand over a resumed file, and a question
    /// typed at it must not be filed into that file.
    pub pending_prompt: Option<String>,
    /// What the startup screen shows, or `None` for a plain empty transcript.
    pub home: Option<Home>,
    /// Which row of `next` the selection is on.
    pub home_cursor: usize,
    /// The brief is in front. Set at launch (so a session file is history to
    /// continue, not a screen to be dropped into), by `/home`, and cleared by
    /// the first thing that adds to the transcript.
    pub home_visible: bool,
    /// Transcript, oldest first.
    pub entries: Vec<Entry>,
    /// The line being edited.
    pub input: String,
    /// Cursor position, in characters (not bytes).
    pub cursor: usize,
    /// Previously submitted lines, oldest first.
    pub history: Vec<String>,
    /// Where `history` navigation currently is.
    pub history_pos: Option<usize>,
    /// What was being typed before history navigation started.
    draft: String,
    /// First visible visual line.
    pub scroll: usize,
    /// Whether the view follows new output.
    pub follow: bool,
    /// Screen row the pointer last sat on, absolute. Feeds the one hover
    /// the transcript has: the `ctrl-r` hint on a thinking header exists
    /// only while the pointer rests on it, and is noise everywhere else.
    pub mouse_row: Option<u16>,
    /// Set when Enter was pressed; the run loop performs the dispatch so the
    /// pure state never needs an `AppContext`.
    pub pending_submit: bool,
    /// Set while a model turn is running on the worker thread.
    pub busy: bool,
    /// The live answer entry the stream is writing, if any. One entry per
    /// turn: deltas append to it, and the finished turn replaces its text
    /// with the authoritative answer before persisting.
    pub stream_answer: Option<usize>,
    /// The first answer entry of the turn in flight. A turn with tool calls
    /// grows several answer segments; the finished turn folds them back
    /// into this one, so the transcript keeps the whole answer in one block
    /// even though the text arrived in pieces around the tools.
    pub stream_answer_first: Option<usize>,
    /// The live thinking entry the stream is writing, if any.
    pub stream_think: Option<usize>,
    /// When the turn in flight started, for the busy line's elapsed time.
    pub turn_started_ms: i64,
    /// A write waiting for the person's yes, rendered as a modal.
    pub approval: Option<String>,
    /// The rendered context pane (legacy string).
    pub context: String,
    /// The model the next turn would use, shown beside the prompt. Read from
    /// the config on refresh, not on every frame: a file read per keystroke is
    /// the wrong price for a label.
    pub model: String,
    /// The graph's state when it needs a person, with the verb that fixes it.
    ///
    /// `None` when the graph is indexed and current — the header shows nothing
    /// rather than repeating counters `/home` already prints.
    pub graph_attention: Option<Attention>,
    /// Workspace root, shortened for the header.
    pub root: String,
    /// Vault path, shortened for the header.
    pub vault: String,
    /// Last transcript viewport, in characters/lines, for scrolling.
    view_width: usize,
    view_height: usize,
    /// Set by the run loop; quitting is read after the draw.
    pub quit: bool,
    /// First ctrl-c while a turn is running cancels; the second quits. Reset by
    /// any other key, so the two presses must be deliberate.
    pub ctrl_c_armed: bool,
    /// Asked the run loop to drop the in-flight turn and discard its answer.
    pub cancel_turn: bool,
    /// Fuzzy path completion state for @file references.
    pub completion: CompletionState,
    /// Commands typed while a turn is in flight (queued instead of rejected).
    pub command_queue: VecDeque<String>,
    /// Pending shell command (`!cmd`) awaiting sandbox policy confirmation.
    pub pending_shell: Option<PendingShell>,
    /// Whether the "not confined" line has been printed this session. Said
    /// once, because it is true of every command and repeating it under each
    /// one would train the reader to skip the line that matters.
    pub sandbox_notice_shown: bool,
    /// UI theme variant
    pub theme: Theme,
    /// Screen reader mode
    pub screen_reader: ScreenReaderMode,
    /// Transcript pane
    pub transcript_pane: TranscriptPane,
    /// Tasks pane
    pub tasks_pane: TasksPane,
    /// Real child processes behind the rail.
    pub jobs: Jobs,
    /// Diff pane
    pub diff_pane: DiffPane,
    /// Active pane focus: 0 = transcript, 1 = context, 2 = tasks, 3 = diff
    pub active_pane: u8,
    /// Which pane layout the frame uses (0 = transcript-only, 1 = split).
    pub pane_layout: u8,
    /// Overlay stack for modal UIs
    pub overlay_stack: crate::tui::overlays::OverlayStack,
    /// The `/` dropdown: which commands match the line being typed.
    pub slash: crate::tui::slash::SlashMenu,
    /// The signed-in cloud account, when this machine has one.
    pub account: Option<String>,
    /// The project this machine files sessions under, as `mn projects use`
    /// stored it, resolved to a name once at launch. `None` means the console's
    /// unassigned queue, which is a state worth showing rather than hiding.
    pub project: Option<String>,
    /// Sign-in events from the device-flow thread, and the sender kept beside
    /// the receiver so one attempt does not outlive the channel it reports on.
    pub login_tx: mpsc::Sender<crate::tui::login::LoginEvent>,
    pub login_rx: mpsc::Receiver<crate::tui::login::LoginEvent>,
    /// Set while a device flow is waiting for approval in the browser.
    pub login_busy: bool,
    /// When the current attempt started, so the footer can count the wait. A
    /// sign-in that says nothing for ten minutes reads as a hang.
    pub login_started: Option<std::time::Instant>,
    /// The code and the link of the attempt in flight, drawn above the prompt
    /// until it ends: the transcript's copy can be cleared or scrolled away.
    pub login_prompt: Option<crate::tui::login::LoginPrompt>,
    /// Asked of the waiting thread: set to stop the next poll. Each attempt owns
    /// its own flag, so a cancel cannot reach an attempt that already ended.
    pub login_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Configuration
    pub config: crate::Config,
    /// Config file path
    pub config_path: std::path::PathBuf,
    /// The invocation's paths and credentials, for an overlay that talks to the
    /// cloud. `None` in a unit test, which must never open the keyring; the run
    /// loop fills it in before the first frame.
    pub ctx: Option<crate::context::AppContext>,
    /// The spawn tree behind the Deck and the Trace. `None` until the run loop
    /// has a workspace context to root the child logs in.
    pub delegator: Option<memnoth_agent::Delegator>,
    /// The run that represents this session at the root of the tree.
    pub delegation_root: Option<String>,
    /// The child the Focus overlay is showing, chosen in the Deck or Trace.
    pub focus_agent: Option<String>,
    /// Channel for provider test results from background threads
    pub provider_test_tx: mpsc::Sender<ProviderTestResult>,
    pub provider_test_rx: mpsc::Receiver<ProviderTestResult>,
    /// Channel for provider model lists, fetched off the UI thread. A
    /// synchronous fetch here blocks the frame, which is how `/model` used to
    /// freeze for the length of an eight-second timeout.
    pub models_tx: mpsc::Sender<ModelsLoaded>,
    pub models_rx: mpsc::Receiver<ModelsLoaded>,
    /// Verbs run off the UI thread, and their answers come back here.
    ///
    /// `/stats` makes two cloud calls with a 30-second timeout each. On the UI
    /// thread that froze the screen for up to a minute, which reads as a hang
    /// rather than as a slow network.
    pub verb_tx: mpsc::Sender<VerbDone>,
    pub verb_rx: mpsc::Receiver<VerbDone>,
    /// The line whose verb is still running, so a second one is refused by name
    /// instead of interleaving its answer with the first.
    pub running_verb: Option<String>,
    /// Surface back stack: surfaces visited since the last root return.
    pub back_stack: Vec<String>,
    /// Current focus owner: which surface owns the composer and input.
    pub focus_owner: String,
    /// Surface shell: eight surfaces, focus owner, back stack.
    pub surface: crate::tui::surface::Surface,
    /// Focus owner for the surface shell.
    pub surface_focus: crate::tui::surface::FocusOwner,
    /// Surface back stack as structured entries.
    pub surface_back: Vec<crate::tui::surface::BackEntry>,
    /// Keymap table for the surface shell.
    pub surface_keymap: crate::tui::keymap::Keymap,
    /// Keymap overrides: key chord → command name, persisted per machine.
    pub keymap: Option<std::collections::HashMap<String, String>>,
}
/// Result of a provider connection test
#[derive(Debug, Clone)]
pub struct ProviderTestResult {
    pub index: usize,
    /// Preset the test ran against. The row is found by this, not by `index`:
    /// the list is re-sorted on reload, so a position recorded when the thread
    /// started can point at a different provider when the answer lands.
    pub provider: String,
    pub success: bool,
    pub message: String,
    pub model_count: usize,
    pub timestamp: String,
}

/// One provider's own `GET /models`, as it came back.
#[derive(Debug, Clone)]
pub struct ModelsLoaded {
    /// Preset name the list belongs to (`kilo`, `anthropic`, …).
    pub provider: String,
    pub ids: Vec<String>,
    /// `Some` when the fetch failed; `ids` is then empty and must not be read
    /// as "this provider has no models".
    pub error: Option<String>,
}

/// A verb's answer, arriving from the thread that ran it.
///
/// The prompt travels with it: the answer is written against the line that asked
/// for it, and the line was cleared the moment Enter was pressed.
#[derive(Debug)]
pub struct VerbDone {
    pub prompt: String,
    pub result: Result<crate::output::Outcome, String>,
}

impl App {
    /// A fresh screen.
    pub fn new(name: &str, resumed: bool) -> Self {
        let (provider_test_tx, provider_test_rx) = mpsc::channel();
        let (models_tx, models_rx) = mpsc::channel();
        let (login_tx, login_rx) = mpsc::channel();
        let (verb_tx, verb_rx) = mpsc::channel();
        Self {
            name: name.to_string(),
            resumed,
            pending_session: None,
            pending_prompt: None,
            home: None,
            home_cursor: 0,
            home_visible: false,
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            scroll: 0,
            follow: true,
            mouse_row: None,
            pending_submit: false,
            busy: false,
            stream_answer: None,
            stream_answer_first: None,
            stream_think: None,
            turn_started_ms: 0,
            approval: None,
            context: String::new(),
            delegator: None,
            delegation_root: None,
            focus_agent: None,
            model: String::new(),
            graph_attention: None,
            root: String::new(),
            vault: String::new(),
            view_width: 80,
            view_height: 20,
            quit: false,
            ctrl_c_armed: false,
            cancel_turn: false,
            completion: CompletionState::default(),
            command_queue: VecDeque::new(),
            pending_shell: None,
            sandbox_notice_shown: false,
            surface: crate::tui::surface::Surface::Chat,
            surface_focus: crate::tui::surface::FocusOwner::Chat,
            surface_back: Vec::new(),
            surface_keymap: crate::tui::keymap::Keymap::new(),
            theme: Theme::Default,
            screen_reader: ScreenReaderMode::Off,
            transcript_pane: TranscriptPane::new(),
            tasks_pane: TasksPane::new(),
            jobs: Jobs::new(),
            diff_pane: DiffPane::new(),
            pane_layout: 0,
            active_pane: 0,
            overlay_stack: crate::tui::overlays::OverlayStack::new(),
            slash: crate::tui::slash::SlashMenu::new(),
            // Read from the credential store by `tui::run`, not here: a unit
            // test must never open the keyring.
            account: None,
            project: None,
            login_tx,
            login_rx,
            login_busy: false,
            login_started: None,
            login_prompt: None,
            login_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            config: crate::Config::default(),
            config_path: std::path::PathBuf::new(),
            ctx: None,
            verb_tx,
            verb_rx,
            running_verb: None,
            provider_test_tx,
            provider_test_rx,
            models_tx,
            models_rx,
            back_stack: Vec::new(),
            focus_owner: "chat".to_string(),
            keymap: None,
        }
    }

    /// Seed the transcript from a session file, so a session that is continued
    /// continues the conversation that was actually had.
    ///
    /// The rows land behind the brief rather than on screen: whoever calls this
    /// at startup leaves `home_visible` set, and `esc` is what puts them in
    /// front of the person. Nothing is re-read later, so the transcript and the
    /// file cannot disagree about what was said.
    pub fn seed(&mut self, session: &Session) {
        for message in &session.messages {
            let role = match message.role {
                Role::User => EntryRole::User,
                Role::Assistant => EntryRole::Answer,
                Role::System | Role::Tool => EntryRole::Info,
            };
            self.entries.push(Entry {
                role,
                text: message.content.clone(),
                at_ms: message.at_ms,
                live: false,
                expanded: false,
                stats: None,
            });
        }
        self.follow = true;
    }

    /// Record the two paths the header shows.
    pub fn set_paths(&mut self, root: String, vault: String) {
        self.root = root;
        self.vault = vault;
    }

    /// Append one entry and snap the view to it.
    ///
    /// The first entry is also what takes the brief off screen: a welcome that
    /// stayed at the top of a long conversation would only push the answer
    /// down. `/home` brings it back, which is why it is a flag and not a
    /// consequence of the transcript being empty.
    pub fn push(&mut self, role: EntryRole, text: impl Into<String>) {
        self.entries.push(Entry {
            role,
            text: text.into(),
            // The same clock the session file records against, so a resumed
            // transcript and a live one put the same kind of stamp on a row.
            at_ms: crate::session::now_ms(),
            live: false,
            expanded: false,
            stats: None,
        });
        self.home_visible = false;
        self.home_cursor = 0;
        self.follow = true;
    }

    /// One streamed text piece: appended to the turn's live answer entry,
    /// which is created on the first piece and marked live until the turn
    /// finishes. Coalescing here means the entry count never grows with the
    /// token count.
    pub fn append_stream_delta(&mut self, piece: &str) {
        let index = match self.stream_answer {
            Some(index) if index < self.entries.len() => index,
            _ => {
                self.entries.push(Entry {
                    role: EntryRole::Answer,
                    text: String::new(),
                    at_ms: crate::session::now_ms(),
                    live: true,
                    expanded: false,
                    stats: None,
                });
                let index = self.entries.len() - 1;
                self.stream_answer = Some(index);
                if self.stream_answer_first.is_none() {
                    self.stream_answer_first = Some(index);
                }
                index
            }
        };
        if let Some(entry) = self.entries.get_mut(index) {
            entry.text.push_str(piece);
        }
        // Deliberately no `follow = true` here: a token is not a request to
        // look at it. Forcing the view back to the bottom on every delta
        // yanked the viewport out from under anyone who had scrolled up to
        // read — the stream fought the reader, and the reader lost.
        self.home_visible = false;
    }

    /// One streamed reasoning piece: the turn's thinking block. It starts
    /// collapsed — the header plus a three-line tail — because a reasoning
    /// block that is fully expanded while it streams pushes the answer off
    /// the screen, and every agent's users have asked for the smaller one.
    /// `ctrl-r` opens the whole of it.
    pub fn append_reasoning_delta(&mut self, piece: &str) {
        let index = match self.stream_think {
            Some(index) if index < self.entries.len() => index,
            _ => {
                let entry = Entry {
                    role: EntryRole::Thinking,
                    text: String::new(),
                    at_ms: crate::session::now_ms(),
                    live: true,
                    expanded: false,
                    stats: None,
                };
                // Thinking belongs before the answer it produced, always.
                // The push used to put the block wherever the delta landed:
                // when a provider emits reasoning after the first answer
                // chunk, the thinking calendarised itself *under* the
                // response it explained. If this turn's answer already
                // exists, the block inserts ahead of it.
                let index = match self.stream_answer_first {
                    Some(first) if first < self.entries.len() => {
                        self.entries.insert(first, entry);
                        for slot in [&mut self.stream_answer, &mut self.stream_answer_first] {
                            if let Some(position) = slot {
                                *position += 1;
                            }
                        }
                        first
                    }
                    _ => {
                        self.entries.push(entry);
                        self.entries.len() - 1
                    }
                };
                self.stream_think = Some(index);
                index
            }
        };
        if let Some(entry) = self.entries.get_mut(index) {
            entry.text.push_str(piece);
        }
    }

    /// Expand or collapse the newest thinking block.
    pub fn toggle_thinking(&mut self) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.role == EntryRole::Thinking)
        {
            entry.expanded = !entry.expanded;
        }
    }

    /// When the first answer piece lands, the thinking that produced it tucks
    /// itself away: one row until the person asks for it.
    pub fn collapse_thinking(&mut self) {
        if let Some(index) = self.stream_think
            && let Some(entry) = self.entries.get_mut(index)
        {
            entry.expanded = false;
            entry.live = false;
        }
    }

    /// End the turn's streaming: the live answer and thinking entries stop
    /// carrying the caret, and the next turn starts without them.
    pub fn finish_stream(&mut self) {
        if let Some(index) = self.stream_answer
            && let Some(entry) = self.entries.get_mut(index)
        {
            entry.live = false;
        }
        self.collapse_thinking();
        self.stream_answer = None;
        self.stream_answer_first = None;
        self.stream_think = None;
    }

    /// Append a child's card to the parent timeline: the delegation row a person
    /// sees where the work happened, not only in the deck.
    pub fn push_card(&mut self, card: &crate::tui::agents::cards::AgentCard) {
        self.push(EntryRole::Info, card.text());
    }

    /// Append a tool call that is running, and mark the app busy in one place.
    ///
    /// The row is rewritten by [`App::resolve_tools`] when the round lands,
    /// which is what makes a turn look like a live record instead of a scroll
    /// of past tense.
    pub fn push_tool(&mut self, text: impl Into<String>) {
        // The answer pauses while a tool runs. Closing its segment here means
        // the next text opens a fresh entry *below* the tool rows, so the
        // timeline reads in the order it happened; appending into the open
        // segment instead grew new tokens above the tools, where following
        // eyes never saw them.
        if let Some(index) = self.stream_answer.take()
            && let Some(entry) = self.entries.get_mut(index)
        {
            entry.live = false;
        }
        self.push(EntryRole::Tool, text);
        // A running row is live: it carries its elapsed time until the round
        // lands, so a turn that works is a turn that reports its own pace.
        if let Some(entry) = self.entries.last_mut() {
            entry.live = true;
        }
    }

    /// Mark every running tool row at or after `from` as finished.
    ///
    /// Index-based rather than text-based: two calls to the same tool with the
    /// same argument are two rows, and only the ones this turn opened may
    /// change.
    pub fn resolve_tools(&mut self, from: usize) {
        for entry in self.entries.iter_mut().skip(from) {
            if entry.role == EntryRole::Tool {
                entry.role = EntryRole::ToolDone;
                entry.live = false;
            }
        }
    }

    /// Empty the transcript, which brings the startup screen back (this is
    /// what `/clear` does).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.scroll = 0;
        self.follow = true;
        self.home_visible = false;
        self.home_cursor = 0;
    }

    /// Whether the brief owns the transcript area: on an empty session, or
    /// after `/home`.
    pub fn home_showing(&self) -> bool {
        self.home.is_some() && (self.entries.is_empty() || self.home_visible)
    }

    /// Show the brief over an existing transcript (`/home`).
    pub fn open_home(&mut self) {
        if self.home.is_some() {
            self.home_visible = true;
            self.home_cursor = 0;
        }
    }

    /// Put the transcript back (`esc` on the brief). Leaves an empty session
    /// showing the brief, because there is nothing behind it to go back to.
    pub fn close_home(&mut self) {
        if !self.entries.is_empty() {
            self.home_visible = false;
        }
    }

    /// Positions in `steps` the selection may occupy, in order.
    fn home_runnable(&self) -> Vec<usize> {
        match &self.home {
            Some(home) => (0..home.steps.len())
                .filter(|index| home.steps[*index].runnable)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Move the selection by `delta` rows, wrapping at both ends.
    ///
    /// Wrapping rather than clamping is deliberate: the list is three rows and
    /// the top row is the recommended one, so the fast way back to it is one
    /// key, not three.
    pub fn home_move(&mut self, delta: i32) {
        let runnable = self.home_runnable();
        if runnable.is_empty() {
            return;
        }
        let Some(position) = runnable.iter().position(|index| *index == self.home_cursor) else {
            self.home_cursor = runnable[0];
            return;
        };
        let len = runnable.len() as i64;
        let next = (position as i64 + delta as i64).rem_euclid(len) as usize;
        self.home_cursor = runnable[next];
    }

    /// Jump to step `index` (1-based, as it is printed) if it is runnable.
    pub fn home_jump(&mut self, index: usize) {
        if let Some(home) = &self.home
            && home
                .steps
                .get(index.saturating_sub(1))
                .is_some_and(|step| step.runnable)
        {
            self.home_cursor = index - 1;
        }
    }

    /// The command under the selection, ready to be put in the prompt.
    pub fn home_command(&self) -> Option<String> {
        let home = self.home.as_ref()?;
        let step = home.steps.get(self.home_cursor)?;
        step.runnable.then(|| step.command.clone())
    }

    /// The prompt line `tab` writes: the command and a trailing space, so an
    /// argument can be typed without reaching for the arrow keys.
    pub fn home_draft(&self) -> Option<String> {
        Some(format!("{} ", self.home_command()?))
    }

    /// Focus one rail: 0 transcript, 1 context, 2 tasks, 3 diff.
    ///
    /// `pane_layout` is 0 while no rail is focused and 1 once one is, so a key
    /// that only navigates does not silently change the frame on a narrow
    /// terminal.
    pub fn focus_pane(&mut self, pane: u8) {
        self.active_pane = pane.min(3);
        self.pane_layout = if self.active_pane == 0 { 0 } else { 1 };
    }

    /// Tab / Shift+Tab: move through transcript, context, tasks, diff and wrap.
    pub fn cycle_pane(&mut self, forward: bool) {
        let next = if forward {
            (self.active_pane + 1) % 4
        } else {
            (self.active_pane + 3) % 4
        };
        self.focus_pane(next);
    }

    /// Ctrl+T: the task rail on demand, and the transcript back on the second
    /// press. Nothing is hidden permanently, because the transcript is home.
    pub fn toggle_tasks(&mut self) {
        if self.active_pane == 2 {
            self.focus_pane(0);
        } else {
            self.focus_pane(2);
        }
    }

    /// Reap finished jobs and mirror them into the task rail.
    ///
    /// Called once per frame tick. Mirroring rather than drawing the job list
    /// twice keeps the rail, `/tasks status` and the detail view reading from
    /// one set of facts.
    pub fn sync_jobs(&mut self) {
        self.jobs.poll();
        for job in self.jobs.newest_first() {
            let status = match job.status {
                JobStatus::Running => crate::tui::panes::tasks::TaskStatus::Running,
                JobStatus::Completed => crate::tui::panes::tasks::TaskStatus::Completed,
                JobStatus::Failed => crate::tui::panes::tasks::TaskStatus::Failed,
                JobStatus::Cancelled => crate::tui::panes::tasks::TaskStatus::Cancelled,
            };
            let detail = {
                let tail = job.tail(3).join("\n");
                if tail.is_empty() {
                    job.command.clone()
                } else {
                    tail
                }
            };
            if let Some(existing) = self.tasks_pane.tasks.iter_mut().find(|t| t.id == job.id) {
                existing.status = status;
                existing.result = Some(detail);
                existing.completed_at = if status == crate::tui::panes::tasks::TaskStatus::Running {
                    None
                } else {
                    existing.completed_at.clone()
                };
                existing.error = (status == crate::tui::panes::tasks::TaskStatus::Failed)
                    .then(|| format!("exit {}", job.exit.unwrap_or(-1)));
            } else {
                self.tasks_pane
                    .add_task(crate::tui::panes::tasks::BackgroundTask {
                        id: job.id.clone(),
                        name: format!("{} {}", job.id, job.status.label()),
                        description: job.command.clone(),
                        status,
                        progress: 0.0,
                        started_at: job.started_at.clone(),
                        completed_at: (status != crate::tui::panes::tasks::TaskStatus::Running)
                            .then(|| job.started_at.clone()),
                        error: (status == crate::tui::panes::tasks::TaskStatus::Failed)
                            .then(|| format!("exit {}", job.exit.unwrap_or(-1))),
                        result: Some(detail),
                    });
            }
        }
    }

    /// Try to receive a provider test result without blocking
    pub fn try_recv_provider_test_result(&mut self) -> Option<ProviderTestResult> {
        self.provider_test_rx.try_recv().ok()
    }

    /// Try to receive a provider's model list without blocking.
    pub fn try_recv_models_loaded(&mut self) -> Option<ModelsLoaded> {
        self.models_rx.try_recv().ok()
    }

    /// Try to receive a finished verb without blocking.
    pub fn try_recv_verb(&mut self) -> Option<VerbDone> {
        self.verb_rx.try_recv().ok()
    }

    /// Take the submitted line: trims it, resets the editor and history
    /// navigation, and leaves the transcript untouched.
    pub fn take_input(&mut self) -> String {
        let line = self.input.trim().to_string();
        self.input.clear();
        self.cursor = 0;
        self.history_pos = None;
        self.draft.clear();
        line
    }

    /// Record a submitted line. Consecutive duplicates are collapsed: pressing
    /// enter twice should not mean "history up" twice.
    pub fn remember_history(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) != Some(line) {
            self.history.push(line.to_string());
        }
        self.history_pos = None;
    }

    /// Up: the previous submitted line, or stay at the oldest.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_pos {
            None => {
                self.draft = self.input.clone();
                self.history_pos = Some(self.history.len() - 1);
            }
            Some(0) => {}
            Some(index) => self.history_pos = Some(index - 1),
        }
        if let Some(index) = self.history_pos {
            self.input = self.history[index].clone();
            self.cursor = self.input.chars().count();
        }
    }

    /// Down: toward the line being drafted, then back to it.
    pub fn history_next(&mut self) {
        let Some(index) = self.history_pos else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_pos = Some(index + 1);
            self.input = self.history[index + 1].clone();
        } else {
            self.history_pos = None;
            self.input = self.draft.clone();
        }
        self.cursor = self.input.chars().count();
    }

    /// Insert one character at the cursor.
    pub fn insert_char(&mut self, ch: char) {
        let byte = self.cursor_byte();
        self.input.insert(byte, ch);
        self.cursor += 1;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.cursor_byte();
        let start = self
            .input
            .char_indices()
            .nth(self.cursor - 1)
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// Delete the character at the cursor.
    pub fn delete(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let start = self.cursor_byte();
        let end = self
            .input
            .char_indices()
            .nth(self.cursor + 1)
            .map(|(index, _)| index)
            .unwrap_or(self.input.len());
        self.input.replace_range(start..end, "");
    }

    /// One character left.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// One character right.
    pub fn move_right(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.cursor += 1;
        }
    }

    /// Start of the line.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// End of the line.
    pub fn move_end(&mut self) {
        self.cursor = self.input.chars().count();
    }

    /// Scroll up one page and stop following.
    pub fn page_up(&mut self) {
        self.follow = false;
        self.scroll = self.scroll.saturating_sub(self.view_height.max(1));
    }

    /// Move the transcript by one wheel notch.
    ///
    /// A touchpad sends a stream of these, so the step is small enough that one
    /// flick is not a page. Scrolling up stops following — a view that jumps
    /// back to the newest line the moment an answer lands cannot be read — and
    /// scrolling to the end resumes it, exactly as `page_down` does.
    pub fn wheel(&mut self, lines: usize, up: bool) {
        let step = lines.max(1);
        if up {
            self.follow = false;
            self.scroll = self.scroll.saturating_sub(step);
            return;
        }
        self.scroll = self.scroll.saturating_add(step);
        if self.scroll >= self.max_scroll() {
            self.to_bottom();
        }
    }

    /// Scroll down one page; reaching the end resumes following.
    pub fn page_down(&mut self) {
        self.scroll = self.scroll.saturating_add(self.view_height.max(1));
        if self.scroll >= self.max_scroll() {
            self.to_bottom();
        }
    }

    /// Snap to the newest line.
    pub fn to_bottom(&mut self) {
        self.scroll = self.max_scroll();
        self.follow = true;
    }

    /// The last scroll offset that still shows content.
    pub fn max_scroll(&self) -> usize {
        self.visual_lines(self.view_width)
            .len()
            .saturating_sub(self.view_height.max(1))
    }

    /// Remember the viewport measured during the last draw.
    pub fn set_viewport(&mut self, width: usize, height: usize) {
        self.view_width = width.max(1);
        self.view_height = height.max(1);
    }

    /// The wrapped transcript, one entry possibly spanning many lines.
    ///
    /// Wrapping here rather than in the widget is what makes scrolling exact:
    /// the offset is in visual lines, so `PgUp` moves by a screen and the last
    /// page is not an estimate.
    /// One rendered transcript line: its role, its text, and whether it opens
    /// its entry (the thinking header, for instance, is the only line a
    /// collapsed block has).
    pub fn visual_lines(
        &self,
        width: usize,
    ) -> Vec<(EntryRole, ratatui::text::Line<'static>, bool)> {
        // Every row is drawn after a [`GUTTER`]-wide glyph, so the text may only
        // use what is left of the pane. Wrapping to the full width let the
        // terminal re-wrap the last word onto a line of its own.
        let width = width.saturating_sub(GUTTER).max(1);
        let mut out = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            // A turn ends with a blank line: the space is what separates the
            // previous answer from the next question, not a rule and not a
            // glyph. The blank never carries one either — a `›` on an empty
            // row reads as a second prompt.
            if entry.role == EntryRole::User && index > 0 {
                out.push((EntryRole::User, ratatui::text::Line::from(""), false));
            }
            if entry.role == EntryRole::Thinking {
                // The industry shape, from Claude Code and friends: collapsed
                // is one header row; the body is dimmer than any answer text;
                // and while reasoning is still streaming, a *fixed-height*
                // tail (three lines) shows the last of it. The fixed height
                // is the point — a preview that grows and shrinks with the
                // token count makes the whole screen jump on every chunk.
                let marker = if entry.expanded { "▾" } else { "▸" };
                // The header says one word. A character count is noise the
                // eye re-reads every frame, and the expand hint lives under
                // the pointer (see `ui::draw_transcript`), not in the text.
                let header = ratatui::text::Line::from(format!("{marker} thinking"));
                out.push((EntryRole::Thinking, header, true));
                if !entry.text.trim().is_empty() {
                    // The body indents two columns deeper than the rail, so
                    // the wrap budget comes off the same two: nested reasoning
                    // under an accent rail cannot be mistaken for answer text
                    // at a glance, which is the entire point of the indent.
                    // The body is markdown, like the answer: reasoning that
                    // sketches a table or a chart renders as structure, not
                    // as pipes and bars the reader must decode by hand.
                    let wrapped = crate::tui::markdown::lines(&entry.text, width.saturating_sub(2));
                    let shown: &[ratatui::text::Line<'static>] = if entry.expanded {
                        &wrapped
                    } else if entry.live {
                        &wrapped[wrapped.len().saturating_sub(3)..]
                    } else {
                        &[][..]
                    };
                    for line in shown {
                        out.push((EntryRole::Thinking, line.clone(), false));
                    }
                }
                continue;
            }
            // A running tool row carries its elapsed time: a turn that works
            // reports its own pace, and the row stops changing the moment
            // the result lands.
            let text = if entry.role == EntryRole::Tool && entry.live {
                let elapsed = (crate::session::now_ms() - entry.at_ms).max(0) / 1000;
                let age = if elapsed < 60 {
                    format!("{elapsed}s")
                } else {
                    format!("{}m {:02}s", elapsed / 60, elapsed % 60)
                };
                format!("{} · {age}", entry.text)
            } else {
                entry.text.clone()
            };
            // An answer is markdown and is rendered as such: headings without
            // their hashes, bullets with a hanging indent, code without its
            // backticks. A prompt or a tool row is text the person typed or the
            // tool printed, so it is wrapped and never reinterpreted.
            let rendered: Vec<ratatui::text::Line<'static>> = match entry.role {
                EntryRole::Answer | EntryRole::Info => crate::tui::markdown::lines(&text, width),
                _ => wrap_text(&text, width)
                    .into_iter()
                    .map(ratatui::text::Line::from)
                    .collect(),
            };
            let last = rendered.len().saturating_sub(1);
            let is_answer = entry.role == EntryRole::Answer;
            let stats = if !entry.live {
                entry.stats.clone()
            } else {
                None
            };
            for (position, line) in rendered.into_iter().enumerate() {
                let mut line = line;
                if entry.live && position == last {
                    line.spans.push(ratatui::text::Span::styled(
                        "▍",
                        ratatui::style::Style::default(),
                    ));
                }
                out.push((entry.role, line, position == 0));
            }
            // The pace line belongs to the answer, not to the session: one
            // dim row under it, and the next user's blank line reclaims the
            // spacing. It is the only post-answer row the design keeps.
            if is_answer && let Some(stats) = stats {
                out.push((EntryRole::Info, ratatui::text::Line::from(stats), false));
            }
        }
        out
    }

    fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.input.len())
    }

    /// Start or continue @file fuzzy completion.
    pub fn complete_path(&mut self, workspace_root: &Path) {
        if !self.completion.active {
            // Find the @ prefix at or before cursor
            let input_chars: Vec<char> = self.input.chars().collect();
            let mut at_pos = None;
            for i in (0..self.cursor.min(input_chars.len())).rev() {
                if input_chars[i] == '@' {
                    at_pos = Some(i);
                    break;
                }
            }
            let Some(start) = at_pos else { return };

            self.completion.prefix = self.input[start..self.cursor].to_string();
            self.completion.cursor_at_start = self.cursor;
            self.completion.workspace_root = workspace_root.to_path_buf();

            // Find matches
            let query = &self.completion.prefix[1..]; // skip @
            self.completion.matches = fuzzy_find_paths(workspace_root, query);
            if self.completion.matches.is_empty() {
                self.completion.active = false;
                return;
            }
            self.completion.index = 0;
            self.completion.active = true;
        } else {
            // Cycle to next match
            if !self.completion.matches.is_empty() {
                self.completion.index = (self.completion.index + 1) % self.completion.matches.len();
            }
        }

        // Apply the current match
        self.apply_completion();
    }

    /// Apply the current completion match to the input.
    fn apply_completion(&mut self) {
        if !self.completion.active || self.completion.matches.is_empty() {
            return;
        }
        let input_chars: Vec<char> = self.input.chars().collect();
        let start = self.completion.cursor_at_start - self.completion.prefix.chars().count();
        let end = self.completion.cursor_at_start;

        let match_path = &self.completion.matches[self.completion.index];
        let replacement = match_path.to_string_lossy().to_string();

        let mut new_input: String = input_chars[..start].iter().collect();
        new_input.push('@');
        new_input.push_str(&replacement);
        new_input.push_str(&input_chars[end..].iter().collect::<String>());

        self.input = new_input;
        self.cursor = start + 1 + replacement.chars().count();
    }

    /// Cancel completion (e.g., on Esc or other key).
    /// Whether esc belongs to the pending sign-in rather than the prompt.
    ///
    /// esc already means "cancel what is in front of me" everywhere in this
    /// screen, so a wait for approval is its case too — but only where nothing
    /// else claims it. A running turn, a half-typed line, an open brief or a
    /// completion all answer to esc first; while any of them is in play the
    /// footer names `/login` instead, which cancels in every one of those states.
    pub fn esc_cancels_signin(&self) -> bool {
        self.login_busy
            && !self.busy
            && self.input.is_empty()
            && !self.home_showing()
            && !self.completion.active
            && !self.slash.is_open()
    }

    pub fn cancel_completion(&mut self) {
        if self.completion.active {
            // Restore original input
            let input_chars: Vec<char> = self.input.chars().collect();
            let start = self.completion.cursor_at_start - self.completion.prefix.chars().count();
            let end = self.completion.cursor_at_start;

            let mut new_input: String = input_chars[..start].iter().collect();
            new_input.push_str(&self.completion.prefix);
            new_input.push_str(&input_chars[end..].iter().collect::<String>());

            self.input = new_input;
            self.cursor = self.completion.cursor_at_start;
            self.completion = CompletionState::default();
        }
    }

    /// Re-derive the `/` dropdown from the line being edited.
    ///
    /// The dropdown is a function of the prompt, not a separate mode: every
    /// edit recomputes it, so there is no state that can disagree with what is
    /// on screen.
    pub fn sync_slash(&mut self) {
        self.slash.sync(&self.input, self.cursor);
    }

    /// Queue a command for later execution (when not busy).
    pub fn queue_command(&mut self, cmd: String) {
        self.command_queue.push_back(cmd);
    }

    /// Take the next queued command.
    pub fn take_queued_command(&mut self) -> Option<String> {
        self.command_queue.pop_front()
    }

    /// Check if there are queued commands.
    pub fn has_queued_commands(&self) -> bool {
        !self.command_queue.is_empty()
    }
}

/// Break text into lines of at most `width` characters.
///
/// Greedy on spaces, because a transcript that breaks mid-word reads worse than
/// one that leaves a ragged edge; a single word longer than the pane is split
/// hard so it can never overflow and hide the rest of the line.
/// Columns a transcript row spends on its glyph, before any text.
pub const GUTTER: usize = 2;

pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut line_len = 0usize;
        for word in paragraph.split(' ') {
            let word_len = word.chars().count();
            if line_len == 0 {
                if word_len <= width {
                    line.push_str(word);
                    line_len = word_len;
                } else {
                    let mut chunks = chunk_chars(word, width).into_iter().peekable();
                    while let Some(chunk) = chunks.next() {
                        if chunks.peek().is_some() {
                            out.push(chunk);
                        } else {
                            line_len = chunk.chars().count();
                            line = chunk;
                        }
                    }
                }
            } else if line_len + 1 + word_len <= width {
                line.push(' ');
                line.push_str(word);
                line_len += 1 + word_len;
            } else {
                out.push(std::mem::take(&mut line));
                if word_len <= width {
                    line.push_str(word);
                    line_len = word_len;
                } else {
                    let mut chunks = chunk_chars(word, width).into_iter().peekable();
                    line_len = 0;
                    while let Some(chunk) = chunks.next() {
                        if chunks.peek().is_some() {
                            out.push(chunk);
                        } else {
                            line_len = chunk.chars().count();
                            line = chunk;
                        }
                    }
                }
            }
        }
        if !line.is_empty() || line_len > 0 {
            out.push(line);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn chunk_chars(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if current.chars().count() == width {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Parse one submitted line.
pub fn parse_line(raw: &str) -> Parsed {
    parse_line_inner(raw, true)
}

/// The parser without the "did you mean" suggestion.
///
/// The suggestion reads the command projection, and building that projection
/// asks this parser whether each row runs. Going through the full parser from
/// the builder re-enters the projection while it is still initialising and
/// deadlocks — the hang that used to freeze every `App::new` in a test.
pub(crate) fn parse_line_quietly(raw: &str) -> Parsed {
    parse_line_inner(raw, false)
}

/// The parser itself. `suggest` is false only while the projection is built.
fn parse_line_inner(raw: &str, suggest: bool) -> Parsed {
    let line = raw.trim();
    if line.is_empty() {
        return Parsed::Empty;
    }
    if line == ":q" || line == ":quit" {
        return Parsed::Quit;
    }
    // !cmd: shell passthrough
    if let Some(cmd) = line.strip_prefix('!') {
        return Parsed::Shell(cmd.trim().to_string());
    }
    if !line.starts_with('/') {
        return Parsed::Ask(line.split_whitespace().map(str::to_string).collect());
    }

    let mut words = line[1..].split_whitespace();
    let Some(head) = words.next() else {
        return Parsed::Unknown("empty command — type /help".to_string());
    };
    let args: Vec<String> = words.map(str::to_string).collect();
    match head {
        "help" | "?" => Parsed::Help,
        "quit" | "exit" | "q" => Parsed::Quit,
        "clear" => Parsed::Clear,
        "home" => Parsed::Home,
        // `/new chat-3` names the conversation; `/new` takes the next free name.
        // One word for the thing every agent CLI has: start over without
        // leaving, and without losing what you just said.
        "new" => Parsed::NewChat(args.first().cloned()),
        // `/resume` is the name the other tools use for this list. An alias
        // costs one line and keeps muscle memory working.
        "sessions" | "resume" => Parsed::Sessions,
        // `/model` was a second, mock copy of the same browser. One name is an
        // alias now, so both spellings open the surface that actually works.
        "model" | "models" => Parsed::ModelsOverlay,
        "tasks" => parse_tasks(&args),
        "connect" => Parsed::ConnectOverlay,
        "login" => Parsed::Login,
        "logout" => Parsed::Logout,
        // The command line's `whoami`, through the same verb: one answer about
        // this machine's account, so the two doors cannot describe it two ways.
        "whoami" => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Whoami(WhoamiArgs {}),
        })),
        "search" => {
            if args.is_empty() {
                Parsed::SearchOverlay
            } else {
                Parsed::Search(args)
            }
        }
        "diff" => Parsed::DiffOverlay,
        "rewind" => Parsed::RewindOverlay,
        "checkpoints" => Parsed::CheckpointsOverlay,
        "permissions" => Parsed::PermissionsOverlay,
        "feedback" => Parsed::FeedbackOverlay,
        "doctor" => Parsed::Run(Verb::Doctor(DoctorArgs {
            bundle: None,
            fix: false,
            yes: false,
            dry_run: false,
        })),
        "drift" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Drift(DriftArgs {
                since: flags.get("since").cloned(),
            }))
        }
        "simulate" => {
            let (_, positional) = split_flags(&args);
            if positional.is_empty() {
                return Parsed::Usage("usage: /simulate <change>".to_string());
            }
            Parsed::Run(Verb::Simulate(SimulateArgs {
                change: positional.join(" "),
            }))
        }
        "lineage" => {
            let (flags, positional) = split_flags(&args);
            if positional.is_empty() {
                return Parsed::Usage("usage: /lineage <entity> [--depth N]".to_string());
            }
            Parsed::Run(Verb::Lineage(LineageArgs {
                entity: positional[0].clone(),
                depth: flags
                    .get("depth")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(3),
            }))
        }
        "why" => {
            let (flags, positional) = split_flags(&args);
            if positional.is_empty() {
                return Parsed::Usage("usage: /why <symbol> [--at WHEN]".to_string());
            }
            Parsed::Run(Verb::Why(WhyArgs {
                symbol: positional.join(" "),
                at: flags.get("at").cloned(),
            }))
        }
        "impact" => {
            let (flags, positional) = split_flags(&args);
            if positional.is_empty() {
                return Parsed::Usage("usage: /impact <symbol> [--depth N]".to_string());
            }
            Parsed::Run(Verb::Impact(ImpactArgs {
                symbol: positional.join(" "),
                depth: flags
                    .get("depth")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(2),
                diff: flags.contains_key("diff"),
            }))
        }
        "plan" => {
            let (flags, positional) = split_flags(&args);
            // `/plan step <text>` appends a step; anything else names the
            // session and shows the artifact.
            let (name, step) = match positional.first().map(String::as_str) {
                Some("step") => (None, Some(positional[1..].join(" "))),
                Some(_) => (Some(positional.join(" ")), None),
                None => (None, None),
            };
            Parsed::Run(Verb::Plan(PlanArgs {
                name,
                step,
                enable: false,
                disable: false,
                json: flags.contains_key("json"),
            }))
        }
        "goal" => {
            let (_flags, positional) = split_flags(&args);
            if positional.is_empty() {
                Parsed::Run(Verb::Goal(GoalArgs { condition: None }))
            } else {
                Parsed::Run(Verb::Goal(GoalArgs {
                    condition: Some(positional.join(" ")),
                }))
            }
        }
        "checkpoint" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Checkpoint(CheckpointArgs {
                name: flags.get("name").cloned(),
            }))
        }
        "undo" => Parsed::Run(Verb::Undo),
        "redo" => Parsed::Run(Verb::Redo),
        "delegate" => {
            let (_, positional) = split_flags(&args);
            if positional.len() < 2 {
                return Parsed::Usage("usage: /delegate <name> <task>".to_string());
            }
            Parsed::Run(Verb::Delegate(DelegateArgs {
                name: positional[0].clone(),
                task: positional[1..].join(" "),
            }))
        }
        "merge" => {
            let (_, positional) = split_flags(&args);
            if positional.is_empty() {
                return Parsed::Usage("usage: /merge <branch>".to_string());
            }
            Parsed::Run(Verb::Merge(MergeArgs {
                branch: positional.join(" "),
            }))
        }
        "handoff" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Handoff(HandoffTopArgs {
                path: flags.get("path").cloned(),
            }))
        }
        "compact" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Compact(CompactArgs {
                focus: flags.get("focus").cloned(),
            }))
        }
        "btw" => {
            let (_, positional) = split_flags(&args);
            if positional.is_empty() {
                return Parsed::Usage("usage: /btw <question>".to_string());
            }
            Parsed::Run(Verb::Btw(BtwArgs {
                question: positional.join(" "),
            }))
        }
        "recap" => Parsed::Run(Verb::Recap),
        "export" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Export(ExportArgs {
                path: flags.get("path").cloned(),
            }))
        }
        "rename" => {
            let (_, positional) = split_flags(&args);
            if positional.is_empty() {
                return Parsed::Usage("usage: /rename <name>".to_string());
            }
            Parsed::Run(Verb::Rename(RenameArgs {
                name: positional.join(" "),
            }))
        }
        // `/keymap`, `/keymap set alt+d=surface: review`, `/keymap unset alt+d`,
        // `/keymap reset`. The words become the same flags the CLI parses, so a
        // chord rebound in a session is a chord rebound on the command line.
        "keymap" => {
            let mut parsed = KeymapArgs::default();
            let mut words = args.iter();
            while let Some(word) = words.next() {
                match word.as_str() {
                    "set" => {
                        if let Some(spec) = words.next() {
                            parsed.set = Some(spec.clone());
                        }
                    }
                    "unset" => parsed.unset = words.next().cloned(),
                    "reset" => parsed.reset = true,
                    _ => {}
                }
            }
            Parsed::Run(Verb::Keymap(parsed))
        }
        "onboarding" => Parsed::Onboarding,
        "brief" => Parsed::Run(Verb::Brief),
        "acp" => Parsed::Info(
            "`memnoth acp` runs the ACP server until interrupted — run it in another terminal"
                .to_string(),
        ),
        "kg" => match args.first().map(String::as_str) {
            // Both run until interrupted. In a session that would freeze the
            // screen with no way back to the prompt, so name the shell command
            // instead of pretending the verb does not exist.
            Some("watch") => Parsed::Info(
                "`memnoth kg watch` runs until interrupted — run it in another terminal"
                    .to_string(),
            ),
            Some("serve") => Parsed::Info(
                "`memnoth kg serve` runs an HTTP server — run it in another terminal".to_string(),
            ),
            _ => parse_kg(&args),
        },
        // `/stats` is not a second money surface: it folds into `/billing`,
        // the one place invoices, plan and limits live.
        "stats" => Parsed::Run(Verb::Billing(BillingCommands {
            action: BillingCommand::Limits(BillingLimitsArgs { json: false }),
        })),
        "vault" => parse_vault(&args),
        "mcp" => parse_mcp(&args),
        // `/update` runs the same verb `mn update` parses: one implementation,
        // two doors. Progress arrives as the verb's answer rather than as
        // stdout, because this screen owns the terminal while it runs.
        "update" => {
            let (flags, words) = split_flags(&args);
            Parsed::Run(Verb::Update(crate::cli::UpdateArgs {
                check: flags.contains_key("check") || words.iter().any(|word| word == "check"),
                channel: flags.get("channel").cloned(),
                version: flags.get("version").cloned(),
                no_verify: flags.contains_key("no-verify")
                    || words.iter().any(|word| word == "no-verify"),
                rollback: flags.contains_key("rollback")
                    || words.iter().any(|word| word == "rollback"),
            }))
        }
        "sync" => {
            let (flags, _) = split_flags(&args);
            if flags.contains_key("watch") {
                Parsed::Info(
                    "`memnoth cloud sync --watch` runs until interrupted — run it in another terminal"
                        .to_string(),
                )
            } else {
                Parsed::Run(Verb::Cloud(CloudArgs {
                    action: CloudCommand::Sync(SyncArgs {
                        dry_run: flags.contains_key("dry-run"),
                        watch: false,
                        json: false,
                    }),
                }))
            }
        }
        "palette" => Parsed::PaletteOverlay,
        "commands" => Parsed::CommandsOverlay,
        "projects" => parse_projects(&args),
        "keys" => parse_keys(&args),
        "org" => parse_org(&args),
        "teams" => parse_teams(&args),
        "worktree" => parse_worktree(&args),
        "batch" => parse_batch(&args),
        "parallel" => parse_parallel(&args),
        "automation" => parse_automation(&args),
        "docs" => parse_docs(&args),
        "serve" => match parse_serve(&args) {
            Parsed::Run(Verb::Serve(serve)) => Parsed::Info(format!(
                "the editor server belongs in its own terminal — run: memnoth serve --host {} --port {}",
                serve.host, serve.port
            )),
            other => other,
        },
        "watch" => match parse_watch(&args) {
            // `--once` is finite, so it can run here without freezing the session.
            Parsed::Run(Verb::Watch(watch)) if watch.once => Parsed::Run(Verb::Watch(watch)),
            Parsed::Run(Verb::Watch(watch)) => Parsed::Info(format!(
                "watching runs until interrupted — run: memnoth watch --debounce {}",
                watch.debounce
            )),
            other => other,
        },
        "init" => Parsed::Init,
        "memory" => Parsed::Memory,
        "theme" | "set-theme" => {
            if args.is_empty() {
                Parsed::Usage("usage: /theme <default|high-contrast|no-color>".to_string())
            } else {
                match args[0].as_str() {
                    "default" => Parsed::Run(Verb::SetTheme(crate::tui::app::Theme::Default)),
                    "high-contrast" => {
                        Parsed::Run(Verb::SetTheme(crate::tui::app::Theme::HighContrast))
                    }
                    "no-color" | "nocolor" => {
                        Parsed::Run(Verb::SetTheme(crate::tui::app::Theme::NoColor))
                    }
                    other => Parsed::Unknown(format!(
                        "unknown theme: {other} — use default, high-contrast, or no-color"
                    )),
                }
            }
        }
        "screen-reader" | "set-screen-reader" => {
            if args.is_empty() {
                Parsed::Usage("usage: /screen-reader <off|plain>".to_string())
            } else {
                match args[0].as_str() {
                    "off" => Parsed::Run(Verb::SetScreenReader(
                        crate::tui::app::ScreenReaderMode::Off,
                    )),
                    "plain" => Parsed::Run(Verb::SetScreenReader(
                        crate::tui::app::ScreenReaderMode::Plain,
                    )),
                    other => Parsed::Unknown(format!(
                        "unknown screen reader mode: {other} — use off or plain"
                    )),
                }
            }
        }
        "skill" | "skills" => parse_skill(&args),
        "sandbox" => {
            if args.is_empty() {
                Parsed::Usage("usage: /sandbox <test <cmd>|setup>".to_string())
            } else {
                match args[0].as_str() {
                    "test" => {
                        if args.len() < 2 {
                            Parsed::Usage("usage: /sandbox test <cmd>".to_string())
                        } else {
                            Parsed::Run(Verb::Sandbox(crate::cli::SandboxArgs {
                                command: crate::cli::SandboxCommand::Test {
                                    command: args[1..].join(" "),
                                },
                            }))
                        }
                    }
                    "setup" => Parsed::Run(Verb::Sandbox(crate::cli::SandboxArgs {
                        command: crate::cli::SandboxCommand::Setup,
                    })),
                    other => Parsed::Unknown(format!(
                        "unknown sandbox subcommand: {other} — use test <cmd> or setup"
                    )),
                }
            }
        }
        "budget" => Parsed::Run(Verb::Budget(crate::cli::BudgetArgs {})),
        "trust" => match args.first().map(String::as_str) {
            Some("list") => Parsed::Run(Verb::Trust(crate::cli::TrustArgs {
                action: crate::cli::TrustCommand::List,
            })),
            Some("forget") => match split_flags(&args[1..]).1.first() {
                Some(path) => Parsed::Run(Verb::Trust(crate::cli::TrustArgs {
                    action: crate::cli::TrustCommand::Forget(crate::cli::TrustForgetArgs {
                        path: path.clone(),
                    }),
                })),
                None => Parsed::Usage("usage: /trust forget <path>".to_string()),
            },
            _ => Parsed::Usage("usage: /trust <list|forget <path>>".to_string()),
        },
        "extension" => match args.first().map(String::as_str) {
            Some("list") => Parsed::Run(Verb::Extension(crate::cli::ExtensionArgs {
                action: crate::cli::ExtensionCommand::List(crate::cli::ExtensionListArgs {
                    json: split_flags(&args).0.contains_key("json"),
                }),
            })),
            Some("doctor") => Parsed::Run(Verb::Extension(crate::cli::ExtensionArgs {
                action: crate::cli::ExtensionCommand::Doctor(crate::cli::ExtensionDoctorArgs {
                    json: split_flags(&args).0.contains_key("json"),
                }),
            })),
            Some("new") => match split_flags(&args[1..]).1.first() {
                Some(path) => Parsed::Run(Verb::Extension(crate::cli::ExtensionArgs {
                    action: crate::cli::ExtensionCommand::New(crate::cli::ExtensionNewArgs {
                        path: path.clone(),
                    }),
                })),
                None => Parsed::Usage("usage: /extension new <path>".to_string()),
            },
            Some("uninstall") => match split_flags(&args[1..]).1.first() {
                Some(name) => Parsed::Run(Verb::Extension(crate::cli::ExtensionArgs {
                    action: crate::cli::ExtensionCommand::Uninstall(
                        crate::cli::ExtensionUninstallArgs { name: name.clone() },
                    ),
                })),
                None => Parsed::Usage("usage: /extension uninstall <name>".to_string()),
            },
            _ => Parsed::Usage(
                "usage: /extension <new <path>|list|uninstall <name>|doctor>".to_string(),
            ),
        },
        "session" => parse_session(&args),
        "ask" => parse_ask(&args),
        "auth" => parse_auth(&args),
        "config" => parse_config(&args),
        "cloud" => parse_cloud(&args),
        "billing" => parse_billing(&args),
        "privacy" => parse_privacy(&args),
        "telemetry" => parse_telemetry(&args),
        "audit" => parse_audit(&args),
        "receipt" => parse_receipt(&args),
        "bench" => parse_bench(&args),
        "agents" => parse_agents(&args),
        "completion" => parse_completion(&args),
        "version" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Version(VersionArgs {
                json: flags.contains_key("json"),
            }))
        }
        "schema" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Schema(SchemaArgs {
                output: flags.get("path").map(PathBuf::from),
                check: flags.contains_key("check"),
            }))
        }
        "install" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Install(InstallArgs {
                force: flags.contains_key("force"),
            }))
        }
        "uninstall" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Uninstall(UninstallArgs {
                keep_state: flags.contains_key("keep-state"),
                yes: flags.contains_key("yes"),
                dry_run: flags.contains_key("dry-run"),
            }))
        }
        "open" => {
            let (flags, positionals) = split_flags(&args);
            match flags
                .get("page")
                .cloned()
                .or_else(|| positionals.first().cloned())
            {
                Some(page) => Parsed::Run(Verb::Open(OpenArgs {
                    page,
                    project_id: None,
                    session_id: None,
                    no_browser: false,
                    json: false,
                })),
                None => Parsed::Usage(
                    "usage: /open <page> (console, dashboard, usage, billing)".to_string(),
                ),
            }
        }
        "setup" => {
            let (flags, _) = split_flags(&args);
            Parsed::Run(Verb::Setup(SetupArgs {
                skip: flags
                    .get("skip")
                    .map(|value| value.split(',').map(str::to_string).collect())
                    .unwrap_or_default(),
                yes: flags.contains_key("yes"),
                json: flags.contains_key("json"),
            }))
        }
        "demo" => Parsed::Run(Verb::Demo),
        "status" => Parsed::Run(Verb::Session(SessionArgs {
            action: SessionCommand::Status(StatusArgs {
                name: None,
                json: false,
            }),
        })),
        other => Parsed::Unknown(if suggest {
            unknown_command(other)
        } else {
            format!("unknown command /{other}")
        }),
    }
}

/// `/session` and its subcommands. Bare `/session` lists.
fn parse_session(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Run(Verb::Session(SessionArgs {
            action: SessionCommand::List(ListArgs { json: false }),
        }));
    };
    match action.as_str() {
        "start" => {
            let (flags, positionals) = split_flags(&args[1..]);
            Parsed::Run(Verb::Session(SessionArgs {
                action: SessionCommand::Start(StartArgs {
                    name: flags
                        .get("name")
                        .cloned()
                        .or_else(|| positionals.first().cloned()),
                    resume: flags.get("resume").cloned(),
                    model: flags.get("model").cloned(),
                }),
            }))
        }
        "resume" => match args.get(1) {
            Some(name) => Parsed::Run(Verb::Session(SessionArgs {
                action: SessionCommand::Resume(ResumeArgs { name: name.clone() }),
            })),
            None => Parsed::Usage("usage: /session resume <name>".to_string()),
        },
        "list" => Parsed::Run(Verb::Session(SessionArgs {
            action: SessionCommand::List(ListArgs { json: false }),
        })),
        "status" => Parsed::Run(Verb::Session(SessionArgs {
            action: SessionCommand::Status(StatusArgs {
                name: args.get(1).cloned(),
                json: false,
            }),
        })),
        "stop" => Parsed::Run(Verb::Session(SessionArgs {
            action: SessionCommand::Stop(StopArgs {
                name: args.get(1).cloned(),
                force: false,
            }),
        })),
        other => Parsed::Unknown(format!(
            "unknown session command {other:?} — use start, resume, list, status or stop"
        )),
    }
}

/// `/ask <prompt>` — one shot, printed in the transcript.
fn parse_ask(args: &[String]) -> Parsed {
    let (flags, positionals) = split_flags(args);
    let prompt = flags.get("prompt").cloned();
    if prompt.is_none() && positionals.iter().all(|word| word.trim().is_empty()) {
        return Parsed::Usage("usage: /ask <prompt>".to_string());
    }
    Parsed::Run(Verb::Ask(AskArgs {
        words: positionals,
        prompt,
        output_format: flags
            .get("output-format")
            .cloned()
            .unwrap_or_else(|| "text".to_string()),
    }))
}

/// `/auth` and its subcommands.
fn parse_auth(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Usage("usage: /auth <login|logout|status|whoami>".to_string());
    };
    match action.as_str() {
        "login" => {
            let (flags, _) = split_flags(&args[1..]);
            Parsed::Run(Verb::Auth(AuthArgs {
                action: AuthCommand::Login(AuthLoginArgs {
                    no_browser: flags.contains_key("no-browser"),
                    with_token: flags.get("with-token").cloned(),
                    client_name: flags.get("client-name").cloned(),
                    tier: flags.get("tier").cloned(),
                }),
            }))
        }
        "logout" => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Logout(AuthLogoutArgs {}),
        })),
        "status" => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Status(AuthStatusArgs {}),
        })),
        "whoami" => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Whoami(WhoamiArgs {}),
        })),
        other => Parsed::Unknown(format!(
            "unknown auth command {other:?} — use login, logout, status or whoami"
        )),
    }
}

/// `/config` and its subcommands. Bare `/config` opens the editor.
fn parse_config(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Run(Verb::Config(ConfigArgs {
            action: ConfigCommand::Edit(ConfigEditArgs { json: false }),
        }));
    };
    match action.as_str() {
        "get" => match args.get(1) {
            Some(key) => Parsed::Run(Verb::Config(ConfigArgs {
                action: ConfigCommand::Get(ConfigGetArgs {
                    key: key.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage("usage: /config get <key>".to_string()),
        },
        "set" => match (args.get(1), args.get(2)) {
            (Some(key), Some(value)) => Parsed::Run(Verb::Config(ConfigArgs {
                action: ConfigCommand::Set(ConfigSetArgs {
                    key: key.clone(),
                    value: value.clone(),
                    json: false,
                }),
            })),
            _ => Parsed::Usage("usage: /config set <key> <value>".to_string()),
        },
        "list" => Parsed::Run(Verb::Config(ConfigArgs {
            action: ConfigCommand::List(ConfigListArgs { json: false }),
        })),
        "explain" => match args.get(1) {
            Some(key) => Parsed::Run(Verb::Config(ConfigArgs {
                action: ConfigCommand::Explain(ConfigExplainArgs {
                    key: key.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage("usage: /config explain <key>".to_string()),
        },
        "edit" => Parsed::Run(Verb::Config(ConfigArgs {
            action: ConfigCommand::Edit(ConfigEditArgs { json: false }),
        })),
        other => Parsed::Unknown(format!(
            "unknown config command {other:?} — use get, set, list, explain or edit"
        )),
    }
}

/// `/cloud` and its subcommands: the profile this session reads.
fn parse_cloud(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("login") => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Login(AuthLoginArgs {
                no_browser: false,
                with_token: None,
                client_name: None,
                tier: None,
            }),
        })),
        Some("logout") => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Logout(AuthLogoutArgs {}),
        })),
        Some("status") => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Status(AuthStatusArgs {}),
        })),
        Some("whoami") => Parsed::Run(Verb::Auth(AuthArgs {
            action: AuthCommand::Whoami(WhoamiArgs {}),
        })),
        Some("sync") => {
            let (flags, _) = split_flags(&args[1..]);
            Parsed::Run(Verb::Cloud(CloudArgs {
                action: CloudCommand::Sync(SyncArgs {
                    dry_run: flags.contains_key("dry-run"),
                    watch: false,
                    json: flags.contains_key("json"),
                }),
            }))
        }
        Some("credits") => Parsed::Run(Verb::Cloud(CloudArgs {
            action: CloudCommand::Credits(CreditsArgs { json: false }),
        })),
        Some("billing") => {
            Parsed::Usage("usage: /cloud billing — see /billing <invoices|plan|limits>".to_string())
        }
        Some("profile") => match args.get(1).map(String::as_str) {
            Some("set") => match (args.get(2), args.get(3)) {
                (Some(key), Some(value)) => Parsed::Run(Verb::Cloud(CloudArgs {
                    action: CloudCommand::Profile(ProfileArgs {
                        action: ProfileCommand::Set(ProfileSetArgs {
                            key: key.clone(),
                            value: value.clone(),
                            json: false,
                        }),
                    }),
                })),
                _ => Parsed::Usage("usage: /cloud profile set <key> <value>".to_string()),
            },
            _ => Parsed::Run(Verb::Cloud(CloudArgs {
                action: CloudCommand::Profile(ProfileArgs {
                    action: ProfileCommand::Show(ProfileShowArgs { json: false }),
                }),
            })),
        },
        Some(_) => Parsed::Usage("usage: /cloud <login|logout|status|whoami|profile>".to_string()),
        None => Parsed::Usage("usage: /cloud <login|logout|status|whoami|profile>".to_string()),
    }
}

/// `/billing` and its subcommands.
fn parse_billing(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("invoices") => Parsed::Run(Verb::Billing(BillingCommands {
            action: BillingCommand::Invoices(BillingInvoicesArgs { json: false }),
        })),
        Some("plan") => Parsed::Run(Verb::Billing(BillingCommands {
            action: BillingCommand::Plan(BillingPlanArgs { json: false }),
        })),
        Some("limits") => Parsed::Run(Verb::Billing(BillingCommands {
            action: BillingCommand::Limits(BillingLimitsArgs { json: false }),
        })),
        Some("credits") => Parsed::Run(Verb::Cloud(CloudArgs {
            action: CloudCommand::Credits(CreditsArgs { json: false }),
        })),
        _ => Parsed::Usage("usage: /billing <invoices|plan|limits>".to_string()),
    }
}

/// `/privacy` and its subcommands.
fn parse_privacy(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("status") => Parsed::Run(Verb::Privacy(PrivacyArgs {
            action: PrivacyCommand::Status(PrivacyStatusArgs { json: false }),
        })),
        Some("on") => Parsed::Run(Verb::Privacy(PrivacyArgs {
            action: PrivacyCommand::On(PrivacyOnArgs { json: false }),
        })),
        Some("off") => Parsed::Run(Verb::Privacy(PrivacyArgs {
            action: PrivacyCommand::Off(PrivacyOffArgs { json: false }),
        })),
        Some("mode") => match args.get(1) {
            Some(mode) => Parsed::Run(Verb::Privacy(PrivacyArgs {
                action: PrivacyCommand::Mode(PrivacyModeArgs {
                    mode: mode.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage("usage: /privacy mode <mode>".to_string()),
        },
        Some("show-data") => Parsed::Run(Verb::Privacy(PrivacyArgs {
            action: PrivacyCommand::ShowData(PrivacyShowDataArgs { json: false }),
        })),
        _ => Parsed::Usage("usage: /privacy <status|on|off|mode|show-data>".to_string()),
    }
}

/// `/telemetry` and its subcommands.
fn parse_telemetry(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("status") => Parsed::Run(Verb::Telemetry(TelemetryArgs {
            action: TelemetryCommand::Status(TelemetryStatusArgs { json: false }),
        })),
        Some("on") => Parsed::Run(Verb::Telemetry(TelemetryArgs {
            action: TelemetryCommand::On(TelemetryOnArgs { json: false }),
        })),
        Some("off") => Parsed::Run(Verb::Telemetry(TelemetryArgs {
            action: TelemetryCommand::Off(TelemetryOffArgs { json: false }),
        })),
        _ => Parsed::Usage("usage: /telemetry <status|on|off>".to_string()),
    }
}

/// `/audit` and its subcommands.
fn parse_audit(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("show") => Parsed::Run(Verb::Audit(AuditArgs {
            action: AuditCommand::Show(AuditShowArgs {
                limit: 50,
                json: false,
            }),
        })),
        Some("verify") => Parsed::Run(Verb::Audit(AuditArgs {
            action: AuditCommand::Verify(AuditVerifyArgs { json: false }),
        })),
        Some("export") => Parsed::Run(Verb::Audit(AuditArgs {
            action: AuditCommand::Export(AuditExportArgs {
                out: None,
                json: false,
            }),
        })),
        _ => Parsed::Usage("usage: /audit <show|verify|export>".to_string()),
    }
}

/// `/receipt` and its subcommands.
fn parse_receipt(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("write") => Parsed::Run(Verb::Receipt(ReceiptArgs {
            action: ReceiptCommand::Write(ReceiptWriteArgs {
                commit: None,
                json: false,
            }),
        })),
        Some("show") => Parsed::Run(Verb::Receipt(ReceiptArgs {
            action: ReceiptCommand::Show(ReceiptShowArgs {
                commit: None,
                json: false,
            }),
        })),
        Some("verify") => Parsed::Run(Verb::Receipt(ReceiptArgs {
            action: ReceiptCommand::Verify(ReceiptVerifyArgs {
                commit: None,
                json: false,
            }),
        })),
        _ => Parsed::Usage("usage: /receipt <write|show|verify>".to_string()),
    }
}

/// `/agents` and its subcommands.
fn parse_agents(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("run") => match args.get(1).cloned() {
            Some(task) => Parsed::Run(Verb::Agents(AgentsArgs {
                action: AgentsCommand::Run(AgentRunArgs { task, json: false }),
            })),
            None => Parsed::Usage("usage: /agents run <task>".to_string()),
        },
        Some("list") => Parsed::Run(Verb::Agents(AgentsArgs {
            action: AgentsCommand::List(AgentListArgs { json: false }),
        })),
        Some("result") => match args.get(1) {
            Some(id) => Parsed::Run(Verb::Agents(AgentsArgs {
                action: AgentsCommand::Result(AgentResultArgs {
                    id: id.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage("usage: /agents result <id>".to_string()),
        },
        Some("unpack") => match args.get(1) {
            Some(id) => Parsed::Run(Verb::Agents(AgentsArgs {
                action: AgentsCommand::Unpack(AgentUnpackArgs {
                    id: id.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage("usage: /agents unpack <id>".to_string()),
        },
        _ => Parsed::Usage("usage: /agents <run|list|result|unpack>".to_string()),
    }
}

/// `/completion` and its subcommands.
fn parse_completion(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("show") => match args.get(1) {
            Some(shell) => Parsed::Run(Verb::Completion(CompletionArgs {
                action: CompletionCommand::Show(CompletionShowArgs {
                    shell: shell.clone(),
                    output: None,
                    check: false,
                    json: false,
                }),
            })),
            None => Parsed::Usage("usage: /completion show <shell>".to_string()),
        },
        Some("install") => match args.get(1) {
            Some(shell) => Parsed::Run(Verb::Completion(CompletionArgs {
                action: CompletionCommand::Install(CompletionInstallArgs {
                    shell: Some(shell.clone()),
                    json: false,
                }),
            })),
            None => Parsed::Usage("usage: /completion install <shell>".to_string()),
        },
        Some("uninstall") => match args.get(1) {
            Some(shell) => Parsed::Run(Verb::Completion(CompletionArgs {
                action: CompletionCommand::Uninstall(CompletionUninstallArgs {
                    shell: Some(shell.clone()),
                }),
            })),
            None => Parsed::Usage("usage: /completion uninstall <shell>".to_string()),
        },
        _ => Parsed::Usage("usage: /completion <show|install|uninstall> <shell>".to_string()),
    }
}

/// `/bench` and its subcommands.
fn parse_bench(args: &[String]) -> Parsed {
    use crate::bench::{AblationArgs, BenchArgs, BenchCommand, RunArgs, ScoreboardArgs, Suite};
    match args.first().map(String::as_str) {
        Some("run") => {
            let (flags, _) = split_flags(&args[1..]);
            let suite = match flags.get("suite").map(String::as_str) {
                Some("trs") => Suite::Trs,
                Some("kg") => Suite::Kg,
                _ => Suite::Mts,
            };
            Parsed::Run(Verb::Bench(BenchArgs {
                command: BenchCommand::Run(RunArgs {
                    suite,
                    dataset: None,
                    model: flags.get("model").cloned(),
                    effort: flags.get("effort").cloned(),
                    n: flags
                        .get("n")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(3),
                    seed: 20_261_110,
                    bootstrap: 10_000,
                    ci_level: 0.95,
                    output: flags.get("output").map(PathBuf::from),
                    ablation: flags.contains_key("ablation"),
                    fail_below: None,
                }),
            }))
        }
        Some("scoreboard") => {
            let (flags, positionals) = split_flags(&args[1..]);
            let input = flags
                .get("input")
                .cloned()
                .or_else(|| positionals.first().cloned());
            match input {
                Some(input) => Parsed::Run(Verb::Bench(BenchArgs {
                    command: BenchCommand::Scoreboard(ScoreboardArgs {
                        input: PathBuf::from(input),
                        regenerate: flags.contains_key("regenerate"),
                    }),
                })),
                None => Parsed::Usage("usage: /bench scoreboard --input <file>".to_string()),
            }
        }
        Some("ablation") => {
            let (flags, _) = split_flags(&args[1..]);
            Parsed::Run(Verb::Bench(BenchArgs {
                command: BenchCommand::Ablation(AblationArgs {
                    input: flags.get("input").map(PathBuf::from),
                    run_pending: flags.contains_key("run-pending"),
                }),
            }))
        }
        _ => Parsed::Usage("usage: /bench <run|scoreboard|ablation>".to_string()),
    }
}

/// `/tasks` and its subcommands.
fn parse_tasks(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Tasks(TasksCommand::Show);
    };
    match action.as_str() {
        "run" => {
            let command = args[1..].join(" ");
            if command.trim().is_empty() {
                Parsed::Usage("usage: /tasks run <command>".to_string())
            } else {
                Parsed::Tasks(TasksCommand::Run(command))
            }
        }
        "status" | "list" => Parsed::Tasks(TasksCommand::Status),
        "logs" | "log" => Parsed::Tasks(TasksCommand::Logs(args.get(1).cloned())),
        "cancel" | "stop" | "kill" => match args.get(1) {
            Some(id) => Parsed::Tasks(TasksCommand::Cancel(id.clone())),
            None => Parsed::Usage("usage: /tasks cancel <id>".to_string()),
        },
        other => Parsed::Unknown(format!(
            "unknown tasks command {other:?} — use run, status, logs, or cancel"
        )),
    }
}

/// A miss that names the command closest to what was typed.
///
/// The palette is the list of things that exist, so suggestions come from it
/// rather than from a second hand-written list that can drift.
fn unknown_command(typed: &str) -> String {
    let mut heads: Vec<String> = crate::tui::overlays::palette::commands()
        .into_iter()
        .filter_map(|item| item.command.strip_prefix('/').map(str::to_string))
        .filter_map(|command| command.split_whitespace().next().map(str::to_string))
        .collect();
    heads.sort();
    heads.dedup();

    let closest = heads
        .into_iter()
        .map(|head| {
            let distance = edit_distance(typed, &head);
            (distance, head)
        })
        .filter(|(distance, _)| *distance <= 2)
        .min();

    match closest {
        Some((_, head)) => format!("unknown command /{typed} — did you mean /{head}? type /help"),
        None => format!("unknown command /{typed} — type /help or press ctrl-k for the palette"),
    }
}

/// Levenshtein distance, capped at the caller's threshold by simple DP.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            let cost = usize::from(left != right);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

fn parse_skill(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Usage(SKILL_USAGE.to_string());
    };
    let rest = &args[1..];
    match action.as_str() {
        "add" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /skill add <path> [--name <name>]".to_string());
            }
            let path = positional[0].clone();
            let name = flags.get("name").cloned();
            Parsed::Run(Verb::Skills(SkillArgs {
                action: SkillCommand::Add(SkillAddArgs { path, name }),
            }))
        }
        "list" => {
            let (flags, _positional) = split_flags(rest);
            let detail = flags.contains_key("detail") || flags.contains_key("d");
            let tag = flags.get("tag").cloned();
            let json = flags.contains_key("json");
            Parsed::Run(Verb::Skills(SkillArgs {
                action: SkillCommand::List(SkillListArgs { detail, tag, json }),
            }))
        }
        "remove" | "rm" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage(
                    "usage: /skill remove <name> [--force] [--yes] [--dry-run]".to_string(),
                );
            }
            let name = positional[0].clone();
            let force = flags.contains_key("force") || flags.contains_key("f");
            let yes = flags.contains_key("yes");
            let dry_run = flags.contains_key("dry-run");
            Parsed::Run(Verb::Skills(SkillArgs {
                action: SkillCommand::Remove(SkillRemoveArgs {
                    name,
                    yes,
                    dry_run,
                    force,
                }),
            }))
        }
        "run" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /skill run <name> [KEY=VALUE...]".to_string());
            }
            let name = positional[0].clone();
            let skill_args = positional[1..].to_vec();
            let json = flags.contains_key("json");
            Parsed::Run(Verb::Skills(SkillArgs {
                action: SkillCommand::Run(SkillRunArgs {
                    name,
                    args: skill_args,
                    json,
                }),
            }))
        }
        "edit" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /skill edit <name> [--editor <editor>]".to_string());
            }
            let name = positional[0].clone();
            let editor = flags
                .get("editor")
                .cloned()
                .or_else(|| flags.get("e").cloned());
            Parsed::Run(Verb::Skills(SkillArgs {
                action: SkillCommand::Edit(SkillEditArgs { name, editor }),
            }))
        }
        other => Parsed::Unknown(format!("unknown skill command {other:?}\n{SKILL_USAGE}")),
    }
}

fn parse_vault(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Usage(VAULT_USAGE.to_string());
    };
    let rest = &args[1..];
    match action.as_str() {
        "init" => Parsed::Run(Verb::Vault(VaultArgs {
            action: VaultCommand::Init,
        })),
        "status" => Parsed::Run(Verb::Vault(VaultArgs {
            action: VaultCommand::Status,
        })),
        "asof" => {
            let (flags, positional) = split_flags(rest);
            let at = flags
                .get("at")
                .cloned()
                .or_else(|| positional.first().cloned());
            let name = flags.get("name").cloned();
            let limit = flags
                .get("limit")
                .and_then(|value| value.parse().ok())
                .unwrap_or(200);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::AsOf { at, name, limit },
            }))
        }
        "history" => match rest.first() {
            Some(name) => Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::History { name: name.clone() },
            })),
            None => Parsed::Usage("usage: /vault history <name>".to_string()),
        },
        "contradictions" => {
            let (flags, positional) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Contradictions {
                    name: flags
                        .get("name")
                        .cloned()
                        .or_else(|| positional.first().cloned()),
                },
            }))
        }
        "remember" => parse_remember(rest),
        "forget" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage(
                    "usage: /vault forget <name> [--reason ...] [--apply]".to_string(),
                );
            }
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Forget {
                    name: positional[0].clone(),
                    reason: flags
                        .get("reason")
                        .cloned()
                        .unwrap_or_else(|| "forgotten from session".to_string()),
                    apply: flags.contains_key("apply"),
                },
            }))
        }
        "correct" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage(
                    "usage: /vault correct <name> [--summary ...] [--reason ...]".to_string(),
                );
            }
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Correct {
                    name: positional[0].clone(),
                    new_name: flags.get("new-name").cloned(),
                    summary: flags.get("summary").cloned(),
                    confidence: flags.get("confidence").and_then(|value| value.parse().ok()),
                    reason: flags
                        .get("reason")
                        .cloned()
                        .unwrap_or_else(|| "corrected from session".to_string()),
                },
            }))
        }
        "recall" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /vault recall <query>".to_string());
            }
            let limit = flags
                .get("limit")
                .and_then(|value| value.parse().ok())
                .unwrap_or(10);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Recall {
                    query: positional,
                    limit,
                    at: flags.get("at").cloned(),
                },
            }))
        }
        "link" => {
            let (flags, positional) = split_flags(rest);
            if positional.len() < 3 {
                return Parsed::Usage(
                    "usage: /vault link <source> <target> <kind> [--confidence 1.0] [--reason ...]"
                        .to_string(),
                );
            }
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Link {
                    source: positional[0].clone(),
                    target: positional[1].clone(),
                    kind: positional[2].clone(),
                    confidence: flags
                        .get("confidence")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(1.0),
                    reason: flags
                        .get("reason")
                        .cloned()
                        .unwrap_or_else(|| "linked from session".to_string()),
                },
            }))
        }
        "unlink" => {
            let (flags, positional) = split_flags(rest);
            if positional.len() < 3 {
                return Parsed::Usage(
                    "usage: /vault unlink <source> <target> <kind> [--reason ...]".to_string(),
                );
            }
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Unlink {
                    source: positional[0].clone(),
                    target: positional[1].clone(),
                    kind: positional[2].clone(),
                    reason: flags
                        .get("reason")
                        .cloned()
                        .unwrap_or_else(|| "unlinked from session".to_string()),
                },
            }))
        }
        "search" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /vault search <query>".to_string());
            }
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Search {
                    query: positional,
                    limit: flags
                        .get("limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(10),
                    semantic: flags.contains_key("semantic"),
                    at: flags.get("at").cloned(),
                },
            }))
        }
        "export" => {
            let (flags, positional) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Export {
                    output: flags
                        .get("output")
                        .or_else(|| flags.get("o"))
                        .cloned()
                        .or_else(|| positional.first().cloned()),
                    format: flags
                        .get("format")
                        .cloned()
                        .unwrap_or_else(|| "json".to_string()),
                    include_history: flags.contains_key("include-history")
                        || flags.contains_key("history"),
                    kind: flags.get("kind").cloned(),
                },
            }))
        }
        "import" => {
            let (flags, positional) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Import {
                    input: flags
                        .get("input")
                        .or_else(|| flags.get("i"))
                        .cloned()
                        .or_else(|| positional.first().cloned()),
                    format: flags
                        .get("format")
                        .cloned()
                        .unwrap_or_else(|| "json".to_string()),
                    dry_run: flags.contains_key("dry-run"),
                    on_conflict: flags
                        .get("on-conflict")
                        .cloned()
                        .unwrap_or_else(|| "skip".to_string()),
                },
            }))
        }
        "gc" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Gc {
                    keep: flags
                        .get("keep")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(20),
                    apply: flags.contains_key("apply"),
                },
            }))
        }
        "prune" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Prune {
                    keep_days: flags
                        .get("keep-days")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(365),
                    apply: flags.contains_key("apply"),
                },
            }))
        }
        "reindex" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Reindex {
                    embeddings: flags.contains_key("embeddings"),
                    full: flags.contains_key("full"),
                },
            }))
        }
        "stats" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Stats {
                    json: flags.contains_key("json"),
                    growth: flags.contains_key("growth"),
                },
            }))
        }
        "backup" => {
            let (flags, positional) = split_flags(rest);
            let Some(output) = flags
                .get("output")
                .or_else(|| flags.get("o"))
                .cloned()
                .or_else(|| positional.first().cloned())
            else {
                return Parsed::Usage("usage: /vault backup <file>".to_string());
            };
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Backup {
                    output,
                    include_wal: flags.contains_key("include-wal"),
                    compress: flags.contains_key("compress"),
                },
            }))
        }
        "restore" => {
            let (flags, positional) = split_flags(rest);
            let Some(input) = flags
                .get("input")
                .or_else(|| flags.get("i"))
                .cloned()
                .or_else(|| positional.first().cloned())
            else {
                return Parsed::Usage("usage: /vault restore <file>".to_string());
            };
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Restore {
                    input,
                    verify: !flags.contains_key("no-verify"),
                    force: flags.contains_key("force"),
                },
            }))
        }
        "audit" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Audit {
                    limit: flags
                        .get("limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(50),
                    kind: flags.get("kind").cloned(),
                    json: flags.contains_key("json"),
                },
            }))
        }
        "merge" => {
            let (flags, positional) = split_flags(rest);
            if positional.len() < 2 {
                return Parsed::Usage(
                    "usage: /vault merge <from> <into> [--reason ...]".to_string(),
                );
            }
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Merge {
                    from: positional[0].clone(),
                    into: positional[1].clone(),
                    reason: flags
                        .get("reason")
                        .cloned()
                        .unwrap_or_else(|| "merged from session".to_string()),
                },
            }))
        }
        "duplicates" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Duplicates {
                    at: flags.get("at").cloned(),
                    json: flags.contains_key("json"),
                },
            }))
        }
        "top" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Top {
                    limit: flags
                        .get("limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(20),
                    kind: flags.get("kind").cloned(),
                    at: flags.get("at").cloned(),
                },
            }))
        }
        "timeline" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Timeline {
                    limit: flags
                        .get("limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(50),
                    at: flags.get("at").cloned(),
                    kind: flags.get("kind").cloned(),
                    include_observations: flags.contains_key("include-observations"),
                },
            }))
        }
        "explain" => match rest.first() {
            Some(name) => Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Explain {
                    name: name.clone(),
                    at: None,
                },
            })),
            None => Parsed::Usage("usage: /vault explain <name> [--at WHEN]".to_string()),
        },
        "graph" => {
            let (flags, positional) = split_flags(rest);
            let Some(name) = positional.first() else {
                return Parsed::Usage("usage: /vault graph <name> [--depth 2]".to_string());
            };
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Graph {
                    name: name.clone(),
                    depth: flags
                        .get("depth")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(2),
                    at: flags.get("at").cloned(),
                    json: flags.contains_key("json"),
                },
            }))
        }
        other => Parsed::Unknown(format!("unknown vault command {other:?}\n{VAULT_USAGE}")),
    }
}

fn parse_remember(rest: &[String]) -> Parsed {
    let mut iter = rest.iter();
    let Some(name) = iter.next() else {
        return Parsed::Usage("usage: /vault remember <name> [--kind K] <fact…>".to_string());
    };
    if name.starts_with("--") {
        return Parsed::Usage("usage: /vault remember <name> [--kind K] <fact…>".to_string());
    }
    let mut kind = "fact".to_string();
    let mut confidence = None;
    let mut entity_kind = "concept".to_string();
    let mut text = Vec::new();
    while let Some(token) = iter.next() {
        match token.as_str() {
            "--kind" => kind = iter.next().cloned().unwrap_or_else(|| "fact".into()),
            "--confidence" => confidence = iter.next().and_then(|value| value.parse().ok()),
            "--entity-kind" => {
                entity_kind = iter.next().cloned().unwrap_or_else(|| "concept".into())
            }
            other if other.starts_with("--") => {
                let bare = &other[2..];
                if OBSERVATION_KINDS.contains(&bare) {
                    kind = bare.to_string();
                } else {
                    text.push(other.to_string());
                }
            }
            other => text.push(other.to_string()),
        }
    }
    if text.is_empty() {
        return Parsed::Usage("the fact is empty: /vault remember <name> <fact…>".to_string());
    }
    Parsed::Run(Verb::Vault(VaultArgs {
        action: VaultCommand::Remember {
            name: name.clone(),
            kind,
            confidence,
            entity_kind,
            text,
            // The TUI has no authored identity of its own; the write path
            // falls back to MEMNOTH_USER, then the OS user.
            author: None,
        },
    }))
}

fn parse_kg(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Usage(KG_USAGE.to_string());
    };
    let rest = &args[1..];
    match action.as_str() {
        "index" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Index {
                    full: flags.contains_key("full"),
                    no_embed: flags.contains_key("no-embed"),
                },
            }))
        }
        "status" => Parsed::Run(Verb::Kg(KgArgs {
            action: KgCommand::Status,
        })),
        "impact" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /kg impact <symbol>".to_string());
            }
            let depth = flags
                .get("depth")
                .and_then(|value| value.parse().ok())
                .unwrap_or(2);
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Impact {
                    symbol: positional.join(" "),
                    depth,
                },
            }))
        }
        "search" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /kg search <query>".to_string());
            }
            let limit = flags
                .get("limit")
                .and_then(|value| value.parse().ok())
                .unwrap_or(20);
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Search {
                    query: positional,
                    limit,
                },
            }))
        }
        "explore" => match rest.first() {
            Some(symbol) => Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Explore {
                    symbol: symbol.clone(),
                },
            })),
            None => Parsed::Usage("usage: /kg explore <symbol>".to_string()),
        },
        "refs" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /kg refs <symbol>".to_string());
            }
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Refs {
                    symbol: positional.join(" "),
                    limit: flags
                        .get("limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(100),
                },
            }))
        }
        "callers" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /kg callers <symbol>".to_string());
            }
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Callers {
                    symbol: positional.join(" "),
                    limit: flags
                        .get("limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(50),
                },
            }))
        }
        "callees" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /kg callees <symbol>".to_string());
            }
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Callees {
                    symbol: positional.join(" "),
                    limit: flags
                        .get("limit")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(50),
                },
            }))
        }
        "def" => match rest.first() {
            Some(symbol) => Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Def {
                    symbol: symbol.clone(),
                },
            })),
            None => Parsed::Usage("usage: /kg def <symbol>".to_string()),
        },
        "diff-impact" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::DiffImpact {
                    staged: flags.contains_key("staged"),
                },
            }))
        }
        "callpath" => {
            let (_, positional) = split_flags(rest);
            if positional.len() < 2 {
                return Parsed::Usage("usage: /kg callpath <from> <to>".to_string());
            }
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Callpath {
                    from: positional[0].clone(),
                    to: positional[1].clone(),
                },
            }))
        }
        "outline" => match rest.first() {
            Some(file) => Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Outline { file: file.clone() },
            })),
            None => Parsed::Usage("usage: /kg outline <file>".to_string()),
        },
        "tree" => match rest.first() {
            Some(dir) => Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Tree { dir: dir.clone() },
            })),
            None => Parsed::Usage("usage: /kg tree <dir>".to_string()),
        },
        "review" => {
            let (flags, _) = split_flags(rest);
            // `/kg review --base main` is the same verb the CLI has, so the
            // revision travels with it: `split_flags` keeps the value after the
            // flag, and `review` itself refuses `--staged --base` together.
            // A bare `--base` is asked for, not guessed: reviewing the working
            // tree because the revision was missing is the wrong answer.
            let base = flags.get("base").cloned();
            if base.as_deref() == Some("true") {
                return Parsed::Usage("usage: /kg review [--staged | --base <rev>]".to_string());
            }
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Review {
                    staged: flags.contains_key("staged"),
                    base,
                },
            }))
        }
        "tests-for" => match rest.first() {
            Some(symbol) => Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::TestsFor {
                    symbol: symbol.clone(),
                },
            })),
            None => Parsed::Usage("usage: /kg tests-for <symbol>".to_string()),
        },
        "owners" => match rest.first() {
            Some(path) => Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Owners { path: path.clone() },
            })),
            None => Parsed::Usage("usage: /kg owners <path>".to_string()),
        },
        "dead-code" => Parsed::Run(Verb::Kg(KgArgs {
            action: KgCommand::DeadCode,
        })),
        "cycles" => Parsed::Run(Verb::Kg(KgArgs {
            action: KgCommand::Cycles,
        })),
        "hotspots" => Parsed::Run(Verb::Kg(KgArgs {
            action: KgCommand::Hotspots,
        })),
        "unused-exports" => Parsed::Run(Verb::Kg(KgArgs {
            action: KgCommand::UnusedExports,
        })),
        "deps" => match rest.first() {
            Some(file) => Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Deps { file: file.clone() },
            })),
            None => Parsed::Usage("usage: /kg deps <file>".to_string()),
        },
        "query" => {
            let (_, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Usage("usage: /kg query <expr>".to_string());
            }
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Query {
                    expr: positional.join(" "),
                },
            }))
        }
        "watch" => Parsed::Run(Verb::Kg(KgArgs {
            action: KgCommand::Watch,
        })),
        "serve" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Serve {
                    port: flags
                        .get("port")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(3030),
                },
            }))
        }
        other => Parsed::Unknown(format!("unknown kg command {other:?}\n{KG_USAGE}")),
    }
}

fn parse_worktree(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Usage("usage: /worktree create|list|clean|diff ...".to_string());
    };
    let rest = &args[1..];
    match action.as_str() {
        "create" => {
            let (flags, positional) = split_flags(rest);
            let Some(name) = positional.first() else {
                return Parsed::Usage(
                    "usage: /worktree create <name> [--branch B] [--base B]".to_string(),
                );
            };
            Parsed::Run(Verb::Worktree(WorktreeArgs {
                action: WorktreeCommand::Create(WorktreeCreateArgs {
                    name: name.clone(),
                    branch: flags.get("branch").cloned(),
                    base: flags.get("base").cloned(),
                    json: flags.contains_key("json"),
                }),
            }))
        }
        "list" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Worktree(WorktreeArgs {
                action: WorktreeCommand::List(WorktreeListArgs {
                    json: flags.contains_key("json"),
                }),
            }))
        }
        "clean" => {
            let (flags, positional) = split_flags(rest);
            let Some(name) = positional.first() else {
                return Parsed::Usage(
                    "usage: /worktree clean <name> [--force] [--yes] [--dry-run]".to_string(),
                );
            };
            Parsed::Run(Verb::Worktree(WorktreeArgs {
                action: WorktreeCommand::Clean(WorktreeCleanArgs {
                    name: name.clone(),
                    yes: flags.contains_key("yes"),
                    dry_run: flags.contains_key("dry-run"),
                    force: flags.contains_key("force"),
                    json: flags.contains_key("json"),
                }),
            }))
        }
        "diff" => {
            let (flags, positional) = split_flags(rest);
            let Some(from) = positional.first() else {
                return Parsed::Usage("usage: /worktree diff <from> [to] [paths...]".to_string());
            };
            Parsed::Run(Verb::Worktree(WorktreeArgs {
                action: WorktreeCommand::Diff(WorktreeDiffArgs {
                    from: from.clone(),
                    to: positional.get(1).cloned(),
                    paths: positional.iter().skip(2).cloned().collect(),
                    json: flags.contains_key("json"),
                }),
            }))
        }
        other => Parsed::Usage(format!(
            "unknown worktree command {other:?}\nusage: /worktree create|list|clean|diff"
        )),
    }
}

fn parse_batch(args: &[String]) -> Parsed {
    let (flags, positional) = split_flags(args);
    if positional.is_empty() {
        return Parsed::Usage(
            "usage: /batch <task> [--workers N] [--command \"cmd args\"]".to_string(),
        );
    }
    Parsed::Run(Verb::Batch(BatchCmdArgs {
        task: positional.join(" "),
        workers: flags
            .get("workers")
            .and_then(|value| value.parse().ok())
            .unwrap_or(4),
        command: flags
            .get("command")
            .map(|value| value.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        claim_glob: flags.get("claim-glob").cloned(),
        claim_ttl: flags
            .get("claim-ttl")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1800),
        workdir: flags.get("workdir").cloned(),
        format: flags
            .get("format")
            .cloned()
            .unwrap_or_else(|| "text".to_string()),
        json: flags.contains_key("json"),
    }))
}

fn parse_parallel(args: &[String]) -> Parsed {
    let (flags, positional) = split_flags(args);
    let Some(command) = positional.first() else {
        return Parsed::Usage(
            "usage: /parallel <command> [targets...] [--max-concurrent N]".to_string(),
        );
    };
    Parsed::Run(Verb::Parallel(ParallelArgs {
        command: command.clone(),
        targets: positional.iter().skip(1).cloned().collect(),
        args: flags
            .get("args")
            .map(|value| value.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        max_concurrent: flags
            .get("max-concurrent")
            .and_then(|value| value.parse().ok())
            .unwrap_or(4),
        json: flags.contains_key("json"),
    }))
}

fn parse_automation(args: &[String]) -> Parsed {
    let Some(group) = args.first() else {
        return Parsed::Usage("usage: /automation hooks|action|ci ...".to_string());
    };
    let rest = &args[1..];
    match group.as_str() {
        "hooks" => {
            if rest.first().map(String::as_str) != Some("install") {
                return Parsed::Usage("usage: /automation hooks install [--force]".to_string());
            }
            let (flags, _) = split_flags(&rest[1..]);
            Parsed::Run(Verb::Automation(AutomationArgs {
                action: AutomationCommand::Hook(HookArgs {
                    action: HookCommand::Install(HookInstallArgs {
                        force: flags.contains_key("force"),
                        json: flags.contains_key("json"),
                    }),
                }),
            }))
        }
        "action" => {
            if rest.first().map(String::as_str) != Some("init") {
                return Parsed::Usage("usage: /automation action init [--name N]".to_string());
            }
            let (flags, _) = split_flags(&rest[1..]);
            Parsed::Run(Verb::Automation(AutomationArgs {
                action: AutomationCommand::Action(ActionArgs {
                    action: ActionCommand::Init(ActionInitArgs {
                        name: flags.get("name").cloned(),
                        kg_index: !flags.contains_key("no-kg-index"),
                        sync: !flags.contains_key("no-sync"),
                        force: flags.contains_key("force"),
                        json: flags.contains_key("json"),
                    }),
                }),
            }))
        }
        "ci" => match rest.first().map(String::as_str) {
            Some("detect") => {
                let (flags, _) = split_flags(&rest[1..]);
                Parsed::Run(Verb::Automation(AutomationArgs {
                    action: AutomationCommand::Ci(CiArgs {
                        action: CiCommand::Detect(CiDetectArgs {
                            json: flags.contains_key("json"),
                        }),
                    }),
                }))
            }
            Some("index") => {
                let (flags, _) = split_flags(&rest[1..]);
                Parsed::Run(Verb::Automation(AutomationArgs {
                    action: AutomationCommand::Ci(CiArgs {
                        action: CiCommand::Index(CiIndexArgs {
                            full: flags.contains_key("full"),
                            no_embed: flags.contains_key("no-embed"),
                            sync: flags.contains_key("sync"),
                            force: flags.contains_key("force"),
                            json: flags.contains_key("json"),
                        }),
                    }),
                }))
            }
            _ => Parsed::Usage("usage: /automation ci detect|index".to_string()),
        },
        other => Parsed::Usage(format!(
            "unknown automation group {other:?}\nusage: /automation hooks|action|ci"
        )),
    }
}

fn parse_docs(args: &[String]) -> Parsed {
    let (flags, _) = split_flags(args);
    Parsed::Run(Verb::Docs(DocsArgs {
        output: flags
            .get("output")
            .or_else(|| flags.get("o"))
            .map(PathBuf::from),
        man: flags.contains_key("man"),
        check: flags.contains_key("check"),
    }))
}

fn parse_serve(args: &[String]) -> Parsed {
    let (flags, _) = split_flags(args);
    Parsed::Run(Verb::Serve(ServeArgs {
        port: flags
            .get("port")
            .and_then(|value| value.parse().ok())
            .unwrap_or(3030),
        host: flags
            .get("host")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1".to_string()),
        cors: flags.contains_key("cors"),
        token: flags.get("token").cloned(),
        graph_path: flags.get("graph-path").cloned(),
        json: flags.contains_key("json"),
    }))
}

fn parse_watch(args: &[String]) -> Parsed {
    let (flags, _) = split_flags(args);
    Parsed::Run(Verb::Watch(WatchArgs {
        port: flags.get("port").and_then(|value| value.parse().ok()),
        debounce: flags
            .get("debounce")
            .and_then(|value| value.parse().ok())
            .unwrap_or(500),
        vault_changes: flags.contains_key("vault-changes"),
        once: flags.contains_key("once"),
        json: flags.contains_key("json"),
    }))
}

fn parse_mcp(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("doctor") => Parsed::Run(Verb::Mcp(McpArgs {
            action: McpCommand::Doctor,
        })),
        // Minting prints a bearer into the transcript, and the transcript is
        // a durable record: the token would outlive its own expiry in the
        // session log. Terminal-only by default (D26).
        Some("token") => Parsed::Info(
            "`mcp token` mints a bearer and prints it — run it in a terminal:\n  memnoth mcp token"
                .to_string(),
        ),
        Some("list") => Parsed::Run(Verb::Mcp(McpArgs {
            action: McpCommand::List,
        })),
        Some("test") => {
            let (flags, _) = split_flags(&args[1..]);
            Parsed::Run(Verb::Mcp(McpArgs {
                action: McpCommand::Test {
                    tool: flags.get("tool").cloned(),
                    profile: flags.get("profile").cloned(),
                },
            }))
        }
        Some("add" | "call" | "serve" | "selftest") => Parsed::Info(
            "`mcp serve`, `mcp add`, `mcp call` and `mcp selftest` need the real terminal or a \
             stdio pipe; run them from a shell:\n  memnoth mcp serve"
                .to_string(),
        ),
        _ => Parsed::Usage("usage: /mcp <doctor|token|list|test>".to_string()),
    }
}

/// The project verbs, as `/projects --help` would write them.
const PROJECTS_USAGE: &str = "usage: /projects <list | create <name> [--visibility org|restricted] | rename <id> <name> | \
     delete <id> --yes | use <id> | use --clear | unassigned | file <session> [--project ID]>";

/// `/projects` — the org's projects, from inside the session.
fn parse_projects(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Run(Verb::Projects(ProjectsArgs {
            action: ProjectsCommand::List(ProjectsListArgs { json: false }),
        }));
    };
    let rest = &args[1..];
    match action.as_str() {
        "list" => Parsed::Run(Verb::Projects(ProjectsArgs {
            action: ProjectsCommand::List(ProjectsListArgs { json: false }),
        })),
        "create" => {
            let (flags, positional) = split_flags(rest);
            match positional.first() {
                Some(name) => Parsed::Run(Verb::Projects(ProjectsArgs {
                    action: ProjectsCommand::Create(ProjectsCreateArgs {
                        name: name.clone(),
                        visibility: flags.get("visibility").cloned(),
                        json: false,
                    }),
                })),
                None => Parsed::Usage(PROJECTS_USAGE.to_string()),
            }
        }
        "rename" => {
            let (_, positional) = split_flags(rest);
            match (positional.first(), positional.get(1)) {
                (Some(id), Some(name)) => Parsed::Run(Verb::Projects(ProjectsArgs {
                    action: ProjectsCommand::Rename(ProjectsRenameArgs {
                        id: id.clone(),
                        name: name.clone(),
                        json: false,
                    }),
                })),
                _ => Parsed::Usage(PROJECTS_USAGE.to_string()),
            }
        }
        "delete" => {
            let (flags, positional) = split_flags(rest);
            let Some(id) = positional.first() else {
                return Parsed::Usage(PROJECTS_USAGE.to_string());
            };
            if !flags.contains_key("yes") {
                // Deleting asks a yes/no question on stdin, and the session already
                // owns the terminal: the prompt would be unanswerable, or worse,
                // answered by whatever key was pressed next. Here the answer is a
                // flag the person types on purpose.
                return Parsed::Usage(
                    "deleting a project asks for confirmation, and this session owns the \
                     terminal — answer it by typing the flag:\n  /projects delete <id> --yes"
                        .to_string(),
                );
            }
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Delete(ProjectsDeleteArgs {
                    id: id.clone(),
                    yes: true,
                    dry_run: false,
                    json: false,
                }),
            }))
        }
        // A bare `use` prints the current choice; `--clear` stops filing. The
        // id is checked against the org's list by the verb itself, so a typo is
        // answered with the projects that exist rather than stored.
        "use" => {
            let (flags, positional) = split_flags(rest);
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Use(ProjectsUseArgs {
                    id: positional.first().cloned(),
                    clear: flags.contains_key("clear"),
                    json: false,
                }),
            }))
        }
        // The queue the console calls "awaiting a project", and the verb that
        // empties it. Both live here because a person filing a session is in the
        // session, not on the console.
        "unassigned" => Parsed::Run(Verb::Projects(ProjectsArgs {
            action: ProjectsCommand::Unassigned(ProjectsUnassignedArgs { json: false }),
        })),
        "file" => {
            let (flags, positional) = split_flags(rest);
            match positional.first() {
                Some(session) => Parsed::Run(Verb::Projects(ProjectsArgs {
                    action: ProjectsCommand::File(ProjectsFileArgs {
                        session: session.clone(),
                        project: flags.get("project").cloned(),
                        json: false,
                    }),
                })),
                None => Parsed::Usage(PROJECTS_USAGE.to_string()),
            }
        }
        other => Parsed::Unknown(format!(
            "unknown projects command {other:?}\n{PROJECTS_USAGE}"
        )),
    }
}

/// The key verbs, as `/keys --help` would write them.
const KEYS_USAGE: &str = "usage: /keys <list | create [--name N] [--scope read|write|admin] \
                       [--project ID] [--expires-in-days N] | revoke <id> | revoke-all --yes>";

/// `/keys` — the org's API keys, from inside the session.
///
/// Minting is refused here rather than run: a new key prints its secret once, and
/// the session writes every answer into its transcript on disk, so the one and
/// only copy of that credential would be persisted in a session file.
fn parse_keys(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Run(Verb::Keys(KeysArgs {
            action: KeysCommand::List(KeysListArgs { json: false }),
        }));
    };
    let rest = &args[1..];
    match action.as_str() {
        "list" => Parsed::Run(Verb::Keys(KeysArgs {
            action: KeysCommand::List(KeysListArgs { json: false }),
        })),
        "create" => {
            // A form, not a transcript answer: the secret is shown once and a
            // session writes every answer to a file on disk. Whatever was typed
            // becomes the form's starting values, so no flag is dropped.
            let (flags, _) = split_flags(rest);
            let expires_in_days = match flags.get("expires-in-days") {
                Some(raw) => match raw.parse::<i64>() {
                    Ok(days) if days > 0 => Some(days),
                    _ => {
                        return Parsed::Unknown(
                            "--expires-in-days takes a positive number of days".to_string(),
                        );
                    }
                },
                None => None,
            };
            Parsed::KeyCreate {
                name: flags.get("name").cloned(),
                scope: flags.get("scope").cloned(),
                project: flags.get("project").cloned(),
                expires_in_days,
            }
        }
        "revoke" => match rest.first() {
            Some(id) => {
                let (flags, _) = split_flags(rest);
                Parsed::Run(Verb::Keys(KeysArgs {
                    action: KeysCommand::Revoke(KeysRevokeArgs {
                        id: id.clone(),
                        yes: flags.contains_key("yes"),
                        dry_run: flags.contains_key("dry-run"),
                        json: false,
                    }),
                }))
            }
            None => Parsed::Usage(KEYS_USAGE.to_string()),
        },
        "revoke-all" => {
            let (flags, _) = split_flags(rest);
            if !flags.contains_key("yes") {
                return Parsed::Usage(
                    "revoking every key asks for confirmation, and this session owns the \
                     terminal — answer it by typing the flag:\n  /keys revoke-all --yes"
                        .to_string(),
                );
            }
            Parsed::Run(Verb::Keys(KeysArgs {
                action: KeysCommand::RevokeAll(KeysRevokeAllArgs {
                    yes: true,
                    dry_run: false,
                    json: false,
                }),
            }))
        }
        other => Parsed::Unknown(format!("unknown keys command {other:?}\n{KEYS_USAGE}")),
    }
}

/// The org verbs, as `/org --help` would write them.
const ORG_USAGE: &str = "usage: /org <show | rename <name> | members | add-member <email> \
                       [--role owner|admin|member] | set-role <user-id> <role> | \
                       remove-member <user-id> | invites | invite [<email>] [--role ROLE] | \
                       withdraw <id>>";

/// `/org` — the tenant, its roster and its invites, from inside the session.
///
/// `invite` is refused like `/keys create`: the response carries the invite's link
/// and code exactly once, and this session persists every answer to disk.
fn parse_org(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Run(Verb::Org(OrgArgs {
            action: OrgCommand::Show(OrgShowArgs { json: false }),
        }));
    };
    let rest = &args[1..];
    match action.as_str() {
        "show" => Parsed::Run(Verb::Org(OrgArgs {
            action: OrgCommand::Show(OrgShowArgs { json: false }),
        })),
        "rename" => match rest.first() {
            Some(name) => Parsed::Run(Verb::Org(OrgArgs {
                action: OrgCommand::Rename(OrgRenameArgs {
                    name: name.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage(ORG_USAGE.to_string()),
        },
        "members" => Parsed::Run(Verb::Org(OrgArgs {
            action: OrgCommand::Members(OrgMembersArgs { json: false }),
        })),
        "add-member" => {
            let (flags, positional) = split_flags(rest);
            match positional.first() {
                Some(email) => Parsed::Run(Verb::Org(OrgArgs {
                    action: OrgCommand::AddMember(OrgAddMemberArgs {
                        email: email.clone(),
                        role: flags.get("role").cloned(),
                        json: false,
                    }),
                })),
                None => Parsed::Usage(ORG_USAGE.to_string()),
            }
        }
        "set-role" => {
            let (_, positional) = split_flags(rest);
            match (positional.first(), positional.get(1)) {
                (Some(user_id), Some(role)) => Parsed::Run(Verb::Org(OrgArgs {
                    action: OrgCommand::SetRole(OrgSetRoleArgs {
                        user_id: user_id.clone(),
                        role: role.clone(),
                        json: false,
                    }),
                })),
                _ => Parsed::Usage(ORG_USAGE.to_string()),
            }
        }
        "remove-member" => match rest.first() {
            Some(user_id) => Parsed::Run(Verb::Org(OrgArgs {
                action: OrgCommand::RemoveMember(OrgRemoveMemberArgs {
                    user_id: user_id.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage(ORG_USAGE.to_string()),
        },
        "invites" => Parsed::Run(Verb::Org(OrgArgs {
            action: OrgCommand::Invites(OrgInvitesArgs { json: false }),
        })),
        "invite" => {
            // Same reason as `/keys create`: the link and the code exist in the
            // response and never again, and this session persists its answers.
            let (flags, positional) = split_flags(rest);
            Parsed::Invite {
                email: positional.first().cloned(),
                role: flags.get("role").cloned(),
            }
        }
        "withdraw" => match rest.first() {
            Some(id) => Parsed::Run(Verb::Org(OrgArgs {
                action: OrgCommand::Withdraw(OrgWithdrawArgs {
                    id: id.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage(ORG_USAGE.to_string()),
        },
        other => Parsed::Unknown(format!("unknown org command {other:?}\n{ORG_USAGE}")),
    }
}

/// The team verbs, including the invite that joins an organization.
const TEAMS_USAGE: &str = "usage: /teams <list | create <name> | members [<id>] | join <code>>";

/// `/teams` — the org's teams, and the invite that joins one.
fn parse_teams(args: &[String]) -> Parsed {
    let list = || {
        Parsed::Run(Verb::Teams(TeamsArgs {
            action: TeamsCommand::List(TeamsListArgs { json: false }),
        }))
    };
    let Some(action) = args.first() else {
        return list();
    };
    let rest = &args[1..];
    match action.as_str() {
        "list" => list(),
        "create" => match rest.first() {
            Some(name) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Create(TeamsCreateArgs {
                    name: name.clone(),
                    json: false,
                }),
            })),
            None => Parsed::Usage(TEAMS_USAGE.to_string()),
        },
        "members" => match rest.first() {
            Some(id) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Members(TeamsMembersArgs {
                    id: id.clone(),
                    json: false,
                }),
            })),
            // No id: ask with the org's teams rather than making the reader go
            // and copy a uuid out of `/teams`.
            None => Parsed::TeamsOverlay,
        },
        "join" => match rest.first() {
            Some(code) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Join(TeamsJoinArgs { code: code.clone() }),
            })),
            None => Parsed::Usage(TEAMS_USAGE.to_string()),
        },
        "claim" => match rest.first() {
            Some(task_id) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Claim(TeamClaimArgs {
                    task_id: task_id.clone(),
                }),
            })),
            None => Parsed::Usage("usage: /teams claim <task-id>".to_string()),
        },
        "release" => match rest.first() {
            Some(task_id) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Release(TeamReleaseArgs {
                    task_id: task_id.clone(),
                }),
            })),
            None => Parsed::Usage("usage: /teams release <task-id>".to_string()),
        },
        "tasks" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Tasks(TeamTasksArgs {
                    pending: flags.contains_key("pending"),
                    in_progress: flags.contains_key("in-progress"),
                    done: flags.contains_key("done"),
                    json: flags.contains_key("json"),
                }),
            }))
        }
        "inbox" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Inbox(TeamInboxArgs {
                    broadcast: flags.contains_key("broadcast"),
                    ack: flags.contains_key("ack"),
                    json: flags.contains_key("json"),
                }),
            }))
        }
        "presence" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Presence(TeamPresenceArgs {
                    stale: flags.contains_key("stale"),
                    sync: flags.contains_key("sync"),
                    json: flags.contains_key("json"),
                }),
            }))
        }
        "lock" => match rest.first() {
            Some(path) => {
                let (flags, _) = split_flags(rest);
                Parsed::Run(Verb::Teams(TeamsArgs {
                    action: TeamsCommand::Lock(TeamLockArgs {
                        path: path.clone(),
                        ttl: flags
                            .get("ttl")
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(3600),
                        json: flags.contains_key("json"),
                    }),
                }))
            }
            None => Parsed::Usage("usage: /teams lock <path>".to_string()),
        },
        "handoff" => match (rest.first(), rest.get(1)) {
            (Some(task_id), Some(target)) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Handoff(TeamHandoffArgs {
                    task_id: task_id.clone(),
                    target: target.clone(),
                    message: rest.get(2).cloned(),
                    json: false,
                }),
            })),
            _ => Parsed::Usage("usage: /teams handoff <task-id> <target>".to_string()),
        },
        "review" => match (rest.first(), rest.get(1)) {
            (Some(task_id), Some(reviewer)) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Review(TeamReviewArgs {
                    task_id: task_id.clone(),
                    reviewer: reviewer.clone(),
                    message: rest.get(2).cloned(),
                    json: false,
                }),
            })),
            _ => Parsed::Usage("usage: /teams review <task-id> <reviewer>".to_string()),
        },
        "audit" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Audit(TeamAuditArgs {
                    range: flags
                        .get("range")
                        .cloned()
                        .unwrap_or_else(|| "7d".to_string()),
                    session: flags.get("session").cloned(),
                    json: flags.contains_key("json"),
                }),
            }))
        }
        "namespace" => Parsed::Run(Verb::Teams(TeamsArgs {
            action: TeamsCommand::Namespace(TeamsNamespaceArgs {
                action: rest.first().cloned(),
            }),
        })),
        "provenance" => Parsed::Run(Verb::Teams(TeamsArgs {
            action: TeamsCommand::Provenance(TeamsProvenanceArgs { limit: None }),
        })),
        "disputed" => Parsed::Run(Verb::Teams(TeamsArgs {
            action: TeamsCommand::Disputed(TeamsDisputedArgs {}),
        })),
        "enforce" => Parsed::Run(Verb::Teams(TeamsArgs {
            action: TeamsCommand::Enforce(TeamsEnforceArgs {}),
        })),
        "import-export" => {
            let (flags, _) = split_flags(rest);
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::ImportExport(TeamsImportExportArgs {
                    from_claude: flags.get("from-claude").cloned(),
                    from_codex: flags.get("from-codex").cloned(),
                    from_omp: flags.get("from-omp").cloned(),
                    export: flags.get("export").cloned(),
                    format: flags
                        .get("format")
                        .cloned()
                        .unwrap_or_else(|| "markdown".to_string()),
                }),
            }))
        }
        other => Parsed::Unknown(format!("unknown teams command {other:?}\n{TEAMS_USAGE}")),
    }
}

/// Split `--flag value` / `--flag=value` from positional arguments.
fn split_flags(args: &[String]) -> (BTreeMap<String, String>, Vec<String>) {
    let mut flags = BTreeMap::new();
    let mut positional = Vec::new();
    let mut iter = args.iter();
    while let Some(token) = iter.next() {
        if let Some(rest) = token.strip_prefix("--") {
            if let Some((key, value)) = rest.split_once('=') {
                flags.insert(key.to_string(), value.to_string());
            } else if let Some(next) = iter.clone().next().filter(|next| !next.starts_with("--")) {
                flags.insert(rest.to_string(), next.as_str().to_string());
                iter.next();
            } else {
                flags.insert(rest.to_string(), "true".to_string());
            }
        } else {
            positional.push(token.clone());
        }
    }
    (flags, positional)
}

/// Fuzzy find paths relative to workspace root.
///
/// Matches files whose path contains all query characters in order (subsequence match).
/// Returns paths relative to workspace_root, sorted by match quality (shorter paths first,
/// then by path).
fn fuzzy_find_paths(workspace_root: &Path, query: &str) -> Vec<PathBuf> {
    if query.is_empty() {
        return Vec::new();
    }

    let query_lower = query.to_lowercase();
    let mut matches = Vec::new();

    // Walk the workspace directory
    {
        let entries = WalkDir::new(workspace_root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .collect::<Vec<_>>();

        for entry in entries {
            let path = entry.path();
            // Get relative path from workspace root
            if let Ok(rel_path) = path.strip_prefix(workspace_root) {
                let path_str = rel_path.to_string_lossy().to_lowercase();
                if is_subsequence(&query_lower, &path_str) {
                    matches.push(rel_path.to_path_buf());
                }
            }
        }
    }

    // Sort by: match score (exact prefix > shorter path > alphabetical)
    matches.sort_by(|a, b| {
        let a_str = a.to_string_lossy().to_lowercase();
        let b_str = b.to_string_lossy().to_lowercase();

        // Prefer exact prefix matches
        let a_prefix = a_str.starts_with(&query_lower);
        let b_prefix = b_str.starts_with(&query_lower);
        match (a_prefix, b_prefix) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        // Then by path length (shorter first)
        let a_len = a_str.len();
        let b_len = b_str.len();
        if a_len != b_len {
            return a_len.cmp(&b_len);
        }

        // Then alphabetically
        a_str.cmp(&b_str)
    });

    // Limit to reasonable number for UI
    matches.truncate(50);
    matches
}

/// Check if `needle` is a subsequence of `haystack` (case-insensitive, already lowered).
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut needle_chars = needle.chars().peekable();
    for ch in haystack.chars() {
        if needle_chars.peek() == Some(&ch) {
            needle_chars.next();
            if needle_chars.peek().is_none() {
                return true;
            }
        }
    }
    false
}

const VAULT_USAGE: &str = "usage: /vault init|status|asof|history|contradictions|remember|recall|forget|correct|link|unlink|search|export|import|gc|prune|reindex|stats|backup|restore|audit|merge|duplicates|top|timeline|explain|graph";
const SKILL_USAGE: &str = "usage: /skill add|list|remove|run|edit";
const KG_USAGE: &str = "usage: /kg index|status|impact|search|explore|refs|callers|callees|def|diff-impact|callpath|outline|tree|review|tests-for|owners|dead-code|cycles|hotspots|unused-exports|deps|query|watch|serve";

/// Plain text search results as one answer, with the empty case said plainly.
///
/// Both halves are reported even when one fails: a missing vault must not hide
/// what the graph found, and vice versa.
pub fn combined_search_answer(
    vault: &Result<Value, String>,
    graph: &Result<Value, String>,
) -> String {
    let mut parts = Vec::new();
    let mut counts = Vec::new();

    match vault {
        Ok(data) => {
            let count = data["count"].as_u64().unwrap_or(0) as usize;
            counts.push(count);
            if count == 0 {
                parts.push("vault: nothing matches".to_string());
            } else {
                parts.push(render_recall_hits(data));
            }
        }
        Err(error) => parts.push(format!("vault: unavailable ({error})")),
    }

    match graph {
        Ok(data) => {
            let count = data["count"].as_u64().unwrap_or(0) as usize;
            counts.push(count);
            if count == 0 {
                parts.push("graph: no symbol matches".to_string());
            } else {
                parts.push(render_symbol_hits(data));
            }
        }
        Err(error) => parts.push(format!("graph: unavailable ({error})")),
    }

    if counts.len() == 2 && counts.iter().all(|count| *count == 0) {
        return "Nothing in the vault or the code graph matches that.\n\
                Build the graph with `memnoth kg index`, or record what you know with\n\
                `memnoth vault remember <name> <fact…>`."
            .to_string();
    }

    parts.join("\n\n")
}

/// Render the vault's `recall` JSON as a short list.
///
/// Read from the JSON rather than from a `kg`/`vault` type on purpose: the TUI
/// only needs names and scores, and the JSON shape is the documented contract
/// (`--json` promises the same fields).
fn render_recall_hits(data: &Value) -> String {
    let mut out = String::from("vault\n");
    for hit in data["hits"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {:<28} {:<9} {:.2}  {}\n",
            hit["name"].as_str().unwrap_or("?"),
            hit["kind"].as_str().unwrap_or("?"),
            hit["score"].as_f64().unwrap_or(0.0),
            hit["summary"].as_str().unwrap_or("")
        ));
    }
    out.trim_end().to_string()
}

/// Render the graph's `search` JSON as a short list.
fn render_symbol_hits(data: &Value) -> String {
    let mut out = String::from("graph\n");
    for hit in data["hits"].as_array().into_iter().flatten() {
        let container = hit["container"].as_str().filter(|value| !value.is_empty());
        let qualified = match container {
            Some(container) => format!("{container}::{}", hit["name"].as_str().unwrap_or("?")),
            None => hit["name"].as_str().unwrap_or("?").to_string(),
        };
        out.push_str(&format!(
            "  {:<40} {:<10} {}:{}\n",
            qualified,
            hit["kind"].as_str().unwrap_or("?"),
            hit["path"].as_str().unwrap_or("?"),
            hit["line"].as_u64().unwrap_or(0)
        ));
    }
    out.trim_end().to_string()
}

/// A path, shortened for a header line (`~` for the home directory).
pub fn display_path(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    if let Some(home) = home {
        let home = home.display().to_string();
        if !home.is_empty() && text.starts_with(&home) {
            return format!("~{}", &text[home.len()..]);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The palette is a promise: anything it lists as runnable must resolve.
    /// Rows that still need an argument are skipped because the promise there
    /// is the form, not the verb.
    #[test]
    fn every_palette_command_without_arguments_resolves() {
        for item in crate::tui::overlays::palette::commands() {
            if !item.command.starts_with('/')
                || crate::tui::overlays::palette::needs_arguments(&item)
            {
                continue;
            }
            assert!(
                !matches!(parse_line(&item.command), Parsed::Unknown(_)),
                "the palette lists {:?} but the session cannot run it",
                item.command
            );
        }
    }

    #[test]
    fn a_typo_names_the_command_it_almost_typed() {
        assert!(matches!(parse_line("/sesion"), Parsed::Unknown(ref message)
            if message.contains("did you mean /session")));
        assert!(!matches!(parse_line("/session"), Parsed::Unknown(_)));
        assert!(matches!(parse_line("/palette"), Parsed::PaletteOverlay));
    }

    #[test]
    fn the_command_browser_is_reachable_by_name() {
        // `/commands` is a slash command like any other: it opens the overlay and
        // is never reported as unknown.
        assert!(matches!(parse_line("/commands"), Parsed::CommandsOverlay));
        assert!(matches!(
            parse_line("  /commands  "),
            Parsed::CommandsOverlay
        ));
    }

    #[test]
    fn servers_and_watchers_name_the_shell_command_instead_of_freezing() {
        assert!(matches!(parse_line("/serve"), Parsed::Info(ref message)
            if message.contains("memnoth serve") && message.contains("--port 3030")));
        assert!(matches!(parse_line("/watch"), Parsed::Info(_)));
        assert!(matches!(
            parse_line("/watch --once"),
            Parsed::Run(Verb::Watch(_))
        ));
        assert!(matches!(parse_line("/kg serve"), Parsed::Info(_)));
    }

    #[test]
    fn verbs_parse_to_the_same_commands_the_cli_exposes() {
        assert!(matches!(
            parse_line("/kg status"),
            Parsed::Run(Verb::Kg(KgArgs {
                action: KgCommand::Status,
            }))
        ));
        assert!(matches!(
            parse_line("/vault init"),
            Parsed::Run(Verb::Vault(VaultArgs {
                action: VaultCommand::Init
            }))
        ));
        assert!(matches!(
            parse_line("/doctor"),
            Parsed::Run(Verb::Doctor(_))
        ));
        assert!(matches!(
            parse_line("/mcp doctor"),
            Parsed::Run(Verb::Mcp(_))
        ));
        assert!(matches!(parse_line(":q"), Parsed::Quit));
        assert!(matches!(parse_line("/help"), Parsed::Help));
    }

    /// The cloud groups are typed from the prompt far more often than from a
    /// shell, so each one is checked here rather than only through the CLI parse.
    #[test]
    fn the_cloud_groups_parse_to_the_verbs_they_name() {
        assert!(matches!(
            parse_line("/projects"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::List(_)
            }))
        ));
        assert!(matches!(
            parse_line("/projects create payments --visibility restricted"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Create(ProjectsCreateArgs { ref name, ref visibility, .. })
            })) if name == "payments" && visibility.as_deref() == Some("restricted")
        ));
        // A delete asks a yes/no question on stdin, which the session owns: the
        // flag has to be typed, and asking without it must say so.
        assert!(matches!(
            parse_line("/projects delete pr-1"),
            Parsed::Usage(ref message) if message.contains("--yes")
        ));
        assert!(matches!(
            parse_line("/projects delete pr-1 --yes"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Delete(ProjectsDeleteArgs { yes: true, .. })
            }))
        ));

        assert!(matches!(
            parse_line("/keys"),
            Parsed::Run(Verb::Keys(KeysArgs {
                action: KeysCommand::List(_)
            }))
        ));
        assert!(matches!(
            parse_line("/keys revoke k-1"),
            Parsed::Run(Verb::Keys(KeysArgs {
                action: KeysCommand::Revoke(KeysRevokeArgs { ref id, .. })
            })) if id == "k-1"
        ));
        // The secret is shown once, so minting opens a form that is never
        // written to the session file; the typed flag seeds it.
        assert!(matches!(
            parse_line("/keys create --name laptop"),
            Parsed::KeyCreate {
                name: Some(ref name),
                ..
            } if name == "laptop"
        ));

        assert!(matches!(
            parse_line("/org"),
            Parsed::Run(Verb::Org(OrgArgs {
                action: OrgCommand::Show(_)
            }))
        ));
        assert!(matches!(
            parse_line("/org add-member dev@example.com --role admin"),
            Parsed::Run(Verb::Org(OrgArgs {
                action: OrgCommand::AddMember(OrgAddMemberArgs { ref email, ref role, .. })
            })) if email == "dev@example.com" && role.as_deref() == Some("admin")
        ));

        assert!(matches!(
            parse_line("/teams"),
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::List(_)
            }))
        ));
        assert!(matches!(
            parse_line("/teams members tm-1"),
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Members(TeamsMembersArgs { ref id, .. })
            })) if id == "tm-1"
        ));
        assert!(matches!(
            parse_line("/teams create platform"),
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Create(TeamsCreateArgs { ref name, .. })
            })) if name == "platform"
        ));
        // No id: the picker, because the id only exists in another answer.
        assert!(matches!(parse_line("/teams members"), Parsed::TeamsOverlay));
        assert!(matches!(
            parse_line("/teams members tm-1"),
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Members(TeamsMembersArgs { ref id, .. })
            })) if id == "tm-1"
        ));
    }

    #[test]
    fn remember_accepts_a_bare_kind_flag_and_keeps_the_rest_as_the_fact() {
        let Parsed::Run(Verb::Vault(VaultArgs {
            action:
                VaultCommand::Remember {
                    name,
                    kind,
                    confidence,
                    entity_kind,
                    text,
                    ..
                },
        })) = parse_line("/vault remember auth --decision use httpOnly cookies")
        else {
            panic!("expected a remember verb");
        };
        assert_eq!(name, "auth");
        assert_eq!(kind, "decision");
        assert_eq!(confidence, None);
        assert_eq!(entity_kind, "concept");
        assert_eq!(text, ["use", "httpOnly", "cookies"].map(str::to_string));
    }

    #[test]
    fn remember_refuses_a_line_with_no_fact() {
        assert!(matches!(
            parse_line("/vault remember auth --kind decision"),
            Parsed::Usage(_)
        ));
    }

    #[test]
    fn flags_and_positionals_are_split_without_eating_the_value() {
        let Parsed::Run(Verb::Vault(VaultArgs {
            action: VaultCommand::AsOf { at, name, .. },
        })) = parse_line("/vault asof --at 2026-09-01 --name auth")
        else {
            panic!("expected asof");
        };
        assert_eq!(at.as_deref(), Some("2026-09-01"));
        assert_eq!(name.as_deref(), Some("auth"));

        let Parsed::Run(Verb::Kg(KgArgs {
            action: KgCommand::Search { query, limit },
        })) = parse_line("/kg search --limit 5 MemoryEngine")
        else {
            panic!("expected search");
        };
        assert_eq!(query, ["MemoryEngine"].map(str::to_string));
        assert_eq!(limit, 5);
    }

    #[test]
    fn search_and_model_are_commands_rather_than_questions() {
        match parse_line("/search who calls open_vault") {
            Parsed::Search(words) => {
                assert_eq!(words, ["who", "calls", "open_vault"].map(str::to_string));
            }
            other => panic!("expected a local search, got {other:?}"),
        }
        assert!(matches!(parse_line("/search"), Parsed::SearchOverlay));
        assert!(matches!(parse_line("/model"), Parsed::ModelsOverlay));
        assert!(matches!(parse_line("/models"), Parsed::ModelsOverlay));
        assert!(matches!(parse_line("/login"), Parsed::Login));
        assert!(matches!(parse_line("/logout"), Parsed::Logout));
        // `/whoami` runs the command line's verb rather than a second copy of
        // its output, so the two cannot drift.
        assert!(matches!(
            parse_line("/whoami"),
            Parsed::Run(Verb::Auth(AuthArgs {
                action: AuthCommand::Whoami(_)
            }))
        ));
    }

    #[test]
    fn a_plain_line_is_a_question_to_both_stores() {
        match parse_line("who calls open_vault") {
            Parsed::Ask(words) => {
                assert_eq!(words, ["who", "calls", "open_vault"].map(str::to_string));
            }
            other => panic!("expected an ask, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_command_says_so_instead_of_doing_nothing() {
        let Parsed::Unknown(message) = parse_line("/banana") else {
            panic!("expected an unknown command");
        };
        assert!(message.contains("/banana"));
        assert!(message.contains("/help"));
    }

    #[test]
    fn history_walks_back_to_the_draft_it_started_from() {
        let mut app = App::new("s", false);
        for line in ["one", "two", "three"] {
            app.input = line.to_string();
            app.cursor = line.chars().count();
            let taken = app.take_input();
            app.remember_history(&taken);
        }
        app.input = "draft".into();
        app.cursor = 5;

        app.history_prev();
        assert_eq!(app.input, "three");
        app.history_prev();
        assert_eq!(app.input, "two");
        app.history_prev();
        assert_eq!(app.input, "one");
        app.history_prev();
        assert_eq!(app.input, "one", "the oldest line is the stop");
        app.history_next();
        assert_eq!(app.input, "two");
        app.history_next();
        app.history_next();
        assert_eq!(
            app.input, "draft",
            "the draft comes back, not an empty line"
        );
    }

    #[test]
    fn streamed_pieces_coalesce_into_one_live_entry_each() {
        let mut app = App::new("s", false);
        for piece in ["one ", "two ", "three"] {
            app.append_stream_delta(piece);
        }
        for piece in ["plan ", "check"] {
            app.append_reasoning_delta(piece);
        }
        let answers: Vec<&Entry> = app
            .entries
            .iter()
            .filter(|entry| entry.role == EntryRole::Answer)
            .collect();
        assert_eq!(answers.len(), 1, "one entry per turn");
        assert_eq!(answers[0].text, "one two three");
        assert!(answers[0].live);
        let thinking: Vec<&Entry> = app
            .entries
            .iter()
            .filter(|entry| entry.role == EntryRole::Thinking)
            .collect();
        assert_eq!(thinking.len(), 1);
        assert_eq!(thinking[0].text, "plan check");
        assert!(
            !thinking[0].expanded,
            "collapsed while thinking: header plus tail"
        );
        // The live collapsed form is the header plus the last three lines.
        let visual = app.visual_lines(96);
        let body: Vec<String> = visual
            .iter()
            .filter(|(role, _, header)| *role == EntryRole::Thinking && !*header)
            .map(|(_, line, _)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(body, vec!["plan check".to_string()]);
        app.collapse_thinking();
        assert!(
            !app.entries
                .iter()
                .find(|e| e.role == EntryRole::Thinking)
                .unwrap()
                .expanded
        );
        app.finish_stream();
        assert!(
            !app.entries
                .iter()
                .find(|e| e.role == EntryRole::Answer)
                .unwrap()
                .live
        );
        // The visual form: a collapsed block is one header line.
        let visual = app.visual_lines(72);
        let headers: Vec<_> = visual
            .iter()
            .filter(|(role, _, header)| *role == EntryRole::Thinking && *header)
            .collect();
        assert_eq!(headers.len(), 1);
    }

    #[test]
    fn a_live_thinking_block_shows_a_three_line_tail_until_expanded() {
        // The collapsed stream view is a fixed-height window at the end of
        // the reasoning, not the whole of it: a preview that grows and
        // shrinks with the token count makes the screen jump.
        let mut app = App::new("s", false);
        app.append_reasoning_delta("first line\nsecond line\nthird line\nfourth line\nfifth line");
        // A fresh block is collapsed and live: the tail, not the whole.
        let visual = app.visual_lines(96);
        let body: Vec<String> = visual
            .iter()
            .filter(|(role, _, header)| *role == EntryRole::Thinking && !*header)
            .map(|(_, line, _)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(body.len(), 3, "the tail is three lines: {body:?}");
        assert_eq!(body[2], "fifth line");

        // Expanded, the whole block shows.
        app.toggle_thinking();
        let visual = app.visual_lines(96);
        let body: Vec<String> = visual
            .iter()
            .filter(|(role, _, header)| *role == EntryRole::Thinking && !*header)
            .map(|(_, line, _)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(body.len(), 5);

        // Settled and collapsed: the header is the whole block again.
        app.toggle_thinking();
        app.finish_stream();
        let visual = app.visual_lines(96);
        let body: Vec<String> = visual
            .iter()
            .filter(|(role, _, header)| *role == EntryRole::Thinking && !*header)
            .map(|(_, line, _)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(body.is_empty(), "the settled block is one row: {body:?}");
    }

    #[test]
    fn tool_calls_split_the_answer_into_chronological_segments() {
        let mut app = App::new("s", false);
        app.append_stream_delta("first part");
        app.push_tool("kg search x");
        app.append_stream_delta("second part");
        let answers: Vec<usize> = app
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.role == EntryRole::Answer)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(answers.len(), 2, "text after the tools is its own segment");
        assert_eq!(app.stream_answer_first, Some(answers[0]));
        assert_eq!(app.stream_answer, Some(answers[1]));
        // The tool row sits between the two segments, in the order it happened.
        assert!(answers[0] < 1 && answers[1] > 1);
        assert_eq!(app.entries[1].role, EntryRole::Tool);
    }

    #[test]
    fn a_stream_delta_never_yanks_the_reader_back_to_the_bottom() {
        let mut app = App::new("s", false);
        app.follow = false;
        app.scroll = 0;
        app.append_stream_delta("a token");
        app.append_stream_delta("more tokens");
        assert!(!app.follow, "scrolling up to read must survive the stream");
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn the_pace_line_reads_like_the_other_agents() {
        assert_eq!(
            turn_stats(17_000, 24_000, 94, Some(1_700), 3).as_deref(),
            // Cost and tools compete for the fourth slot; money wins.
            Some("17s · 5.5 tps · ↑24k ↓94 · $0.0017")
        );
        assert_eq!(
            turn_stats(17_000, 24_000, 94, None, 3).as_deref(),
            Some("17s · 5.5 tps · ↑24k ↓94 · 3 tools")
        );
        assert_eq!(
            turn_stats(17_000, 24_000, 94, None, 0).as_deref(),
            Some("17s · 5.5 tps · ↑24k ↓94")
        );
        assert_eq!(
            turn_stats(30_000, 5_000, 500, Some(120_000), 1).as_deref(),
            Some("30s · 16.7 tps · ↑5k ↓500 · $0.120")
        );
        assert_eq!(
            turn_stats(0, 0, 0, None, 0),
            None,
            "an unpriced turn says nothing"
        );
        assert_eq!(
            turn_stats(90_000, 812, 1_500, None, 0).as_deref(),
            Some("1m 30s · 16.7 tps · ↑812 ↓1.5k")
        );
    }

    #[test]
    fn the_pace_line_follows_its_answer() {
        let mut app = App::new("s", false);
        app.append_stream_delta("the answer");
        let index = app.stream_answer.unwrap();
        app.entries[index].stats = Some("17s · 5.5 tps · ↑24k ↓94".to_string());
        app.finish_stream();
        let visual = app.visual_lines(96);
        let texts: Vec<String> = visual
            .iter()
            .map(|(_, line, _)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        let position = texts
            .iter()
            .position(|text| text == "the answer")
            .expect("the answer renders");
        assert_eq!(texts[position + 1], "17s · 5.5 tps · ↑24k ↓94");
    }

    #[test]
    fn a_running_tool_row_reports_its_age() {
        let mut app = App::new("s", false);
        app.push_tool("kg search x");
        let visual = app.visual_lines(72);
        let text: String = visual
            .iter()
            .map(|(_, line, _)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("0s"), "{text}");
        app.resolve_tools(0);
        let visual = app.visual_lines(72);
        let text: String = visual
            .iter()
            .map(|(_, line, _)| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("0s"), "resolved rows stop ageing: {text}");
    }

    #[test]
    fn consecutive_duplicates_do_not_fill_the_history() {
        let mut app = App::new("s", false);
        app.remember_history("same");
        app.remember_history("same");
        app.remember_history("other");
        assert_eq!(app.history, ["same", "other"].map(str::to_string));
    }

    #[test]
    fn the_cursor_edits_by_character_not_byte() {
        let mut app = App::new("s", false);
        for ch in "héllo".chars() {
            app.insert_char(ch);
        }
        assert_eq!(app.input, "héllo");
        app.move_home();
        app.insert_char('›');
        assert_eq!(app.input, "›héllo");
        app.move_end();
        app.backspace();
        assert_eq!(app.input, "›héll");
        app.move_home();
        app.delete();
        assert_eq!(app.input, "héll");
    }

    #[test]
    fn scrolling_clamps_to_the_last_page_and_follows_new_output() {
        let mut app = App::new("s", false);
        for index in 0..30 {
            app.push(EntryRole::Answer, format!("line {index}"));
        }
        app.set_viewport(20, 5);
        assert!(app.follow);
        app.to_bottom();
        let max = app.max_scroll();
        assert!(max > 0);

        app.page_up();
        assert!(!app.follow);
        assert_eq!(app.scroll, max.saturating_sub(5));

        app.page_down();
        assert!(app.follow, "scrolling past the end resumes following");

        app.follow = false;
        app.scroll = 0;
        app.push(EntryRole::Answer, "newest");
        assert!(app.follow, "new output snaps to the bottom");
    }

    /// Following must leave the newest line inside the viewport: the slice the
    /// screen draws starts at `scroll` and must end on the last visual line.
    #[test]
    fn the_newest_visual_line_is_inside_the_viewport() {
        let mut app = App::new("s", false);
        for index in 0..40 {
            app.push(EntryRole::Answer, format!("filler line {index:02}"));
        }
        app.push(
            EntryRole::Answer,
            "**Temporal vault**\n\nA belief is a claim with a date it became true, never a \
             fact that floats free of one.\n\n- Impact analysis — what breaks if you change a \
             symbol, which is exactly the question this sentence is long enough to have to \
             answer across several wrapped lines before it finally ends here (`kg_impact`)\n\
             - Read files directly with line ranges\n\n`kg_impact_status` is one name, never italics.",
        );
        app.set_viewport(83, 36);
        app.to_bottom();

        let visual = app.visual_lines(app.view_width);
        assert_eq!(
            app.scroll + app.view_height,
            visual.len(),
            "the followed slice must end on the last line"
        );
    }

    /// The wheel is a small step, not a page, and it leaves following off so the
    /// next answer cannot yank the view back.
    #[test]
    fn the_wheel_moves_a_few_lines_and_stops_following() {
        let mut app = App::new("s", false);
        for index in 0..30 {
            app.push(EntryRole::Answer, format!("line {index}"));
        }
        app.set_viewport(20, 5);
        app.to_bottom();
        let max = app.max_scroll();
        assert!(max > 3);

        app.wheel(3, true);
        assert!(!app.follow, "the wheel stops following");
        assert_eq!(app.scroll, max - 3);

        app.wheel(3, false);
        assert_eq!(app.scroll, max);
        assert!(app.follow, "scrolling to the end resumes following");

        // Past the top is the top, not a negative offset.
        app.wheel(3, true);
        app.wheel(1000, true);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn wrapping_preserves_paragraphs_and_never_overflows() {
        let lines = wrap_text("alpha beta gamma\n\nsecond paragraph", 10);
        assert_eq!(lines[0], "alpha beta");
        assert!(lines.iter().all(|line| line.chars().count() <= 10));
        assert!(lines.iter().any(String::is_empty), "blank lines survive");

        let long = wrap_text("supercalifragilisticexpialidocious", 8);
        assert_eq!(long.concat(), "supercalifragilisticexpialidocious");
        assert!(long.iter().all(|line| line.chars().count() <= 8));

        // A zero-width pane is legal during a resize; it must not loop or panic.
        assert!(!wrap_text("anything", 0).is_empty());
    }

    #[test]
    fn a_resumed_session_seeds_the_transcript_in_order() {
        let session = Session::from_rows(
            "payments",
            "memnoth-local",
            [
                (Role::User, "first".to_string()),
                (Role::Assistant, "second".to_string()),
            ],
        );
        let mut app = App::new("payments", true);
        app.seed(&session);
        assert_eq!(app.entries.len(), 2);
        assert_eq!(app.entries[0].role, EntryRole::User);
        assert_eq!(app.entries[1].text, "second");
    }

    #[test]
    fn the_cloud_groups_parse_to_the_verbs_the_cli_exposes() {
        assert!(matches!(
            parse_line("/org members"),
            Parsed::Run(Verb::Org(OrgArgs {
                action: OrgCommand::Members(_)
            }))
        ));
        // A bare group means its read: the answer people want from `/org` and
        // `/projects` is the list, not a usage line.
        assert!(matches!(
            parse_line("/teams"),
            Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::List(_)
            }))
        ));
        assert!(matches!(
            parse_line("/projects"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::List(_)
            }))
        ));
        assert!(matches!(
            parse_line("/keys list"),
            Parsed::Run(Verb::Keys(KeysArgs {
                action: KeysCommand::List(_)
            }))
        ));
    }

    /// `use` is the verb that files this machine's sessions, so both of its
    /// shapes have to reach the CLI: an id to store, and `--clear` to stop.
    #[test]
    fn the_project_use_verb_parses_with_and_without_an_id() {
        assert!(matches!(
            parse_line("/projects use 4f2a"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Use(ProjectsUseArgs { ref id, clear: false, .. })
            })) if id.as_deref() == Some("4f2a")
        ));
        assert!(matches!(
            parse_line("/projects use --clear"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Use(ProjectsUseArgs { clear: true, .. })
            }))
        ));
        // Bare `use` asks which project this machine is filing under.
        assert!(matches!(
            parse_line("/projects use"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Use(ProjectsUseArgs { ref id, clear: false, .. })
            })) if id.is_none()
        ));
    }

    #[test]
    fn creating_a_project_carries_its_visibility_through() {
        let Parsed::Run(Verb::Projects(ProjectsArgs {
            action: ProjectsCommand::Create(create),
        })) = parse_line("/projects create acme --visibility restricted")
        else {
            panic!("expected a create verb");
        };
        assert_eq!(create.name, "acme");
        assert_eq!(create.visibility.as_deref(), Some("restricted"));
    }

    #[test]
    fn destructive_cloud_commands_need_the_flag_the_session_can_read() {
        // The confirmation prompt reads stdin, which the session owns; the flag is
        // how a person answers it here.
        assert!(matches!(
            parse_line("/projects delete abc"),
            Parsed::Usage(ref message) if message.contains("--yes")
        ));
        assert!(matches!(
            parse_line("/projects delete abc --yes"),
            Parsed::Run(Verb::Projects(ProjectsArgs {
                action: ProjectsCommand::Delete(_)
            }))
        ));
        assert!(matches!(
            parse_line("/keys revoke-all"),
            Parsed::Usage(ref message) if message.contains("--yes")
        ));
        assert!(matches!(
            parse_line("/keys revoke-all --yes"),
            Parsed::Run(Verb::Keys(KeysArgs {
                action: KeysCommand::RevokeAll(_)
            }))
        ));
    }

    #[test]
    fn commands_that_print_a_secret_once_open_a_form_rather_than_a_transcript_line() {
        // The session persists every answer into its transcript, so these two
        // are the only cloud commands that do not answer in it: they open a
        // screen whose result is shown once and never written down.
        assert!(matches!(
            parse_line("/keys create --name laptop"),
            Parsed::KeyCreate {
                name: Some(ref name),
                scope: None,
                project: None,
                expires_in_days: None,
            } if name == "laptop"
        ));
        assert!(matches!(
            parse_line("/org invite bob@example.com"),
            Parsed::Invite {
                email: Some(ref email),
                role: None,
            } if email == "bob@example.com"
        ));
        // A bare command opens the same form, empty.
        assert!(matches!(
            parse_line("/keys create"),
            Parsed::KeyCreate { name: None, .. }
        ));
        assert!(matches!(
            parse_line("/org invite"),
            Parsed::Invite { email: None, .. }
        ));
        // An expiry that is not a number is refused before a form opens, not
        // silently dropped inside it.
        assert!(matches!(
            parse_line("/keys create --expires-in-days soon"),
            Parsed::Unknown(ref message) if message.contains("--expires-in-days")
        ));
        // The read-only siblings stay reachable.
        assert!(matches!(
            parse_line("/org invites"),
            Parsed::Run(Verb::Org(OrgArgs {
                action: OrgCommand::Invites(_)
            }))
        ));
    }

    #[test]
    fn an_empty_combined_search_says_what_to_do_next() {
        let answer = combined_search_answer(
            &Ok(serde_json::json!({"count": 0})),
            &Ok(serde_json::json!({"count": 0})),
        );
        assert!(answer.contains("kg index"));
        assert!(answer.contains("vault remember"));
    }
}

#[cfg(test)]
mod update_parse_tests {
    use super::*;

    #[test]
    fn slash_update_parses_check_channel_version_and_no_verify() {
        let Parsed::Run(Verb::Update(args)) = parse_line("/update check") else {
            panic!("`/update check` must parse to an update verb");
        };
        assert!(args.check);

        let Parsed::Run(Verb::Update(args)) = parse_line("/update --version 0.2.0 --no-verify")
        else {
            panic!("`/update --version …` must parse to an update verb");
        };
        assert_eq!(args.version.as_deref(), Some("0.2.0"));
        assert!(args.no_verify);

        let Parsed::Run(Verb::Update(args)) = parse_line("/update --channel beta") else {
            panic!("`/update --channel beta` must parse to an update verb");
        };
        assert_eq!(args.channel.as_deref(), Some("beta"));
        assert!(!args.check);
    }
}

/// Every command in the registry must resolve through the session parser
/// when typed without arguments. A row that the session cannot run is a
/// defect: `mn docs`, completions, and the palette all describe a command
/// the user cannot reach.
#[test]
fn every_registry_command_without_arguments_resolves() {
    let registry = memnoth_command::build_p0_registry().build();
    for spec in registry.all() {
        let command = format!("/{}", spec.cli_path.join(" "));
        assert!(
            !matches!(parse_line(&command), Parsed::Unknown(_)),
            "registry command {:?} (cli_path {:?}) is not reachable from the session",
            spec.id,
            spec.cli_path
        );
    }
}
