//! Work — one ranked surface for everything this session started or can start.
//!
//! Runs and commands share a single list ordered by what needs the person most:
//! waiting-on-you first, then live work, then history. `1/2/3` narrows the same
//! surface to runs or commands without changing screens — a filter, not a tab.
//! The vocabulary is Memnoth's own (`delegate`, `run`, `job`, `worktree`), not
//! a borrowed category taxonomy.

use std::path::Path;
use std::process::Command;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::output::ellipsize;
use crate::tui::agents::{self, AgentStatus};
use crate::tui::app::{App, EntryRole};
use crate::tui::jobs::JobStatus;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crate::tui::theme::{Tokens, tokens};

const FILTERS: [WorkFilter; 3] = [WorkFilter::All, WorkFilter::Runs, WorkFilter::Commands];

/// Rows the Work panel takes when docked in the composer's place. The transcript
/// above keeps the rest of the frame; Up restores the composer.
pub const DOCKED_HEIGHT: u16 = 14;
/// Never dock into fewer rows: the filter row, list, detail and footer all need
/// room or the panel becomes unreadable.
pub const MIN_DOCKED_HEIGHT: u16 = 9;

/// A selected row is ink, not a fill: accent foreground plus bold, so no orange
/// band covers the text. The literal `›` pointer stays its own cue; under
/// NO_COLOR reverse video carries the meaning without asking for colour.
fn selection(t: Tokens) -> Style {
    if t.colored() {
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}

/// State words are drawn literally; colour is a redundant second cue only.
fn state_color(state: &str, t: Tokens) -> Color {
    match state {
        "waiting" | "orphaned" | "failed" | "parked" => t.error,
        "running" | "starting" | "queued" => t.text,
        "done" | "completed" => t.success,
        _ => t.muted,
    }
}

/// Urgency rank: what needs a person first, then live work, then history.
/// Lower ranks sort to the top of the Work list.
fn urgency(rank: &str) -> u8 {
    match rank {
        "waiting" => 0, // needs input / approval
        "orphaned" => 1,
        "failed" => 2,
        "running" | "starting" | "queued" => 3,
        "parked" => 4,
        "done" | "completed" => 5,
        _ => 6,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum WorkFilter {
    #[default]
    All,
    Runs,
    Commands,
}

impl WorkFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Runs => "Runs",
            Self::Commands => "Commands",
        }
    }

    fn from_key(key: KeyCode) -> Option<Self> {
        match key {
            KeyCode::Char('1') => Some(Self::All),
            KeyCode::Char('2') => Some(Self::Runs),
            KeyCode::Char('3') => Some(Self::Commands),
            _ => None,
        }
    }

    fn next(self, delta: isize) -> Self {
        let i = FILTERS.iter().position(|f| *f == self).unwrap_or(0) as isize;
        FILTERS[(i + delta).rem_euclid(FILTERS.len() as isize) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkKind {
    Run,
    Command,
    Action,
}

#[derive(Debug, Clone)]
enum RowAction {
    Run(String),
    Job(String),
    Tool(String),
    Delegate,
    RunCommand,
}

#[derive(Debug, Clone)]
struct WorkRow {
    kind: WorkKind,
    /// The literal state word, always drawn as text.
    state: String,
    /// Primary label (task or command).
    title: String,
    /// Supporting detail: model, cwd, exit, tail — drawn muted.
    detail: String,
    /// Optional trailing badge (unread mark, exit code).
    badge: String,
    /// Urgency key for the single ranked list.
    rank: String,
    /// Tree indentation for child runs.
    depth: usize,
    action: RowAction,
}

#[derive(Debug, Default)]
pub(crate) struct ActivityOverlay {
    visible: bool,
    filter: WorkFilter,
    selected: usize,
    query: String,
    notice: Option<String>,
}

impl ActivityOverlay {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn rows(&self, app: &App) -> Vec<WorkRow> {
        let mut rows = Vec::new();

        // Runs — delegated child agents, with their own spawn tree.
        if self.filter != WorkFilter::Commands {
            for agent in agents::roster(app)
                .into_iter()
                .filter(|a| a.parent_id.is_some())
            {
                let state = agent_status(agent.status).to_string();
                rows.push(WorkRow {
                    kind: WorkKind::Run,
                    state: state.clone(),
                    title: agent.role.clone(),
                    detail: agent.task.clone(),
                    badge: agent.model.clone().unwrap_or_else(|| "local".to_string()),
                    rank: state,
                    depth: agent.depth.min(3),
                    action: RowAction::Run(agent.agent_id),
                });
            }
        }

        // Commands — background jobs plus recent model tool calls.
        if self.filter != WorkFilter::Runs {
            for job in app.jobs.newest_first() {
                let state = job_status(job.status).to_string();
                let tail = job.tail(1).pop().unwrap_or_default();
                let detail = format!(
                    "{} · {}",
                    ellipsize(&job.command, 34),
                    ellipsize(tail.trim(), 28)
                );
                rows.push(WorkRow {
                    kind: WorkKind::Command,
                    state: state.clone(),
                    title: format!("#{}", job.id),
                    detail,
                    badge: job.exit.map(|code| code.to_string()).unwrap_or_default(),
                    rank: state,
                    depth: 0,
                    action: RowAction::Job(job.id.clone()),
                });
            }
            for entry in app
                .entries
                .iter()
                .rev()
                .filter(|e| {
                    matches!(
                        e.role,
                        EntryRole::Tool
                            | EntryRole::ToolDone
                            | EntryRole::ToolFailed
                            | EntryRole::ToolInterrupted
                    )
                })
                .take(12)
            {
                let state = if entry.role == EntryRole::Tool {
                    "running"
                } else {
                    "done"
                };
                rows.push(WorkRow {
                    kind: WorkKind::Command,
                    state: state.to_string(),
                    title: "tool".to_string(),
                    detail: ellipsize(&entry.text.replace('\n', " "), 56),
                    badge: String::new(),
                    rank: state.to_string(),
                    depth: 0,
                    action: RowAction::Tool(entry.text.clone()),
                });
            }
        }

        // Actions are always available and sort last.
        rows.push(WorkRow {
            kind: WorkKind::Action,
            state: String::new(),
            title: "delegate a run".to_string(),
            detail: "Spawn a scoped agent with its own worktree".to_string(),
            badge: String::new(),
            rank: "action".to_string(),
            depth: 0,
            action: RowAction::Delegate,
        });
        rows.push(WorkRow {
            kind: WorkKind::Action,
            state: String::new(),
            title: "open system shell".to_string(),
            detail: "Keep a shell open at the workspace root while you work".to_string(),
            badge: String::new(),
            rank: "action".to_string(),
            depth: 0,
            action: RowAction::RunCommand,
        });

        // One ranked list: needs-you first, then live work, then history.
        rows.sort_by_key(|row| urgency(&row.rank));

        // Live filter narrows the same list; never a second command source.
        if !self.query.is_empty() {
            let q = self.query.to_ascii_lowercase();
            rows.retain(|row| {
                row.title.to_ascii_lowercase().contains(&q)
                    || row.detail.to_ascii_lowercase().contains(&q)
                    || row.state.contains(&q)
            });
        }
        rows
    }

    fn clamp(&mut self, len: usize) {
        self.selected = if len == 0 {
            0
        } else {
            self.selected.min(len - 1)
        };
    }

    fn move_selection(&mut self, app: &App, delta: isize) {
        let len = self.rows(app).len();
        self.clamp(len);
        if len == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
    }

    fn activate(&mut self, app: &mut App) -> OverlayAction {
        let Some(row) = self.rows(app).get(self.selected).cloned() else {
            return OverlayAction::None;
        };
        match row.action {
            RowAction::Run(id) => {
                app.focus_agent = Some(id);
                OverlayAction::Push(OverlayKind::Focus)
            }
            RowAction::Job(id) => {
                if let Some(job) = app.jobs.get(&id) {
                    let tail = job.tail(40);
                    let body = if tail.is_empty() {
                        format!("{} · {} · no output yet", id, job.status.label())
                    } else {
                        format!("{} · {}\n{}", id, job.status.label(), tail.join("\n"))
                    };
                    app.push(EntryRole::Answer, body);
                }
                OverlayAction::Close
            }
            RowAction::Tool(text) => {
                app.push(EntryRole::Answer, text);
                OverlayAction::Close
            }
            RowAction::Delegate => {
                set_prompt(app, "/delegate ");
                OverlayAction::Close
            }
            RowAction::RunCommand => {
                let root = app
                    .ctx
                    .as_ref()
                    .map(|ctx| ctx.root.as_path())
                    .unwrap_or_else(|| Path::new("."));
                match open_system_terminal(root) {
                    Ok(()) => self.notice = Some("Opened a shell at the workspace root.".into()),
                    Err(error) => self.notice = Some(format!("Could not open a shell: {error}")),
                }
                OverlayAction::None
            }
        }
    }
}

impl Overlay for ActivityOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Activity
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.filter = WorkFilter::All;
        self.selected = 0;
        self.query.clear();
        self.notice = None;
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        if let Some(filter) = WorkFilter::from_key(key.code) {
            self.filter = filter;
            self.selected = 0;
            self.query.clear();
            self.notice = None;
            return OverlayAction::None;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Right => {
                self.filter = self.filter.next(1);
                self.selected = 0;
                self.query.clear();
                self.notice = None;
                OverlayAction::None
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.filter = self.filter.next(-1);
                self.selected = 0;
                self.query.clear();
                self.notice = None;
                OverlayAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(app, 1);
                OverlayAction::None
            }
            // Up is the symmetric exit: Down on an empty composer opened Work,
            // so Up closes it and brings the composer back. `k` still walks the
            // list for anyone who expects vim keys.
            KeyCode::Up => OverlayAction::Close,
            KeyCode::Char('k') => {
                self.move_selection(app, -1);
                OverlayAction::None
            }
            // Specific chars with meaning must precede the query-append arm.
            KeyCode::Char('X') => {
                if let Some(WorkRow {
                    action: RowAction::Job(id),
                    ..
                }) = self.rows(app).get(self.selected)
                {
                    match app.jobs.cancel(id) {
                        Ok(true) => {
                            app.sync_jobs();
                            self.notice = Some(format!("Stopped job {id}."));
                        }
                        Ok(false) => self.notice = Some(format!("Job {id} already finished.")),
                        Err(error) => self.notice = Some(format!("Could not stop {id}: {error}")),
                    }
                }
                OverlayAction::None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if c == '/' && self.query.is_empty() {
                    return OverlayAction::None;
                }
                self.query.push(c);
                self.clamp(self.rows(app).len());
                self.selected = 0;
                OverlayAction::None
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.clamp(self.rows(app).len());
                OverlayAction::None
            }
            KeyCode::Esc => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.clamp(self.rows(app).len());
                    OverlayAction::None
                } else {
                    OverlayAction::Close
                }
            }
            KeyCode::Enter => self.activate(app),
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        let t = tokens();
        // The Work surface is docked in the composer's band, not floated over
        // the frame: it fills the rectangle it is given. Callers pass the whole
        // frame only for the legacy floating case, where we still centre a card.
        let rect = if area.height > DOCKED_HEIGHT + 4 {
            let width = area.width.saturating_sub(2).min(96);
            let height = area.height.saturating_sub(2).min(22);
            let x = area.x + (area.width - width) / 2;
            let y = area.y + (area.height - height) / 2;
            Rect::new(x, y, width, height)
        } else {
            area
        };
        if rect.width < 40 || rect.height < MIN_DOCKED_HEIGHT {
            frame.render_widget(
                Paragraph::new("Resize the terminal to open Work")
                    .style(Style::default().fg(t.muted))
                    .alignment(Alignment::Center),
                area,
            );
            return;
        }
        frame.render_widget(Clear, rect);
        frame.render_widget(Block::default().style(Style::default().bg(t.surface)), rect);
        let inner = Rect::new(
            rect.x + 3,
            rect.y,
            rect.width.saturating_sub(6),
            rect.height,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // filter row
                Constraint::Length(1), // rule
                Constraint::Min(2),    // list + detail
                Constraint::Length(2), // footer
            ])
            .split(inner);

        // Filter row — one surface, three ways to narrow it.
        let filters = FILTERS
            .iter()
            .flat_map(|filter| {
                let active = self.filter == *filter;
                let style = if active {
                    selection(t).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.muted)
                };
                [
                    Span::styled(format!(" {} ", filter.label()), style),
                    Span::raw("  "),
                ]
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Line::from(filters)), chunks[0]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(t.border),
            ))),
            chunks[1],
        );

        // List and detail share the body: the list owns the upper portion.
        let body_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(62), Constraint::Min(3)])
            .split(chunks[2]);

        let rows = self.rows(app);
        self.clamp(rows.len());
        let available = (body_chunks[0].height as usize / 2).max(1);
        let start = self.selected.saturating_sub(available.saturating_sub(1));
        let mut lines = Vec::new();
        for (offset, row) in rows.iter().skip(start).take(available).enumerate() {
            let index = start + offset;
            let selected = index == self.selected;
            let row_style = if selected {
                selection(t)
            } else {
                Style::default()
            };
            let pointer = if selected { "› " } else { "  " };
            let pointer_style = if selected {
                row_style.fg(t.accent)
            } else {
                Style::default()
            };
            let indent = "  ".repeat(row.depth);
            let state_style = Style::default().fg(state_color(&row.state, t));
            let title_style = if selected {
                row_style.add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.text)
            };
            let state_label = if row.state.is_empty() {
                String::new()
            } else {
                format!("{} ", row.state)
            };
            lines.push(Line::from(vec![
                Span::styled(pointer, pointer_style),
                Span::styled(indent, Style::default().fg(t.muted)),
                Span::styled(state_label, state_style),
                Span::styled(ellipsize(&row.title, 24), title_style),
                Span::styled(
                    format!("  {}", ellipsize(&row.detail, 34)),
                    Style::default().fg(t.muted),
                ),
                Span::styled(row.badge.clone(), state_style),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "No live work yet. Press n to start one.",
                Style::default().fg(t.muted),
            )));
        }
        frame.render_widget(Paragraph::new(lines), body_chunks[0]);

        // Detail drawer for the selected row.
        let detail = rows
            .get(self.selected)
            .map(|row| {
                let head = match row.kind {
                    WorkKind::Run => "RUN",
                    WorkKind::Command => "COMMAND",
                    WorkKind::Action => "ACTION",
                };
                vec![
                    Line::from(vec![
                        Span::styled(
                            head,
                            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  {}", row.title),
                            Style::default().fg(t.text).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(Span::styled(
                        ellipsize(&row.detail, inner.width.saturating_sub(2) as usize),
                        Style::default().fg(t.muted),
                    )),
                ]
            })
            .unwrap_or_else(|| {
                vec![Line::from(Span::styled(
                    "Nothing selected.",
                    Style::default().fg(t.muted),
                ))]
            });
        frame.render_widget(Paragraph::new(detail), body_chunks[1]);

        // Contextual footer: the three actions that matter on this surface.
        let filter_hint = if self.query.is_empty() {
            self.filter.label().to_string()
        } else {
            format!("{} /{}", self.filter.label(), self.query)
        };
        let help = format!(
            "⏎ open · n delegate · 1/2/3 {} · / filter · x stop ·  ? help · esc dismiss",
            filter_hint
        );
        let footer = self
            .notice
            .as_deref()
            .map(|notice| format!("{notice}   ·   {help}"))
            .unwrap_or_else(|| help.clone());
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(t.muted)),
            chunks[3],
        );
    }
}

fn agent_status(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting => "starting",
        AgentStatus::Running => "running",
        AgentStatus::Waiting => "waiting",
        AgentStatus::Done => "done",
        AgentStatus::Failed => "failed",
        AgentStatus::Killed => "stopped",
        AgentStatus::Orphaned => "orphaned",
        AgentStatus::Parked => "parked",
    }
}

fn job_status(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Running => "running",
        JobStatus::Completed => "done",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "stopped",
    }
}

fn set_prompt(app: &mut App, value: &str) {
    app.input = value.to_string();
    app.cursor = app.input.chars().count();
    app.sync_slash();
}

fn open_system_terminal(root: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        if Command::new("wt.exe")
            .args([
                "-w",
                "0",
                "new-tab",
                "--title",
                "Memnoth shell",
                "--startingDirectory",
            ])
            .arg(root)
            .current_dir(root)
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
        Command::new("cmd.exe")
            .args(["/C", "start", "", "powershell.exe", "-NoExit", "-NoLogo"])
            .current_dir(root)
            .spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let shell_path = root.to_string_lossy().replace('\'', "'\\''");
        let script = format!(
            "tell application \"Terminal\" to do script \"cd -- '{shell_path}'; exec $SHELL -l\""
        )
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
        Command::new("osascript").args(["-e", &script]).spawn()?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let candidates: [(&str, &[&str]); 4] = [
            ("x-terminal-emulator", &[]),
            ("gnome-terminal", &["--"]),
            ("konsole", &["--separate"]),
            ("xterm", &["-e", "sh", "-l"]),
        ];
        for (program, args) in candidates {
            if Command::new(program)
                .args(args)
                .current_dir(root)
                .spawn()
                .is_ok()
            {
                return Ok(());
            }
        }
        anyhow::bail!("no supported terminal emulator was found")
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = root;
        anyhow::bail!("opening a system shell is not supported on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Palette;

    #[test]
    fn selection_stays_visible_without_colour() {
        let plain = selection(Tokens::for_palette(Palette::NoColor));
        assert_eq!(plain.fg, None);
        assert_eq!(plain.bg, None);
        assert!(
            plain.add_modifier.contains(Modifier::REVERSED),
            "reverse video is the only thing marking the row when colour is gone"
        );

        let t = Tokens::for_palette(Palette::Dark);
        let themed = selection(t);
        // Ink-only: accent foreground + bold, never a selected_bg band.
        assert_eq!(
            themed.fg,
            Some(t.accent),
            "selection is accent ink, not near-black selected_fg"
        );
        assert_eq!(themed.bg, None, "a selected row must have no fill");
        assert!(
            themed.add_modifier.contains(Modifier::BOLD),
            "bold carries the selection in ink"
        );
    }

    #[test]
    fn state_colours_are_semantic_and_never_the_only_signal() {
        let plain = Tokens::for_palette(Palette::NoColor);
        for state in [
            "starting",
            "running",
            "waiting",
            "done",
            "completed",
            "failed",
            "stopped",
            "orphaned",
            "parked",
            "queued",
        ] {
            assert_eq!(
                state_color(state, plain),
                Color::Reset,
                "{state} must have no colour under NO_COLOR"
            );
        }

        let t = Tokens::for_palette(Palette::Dark);
        assert_eq!(state_color("failed", t), t.error);
        assert_eq!(state_color("orphaned", t), t.error);
        assert_eq!(state_color("done", t), t.success);
        assert_eq!(state_color("running", t), t.text);
    }

    #[test]
    fn up_restores_the_composer_while_k_still_walks_the_list() {
        let mut overlay = ActivityOverlay::new();
        // Down on an empty composer opened Work; Up must hand control back.
        overlay.show();
        let up = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::NONE,
        );
        assert_eq!(
            overlay.handle_key(up, &mut App::new("s", false)),
            OverlayAction::Close,
            "Up closes Work so the chatbox comes back"
        );
        // Vim navigation stays available for people who expect k to move.
        let k = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('k'),
            crossterm::event::KeyModifiers::NONE,
        );
        assert_eq!(
            overlay.handle_key(k, &mut App::new("s", false)),
            OverlayAction::None,
            "k must stay a list move, not an exit"
        );
    }

    #[test]
    fn urgency_puts_needs_you_first_and_history_last() {
        assert!(urgency("waiting") < urgency("running"));
        assert!(urgency("running") < urgency("done"));
        assert!(urgency("failed") < urgency("done"));
        assert!(urgency("parked") < urgency("done"));
        assert!(urgency("done") < urgency("action"));
    }

    #[test]
    fn filter_keys_and_cycling_are_stable() {
        assert_eq!(
            WorkFilter::from_key(KeyCode::Char('1')),
            Some(WorkFilter::All)
        );
        assert_eq!(
            WorkFilter::from_key(KeyCode::Char('2')),
            Some(WorkFilter::Runs)
        );
        assert_eq!(
            WorkFilter::from_key(KeyCode::Char('3')),
            Some(WorkFilter::Commands)
        );
        assert_eq!(WorkFilter::All.next(1), WorkFilter::Runs);
        assert_eq!(WorkFilter::Commands.next(1), WorkFilter::All);
        assert_eq!(WorkFilter::All.next(-1), WorkFilter::Commands);
    }

    #[test]
    fn status_labels_remain_literal_words() {
        assert_eq!(agent_status(AgentStatus::Failed), "failed");
        assert_eq!(agent_status(AgentStatus::Done), "done");
        assert_eq!(agent_status(AgentStatus::Orphaned), "orphaned");
        assert_eq!(JobStatus::Running.label(), "running");
        assert_eq!(JobStatus::Completed.label(), "done");
        assert_eq!(JobStatus::Failed.label(), "failed");
        assert_eq!(job_status(JobStatus::Cancelled), "stopped");
    }

    #[test]
    fn filter_row_uses_native_vocabulary() {
        assert_eq!(WorkFilter::All.label(), "All");
        assert_eq!(WorkFilter::Runs.label(), "Runs");
        assert_eq!(WorkFilter::Commands.label(), "Commands");
    }
}
