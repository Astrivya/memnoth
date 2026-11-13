//! W5-11: the first run gets one four-move card.
//!
//! Ask, Remember, Prove, Delegate. Each row is state-derived, carries one real
//! example, and names the unlock instead of a dead example when the machine
//! cannot run it. The card is the shortest path to understanding why the
//! product is different — it is not a second setup checklist.
//!
//! `ui.onboarded` and `ui.onboarding_version` are written only when the card is
//! dismissed: `s`/`esc`/`q` skips, or `Enter` runs (or prefills) a row. Merely
//! drawing the card marks nothing. `/onboarding` reopens the same card with the
//! current state and never touches either field.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};

/// The version written when the card is dismissed. Bump only for a migration,
/// never to auto-show a new tour: `/onboarding` is the opt-in.
pub(crate) const ONBOARDING_VERSION: u32 = 1;

/// What `Enter` does on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowAction {
    /// Put the line in the prompt and submit it.
    Run,
    /// Put the line in the prompt and stop: the person confirms the form.
    Prefill,
    /// The verb cannot run in this build; the transcript gets the honest line.
    Explain,
}

/// One capability row.
#[derive(Debug, Clone)]
pub(crate) struct MoveRow {
    pub label: &'static str,
    /// The example, or the unlock command when the machine is missing a piece.
    pub example: String,
    /// One line on what the move teaches.
    pub blurb: String,
    /// Advanced commands named by `?`.
    pub advanced: &'static str,
    pub action: RowAction,
    /// A line to show when the move cannot run here.
    pub explain: Option<String>,
}

/// The card.
#[derive(Debug)]
pub struct OnboardingOverlay {
    visible: bool,
    rows: Vec<MoveRow>,
    selected: usize,
    show_detail: bool,
    /// First-run cards record their dismissal; `/onboarding` is a view.
    record_on_close: bool,
}

impl OnboardingOverlay {
    /// The automatic card: dismissal writes the seen flag.
    pub fn first_run() -> Self {
        Self {
            visible: false,
            rows: Vec::new(),
            selected: 0,
            show_detail: false,
            record_on_close: true,
        }
    }

    /// `/onboarding`: the same card, current state, no writes.
    pub fn replay() -> Self {
        Self {
            record_on_close: false,
            ..Self::first_run()
        }
    }

    /// Record the card as seen. Called only from a dismissal path.
    fn mark_seen(app: &mut App) {
        let ui = app.config.ui.get_or_insert_default();
        ui.onboarded = Some(true);
        ui.onboarding_version = Some(ONBOARDING_VERSION);
        // Best-effort: a read-only config must not trap the person on the card.
        let _ = app.config.save(&app.config_path);
    }

    fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.rows.len();
    }

    fn select_previous(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = (self.selected + self.rows.len() - 1) % self.rows.len();
    }

    /// Act on the selected row. Returns the action the host should take.
    fn choose(&mut self, app: &mut App) -> OverlayAction {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return OverlayAction::Close;
        };
        match row.action {
            RowAction::Explain => {
                if let Some(explain) = row.explain {
                    app.push(crate::tui::app::EntryRole::Info, explain);
                }
            }
            RowAction::Run | RowAction::Prefill => {
                app.input = row.example.clone();
                app.cursor = app.input.chars().count();
                if row.action == RowAction::Run {
                    app.pending_submit = true;
                }
            }
        }
        if self.record_on_close {
            Self::mark_seen(app);
        }
        OverlayAction::Close
    }
}

impl Overlay for OnboardingOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Onboarding
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.selected = 0;
        self.show_detail = false;
    }

    fn on_open(&mut self, app: &mut App) {
        if let Some(ctx) = app.ctx.as_ref() {
            self.rows = rows_for(ctx);
        }
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_previous();
                OverlayAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                OverlayAction::None
            }
            KeyCode::Char('?') => {
                self.show_detail = !self.show_detail;
                OverlayAction::None
            }
            KeyCode::Enter => self.choose(app),
            KeyCode::Tab => {
                // Insert only. The prompt is where a person edits the example
                // before running it, and Tab is not a completion of the card.
                if let Some(row) = self.rows.get(self.selected) {
                    app.input = row.example.clone();
                    app.cursor = app.input.chars().count();
                }
                OverlayAction::Close
            }
            KeyCode::Char('s' | 'q') | KeyCode::Esc => {
                if self.record_on_close {
                    Self::mark_seen(app);
                }
                OverlayAction::Close
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible || area.width < 24 || area.height < 8 || self.rows.is_empty() {
            return;
        }
        let width = area.width.min(68);
        let height = if self.show_detail { 18 } else { 16 };
        let height = height.min(area.height);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let popup = Rect::new(x, y, width, height);
        // The card clears its own rect only. Clearing the whole frame was tried
        // and reverted: ratatui diffs frames, so the header and sidebar it wiped
        // are never redrawn while the modal is open, and the screen reads as
        // blank. The overlap in the margins is cosmetic and stays until the
        // layout gives the card a column of its own.
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .title(" What Mennoth can do ")
            .title_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.height < 6 || inner.width < 10 {
            return;
        }

        let mut lines: Vec<Line> = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            let selected = index == self.selected;
            let marker = if selected { " ▸ " } else { "   " };
            let label_style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::styled(format!("{:<10}", row.label), label_style),
                Span::styled(
                    row.example.clone(),
                    if selected {
                        Style::default().fg(Color::White)
                    } else {
                        Style::default().fg(Color::Gray)
                    },
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("              ", Style::default()),
                Span::styled(row.blurb.clone(), Style::default().fg(Color::DarkGray)),
            ]));
        }

        if self.show_detail {
            lines.push(Line::from(""));
            if let Some(row) = self.rows.get(self.selected) {
                lines.push(Line::from(vec![
                    Span::styled("   detail    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(row.advanced, Style::default().fg(Color::Gray)),
                ]));
                if let Some(explain) = &row.explain {
                    lines.push(Line::from(vec![
                        Span::styled("   next      ", Style::default().fg(Color::DarkGray)),
                        Span::styled(explain.clone(), Style::default().fg(Color::Yellow)),
                    ]));
                }
            }
        }

        // Two footer lines, pinned to the bottom: the safety contract is not a
        // fifth course, and the keys are not a paragraph.
        let list_height = inner.height.saturating_sub(2) as usize;
        let visible: Vec<Line> = lines.into_iter().take(list_height).collect();
        let list_area = Rect::new(inner.x, inner.y, inner.width, list_height as u16);
        frame.render_widget(Paragraph::new(visible), list_area);

        let safety = Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 1);
        let keys = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " Safety    reads outside workspace denied · writes ask · /permissions",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Left),
            safety,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " Enter try · Tab insert · ↑↓ move · s skip · ? detail · esc skip",
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Left),
            keys,
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("enter", "try the selected move"),
            ("tab", "put it in the prompt"),
            ("up/down", "move"),
            ("?", "advanced commands for the move"),
            ("s/esc", "skip; the card does not return"),
        ]
    }
}

/// The four moves against this machine's real state.
///
/// A row that cannot run names the unlock, not a dead example. Delegate is
/// honest about the 1.0 boundary: the child runtime is a CLI door until the
/// Agent Deck ships.
pub(crate) fn rows_for(ctx: &crate::context::AppContext) -> Vec<MoveRow> {
    let (_, model_ready) = crate::tui::model_state(ctx);
    let graph_indexed = crate::tui::graph_state(ctx).indexed;
    let vault_ready = ctx.vault_path.exists();

    let mut rows = Vec::with_capacity(4);

    rows.push(if model_ready {
        MoveRow {
            label: "Ask",
            example: "why did the retry cap change?".to_string(),
            blurb: "one answer from memory and code".to_string(),
            advanced: "/why · /impact · /simulate",
            action: RowAction::Run,
            explain: None,
        }
    } else {
        MoveRow {
            label: "Ask",
            example: "/model".to_string(),
            blurb: "connect a model, then ask this".to_string(),
            advanced: "/model · /connect",
            action: RowAction::Run,
            explain: None,
        }
    });

    rows.push(if vault_ready {
        MoveRow {
            label: "Remember",
            example: "/vault remember retry cap is 5".to_string(),
            blurb: "a dated belief, not a note".to_string(),
            advanced: "/vault asof · /vault history · /vault contradictions",
            action: RowAction::Prefill,
            explain: None,
        }
    } else {
        MoveRow {
            label: "Remember",
            example: "/vault init".to_string(),
            blurb: "create memory, then remember this".to_string(),
            advanced: "/vault init · /vault remember",
            action: RowAction::Run,
            explain: None,
        }
    });

    rows.push(if graph_indexed {
        MoveRow {
            label: "Prove",
            example: "/why retry-cap".to_string(),
            blurb: "source, date, what replaced it".to_string(),
            advanced: "/lineage · /receipt · /drift",
            action: RowAction::Run,
            explain: None,
        }
    } else {
        MoveRow {
            label: "Prove",
            example: "/kg index".to_string(),
            blurb: "index the code, then prove a change".to_string(),
            advanced: "/kg index · /search",
            action: RowAction::Run,
            explain: None,
        }
    });

    rows.push(MoveRow {
        label: "Delegate",
        example: "mn agents run review-auth \"review refresh\"".to_string(),
        blurb: "a child with scope, receipt, and merge".to_string(),
        advanced: "mn agents run · mn agents result · /handoff",
        action: RowAction::Explain,
        explain: Some(
            "delegation runs from the CLI in 1.0: mn agents run <name> \"<task>\"".to_string(),
        ),
    });

    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replayed_card_does_not_record_itself() {
        let card = OnboardingOverlay::replay();
        assert!(!card.record_on_close);
        assert!(OnboardingOverlay::first_run().record_on_close);
    }

    #[test]
    fn the_selection_wraps_instead_of_stopping() {
        let mut card = OnboardingOverlay::first_run();
        card.rows = vec![
            MoveRow {
                label: "Ask",
                example: "/model".into(),
                blurb: String::new(),
                advanced: "",
                action: RowAction::Run,
                explain: None,
            },
            MoveRow {
                label: "Prove",
                example: "/kg index".into(),
                blurb: String::new(),
                advanced: "",
                action: RowAction::Run,
                explain: None,
            },
        ];
        card.select_next();
        assert_eq!(card.selected, 1);
        card.select_next();
        assert_eq!(card.selected, 0);
        card.select_previous();
        assert_eq!(card.selected, 1);
    }
}
