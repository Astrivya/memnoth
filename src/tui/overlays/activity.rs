//! A compact switcher for work the session started or can start.

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

/// A selected row must still be distinguishable when every token is Reset,
/// which is what NO_COLOR (and `TERM=dumb`) produce. Reverse video carries the
/// same meaning without asking the terminal for a colour.
fn selection(t: Tokens) -> Style {
    if t.colored() {
        Style::default().fg(t.selected_fg).bg(t.selected_bg)
    } else {
        Style::default().add_modifier(Modifier::REVERSED)
    }
}

/// State words are drawn literally, so a reader loses nothing when every
/// semantic role collapses to Reset under NO_COLOR. The colour only adds a
/// second, redundant cue: failure is error, completion is success, and a
/// stranded run is a warning.
fn state_color(state: &str, t: Tokens) -> Color {
    match state {
        "failed" => t.error,
        "done" | "completed" => t.success,
        "orphaned" | "parked" => t.warning,
        "running" => t.text,
        _ => t.muted, // starting, waiting, stopped, cancelled
    }
}

const TABS: [ActivityTab; 3] = [
    ActivityTab::Agents,
    ActivityTab::Shell,
    ActivityTab::Terminals,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityTab {
    Agents,
    Shell,
    Terminals,
}

impl ActivityTab {
    fn label(self) -> &'static str {
        match self {
            Self::Agents => "Subagents",
            Self::Shell => "Shell",
            Self::Terminals => "Terminals",
        }
    }

    fn index(self) -> usize {
        TABS.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn next(self, delta: isize) -> Self {
        let len = TABS.len() as isize;
        TABS[(self.index() as isize + delta).rem_euclid(len) as usize]
    }
}

#[derive(Debug, Clone)]
enum RowAction {
    Agent(String),
    Job(String),
    Tool(String),
    NewAgent,
    NewShellTask,
    NewTerminal,
}

#[derive(Debug, Clone)]
struct ActivityRow {
    title: String,
    detail: String,
    state: String,
    action: Option<RowAction>,
}

#[derive(Debug, Default)]
pub(crate) struct ActivityOverlay {
    visible: bool,
    tab: Option<ActivityTab>,
    selected: usize,
    notice: Option<String>,
}

impl ActivityOverlay {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn tab(&self) -> ActivityTab {
        self.tab.unwrap_or(ActivityTab::Shell)
    }

    fn rows(&self, app: &App) -> Vec<ActivityRow> {
        match self.tab() {
            ActivityTab::Agents => {
                let mut rows: Vec<_> = agents::roster(app)
                    .into_iter()
                    .filter(|agent| agent.parent_id.is_some())
                    .map(|agent| ActivityRow {
                        title: format!("{}  {}", agent.role, ellipsize(&agent.task, 55)),
                        detail: agent
                            .worktree
                            .as_deref()
                            .or(agent.model.as_deref())
                            .unwrap_or("subagent run")
                            .to_string(),
                        state: agent_status(agent.status).to_string(),
                        action: Some(RowAction::Agent(agent.agent_id)),
                    })
                    .collect();
                rows.push(ActivityRow {
                    title: "+ Delegate a task…".to_string(),
                    detail: "Start a scoped subagent with /delegate".to_string(),
                    state: String::new(),
                    action: Some(RowAction::NewAgent),
                });
                rows
            }
            ActivityTab::Shell => {
                let mut rows: Vec<_> = app
                    .jobs
                    .newest_first()
                    .map(|job| ActivityRow {
                        title: ellipsize(&job.command, 62),
                        detail: format!("{} · {}", job.id, job.started_at),
                        state: job.status.label().to_string(),
                        action: Some(RowAction::Job(job.id.clone())),
                    })
                    .collect();
                rows.extend(
                    app.entries
                        .iter()
                        .rev()
                        .filter(|entry| {
                            matches!(
                                entry.role,
                                EntryRole::Tool
                                    | EntryRole::ToolDone
                                    | EntryRole::ToolFailed
                                    | EntryRole::ToolInterrupted
                            )
                        })
                        .take(30)
                        .map(|entry| ActivityRow {
                            title: ellipsize(&entry.text.replace('\n', " "), 62),
                            detail: "model tool call".to_string(),
                            state: if entry.role == EntryRole::Tool {
                                "running".to_string()
                            } else {
                                "done".to_string()
                            },
                            action: Some(RowAction::Tool(entry.text.clone())),
                        }),
                );
                rows.push(ActivityRow {
                    title: "+ Run a shell task…".to_string(),
                    detail: "Starts a visible background job in the workspace".to_string(),
                    state: String::new(),
                    action: Some(RowAction::NewShellTask),
                });
                rows
            }
            ActivityTab::Terminals => {
                let root = app
                    .ctx
                    .as_ref()
                    .map(|ctx| ctx.root.display().to_string())
                    .unwrap_or_else(|| "current directory".to_string());
                vec![ActivityRow {
                    title: "+ New terminal".to_string(),
                    detail: format!("Open your system shell at {}", ellipsize(&root, 52)),
                    state: String::new(),
                    action: Some(RowAction::NewTerminal),
                }]
            }
        }
    }

    fn move_selection(&mut self, app: &App, delta: isize) {
        let len = self.rows(app).len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected =
                (self.selected as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
        }
    }

    fn activate(&mut self, app: &mut App) -> OverlayAction {
        let Some(row) = self.rows(app).get(self.selected).cloned() else {
            return OverlayAction::None;
        };
        match row.action {
            Some(RowAction::Agent(id)) => {
                app.focus_agent = Some(id);
                OverlayAction::Push(OverlayKind::Focus)
            }
            Some(RowAction::Job(id)) => {
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
            Some(RowAction::Tool(text)) => {
                app.push(EntryRole::Answer, text);
                OverlayAction::Close
            }
            Some(RowAction::NewAgent) => {
                set_prompt(app, "/delegate ");
                OverlayAction::Close
            }
            Some(RowAction::NewShellTask) => {
                set_prompt(app, "/tasks run ");
                OverlayAction::Close
            }
            Some(RowAction::NewTerminal) => {
                let root = app
                    .ctx
                    .as_ref()
                    .map(|ctx| ctx.root.as_path())
                    .unwrap_or_else(|| Path::new("."));
                match open_system_terminal(root) {
                    Ok(()) => {
                        self.notice =
                            Some("Opened a new system terminal at the workspace root.".into())
                    }
                    Err(error) => self.notice = Some(format!("Could not open a terminal: {error}")),
                }
                OverlayAction::None
            }
            None => OverlayAction::None,
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
    }

    fn on_open(&mut self, app: &mut App) {
        let has_live_agent = agents::roster(app)
            .iter()
            .any(|agent| agent.parent_id.is_some() && agent.status == AgentStatus::Running);
        let has_live_shell = app.jobs.iter().any(|job| job.status == JobStatus::Running);
        self.tab = Some(if has_live_agent {
            ActivityTab::Agents
        } else if has_live_shell {
            ActivityTab::Shell
        } else {
            ActivityTab::Terminals
        });
        self.selected = 0;
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc => OverlayAction::Close,
            KeyCode::Tab | KeyCode::Right => {
                self.tab = Some(self.tab().next(1));
                self.selected = 0;
                self.notice = None;
                OverlayAction::None
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.tab = Some(self.tab().next(-1));
                self.selected = 0;
                self.notice = None;
                OverlayAction::None
            }
            KeyCode::Char('n') => {
                if let Some(index) = self.rows(app).iter().position(|row| {
                    row.action.as_ref().is_some_and(|action| {
                        matches!(
                            action,
                            RowAction::NewAgent | RowAction::NewShellTask | RowAction::NewTerminal
                        )
                    })
                }) {
                    self.selected = index;
                    self.activate(app)
                } else {
                    OverlayAction::None
                }
            }
            KeyCode::Down => {
                self.move_selection(app, 1);
                OverlayAction::None
            }
            KeyCode::Up => {
                self.move_selection(app, -1);
                OverlayAction::None
            }
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(ActivityRow {
                    action: Some(RowAction::Job(id)),
                    ..
                }) = self.rows(app).get(self.selected)
                {
                    match app.jobs.cancel(id) {
                        Ok(true) => {
                            app.sync_jobs();
                            self.notice = Some(format!("Stopped shell job {id}."));
                        }
                        Ok(false) => {
                            self.notice = Some(format!("Shell job {id} already finished."))
                        }
                        Err(error) => self.notice = Some(format!("Could not stop {id}: {error}")),
                    }
                }
                OverlayAction::None
            }
            KeyCode::Enter => self.activate(app),
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, app: &App) {
        let t = tokens();
        let width = area.width.saturating_sub(2).min(94);
        let height = area.height.saturating_sub(2).min(20);
        if width < 34 || height < 8 {
            frame.render_widget(
                Paragraph::new("Resize the terminal to open activity")
                    .style(Style::default().fg(t.muted))
                    .alignment(Alignment::Center),
                area,
            );
            return;
        }
        let x = area.x + (area.width - width) / 2;
        let y = area.y + (area.height - height) / 2;
        let rect = Rect::new(x, y, width, height);
        frame.render_widget(Clear, rect);
        frame.render_widget(Block::default().style(Style::default().bg(t.surface)), rect);
        let inner = Rect::new(
            rect.x + 3,
            rect.y + 1,
            rect.width.saturating_sub(6),
            rect.height - 2,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Min(2),
                Constraint::Length(2),
            ])
            .split(inner);

        let rows = self.rows(app);
        let tabs = TABS
            .iter()
            .flat_map(|tab| {
                let selected = self.tab() == *tab;
                let style = if selected {
                    selection(t).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.muted)
                };
                [
                    Span::styled(format!(" {} ", tab.label()), style),
                    Span::raw("   "),
                ]
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(Line::from(tabs)), chunks[0]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "────────────────────────────────────────────────────────────────────────",
                Style::default().fg(t.border),
            ))),
            chunks[1],
        );

        let available = (chunks[2].height as usize / 2).max(1);
        let start = self.selected.saturating_sub(available.saturating_sub(1));
        let visible_rows = rows.iter().skip(start).take(available);
        let mut lines = Vec::new();
        for (offset, row) in visible_rows.enumerate() {
            let index = start + offset;
            let selected = index == self.selected;
            let pointer = if selected { "› " } else { "  " };
            let row_style = if selected {
                selection(t)
            } else {
                Style::default()
            };
            let title_style = if selected {
                row_style.add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.text)
            };
            // The pointer is the current actionable marker: accent is reserved
            // for it, including on the selected row's highlighted background.
            let pointer_style = if selected {
                row_style.fg(t.accent)
            } else {
                Style::default()
            };
            let state_color = state_color(&row.state, t);
            let state_style = if selected {
                row_style.patch(Style::default().fg(state_color))
            } else {
                Style::default().fg(state_color)
            };
            lines.push(Line::from(vec![
                Span::styled(pointer, pointer_style),
                Span::styled(
                    ellipsize(
                        &row.title,
                        (inner.width as usize).saturating_sub(row.state.len() + 7),
                    ),
                    title_style,
                ),
                Span::raw("  "),
                Span::styled(row.state.clone(), state_style),
            ]));
            let detail_style = if selected {
                row_style
            } else {
                Style::default().fg(t.muted)
            };
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    ellipsize(&row.detail, (inner.width as usize).saturating_sub(5)),
                    detail_style,
                ),
            ]));
        }
        let content = if lines.is_empty() {
            vec![Line::from(Span::styled(
                "Nothing running here yet.",
                Style::default().fg(t.muted),
            ))]
        } else {
            lines
        };
        frame.render_widget(Paragraph::new(content), chunks[2]);

        let help = match self.tab() {
            ActivityTab::Agents => {
                "↑↓ choose · enter inspect · n delegate · tab switch · esc close"
            }
            ActivityTab::Shell => {
                "↑↓ choose · enter output · ctrl-x stop · n run task · tab switch"
            }
            ActivityTab::Terminals => "enter / n new terminal · tab switch · esc close",
        };
        let footer = self
            .notice
            .as_deref()
            .map(|notice| format!("{notice}   ·   {help}"))
            .unwrap_or_else(|| help.to_string());
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
                "Memnoth terminal",
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
        anyhow::bail!("opening a system terminal is not supported on this platform")
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
        assert_eq!(themed.fg, Some(t.selected_fg));
        assert_eq!(themed.bg, Some(t.selected_bg));
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
            "cancelled",
        ] {
            assert_eq!(
                state_color(state, plain),
                Color::Reset,
                "{state} must have no colour under NO_COLOR"
            );
        }

        let t = Tokens::for_palette(Palette::Dark);
        assert_eq!(state_color("failed", t), t.error);
        assert_eq!(state_color("done", t), t.success);
        assert_eq!(state_color("completed", t), t.success);
        assert_eq!(state_color("orphaned", t), t.warning);
        assert_eq!(state_color("parked", t), t.warning);
        assert_eq!(state_color("running", t), t.text);
        assert_eq!(state_color("cancelled", t), t.muted);
    }

    /// The state word itself is drawn from these literals, never from a colour,
    /// so stripping every token still leaves the failure readable.
    #[test]
    fn status_labels_remain_literal_words() {
        assert_eq!(agent_status(AgentStatus::Failed), "failed");
        assert_eq!(agent_status(AgentStatus::Done), "done");
        assert_eq!(agent_status(AgentStatus::Orphaned), "orphaned");
        assert_eq!(JobStatus::Running.label(), "running");
        assert_eq!(JobStatus::Completed.label(), "done");
        assert_eq!(JobStatus::Failed.label(), "failed");
    }
}
