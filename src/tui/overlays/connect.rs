//! ConnectOverlay: a provider is connected by its API key, and that is all.
//!
//! The row set and every write here go through the same functions as
//! `mn connect`: `known_providers()` for presets, `store_provider_key()` for
//! keys, `probe_provider()` for the test. There is no model field, no model
//! column and no model in the details popup: a key is what connects a provider,
//! the preset already knows its endpoint, and what answers a turn is chosen in
//! `/model` from the list the provider publishes. Asking for an id here meant
//! typing one nobody knows before anything could run.
//!
//! The layout is fixed-height on purpose. The status line exists whether or not
//! there is anything to say, so a test result does not shove the table up and
//! move the row under the cursor; and the key prompt puts a real caret in the
//! field instead of a block glyph that scrolls away.
//!
//! Keys:
//!   ↑/↓        move
//!   enter      use this provider; with no key, ask for one right away
//!   k          set or replace its API key (masked, in the credential store)
//!   x          delete the stored key (press twice), so a new one can be set
//!   t          test: one real 10-token completion, then the model count
//!   d          remove the active provider from the config (the key is kept)
//!   i          details
//!   r          reload from disk
//!   esc / q    close

use crate::connect::{
    get_provider_key, known_providers, probe_provider, remove_provider_key, store_provider_key,
};
use crate::output::ellipsize;
use crate::provider::{Kind, Settings};
use crate::tui::app::{App, ModelsLoaded, ProviderTestResult};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

/// A row's last test result and the model count it reported, keyed by provider
/// name and carried across a reload: a list that re-reads itself must not erase
/// a test that finished a moment ago.
type PreviousTests = std::collections::BTreeMap<&'static str, (Option<(bool, String)>, usize)>;

/// What the overlay is doing with the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    Key,
}

/// What the status line is saying, and therefore what colour it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Info,
    Good,
    Bad,
}

impl Tone {
    fn marker(self) -> &'static str {
        match self {
            Tone::Info => "›",
            Tone::Good => "✓",
            Tone::Bad => "✗",
        }
    }

    fn color(self) -> Color {
        match self {
            Tone::Info => Color::Cyan,
            Tone::Good => Color::Green,
            Tone::Bad => Color::Red,
        }
    }
}

/// One provider row.
#[derive(Debug, Clone)]
struct ProviderRow {
    name: &'static str,
    display: &'static str,
    kind: Kind,
    base_url: String,
    /// The model a probe sends. Never rendered: `/model` owns what a turn uses,
    /// and this exists only so the test speaks the same dialect as the session.
    probe_model: String,
    has_key: bool,
    /// `ANTHROPIC_API_KEY`, `stored`, or nothing.
    key_source: String,
    active: bool,
    testing: bool,
    /// Result of the last test, kept for the session.
    last: Option<(bool, String)>,
    model_count: usize,
}

impl ProviderRow {
    fn status(&self) -> (&'static str, Color) {
        if self.testing {
            ("◐ testing", Color::Cyan)
        } else if let Some((ok, _)) = &self.last {
            if *ok {
                ("● ready", Color::Green)
            } else {
                ("✗ failed", Color::Red)
            }
        } else if self.active {
            ("● active", Color::Green)
        } else if self.has_key {
            ("● key set", Color::Green)
        } else {
            ("○ no key", Color::Yellow)
        }
    }

    /// The active marker. `·` rather than a blank keeps the name column aligned
    /// and says "not this one" without looking like a bullet list.
    fn marker(&self) -> &'static str {
        if self.active { "★" } else { " " }
    }

    fn test_detail(&self) -> String {
        match &self.last {
            Some((true, message)) => message.clone(),
            Some((false, _)) => "failed · i says why".to_string(),
            None => "—".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct ConnectOverlay {
    visible: bool,
    rows: Vec<ProviderRow>,
    /// Selection is remembered by preset name, not by index. `reload` sorts the
    /// rows (active first, then the ones with a key), so an index would silently
    /// point at a different provider after every action.
    selected: Option<&'static str>,
    table: TableState,
    mode: Mode,
    buffer: String,
    /// Delete is two presses: the first arms, the second acts.
    delete_armed: Option<&'static str>,
    info: Option<&'static str>,
    note: Option<(Tone, String)>,
}

impl ConnectOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            rows: Vec::new(),
            selected: None,
            table: TableState::default(),
            mode: Mode::Browse,
            buffer: String::new(),
            delete_armed: None,
            info: None,
            note: None,
        }
    }

    /// Rebuild every row from the config and the credential store.
    fn reload(&mut self, app: &App) {
        let settings = Settings::load(&app.config).ok();
        let section = app.config.provider.clone().unwrap_or_default();
        let configured_name = section.name.clone();
        let configured_kind = section.kind.clone();
        let configured_base = section.base_url.clone();
        let configured_model = section.model.clone();

        // Test results belong to a provider, not to a position in the list.
        let previous: PreviousTests = self
            .rows
            .iter()
            .map(|row| (row.name, (row.last.clone(), row.model_count)))
            .collect();

        self.rows = known_providers()
            .into_iter()
            .map(|info| {
                let active = configured_name.as_deref() == Some(info.name)
                    || (configured_name.is_none()
                        && configured_kind
                            .as_deref()
                            .map(|kind| kind.eq_ignore_ascii_case(info.kind.name()))
                            .unwrap_or(false));

                let stored = get_provider_key(info.name)
                    .ok()
                    .flatten()
                    .filter(|key| !key.trim().is_empty());
                let env_set = !info.env_var.is_empty() && std::env::var(info.env_var).is_ok();
                let (has_key, key_source) = match (&stored, env_set) {
                    (Some(_), _) => (true, "stored".to_string()),
                    (None, true) => (true, info.env_var.to_string()),
                    (None, false) => (false, "—".to_string()),
                };

                // Only the active provider's endpoint and model are in the
                // config; every other row falls back to its preset.
                let base_url = if active {
                    configured_base
                        .clone()
                        .unwrap_or_else(|| info.default_base_url.to_string())
                } else {
                    info.default_base_url.to_string()
                };
                let probe_model = if active {
                    configured_model
                        .clone()
                        .filter(|model| !model.trim().is_empty())
                        .unwrap_or_else(|| info.default_model.to_string())
                } else {
                    info.default_model.to_string()
                };

                let remembered = previous.get(info.name).cloned().unwrap_or_default();

                ProviderRow {
                    name: info.name,
                    display: info.display_name,
                    kind: info.kind,
                    base_url,
                    probe_model,
                    has_key,
                    key_source,
                    active,
                    testing: false,
                    last: remembered.0,
                    model_count: remembered.1,
                }
            })
            .collect();

        // A session signed in to Memnoth has a token even though no preset key
        // exists; saying "no key" there would be wrong.
        if let Some(settings) = &settings
            && settings.kind == Kind::Memnoth
            && settings.api_key.is_some()
            && let Some(row) = self.rows.iter_mut().find(|row| row.kind == Kind::Memnoth)
        {
            row.has_key = true;
            if row.key_source == "—" {
                row.key_source = settings
                    .key_source
                    .clone()
                    .unwrap_or_else(|| "account".to_string());
            }
        }

        // What a person came here for goes first: the active provider, then the
        // ones a turn could actually run on, then the rest in preset order
        // (`sort_by_key` is stable).
        self.rows.sort_by_key(|row| {
            let active = if row.active { 0 } else { 1 };
            let keyless = if row.has_key { 0 } else { 1 };
            (active, keyless)
        });

        // Fall back to the active row, so opening the screen puts the cursor on
        // the provider the session is already using.
        let keep = self
            .selected
            .filter(|name| self.rows.iter().any(|row| row.name == *name))
            .or_else(|| self.rows.iter().find(|row| row.active).map(|row| row.name))
            .or_else(|| self.rows.first().map(|row| row.name));
        self.selected = keep;
        self.sync_table();
    }

    fn sync_table(&mut self) {
        let index = self.index();
        self.table.select(if self.rows.is_empty() {
            None
        } else {
            Some(index)
        });
    }

    fn index(&self) -> usize {
        self.selected
            .and_then(|name| self.rows.iter().position(|row| row.name == name))
            .unwrap_or(0)
    }

    fn row(&self) -> Option<&ProviderRow> {
        self.rows.get(self.index())
    }

    fn preset(name: &str) -> Option<crate::connect::ProviderInfo> {
        known_providers().into_iter().find(|info| info.name == name)
    }

    fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.note = Some((tone, text.into()));
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.say(Tone::Bad, text);
    }

    fn ok(&mut self, text: impl Into<String>) {
        self.say(Tone::Good, text);
    }

    /// Move the selection by one row and keep the table's own scroll in step.
    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let next = if delta < 0 {
            self.index().saturating_sub(delta.unsigned_abs())
        } else {
            (self.index() + delta as usize).min(self.rows.len() - 1)
        };
        self.selected = Some(self.rows[next].name);
        self.sync_table();
    }

    /// Write `[provider]` for the selected row, carrying over what this screen
    /// does not own.
    ///
    /// The old implementation built a fresh `ProviderConfig` with
    /// `..Default::default()`, which silently dropped `max_tokens` and the
    /// explicit rates — so switching provider turned off cost accounting.
    ///
    /// The model is not edited here and is not echoed: switching family picks
    /// that provider's default so a turn can run immediately, and switching back
    /// keeps whatever `/model` set for the one that was already active.
    fn save_active(&mut self, app: &mut App) {
        let Some(row) = self.row().cloned() else {
            return;
        };
        // Using a provider with no key only moves the failure to the next turn,
        // where it reads as a model problem instead of a missing connection.
        if !row.has_key {
            self.begin_key();
            self.say(
                Tone::Info,
                format!(
                    "paste the {} key, then press enter to store it",
                    row.display
                ),
            );
            return;
        }
        let mut section = app.config.provider.clone().unwrap_or_default();
        let changed_family = !section
            .kind
            .as_deref()
            .map(|kind| kind.eq_ignore_ascii_case(row.kind.name()))
            .unwrap_or(false)
            || section.name.as_deref() != Some(row.name);

        section.kind = Some(row.kind.name().to_string());
        section.name = Some(row.name.to_string());
        section.base_url = Some(row.base_url.clone());
        // Only a family switch needs a new model: an id from the old provider is
        // a 404 on the new endpoint, and `row.probe_model` is the preset's own
        // default or the model this provider was already using.
        if changed_family {
            section.model = Some(row.probe_model.clone());
            section.input_per_mtok = None;
            section.output_per_mtok = None;
            // An effort level is a statement about the old family's API; a
            // gateway model that never heard of it answers 400.
            section.reasoning_effort = None;
        } else if section
            .model
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            section.model = Some(row.probe_model.clone());
        }

        let mut config = app.config.clone();
        config.provider = Some(section);
        match config.save(&app.config_path) {
            Ok(()) => {
                app.config = config;
                let effort = app
                    .config
                    .provider
                    .as_ref()
                    .and_then(|section| section.reasoning_effort.clone())
                    .map(|effort| format!(" · reasoning {effort}"))
                    .unwrap_or_default();
                self.ok(format!(
                    "{} is active — the next turn uses it{effort}",
                    row.display
                ));
                self.selected = Some(row.name);
                self.reload(app);
            }
            Err(error) => self.fail(format!("cannot save the config: {error:#}")),
        }
    }

    fn begin_key(&mut self) {
        self.mode = Mode::Key;
        self.buffer.clear();
        self.note = None;
    }

    fn start_test(&mut self, app: &mut App) {
        let Some(row) = self.row().cloned() else {
            return;
        };
        let key = match get_provider_key(row.name)
            .ok()
            .flatten()
            .filter(|key| !key.trim().is_empty())
        {
            Some(key) => key,
            None => {
                let from_env = Self::preset(row.name)
                    .filter(|info| !info.env_var.is_empty())
                    .and_then(|info| std::env::var(info.env_var).ok());
                match from_env {
                    Some(key) => key,
                    // Memnoth can hold a token in the account store instead.
                    None => match Settings::load(&app.config)
                        .ok()
                        .filter(|settings| settings.kind == row.kind)
                        .and_then(|settings| settings.api_key)
                    {
                        Some(key) => key,
                        None => {
                            self.begin_key();
                            self.say(
                                Tone::Info,
                                format!(
                                    "no key for {} yet — paste one, then press t to test",
                                    row.display
                                ),
                            );
                            return;
                        }
                    },
                }
            }
        };

        if let Some(slot) = self.rows.iter_mut().find(|slot| slot.name == row.name) {
            slot.testing = true;
            slot.last = None;
        }

        let provider = row.name;
        let kind = row.kind;
        let base_url = row.base_url.clone();
        let model = row.probe_model.clone();
        let display = row.display;
        let tx = app.provider_test_tx.clone();
        std::thread::spawn(move || {
            let result = probe_provider(kind, &base_url, &model, &key);
            let (success, message, model_count) = match result {
                Ok(probe) => (
                    true,
                    format!("{} ms · {} models", probe.latency_ms, probe.model_count),
                    probe.model_count,
                ),
                Err(error) => (false, format!("{error:#}"), 0),
            };
            let _ = tx.send(ProviderTestResult {
                index: 0,
                provider: provider.to_string(),
                success,
                message,
                model_count,
                timestamp: display.to_string(),
            });
        });
        self.say(
            Tone::Info,
            format!("testing {} — one 10-token completion", row.display),
        );
    }

    fn delete_key(&mut self, app: &mut App) {
        let Some(row) = self.row().cloned() else {
            return;
        };
        if self.delete_armed != Some(row.name) {
            self.delete_armed = Some(row.name);
            self.say(
                Tone::Info,
                format!("press x again to delete the stored key for {}", row.display),
            );
            return;
        }
        self.delete_armed = None;
        match remove_provider_key(row.name) {
            Ok(()) => {
                self.reload(app);
                self.ok(format!(
                    "deleted the stored key for {} — press k to set a new one",
                    row.display
                ));
            }
            Err(error) => self.fail(format!("cannot delete the key: {error:#}")),
        }
    }

    fn disconnect(&mut self, app: &mut App) {
        let Some(row) = self.row().cloned() else {
            return;
        };
        if !row.active {
            self.fail("that provider is not the active one");
            return;
        }
        let mut config = app.config.clone();
        config.provider = None;
        match config.save(&app.config_path) {
            Ok(()) => {
                app.config = config;
                self.reload(app);
                self.ok(format!(
                    "{} removed from the config — its key is kept",
                    row.display
                ));
            }
            Err(error) => self.fail(format!("cannot save the config: {error:#}")),
        }
    }

    /// Commit the API key that was being typed.
    fn commit(&mut self, app: &mut App) {
        if self.mode != Mode::Key {
            return;
        }
        let key = self.buffer.trim().to_string();
        self.mode = Mode::Browse;
        self.buffer.clear();
        let Some(row) = self.row().cloned() else {
            return;
        };
        if key.is_empty() {
            self.fail("empty key — nothing stored");
            return;
        }
        match store_provider_key(row.name, &key) {
            Ok(()) => {
                self.reload(app);
                self.ok(format!(
                    "key stored for {} — press enter to use it, t to test it",
                    row.display
                ));
            }
            Err(error) => self.fail(format!("cannot store the key: {error:#}")),
        }
    }

    /// The line that explains the screen, without telling anyone how to type a
    /// model id.
    fn header(&self) -> Line<'static> {
        let ready = self.rows.iter().filter(|row| row.has_key).count();
        let total = self.rows.len();
        let mut spans = Vec::new();
        match self.rows.iter().find(|row| row.active) {
            Some(active) => {
                spans.push(Span::styled(
                    "active ",
                    Style::default().fg(Color::DarkGray),
                ));
                spans.push(Span::styled(
                    active.display,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!(" · {ready} of {total} providers have a key"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            None => {
                spans.push(Span::styled(
                    "no provider active",
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::styled(
                    format!(" · {ready} of {total} have a key · enter uses one"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
        Line::from(spans)
    }

    /// Prefix of the key field, so the caret can be placed without re-deriving
    /// it from a rendered buffer.
    fn key_field_prefix(&self) -> String {
        let display = self.row().map(|row| row.display).unwrap_or("provider");
        format!("key for {display}  ")
    }

    /// Where the caret goes while a key is being typed, and `None` otherwise.
    fn key_caret(&self, row: Rect) -> Option<Position> {
        if self.mode != Mode::Key {
            return None;
        }
        let prefix = self.key_field_prefix().chars().count() as u16;
        let dots = self
            .buffer
            .chars()
            .count()
            .min(row.width.saturating_sub(prefix) as usize) as u16;
        let x = (row.x + prefix + dots).min(row.x + row.width.saturating_sub(1));
        Some(Position::new(x, row.y))
    }

    /// The status line under the table. Always drawn, so nothing moves.
    fn status(&self, width: u16) -> Line<'static> {
        if self.mode == Mode::Key {
            let mut spans = vec![Span::styled(
                self.key_field_prefix(),
                Style::default().fg(Color::Cyan),
            )];
            if self.buffer.is_empty() {
                spans.push(Span::styled(
                    "paste the key",
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(
                    "•".repeat(self.buffer.chars().count()),
                    Style::default().fg(Color::White),
                ));
                spans.push(Span::styled(
                    format!("  {} chars", self.buffer.chars().count()),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            return Line::from(spans);
        }
        match &self.note {
            Some((tone, text)) => {
                let room = (width as usize).saturating_sub(2);
                Line::from(vec![
                    Span::styled(
                        format!("{} ", tone.marker()),
                        Style::default().fg(tone.color()),
                    ),
                    Span::styled(ellipsize(text, room), Style::default().fg(tone.color())),
                ])
            }
            None => Line::from(""),
        }
    }

    /// The hint under the status line. One row, always, ellipsized rather than
    /// wrapped: a hint that wraps is a hint that covers the row it describes.
    fn hints(&self) -> &'static str {
        match self.mode {
            Mode::Key => "enter stores the key · esc cancels · the key is masked",
            Mode::Browse => {
                "↑↓ move · enter use · k key · t test · x remove key · i info · esc close"
            }
        }
    }
}

impl Default for ConnectOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Overlay for ConnectOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Connect
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.mode = Mode::Browse;
        self.buffer.clear();
        self.delete_armed = None;
        self.info = None;
        self.note = None;
    }

    fn on_open(&mut self, app: &mut App) {
        self.reload(app);
    }

    fn on_provider_test(&mut self, result: &ProviderTestResult) {
        // Found by preset, not by position: `reload` re-sorts the list, so the
        // index recorded when the thread started may name another provider by
        // the time the answer lands.
        let Some(row) = self.rows.iter_mut().find(|row| row.name == result.provider) else {
            return;
        };
        row.testing = false;
        row.last = Some((result.success, result.message.clone()));
        if result.success {
            row.model_count = result.model_count;
        }
    }

    fn on_models_loaded(&mut self, result: &ModelsLoaded) {
        // A live list is a better model count than a probe's snapshot, and this
        // screen already reports how many a provider advertises.
        if result.error.is_none()
            && let Some(row) = self.rows.iter_mut().find(|row| row.name == result.provider)
        {
            row.model_count = result.ids.len();
        }
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        if self.info.take().is_some() {
            return OverlayAction::None;
        }
        if self.mode == Mode::Key {
            match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Browse;
                    self.buffer.clear();
                }
                KeyCode::Enter => self.commit(app),
                KeyCode::Backspace => {
                    self.buffer.pop();
                }
                KeyCode::Char(ch)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.buffer.push(ch);
                }
                _ => {}
            }
            return OverlayAction::None;
        }

        // Arming a delete survives exactly one keypress: the second `x` is the
        // confirmation, anything else withdraws the question.
        if key.code != KeyCode::Char('x') {
            self.delete_armed = None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return OverlayAction::Close,
            KeyCode::Up | KeyCode::Char('K') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('J') => self.move_by(1),
            KeyCode::Home => {
                self.selected = self.rows.first().map(|row| row.name);
                self.sync_table();
            }
            KeyCode::End => {
                self.selected = self.rows.last().map(|row| row.name);
                self.sync_table();
            }
            KeyCode::Enter => self.save_active(app),
            KeyCode::Char('k') => self.begin_key(),
            KeyCode::Char('x') => self.delete_key(app),
            KeyCode::Char('t') => self.start_test(app),
            KeyCode::Char('d') => self.disconnect(app),
            KeyCode::Char('i') => self.info = self.row().map(|row| row.name),
            KeyCode::Char('r') => {
                self.reload(app);
                self.ok("reloaded from the config and the credential store");
            }
            _ => {}
        }
        OverlayAction::None
    }

    /// A pasted key is the normal way to enter one.
    ///
    /// Whitespace is dropped: a clipboard paste usually carries a trailing
    /// newline or a space, and a key with a newline in it is a 401 that looks
    /// like a bad key.
    fn handle_paste(&mut self, text: &str) -> bool {
        if self.mode != Mode::Key {
            return false;
        }
        self.buffer
            .extend(text.chars().filter(|ch| !ch.is_whitespace()));
        true
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }
        frame.render_widget(Clear, area);

        // Four bands, and the bottom two exist even when empty: a status line
        // that appears and disappears moves everything above it, including the
        // row the cursor is on.
        let chunks = Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(4),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(self.header())
                .block(Block::default().borders(Borders::ALL).title(" Connect ")),
            chunks[0],
        );

        let mut rows: Vec<Row> = Vec::new();
        for row in &self.rows {
            let (status, color) = row.status();
            let selected = self.selected == Some(row.name);
            let style = if selected {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else {
                Style::default()
            };
            // On the highlighted row every cell is black-on-cyan, including the
            // status: a green dot on cyan is unreadable, and a row whose colours
            // disagree looks like two rows.
            let status_style = Style::default().fg(if selected { Color::Black } else { color });
            rows.push(
                Row::new(vec![
                    Cell::from(format!("{} {}", row.marker(), row.display)),
                    Cell::from(Span::styled(status, status_style)),
                    Cell::from(ellipsize(&row.key_source, 21)),
                    Cell::from(row.test_detail()),
                ])
                .style(style),
            );
        }

        let header = Row::new(vec![
            Cell::from("provider"),
            Cell::from("status"),
            Cell::from("key"),
            Cell::from("last test"),
        ])
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

        // The provider column is the widest thing on the row, and the last test
        // gets what is left: no column is cut mid-word by a fixed width that was
        // measured on someone else's terminal.
        let table = Table::new(
            rows,
            [
                Constraint::Min(26),
                Constraint::Length(11),
                Constraint::Length(22),
                Constraint::Min(12),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} providers ", self.rows.len())),
        )
        .row_highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black));

        let mut state = self.table.clone();
        state.select(if self.rows.is_empty() {
            None
        } else {
            Some(self.index())
        });
        frame.render_stateful_widget(table, chunks[1], &mut state);

        frame.render_widget(Paragraph::new(self.status(chunks[2].width)), chunks[2]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ellipsize(self.hints(), chunks[3].width as usize),
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[3],
        );

        // The caret belongs inside the field being typed into, not on the prompt
        // line behind the overlay.
        if let Some(position) = self.key_caret(chunks[2]) {
            frame.set_cursor_position(position);
        }

        if let Some(name) = self.info
            && let Some(row) = self.rows.iter().find(|row| row.name == name)
        {
            self.render_info(frame, area, row);
        }
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "/connect",
                "Provider connections — the key is the connection",
            ),
            (
                "enter",
                "Use the selected provider; with no key, ask for one",
            ),
            ("k", "Set or replace its API key (masked)"),
            ("x", "Delete the stored key (twice), then k sets a new one"),
            ("t", "Test with one real completion"),
            ("d", "Remove the active provider from the config (key kept)"),
            ("i", "Details, including where the key came from"),
            ("r", "Reload"),
        ]
    }
}

impl ConnectOverlay {
    fn render_info(&self, frame: &mut Frame, area: Rect, row: &ProviderRow) {
        let width = ((area.width as u32 * 2 / 3) as u16).clamp(52, area.width);
        let height = 15.min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);

        let last = match &row.last {
            Some((true, message)) => format!("ok · {message}"),
            Some((false, message)) => format!("failed · {message}"),
            None => "never tested".to_string(),
        };
        let lines = vec![
            format!("preset        {}", row.name),
            format!("wire shape    {}", row.kind.name()),
            format!("endpoint      {}", row.base_url),
            format!("key           {}", row.key_source),
            format!("models seen   {}", row.model_count),
            format!("last test     {last}"),
            format!("active        {}", if row.active { "yes" } else { "no" }),
            String::new(),
            format!(
                "env var       {}",
                Self::preset(row.name)
                    .map(|info| info.env_var)
                    .unwrap_or("—")
            ),
            "keys live in the credential store (keyring, then file),".to_string(),
            "never in config.toml; the same store `mn connect` uses.".to_string(),
        ];
        let text: Vec<Line> = lines.into_iter().map(Line::from).collect();
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", row.display))
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            popup,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn connected() -> ConnectOverlay {
        let mut overlay = ConnectOverlay::new();
        // `show` only; `on_open` reads the credential store, which no test
        // should touch.
        overlay.show();
        overlay
    }

    /// Two rows, built by hand: the active provider with a key, and one without.
    fn sample() -> ConnectOverlay {
        let mut overlay = connected();
        overlay.rows = vec![
            ProviderRow {
                name: "kilo",
                display: "Kilo Gateway",
                kind: Kind::Kilo,
                base_url: "https://ai-gateway.kilo.ai/api/v1".to_string(),
                probe_model: "anthropic/claude-sonnet-4.5".to_string(),
                has_key: true,
                key_source: "stored".to_string(),
                active: true,
                testing: false,
                last: Some((true, "120 ms · 42 models".to_string())),
                model_count: 42,
            },
            ProviderRow {
                name: "anthropic",
                display: "Anthropic (Claude)",
                kind: Kind::Anthropic,
                base_url: "https://api.anthropic.com".to_string(),
                probe_model: "claude-3-5-sonnet-20241022".to_string(),
                has_key: false,
                key_source: "—".to_string(),
                active: false,
                testing: false,
                last: None,
                model_count: 0,
            },
        ];
        overlay.selected = Some("kilo");
        overlay
    }

    fn frame_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_only_thing_to_edit_is_the_key() {
        let mut app = App::new("s", false);
        let mut overlay = connected();

        for key in ['e', 'b', 'm'] {
            overlay.handle_key(KeyEvent::from(KeyCode::Char(key)), &mut app);
            assert_eq!(overlay.mode, Mode::Browse, "{key} opened an editor");
        }

        overlay.handle_key(KeyEvent::from(KeyCode::Char('k')), &mut app);
        assert_eq!(overlay.mode, Mode::Key, "k starts the key prompt");
        overlay.handle_key(KeyEvent::from(KeyCode::Char('a')), &mut app);
        assert_eq!(overlay.buffer, "a", "the key is collected, not the model");

        overlay.handle_key(KeyEvent::from(KeyCode::Esc), &mut app);
        assert_eq!(overlay.mode, Mode::Browse);
        assert!(overlay.buffer.is_empty(), "escaping forgets what was typed");
    }

    #[test]
    fn a_pasted_key_arrives_without_its_newline() {
        let mut app = App::new("s", false);
        let mut overlay = connected();

        // Nothing is being typed, so the paste belongs to nobody here.
        assert!(!overlay.handle_paste("sk-kilo-123\n"));

        overlay.handle_key(KeyEvent::from(KeyCode::Char('k')), &mut app);
        assert!(overlay.handle_paste("sk-kilo-123\n"));
        assert_eq!(overlay.buffer, "sk-kilo-123");
    }

    #[test]
    fn enter_without_a_key_asks_for_one_instead_of_failing_later() {
        let mut app = App::new("s", false);
        let mut overlay = sample();
        overlay.selected = Some("anthropic");

        overlay.handle_key(KeyEvent::from(KeyCode::Enter), &mut app);

        assert_eq!(
            overlay.mode,
            Mode::Key,
            "a provider with no key asks for the key right there"
        );
        let (tone, text) = overlay.note.clone().expect("it says why the field opened");
        assert_eq!(tone, Tone::Info);
        assert!(text.contains("Anthropic"), "{text}");
    }

    #[test]
    fn the_caret_sits_in_the_key_field_and_nowhere_else() {
        let mut overlay = sample();
        let row = Rect::new(4, 20, 80, 1);

        assert_eq!(overlay.key_caret(row), None, "browsing has no caret");

        overlay.mode = Mode::Key;
        overlay.buffer = "sk-kilo-123".to_string();
        let caret = overlay.key_caret(row).expect("typing has a caret");
        let prefix = overlay.key_field_prefix().chars().count() as u16;
        assert_eq!(caret.y, 20, "the caret is on the field's own row");
        assert_eq!(caret.x, 4 + prefix + 11);
    }

    #[test]
    fn a_long_key_never_pushes_the_caret_off_the_screen() {
        let mut overlay = sample();
        overlay.mode = Mode::Key;
        overlay.buffer = "k".repeat(400);
        let row = Rect::new(0, 0, 40, 1);
        let caret = overlay.key_caret(row).expect("typing has a caret");
        assert_eq!(caret.x, 39, "clamped to the last column");
    }

    #[test]
    fn the_screen_shows_providers_and_never_a_model() {
        let app = App::new("t", false);
        let mut overlay = sample();
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                overlay.render(frame, area, &app);
            })
            .unwrap();

        let text = frame_text(&terminal);
        assert!(text.contains("Kilo Gateway"), "{text}");
        assert!(text.contains("Anthropic (Claude)"), "{text}");
        assert!(text.contains("providers"), "{text}");
        assert!(
            !text.contains("claude-"),
            "no model id belongs on this screen:\n{text}"
        );
        // The header row of the table is the column contract.
        let header = text
            .lines()
            .find(|line| line.contains("last test"))
            .expect("the table header is drawn");
        assert!(!header.contains("model"), "no model column:\n{header}");
    }

    #[test]
    fn a_note_does_not_push_the_layout_around() {
        let app = App::new("t", false);
        let mut quiet = sample();
        let mut loud = sample();
        loud.note = Some((Tone::Good, "key stored for Anthropic (Claude)".to_string()));

        let frame = |overlay: &mut ConnectOverlay| {
            let backend = TestBackend::new(100, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| {
                    let area = f.area();
                    overlay.render(f, area, &app);
                })
                .unwrap();
            frame_text(&terminal)
        };

        let quiet_rows = lines_of(&frame(&mut quiet));
        let loud_rows = lines_of(&frame(&mut loud));
        assert_eq!(
            quiet_rows.len(),
            loud_rows.len(),
            "the same number of rows either way"
        );
        // The table and its header sit on the same lines in both frames.
        let quiet_table = quiet_rows
            .iter()
            .position(|line| line.contains("providers"));
        let loud_table = loud_rows.iter().position(|line| line.contains("providers"));
        assert_eq!(quiet_table, loud_table);
        assert!(loud_rows.iter().any(|line| line.contains("key stored")));
        assert!(
            loud_rows.iter().any(|line| line.contains("esc close")),
            "the hints stay visible beside a note:\n{}",
            loud_rows.join("\n")
        );
    }

    fn lines_of(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }
}
