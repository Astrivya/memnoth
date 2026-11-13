//! `/checkpoints`: the snapshots `/checkpoint` wrote, and the one to restore.
//!
//! Every row is a file on disk: the label, the message count, the commit the
//! workspace was on. Enter writes the selected snapshot to
//! `<session>-rewind.jsonl`; nothing here invents a checkpoint or restores one
//! that is not in the store.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::checkpoint::{Checkpoint, CheckpointStore, latest_session};
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// The checkpoint rows for the newest conversation.
#[derive(Debug, Default)]
pub struct CheckpointsOverlay {
    visible: bool,
    session: String,
    rows: Vec<Checkpoint>,
    selected: usize,
    list_state: ListState,
    note: Option<String>,
}

impl CheckpointsOverlay {
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            list_state,
            ..Default::default()
        }
    }

    fn load(&mut self, app: &App) {
        let Some(ctx) = app.ctx.as_ref() else {
            self.note = Some("no workspace context — checkpoints live under ~/.memnoth".into());
            return;
        };
        let Ok(session) = latest_session(&ctx.sessions_dir) else {
            self.note = Some(format!(
                "no conversations in {} yet — /checkpoint writes one",
                ctx.sessions_dir.display()
            ));
            return;
        };
        let store = CheckpointStore::new(&ctx.home);
        self.rows = store.list(&session).unwrap_or_default();
        self.session = session.clone();
        self.selected = 0;
        self.list_state.select(Some(0));
        if self.rows.is_empty() {
            self.note = Some(format!(
                "no checkpoints for {session} — /checkpoint writes one"
            ));
        }
    }

    fn restore_selected(&mut self, app: &App) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        let Some(ctx) = app.ctx.as_ref() else {
            self.note = Some("no workspace context".into());
            return;
        };
        let store = CheckpointStore::new(&ctx.home);
        match store.restore(&ctx.sessions_dir, &self.session, &row) {
            Ok(path) => {
                let _ = store.set_cursor(&self.session, row.seq);
                self.note = Some(format!(
                    "restored checkpoint {} \"{}\" to {} — resume with mn --resume {}-rewind",
                    row.seq,
                    row.name,
                    path.display(),
                    self.session
                ));
            }
            Err(error) => self.note = Some(format!("{error}")),
        }
    }
}

impl Overlay for CheckpointsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Checkpoints
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.list_state.select(Some(0));
    }

    fn on_open(&mut self, app: &mut App) {
        self.load(app);
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => OverlayAction::Close,
            KeyCode::Up => {
                if !self.rows.is_empty() {
                    self.selected = self.selected.saturating_sub(1);
                    self.list_state.select(Some(self.selected));
                }
                OverlayAction::None
            }
            KeyCode::Down => {
                if !self.rows.is_empty() {
                    self.selected = (self.selected + 1).min(self.rows.len() - 1);
                    self.list_state.select(Some(self.selected));
                }
                OverlayAction::None
            }
            KeyCode::Enter => {
                self.restore_selected(app);
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        let block = Block::default()
            .title(format!("Checkpoints — {}", self.session))
            .borders(Borders::ALL);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);

        let inner = area.inner(ratatui::layout::Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(inner);

        let note = self
            .note
            .clone()
            .unwrap_or_else(|| "Enter restores the selected snapshot".to_string());
        frame.render_widget(
            Paragraph::new(note)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::Yellow)),
            chunks[0],
        );

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let style = if index == self.selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                let head = row
                    .git_head
                    .as_deref()
                    .map(|head| head[..head.len().min(8)].to_string())
                    .unwrap_or_else(|| "no git".to_string());
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:>2} ", row.seq),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        format!("{} · {} msgs · {}", row.name, row.messages, head),
                        style,
                    ),
                ]))
            })
            .collect();

        frame.render_stateful_widget(List::new(items), chunks[1], &mut self.list_state);
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![("↑↓", "navigate"), ("Enter", "restore"), ("Esc/q", "close")]
    }
}
