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
    KgArgs, KgCommand, McpArgs, McpCommand, TeamsArgs, TeamsCommand, TeamsJoinArgs,
    VaultArgs, VaultCommand, Verb,
    SkillArgs, SkillCommand, SkillAddArgs, SkillListArgs, SkillRemoveArgs, SkillRunArgs, SkillEditArgs,
};
use crate::tui::panes::{ContextPane, DiffPane, TasksPane, TranscriptPane};
use memnoth_sandbox::rules::{RuleDecision, RuleSet, decide};
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
    Plain,  // Plain text output for mn ask
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
}

/// Observation kinds `remember` accepts, mirrored from the vault command so a
/// `/vault remember x --decision …` line reads naturally in the TUI.
const OBSERVATION_KINDS: [&str; 5] = ["fact", "decision", "lesson", "gotcha", "correction"];

/// What a typed line means.
///
/// `Verb` deliberately has no `PartialEq` (it is a clap surface, not a value
/// type), so this enum does not derive one either; tests match on the variant
/// they expect.
#[derive(Debug)]
pub enum Parsed {
    /// Nothing to do (blank line).
    Empty,
    /// Leave the session.
    Quit,
    /// Print the command list.
    Help,
    /// Clear the transcript.
    Clear,
    /// What model a turn would use, and how to configure it.
    ModelOverlay,
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
    /// `/memory`: edit project memory.
    Memory,
    /// `/feedback`: open feedback overlay.
    FeedbackOverlay,
    /// A line this parser could not read; the message is the transcript entry.
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
}

/// The startup screen, shown while the transcript is empty.
///
/// It is structured rather than a block of text so the layout can decide what
/// each part looks like: the same values read as a wall of prose in a narrow
/// terminal and as a table when the columns line up.
#[derive(Debug, Clone, Default)]
pub struct Home {
    /// The binary's version, printed beside the wordmark.
    pub version: String,
    /// `session "name" (new)`, for the line at the bottom.
    pub session: String,
    /// The workspace root the session opened.
    pub workspace: String,
    /// Vault path, and whether it exists yet.
    pub vault: String,
    /// `ready`, or what is missing.
    pub vault_note: String,
    /// Graph summary, or that it is not indexed and what builds it.
    pub graph: String,
    /// The model a turn would use, and whether it can run.
    pub model: String,
    /// `ready`, or the key that is missing.
    pub model_note: String,
    /// The shortest path to something useful: command, then what it does.
    pub steps: Vec<(String, String)>,
}

/// One entry in the transcript. Wrapped lines still belong to one entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Who it is from.
    pub role: EntryRole,
    /// The text, possibly multi-line.
    pub text: String,
}
/// The session screen's state.
#[derive(Debug)]
pub struct App {
    /// Session name (`--resume`, or `session`).
    pub name: String,
    /// Whether the file already existed when the session opened.
    pub resumed: bool,
    /// What the startup screen shows, or `None` for a plain empty transcript.
    pub home: Option<Home>,
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
    /// Set when Enter was pressed; the run loop performs the dispatch so the
    /// pure state never needs an `AppContext`.
    pub pending_submit: bool,
    /// Set while a model turn is running on the worker thread.
    pub busy: bool,
    /// A write waiting for the person's yes, rendered as a modal.
    pub approval: Option<String>,
    /// The rendered context pane (legacy string).
    pub context: String,
    /// One-line graph summary for the header.
    pub graph: String,
    /// Workspace root, shortened for the header.
    pub root: String,
    /// Vault path, shortened for the header.
    pub vault: String,
    /// Last transcript viewport, in characters/lines, for scrolling.
    view_width: usize,
    view_height: usize,
    /// Set by the run loop; quitting is read after the draw.
    pub quit: bool,
    /// Fuzzy path completion state for @file references.
    pub completion: CompletionState,
    /// Commands typed while a turn is in flight (queued instead of rejected).
    pub command_queue: VecDeque<String>,
    /// Pending shell command (`!cmd`) awaiting sandbox policy confirmation.
    pub pending_shell: Option<PendingShell>,
    /// UI theme variant
    pub theme: Theme,
    /// Screen reader mode
    pub screen_reader: ScreenReaderMode,
    /// Transcript pane
    pub transcript_pane: TranscriptPane,
    /// Context pane
    pub context_pane: ContextPane,
    /// Tasks pane
    pub tasks_pane: TasksPane,
    /// Active pane focus: 0 = transcript, 1 = context, 2 = tasks, 3 = diff
    pub active_pane: u8,
    /// Overlay stack for modal UIs
    pub overlay_stack: crate::tui::overlays::OverlayStack,
    /// Configuration
    pub config: crate::Config,
    /// Config file path
    pub config_path: std::path::PathBuf,
    /// Channel for provider test results from background threads
    pub provider_test_tx: mpsc::Sender<ProviderTestResult>,
    pub provider_test_rx: mpsc::Receiver<ProviderTestResult>,
}
/// Result of a provider connection test
#[derive(Debug, Clone)]
pub struct ProviderTestResult {
    pub index: usize,
    pub success: bool,
    pub message: String,
    pub model_count: usize,
    pub timestamp: String,
}

impl App {
    /// A fresh screen.
    pub fn new(name: &str, resumed: bool) -> Self {
        let (provider_test_tx, provider_test_rx) = mpsc::channel();
        Self {
            name: name.to_string(),
            resumed,
            home: None,
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            scroll: 0,
            follow: true,
            pending_submit: false,
            busy: false,
            approval: None,
            context: String::new(),
            graph: "not indexed".to_string(),
            root: String::new(),
            vault: String::new(),
            view_width: 80,
            view_height: 20,
            quit: false,
            completion: CompletionState::default(),
            command_queue: VecDeque::new(),
            pending_shell: None,
            theme: Theme::Default,
            screen_reader: ScreenReaderMode::Off,
            transcript_pane: TranscriptPane::new(),
            context_pane: ContextPane::new(),
            tasks_pane: TasksPane::new(),
            diff_pane: DiffPane::new(),
            pane_layout: 0,
            active_pane: 0,
            overlay_stack: crate::tui::overlays::OverlayStack::new(),
            config: crate::Config::default(),
            config_path: std::path::PathBuf::new(),
            provider_test_tx,
            provider_test_rx,
        }
    }

    /// Seed the transcript from a session file, so `--resume` shows the
    /// conversation that was actually had.
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
    pub fn push(&mut self, role: EntryRole, text: impl Into<String>) {
        self.entries.push(Entry {
            role,
            text: text.into(),
        });
        self.follow = true;
    }

    /// Empty the transcript, which brings the startup screen back (this is
    /// what `/clear` does).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.scroll = 0;
        self.follow = true;
    }

    /// Try to receive a provider test result without blocking
    pub fn try_recv_provider_test_result(&mut self) -> Option<ProviderTestResult> {
        self.provider_test_rx.try_recv().ok()
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

    /// Scroll down one page; reaching the end resumes following.
    pub fn page_down(&mut self) {
        self.scroll = self.scroll.saturating_add(self.view_height.max(1));
        if self.scroll >= self.max_scroll() {
            self.to_bottom();
        }
    }

    /// Snap to the newest line.
    pub fn to_bottom(&mut self) {
        self.follow = true;
    }

    /// Start or cycle fuzzy path completion for @file references.
    pub fn complete_path(&mut self, root: &Path) {
        // Find the @ prefix in the input
        let input = &self.input;
        let Some(at_pos) = input.rfind('@') else {
            return;
        };

        let prefix = &input[at_pos..self.cursor.min(input.len())];
        let query = prefix.trim_start_matches('@');

        if query.is_empty() && !self.completion.active {
            // First activation: find all candidates
            self.completion.prefix = prefix.to_string();
            self.completion.matches = Self::find_candidates(root, query);
            self.completion.index = 0;
            self.completion.workspace_root = root.to_path_buf();
            self.completion.active = !self.completion.matches.is_empty();
        } else if self.completion.active {
            // Cycle to next candidate
            if !self.completion.matches.is_empty() {
                self.completion.index = (self.completion.index + 1) % self.completion.matches.len();
                let candidate = &self.completion.matches[self.completion.index];
                let replacement = format!("@{}", candidate.display());
                let byte_start = self.cursor_byte_at_char(at_pos);
                let byte_end = self.cursor_byte();
                self.input.replace_range(byte_start..byte_end, &replacement);
                self.cursor = byte_start + replacement.chars().count();
            }
        }
    }

    /// Find candidate paths matching the query under root.
    fn find_candidates(root: &Path, query: &str) -> Vec<PathBuf> {
        use std::fs;
        let mut candidates = Vec::new();

        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                // Skip hidden files/dirs
                if name.starts_with('.') {
                    continue;
                }

                // Simple fuzzy match: query is a substring of name
                if name.contains(query) {
                    // Get relative path from root
                    if let Ok(rel) = path.strip_prefix(root) {
                        candidates.push(rel.to_path_buf());
                    }
                }

                // Recurse into directories (limit depth for performance)
                if path.is_dir() && candidates.len() < 100 {
                    let mut sub = Self::find_candidates(&path, query);
                    candidates.append(&mut sub);
                }
            }
        }

        candidates.sort();
        candidates.truncate(100);
        candidates
    }

    /// Get byte index for a character position.
    fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    /// Get byte index for a character position.
    fn cursor_byte_at_char(&self, char_pos: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_pos)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
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
    pub fn visual_lines(&self, width: usize) -> Vec<(EntryRole, String)> {
        let width = width.max(1);
        let mut out = Vec::new();
        for entry in &self.entries {
            for line in wrap_text(&entry.text, width) {
                out.push((entry.role, line));
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
        "help" | "?" => Parsed::HelpOverlay,
        "quit" | "exit" | "q" => Parsed::Quit,
        "clear" => Parsed::Clear,
        "model" => Parsed::ModelOverlay,
        "connect" => Parsed::ConnectOverlay,
        "search" => {
            if args.is_empty() {
                Parsed::SearchOverlay
            } else {
                Parsed::Search(args)
            }
        }
        "context" => Parsed::ContextOverlay,
        "diff" => Parsed::DiffOverlay,
        "rewind" => Parsed::RewindOverlay,
        "permissions" => Parsed::PermissionsOverlay,
        "feedback" => Parsed::FeedbackOverlay,
        "doctor" => Parsed::Run(Verb::Doctor),
        "kg" => parse_kg(&args),
        "init" => Parsed::Init,
        "memory" => Parsed::Memory,
        "theme" => {
            if args.is_empty() {
                Parsed::Unknown("usage: /theme <default|high-contrast|no-color>".to_string())
            } else {
                match args[0].as_str() {
                    "default" => Parsed::Run(Verb::SetTheme(crate::tui::app::Theme::Default)),
                    "high-contrast" => Parsed::Run(Verb::SetTheme(crate::tui::app::Theme::HighContrast)),
                    "no-color" | "nocolor" => Parsed::Run(Verb::SetTheme(crate::tui::app::Theme::NoColor)),
                    other => Parsed::Unknown(format!("unknown theme: {other} — use default, high-contrast, or no-color")),
                }
            }
        }
        "screen-reader" => {
            if args.is_empty() {
                Parsed::Unknown("usage: /screen-reader <off|plain>".to_string())
            } else {
                match args[0].as_str() {
                    "off" => Parsed::Run(Verb::SetScreenReader(crate::tui::app::ScreenReaderMode::Off)),
                    "plain" => Parsed::Run(Verb::SetScreenReader(crate::tui::app::ScreenReaderMode::Plain)),
                    other => Parsed::Unknown(format!("unknown screen reader mode: {other} — use off or plain")),
                }
            }
        }
        "skill" => parse_skill(&args),
        other => Parsed::Unknown(format!("unknown command /{other} — type /help")),
    }
}

fn parse_skill(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Unknown(SKILL_USAGE.to_string());
    };
    let rest = &args[1..];
    match action.as_str() {
        "add" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Unknown("usage: /skill add <path> [--name <name>]".to_string());
            }
            let path = positional[0].clone();
            let name = flags.get("name").cloned();
            Parsed::Run(Verb::Skill(SkillArgs {
                action: SkillCommand::Add(SkillAddArgs { path, name }),
            }))
        }
        "list" => {
            let (flags, positional) = split_flags(rest);
            let detail = flags.get("detail").is_some() || flags.get("d").is_some();
            let tag = flags.get("tag").cloned();
            let json = flags.get("json").is_some();
            Parsed::Run(Verb::Skill(SkillArgs {
                action: SkillCommand::List(SkillListArgs { detail, tag, json }),
            }))
        }
        "remove" | "rm" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Unknown("usage: /skill remove <name> [--force]".to_string());
            }
            let name = positional[0].clone();
            let force = flags.get("force").is_some() || flags.get("f").is_some();
            Parsed::Run(Verb::Skill(SkillArgs {
                action: SkillCommand::Remove(SkillRemoveArgs { name, force }),
            }))
        }
        "run" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Unknown("usage: /skill run <name> [KEY=VALUE...]".to_string());
            }
            let name = positional[0].clone();
            let skill_args = positional[1..].to_vec();
            let json = flags.get("json").is_some();
            Parsed::Run(Verb::Skill(SkillArgs {
                action: SkillCommand::Run(SkillRunArgs { name, args: skill_args, json }),
            }))
        }
        "edit" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Unknown("usage: /skill edit <name> [--editor <editor>]".to_string());
            }
            let name = positional[0].clone();
            let editor = flags.get("editor").cloned().or_else(|| flags.get("e").cloned());
            Parsed::Run(Verb::Skill(SkillArgs {
                action: SkillCommand::Edit(SkillEditArgs { name, editor }),
            }))
        }
        other => Parsed::Unknown(format!("unknown skill command {other:?}\n{SKILL_USAGE}")),
    }
}


fn parse_vault(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Unknown(VAULT_USAGE.to_string());
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
            None => Parsed::Unknown("usage: /vault history <name>".to_string()),
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
        "recall" => {
            let (flags, positional) = split_flags(rest);
            if positional.is_empty() {
                return Parsed::Unknown("usage: /vault recall <query>".to_string());
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
        other => Parsed::Unknown(format!("unknown vault command {other:?}\n{VAULT_USAGE}")),
    }
}

fn parse_remember(rest: &[String]) -> Parsed {
    let mut iter = rest.iter();
    let Some(name) = iter.next() else {
        return Parsed::Unknown("usage: /vault remember <name> [--kind K] <fact…>".to_string());
    };
    if name.starts_with("--") {
        return Parsed::Unknown("usage: /vault remember <name> [--kind K] <fact…>".to_string());
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
        return Parsed::Unknown("the fact is empty: /vault remember <name> <fact…>".to_string());
    }
    Parsed::Run(Verb::Vault(VaultArgs {
        action: VaultCommand::Remember {
            name: name.clone(),
            kind,
            confidence,
            entity_kind,
            text,
        },
    }))
}

fn parse_kg(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Unknown(KG_USAGE.to_string());
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
                return Parsed::Unknown("usage: /kg impact <symbol>".to_string());
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
                return Parsed::Unknown("usage: /kg search <query>".to_string());
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
        other => Parsed::Unknown(format!("unknown kg command {other:?}\n{KG_USAGE}")),
    }
}

fn parse_mcp(args: &[String]) -> Parsed {
    match args.first().map(String::as_str) {
        Some("doctor") => Parsed::Run(Verb::Mcp(McpArgs {
            action: McpCommand::Doctor,
        })),
        Some("add" | "call" | "serve" | "selftest") => Parsed::Unknown(
            "`mcp serve`, `mcp add`, `mcp call` and `mcp selftest` need the real terminal or a \
             stdio pipe; run them from a shell:\n  memnoth mcp serve"
                .to_string(),
        ),
        _ => Parsed::Unknown("usage: /mcp doctor".to_string()),
    }
}

/// Parse `/teams join <code>` — accept an invite and join an organization.
fn parse_teams(args: &[String]) -> Parsed {
    let Some(action) = args.first() else {
        return Parsed::Unknown("usage: /teams join <code>".to_string());
    };
    let rest = &args[1..];
    match action.as_str() {
        "join" => match rest.first() {
            Some(code) => Parsed::Run(Verb::Teams(TeamsArgs {
                action: TeamsCommand::Join(TeamsJoinArgs { code: code.clone() }),
            })),
            None => Parsed::Unknown("usage: /teams join <code>".to_string()),
        },
        other => Parsed::Unknown(format!(
            "unknown teams command {other:?}\nusage: /teams join <code>"
        )),
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
    if let Ok(entries) = WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .collect::<Vec<_>>()
    {
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

/// The `/help` text.
pub const HELP: &str = "\
memnoth session — commands

  /help                      this list
  /quit                      leave (also :q, ctrl-c)
  /clear                     clear the transcript
  /sessions                  sessions on disk, and how to resume them
  /model                     open model overlay
  /connect                   open provider connections overlay
  /search                    open search overlay
  /search <query>            ask the vault and the graph (no model)
  /context                   open context overlay (token budget grid, compact)
  /diff                      open diff overlay (per-hunk accept/reject)
  /rewind                    open rewind overlay (checkpoints picker)
  /permissions               open permissions overlay
  /feedback                  open feedback overlay (send feedback to memnoth team)
  /vault status              what the vault holds
  /vault asof [--at WHEN] [--name N]   what was believed at an instant
  /vault history <name>      every version, with the reason it changed
  /vault contradictions [name]         beliefs explicitly contradicted
  /vault remember <name> [--kind K] <fact…>   record a fact
  /vault recall <query>      search the vault
  /kg index [--full] [--no-embed]      build or update the code graph
  /kg status                 what was built, when, and from where
  /kg impact <symbol>        definitions, references, tests
  /kg search <query>         find symbols by name
  /mcp doctor                what clients are registered, and where
  /teams join <code>         accept an invite and join an organization
  /init                      generate AGENTS.md
  /memory                    edit project memory
  /theme <theme>             set theme: default, high-contrast, no-color
  /screen-reader <mode>      set screen reader: off, plain
  !<cmd>                     shell passthrough (sandbox policy applies)

Global keys:
  enter                      send message
  ↑/↓                        history navigation
  ←/→                        edit cursor movement
  pgup/pgdn                  scroll transcript
  end                        jump to newest
  ctrl-c                     quit / cancel

Pane keys (when panes visible):
  F1                         focus transcript pane
  F2                         focus context pane
  F3                         focus tasks pane
  F4                         focus diff pane
  ctrl+tab                   cycle panes
  /                          open command palette

Overlay keys (context, rewind, diff, etc.):
  tab / shift+tab            switch tabs
  ↑/↓                        navigate entries
  enter                      view detail / select / resume
  esc                        close overlay / back from detail
  ctrl+c                     copy entry
  p                          pin entry
  r                          refresh

Diff overlay:
  a / r                      accept / reject current hunk
  A / R                      accept / reject all hunks
  n / N                      next / previous conflict
  s                          toggle side-by-side view
  w                          toggle whitespace
  l                          toggle line numbers
  ?                          show help

Rewind overlay:
  enter                      resume session / view turn / view checkpoint
  b                          branch from selected turn
  d                          delete session

Context overlay:
  ctrl+x                     compact context

Tasks overlay:
  ctrl+r                     retry failed task
  ctrl+x                     cancel running task
  ctrl+c                     clear completed tasks
  a                          toggle auto-refresh

Accessibility:
  NO_COLOR env var           disable colors (same as /theme no-color)
  /theme high-contrast       high contrast black/white theme
  /screen-reader plain       plain text output for mn ask
Anything else is a question. With a model configured it runs a turn that can call
the vault and the code graph as tools; without one the same question is answered
by searching both stores, and the transcript says which happened.

A turn may ask to record a belief or edit a file. Nothing is written until you
answer the box: y approve · n or esc reject (a rejection is final for that turn).";

const VAULT_USAGE: &str = "usage: /vault init|status|asof|history|contradictions|remember|recall";
const KG_USAGE: &str = "usage: /kg index|status|impact|search";

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
        assert!(matches!(parse_line("/doctor"), Parsed::Run(Verb::Doctor)));
        assert!(matches!(
            parse_line("/mcp doctor"),
            Parsed::Run(Verb::Mcp(_))
        ));
        assert!(matches!(parse_line(":q"), Parsed::Quit));
        assert!(matches!(parse_line("/help"), Parsed::Help));
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
            Parsed::Unknown(_)
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
        assert!(matches!(parse_line("/search"), Parsed::Unknown(_)));
        assert!(matches!(parse_line("/model"), Parsed::Model));
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
    fn an_empty_combined_search_says_what_to_do_next() {
        let answer = combined_search_answer(
            &Ok(serde_json::json!({"count": 0})),
            &Ok(serde_json::json!({"count": 0})),
        );
        assert!(answer.contains("kg index"));
        assert!(answer.contains("vault remember"));
    }
}
