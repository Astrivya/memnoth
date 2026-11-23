//! Theme picker overlay: every palette in one place, with live preview.
//!
//! Moving the cursor changes the process-wide palette immediately, so the whole
//! live UI follows the candidate before anything is committed. Nothing is
//! written to the config until Enter. Every way out that is not Enter restores
//! the palette that was active when the overlay opened.
//!
//! The row layout follows `docs/research/multi-theme-plan-2026-11-23.md`:
//!
//! ```text
//! ❯ ● Catppuccin Mocha   ██ ░░ ▓▓  Aa 123  · dark
//!   ● Dark (current)     ██ ░░ ▓▓  Aa 123  · dark
//! ```
//!
//! The `❯` selection glyph, the `●` current marker and the `(current)` word are
//! always printed, and carry BOLD, so selection and active theme survive
//! `NO_COLOR` where every token resolves to `Reset`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crate::tui::theme::{Palette, Tokens, init_palette, palette, tokens};

/// Selection and current-theme cues. Text, not color: they are the whole cue
/// when `NO_COLOR` strips every token to `Reset`.
const SELECTED: &str = "❯";
const CURRENT: &str = "●";

/// A colored block for one role. `color` is always a value read from a
/// semantic token set — this module never names a raw `Color` to style with.
/// Under `NO_COLOR` the block becomes a plain marker, so no cue is color-only.
fn swatch(color: Color, colored: bool) -> Span<'static> {
    if colored {
        Span::styled("██", Style::default().fg(color))
    } else {
        Span::raw("░░")
    }
}

/// Dark/light word for a palette, read from its own canvas token.
///
/// This inspects a semantic token value to *name* the mode; it does not pick a
/// colour to draw with. `Reset` is the no-colour palette, which is neither dark
/// nor light and says so.
fn mode_word(p: Palette) -> &'static str {
    match Tokens::for_palette(p).canvas {
        Color::Reset => "plain",
        Color::White => "light",
        Color::Black => "dark",
        Color::Rgb(r, g, b) => {
            // Rec. 709 luma on 0..255. The shortlist's canvases are far enough
            // apart that the threshold is not a judgement call.
            let luma = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
            if luma >= 128.0 { "light" } else { "dark" }
        }
        _ => "dark",
    }
}

#[derive(Debug)]
pub struct ThemePickerOverlay {
    visible: bool,
    palettes: Vec<Palette>,
    filtered: Vec<Palette>,
    /// Selection is tracked by id, not index: a filter that removes earlier
    /// rows must not silently move the highlight to a different theme.
    selected_id: &'static str,
    filter: String,
    filtering: bool,
    /// The palette in force when the picker opened. Preview changes the live
    /// palette; this is what `(current)` marks and what every cancel restores.
    original: Palette,
    /// Guards the one config write: preview never saves, Enter saves once.
    committed: bool,
    note: Option<String>,
    list_state: ListState,
}

impl Default for ThemePickerOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemePickerOverlay {
    pub fn new() -> Self {
        Self::with_original(palette())
    }

    /// Build over a known original. `new` uses the live palette; tests use this
    /// so a parallel test's palette switch cannot change their starting point.
    fn with_original(original: Palette) -> Self {
        let palettes: Vec<Palette> = Palette::all().to_vec();
        let selected_id = palettes
            .iter()
            .find(|p| p.id() == original.id())
            .map_or_else(|| palettes.first().map_or("", |p| p.id()), |p| p.id());
        Self {
            visible: false,
            palettes,
            filtered: Vec::new(),
            selected_id,
            filter: String::new(),
            filtering: false,
            original,
            committed: false,
            note: None,
            list_state: ListState::default(),
        }
    }

    /// The one exit path for every dismiss: Esc, Ctrl-C, and an outside pop.
    /// Restores the exact palette that was active on open and drops the filter.
    fn cancel(&mut self) {
        init_palette(self.original);
        self.filter.clear();
        self.filtering = false;
    }

    /// Recompute the visible rows and keep the selected id if it survived.
    fn rebuild(&mut self) {
        let needle = self.filter.trim().to_ascii_lowercase();
        self.filtered = self
            .palettes
            .iter()
            .copied()
            .filter(|p| {
                needle.is_empty()
                    || p.label().to_ascii_lowercase().contains(&needle)
                    || p.id().to_ascii_lowercase().contains(&needle)
                    || mode_word(*p).contains(&needle)
            })
            .collect();
        if !self.filtered.iter().any(|p| p.id() == self.selected_id)
            && let Some(first) = self.filtered.first().copied()
        {
            self.selected_id = first.id();
        }
        self.sync_list_state();
    }

    fn sync_list_state(&mut self) {
        let index = self
            .filtered
            .iter()
            .position(|p| p.id() == self.selected_id);
        self.list_state.select(index);
    }

    fn selected(&self) -> Option<Palette> {
        self.filtered
            .iter()
            .copied()
            .find(|p| p.id() == self.selected_id)
    }

    /// Move the highlight and immediately preview the whole live UI.
    fn move_by(&mut self, forward: bool) {
        if self.filtered.is_empty() {
            return;
        }
        let index = self
            .filtered
            .iter()
            .position(|p| p.id() == self.selected_id)
            .unwrap_or(0);
        let next = if forward {
            (index + 1).min(self.filtered.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.selected_id = self.filtered[next].id();
        self.sync_list_state();
        self.preview();
    }

    fn select_edge(&mut self, last: bool) {
        let edge = if last {
            self.filtered.last()
        } else {
            self.filtered.first()
        };
        if let Some(p) = edge.copied() {
            self.selected_id = p.id();
            self.sync_list_state();
            self.preview();
        }
    }

    fn preview(&mut self) {
        if let Some(p) = self.selected() {
            // `init_palette` is the palette switch every surface reads; preview
            // is deliberately the same call as commit, just without the save.
            init_palette(p);
        }
    }

    /// Commit the highlighted palette. This is the only place that writes the
    /// config, and `committed` keeps a second Enter from writing it twice.
    fn commit(&mut self, app: &mut App) -> OverlayAction {
        let Some(chosen) = self.selected() else {
            return OverlayAction::None;
        };
        // The shared `set_palette` seam: same switch as preview, now kept.
        init_palette(chosen);
        if !self.committed {
            self.committed = true;
            let id = chosen.id().to_string();
            let mut config = app.config.clone();
            config.ui.get_or_insert_with(Default::default).theme = Some(id.clone());
            if config.save(&app.config_path).is_ok() {
                app.config = config;
            } else {
                self.note = Some(format!("{id} set for this session; config not saved"));
            }
        }
        OverlayAction::Close
    }

    /// Spans for one row, split out so tests can inspect the non-colour cues.
    fn row_spans(&self, p: Palette, t: Tokens, colored: bool) -> Vec<Span<'static>> {
        let selected = p.id() == self.selected_id;
        let is_current = p.id() == self.original.id();
        let candidate = Tokens::for_palette(p);
        let mut spans = Vec::with_capacity(10);

        // `❯` carries selection; `●` plus the `(current)` word carry active.
        // Both are always printed and both take BOLD, so neither depends on ink.
        let selected_style = Style::default().fg(t.accent).add_modifier(Modifier::BOLD);
        spans.push(Span::styled(
            if selected { SELECTED } else { " " },
            selected_style,
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            if is_current { CURRENT } else { " " },
            if is_current {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.muted)
            },
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            p.label(),
            Style::default().fg(t.text).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ));
        spans.push(Span::raw("   "));
        spans.push(swatch(candidate.canvas, colored));
        spans.push(swatch(candidate.text, colored));
        spans.push(swatch(candidate.accent, colored));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "Aa 123",
            if colored {
                Style::default().fg(candidate.text)
            } else {
                Style::default().fg(t.text)
            },
        ));
        spans.push(Span::styled(
            format!("  · {}  ", mode_word(p)),
            Style::default().fg(t.muted),
        ));
        if is_current {
            spans.push(Span::styled(
                "(current)",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ));
        }
        spans
    }

    fn row(&self, p: Palette, t: Tokens, colored: bool) -> ListItem<'static> {
        ListItem::new(Line::from(self.row_spans(p, t, colored)))
    }
}

impl Overlay for ThemePickerOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::ThemePicker
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.filter.clear();
        self.filtering = false;
        self.committed = false;
        self.note = None;
        self.rebuild();
    }

    fn on_open(&mut self, _app: &mut App) {
        // Snapshot before any preview. `present` calls this right after `show`,
        // before the first key can move the cursor.
        self.original = palette();
        if Palette::parse_id(self.original.id()).is_some() {
            self.selected_id = self.original.id();
        }
        self.rebuild();
    }

    fn hide(&mut self) {
        self.visible = false;
        // An outside pop (CloseAll, a replaced stack) is a dismiss too. Enter
        // has already set the chosen palette, so it must not be undone.
        if !self.committed {
            self.cancel();
        }
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // A control chord is a command, never filter text.
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            self.cancel();
            return OverlayAction::Close;
        }

        match key.code {
            KeyCode::Esc => {
                // Esc clears the filter first, then closes. Closing restores
                // the original palette through the same `cancel` as Ctrl-C.
                if self.filtering || !self.filter.is_empty() {
                    self.filter.clear();
                    self.filtering = false;
                    self.rebuild();
                    OverlayAction::None
                } else {
                    self.cancel();
                    OverlayAction::Close
                }
            }
            KeyCode::Up => {
                self.move_by(false);
                OverlayAction::None
            }
            KeyCode::Down => {
                self.move_by(true);
                OverlayAction::None
            }
            KeyCode::PageUp => {
                for _ in 0..5 {
                    self.move_by(false);
                }
                OverlayAction::None
            }
            KeyCode::PageDown => {
                for _ in 0..5 {
                    self.move_by(true);
                }
                OverlayAction::None
            }
            KeyCode::Home => {
                self.select_edge(false);
                OverlayAction::None
            }
            KeyCode::End => {
                self.select_edge(true);
                OverlayAction::None
            }
            KeyCode::Enter => self.commit(app),
            KeyCode::Char('/') if !ctrl && !alt => {
                self.filtering = true;
                self.filter.clear();
                self.rebuild();
                OverlayAction::None
            }
            KeyCode::Backspace if self.filtering => {
                self.filter.pop();
                self.rebuild();
                OverlayAction::None
            }
            KeyCode::Char(ch) if self.filtering && !ctrl && !alt => {
                self.filter.push(ch);
                self.rebuild();
                OverlayAction::None
            }
            KeyCode::Char('k') if !ctrl && !alt => {
                self.move_by(false);
                OverlayAction::None
            }
            KeyCode::Char('j') if !ctrl && !alt => {
                self.move_by(true);
                OverlayAction::None
            }
            _ => OverlayAction::None,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }
        let t = tokens();
        let colored = t.colored();

        let width = ((f32::from(area.width) * 0.8) as u16)
            .max(1)
            .min(area.width);
        let height = ((f32::from(area.height) * 0.7) as u16)
            .max(1)
            .min(area.height);
        let popup = Rect::new(
            (area.width - width) / 2,
            (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);

        let block = Block::default()
            .title(" Theme ")
            .title_style(Style::default().fg(t.text).add_modifier(Modifier::BOLD))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(t.border));
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(inner);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Pick a palette — the whole UI previews as you move",
                Style::default().fg(t.muted),
            ))),
            chunks[0],
        );

        let count = if self.filter.is_empty() {
            format!("{} themes", self.filtered.len())
        } else {
            format!("{} of {}", self.filtered.len(), self.palettes.len())
        };
        let filter_line = Line::from(vec![
            Span::styled(
                " > ",
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if self.filter.is_empty() {
                    "press / to filter by name, id or mode".to_string()
                } else {
                    self.filter.clone()
                },
                if self.filter.is_empty() {
                    Style::default().fg(t.muted)
                } else {
                    Style::default().fg(t.text)
                },
            ),
            Span::styled(format!("   {count}"), Style::default().fg(t.muted)),
        ]);
        frame.render_widget(Paragraph::new(filter_line), chunks[1]);

        if self.filtered.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " no theme matches — esc clears the filter",
                    Style::default().fg(t.warning),
                ))),
                chunks[2],
            );
        } else {
            let items: Vec<ListItem<'static>> = self
                .filtered
                .iter()
                .copied()
                .map(|p| self.row(p, t, colored))
                .collect();
            frame.render_stateful_widget(List::new(items), chunks[2], &mut self.list_state);
        }

        let mut footer = vec![Line::from(Span::styled(
            " ↑↓ preview · / filter · ⏎ apply · esc cancel ",
            Style::default().fg(t.muted),
        ))];
        if let Some(note) = &self.note {
            footer.push(Line::from(Span::styled(
                format!(" {note} "),
                Style::default().fg(t.warning),
            )));
        }
        frame.render_widget(
            Paragraph::new(footer).alignment(Alignment::Center),
            chunks[3],
        );
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑↓ / j k", "preview theme"),
            ("/", "filter themes"),
            ("⏎", "apply and save"),
            ("esc", "clear filter / cancel"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Serialize these tests: `init_palette` is process-wide, and the picker's
    /// whole contract is about observing that shared state.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Open over a known palette so tests do not depend on the process's env.
    fn open(p: Palette) -> (ThemePickerOverlay, App) {
        let mut overlay = ThemePickerOverlay::with_original(p);
        let app = App::new("s", false);
        overlay.show();
        (overlay, app)
    }

    #[test]
    fn navigation_previews_the_palette_without_touching_config() {
        let _guard = TEST_LOCK.lock().unwrap();
        let all = Palette::all();
        assert!(all.len() >= 2, "the picker needs more than one theme");
        let original = all[0];
        let (mut overlay, mut app) = open(original);
        let expected = overlay.filtered[1];

        let action = overlay.handle_key(key(KeyCode::Down), &mut app);
        assert!(matches!(action, OverlayAction::None));
        assert_eq!(palette(), expected, "a cursor move previews immediately");
        assert_eq!(overlay.selected(), Some(expected));
        assert!(app.config.ui.is_none(), "preview must not write config");

        overlay.cancel();
        assert_eq!(palette(), original);
    }

    #[test]
    fn j_and_k_preview_too() {
        let _guard = TEST_LOCK.lock().unwrap();
        let all = Palette::all();
        let (mut overlay, mut app) = open(all[0]);
        let down = overlay.filtered[1];
        overlay.handle_key(key(KeyCode::Char('j')), &mut app);
        assert_eq!(palette(), down);
        overlay.handle_key(key(KeyCode::Char('k')), &mut app);
        assert_eq!(palette(), all[0]);
        overlay.cancel();
    }

    #[test]
    fn esc_after_preview_restores_the_original_palette() {
        let _guard = TEST_LOCK.lock().unwrap();
        let all = Palette::all();
        let original = all[0];
        let (mut overlay, mut app) = open(original);
        overlay.handle_key(key(KeyCode::Down), &mut app);
        assert_ne!(palette(), original, "preview changed the palette");

        let action = overlay.handle_key(key(KeyCode::Esc), &mut app);
        assert!(matches!(action, OverlayAction::Close));
        assert_eq!(palette(), original, "cancel restores the snapshot");
    }

    #[test]
    fn esc_first_clears_the_filter_then_closes() {
        let _guard = TEST_LOCK.lock().unwrap();
        let all = Palette::all();
        let (mut overlay, mut app) = open(all[0]);
        overlay.handle_key(key(KeyCode::Char('/')), &mut app);
        for ch in "zzz".chars() {
            overlay.handle_key(key(KeyCode::Char(ch)), &mut app);
        }
        assert!(overlay.filtered.is_empty());

        let action = overlay.handle_key(key(KeyCode::Esc), &mut app);
        assert!(matches!(action, OverlayAction::None));
        assert!(overlay.filter.is_empty(), "the filter is cleared first");
        assert_eq!(overlay.filtered.len(), all.len());

        let action = overlay.handle_key(key(KeyCode::Esc), &mut app);
        assert!(matches!(action, OverlayAction::Close));
        assert_eq!(palette(), all[0]);
    }

    #[test]
    fn ctrl_c_cancels_without_saving() {
        let _guard = TEST_LOCK.lock().unwrap();
        let all = Palette::all();
        let original = all[0];
        let (mut overlay, mut app) = open(original);
        overlay.handle_key(key(KeyCode::Down), &mut app);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        let action = overlay.handle_key(ctrl_c, &mut app);
        assert!(matches!(action, OverlayAction::Close));
        assert_eq!(palette(), original);
        assert!(app.config.ui.is_none());
    }

    #[test]
    fn enter_commits_and_persists_once() {
        let _guard = TEST_LOCK.lock().unwrap();
        let all = Palette::all();
        let original = all[0];
        let (mut overlay, mut app) = open(original);
        let path =
            std::env::temp_dir().join(format!("memnoth-theme-picker-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        app.config_path = path.clone();
        overlay.handle_key(key(KeyCode::Down), &mut app);
        assert!(!path.exists(), "preview does not write the config");

        let chosen = overlay.selected().expect("a theme is highlighted");
        let action = overlay.handle_key(key(KeyCode::Enter), &mut app);
        assert!(matches!(action, OverlayAction::Close));
        assert_eq!(palette(), chosen);
        assert_eq!(
            app.config.ui.as_ref().and_then(|ui| ui.theme.as_deref()),
            Some(chosen.id())
        );
        let saved = std::fs::read_to_string(&path).expect("config was saved");
        assert!(saved.contains(chosen.id()));
        assert!(overlay.committed, "only one write can follow");

        // A second Enter or the stack's hide must not undo or re-save.
        let _ = overlay.handle_key(key(KeyCode::Enter), &mut app);
        overlay.hide();
        assert_eq!(palette(), chosen);
        assert_eq!(
            std::fs::read_to_string(&path).expect("config still parses"),
            saved
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn filtering_preserves_the_selection_by_id() {
        let _guard = TEST_LOCK.lock().unwrap();
        let (mut overlay, mut app) = open(Palette::Cream);
        let chosen = Palette::Gruvbox;
        overlay.selected_id = chosen.id();
        overlay.sync_list_state();
        overlay.preview();

        overlay.handle_key(key(KeyCode::Char('/')), &mut app);
        for ch in chosen.id().chars() {
            overlay.handle_key(key(KeyCode::Char(ch)), &mut app);
        }
        // "gruvbox" matches only one id; the mode word "dark" would match
        // several themes, so this proves id-specific narrowing.
        assert_eq!(overlay.filter, chosen.id());
        assert_eq!(overlay.filtered.len(), 1, "the filter narrows to one row");
        assert_eq!(overlay.selected_id, chosen.id(), "selection is id-based");
        assert_eq!(palette(), chosen, "filtering does not change the preview");

        let action = overlay.handle_key(key(KeyCode::Esc), &mut app);
        assert!(matches!(action, OverlayAction::None));
        assert_eq!(
            overlay.selected_id,
            chosen.id(),
            "selection survives clearing"
        );
        overlay.cancel();
    }

    #[test]
    fn no_color_rows_carry_glyph_and_bold_instead_of_ink() {
        let _guard = TEST_LOCK.lock().unwrap();
        let (overlay, _app) = open(Palette::NoColor);
        let t = Tokens::for_palette(Palette::NoColor);
        assert!(!t.colored(), "NO_COLOR palette has no ink to rely on");

        let spans = overlay.row_spans(Palette::NoColor, t, t.colored());
        let rendered: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            rendered.contains(SELECTED),
            "selection glyph is always drawn"
        );
        assert!(rendered.contains(CURRENT), "current marker is always drawn");
        assert!(rendered.contains("(current)"));
        assert!(rendered.contains("Aa 123"));
        assert!(
            spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
        for span in &spans {
            assert!(
                span.style.fg.is_none_or(|c| c == Color::Reset),
                "no row cue may depend on colour"
            );
            assert!(
                span.style.bg.is_none_or(|c| c == Color::Reset),
                "no row cue may depend on a fill"
            );
        }
    }

    #[test]
    fn the_overlay_closes_and_restores_from_a_plain_show() {
        let _guard = TEST_LOCK.lock().unwrap();
        // The reachability suite opens overlays with `show` alone. That path
        // must still snapshot a palette and leave via Esc.
        let (mut overlay, mut app) = open(Palette::Dark);
        assert!(overlay.is_visible());
        let action = overlay.handle_key(key(KeyCode::Esc), &mut app);
        assert!(matches!(action, OverlayAction::Close));
        assert_eq!(palette(), Palette::Dark);
    }
}
