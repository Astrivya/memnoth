//! ModelsOverlay: the one model surface — browse, search, compare, switch.
//!
//! The list is the provider's own. When the session has a key, opening this
//! screen asks that provider for `GET /models` on a worker thread and replaces
//! its rows with the answer, so nothing is typed and nothing is guessed: what is
//! on screen is what the endpoint will accept. The compiled-in catalogue is drawn
//! first — a browser that opens empty while a request is in flight is worse than
//! one that opens with rows it labels as the built-in list — and is the whole
//! answer when there is no key, or when the endpoint refuses to list (which is
//! said on screen rather than shown as "no models").
//!
//! The fetch is asynchronous because a synchronous one blocks the draw: the old
//! `R` froze the frame for up to eight seconds, timeout included.
//!
//! `Enter` writes `[provider]` and saves it, but only for a provider that could
//! actually run the turn — selecting a model on a keyless provider used to look
//! like a switch and fail on the next message.
//!
//! The turn path reads `app.config`, not the startup snapshot, so the next
//! message uses the new model without restarting the session.
//!
//! Typing searches from the moment the screen opens — there is no prefix
//! key — and the arrows belong to the focused zone: the tab bar answers
//! `←/→` with a tab, a model row with the reasoning effort, and `↑` past the
//! first row reaches the tab bar. `tab` moves between the zones. The mouse
//! does the same work: tabs, rows, the reasoning column, and the wheel.
//!
//! Keys:
//!   type                     search (backspace edits, esc clears)
//!   ↑/↓ PgUp/PgDn Home/End   move between models
//!   ←/→                      reasoning on a model; tab on the tab bar
//!   Enter                    use this model for the session, and save it
//!   Tab                      tab bar / models
//!   alt+s / ctrl+S           sort field / direction
//!   alt+1..6, alt+0          provider filter / clear
//!   alt+f                    favorite (persisted in `[ui]`)
//!   alt+c                    compare with the current model
//!   alt+i                    details
//!   alt+r / ctrl+R           fetch the provider's live list
//!   esc                      clear the filter, then close

/// One compiled-in catalogue row: id, label, provider, context window, max
/// output, tools, vision, structured JSON.
type CatalogueRow = (
    &'static str,
    &'static str,
    &'static str,
    u32,
    u32,
    bool,
    bool,
    bool,
);

use crate::output::ellipsize;
use crate::provider::{Kind, Settings, builtin_price};
use crate::tui::app::{App, ModelsLoaded};
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};

/// One model, as the browser shows it.
///
/// `connected` is decided when the list is built, not per frame: reading the
/// credential store is a blocking call and a table redraws sixty times a second.
#[derive(Debug, Clone)]
pub(crate) struct ModelRow {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub context: u32,
    pub max_output: u32,
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    /// Effort levels the model's API actually accepts.
    pub variants: Vec<&'static str>,
    pub tools: bool,
    pub vision: bool,
    pub json: bool,
    pub deprecated: bool,
    /// Came from the provider's own `GET /models`, not from this build.
    pub live: bool,
    pub connected: bool,
}

/// The catalogue this build ships.
///
/// Deliberately small and explicit: a table that tries to track every model is
/// wrong within a release. Prices come from the same table the cost estimator
/// uses, so the number on screen is the number in `session status`.
pub(crate) fn catalogue() -> Vec<ModelRow> {
    let specs: [CatalogueRow; 13] = [
        // id, label, provider, context, max output, tools, vision, json
        (
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            "anthropic",
            200_000,
            64_000,
            true,
            true,
            false,
        ),
        (
            "claude-opus-4-1",
            "Claude Opus 4.1",
            "anthropic",
            200_000,
            32_000,
            true,
            true,
            false,
        ),
        (
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            "anthropic",
            200_000,
            32_000,
            true,
            true,
            false,
        ),
        (
            "gpt-5", "GPT-5", "openai", 400_000, 128_000, true, true, true,
        ),
        ("o3", "o3", "openai", 200_000, 100_000, true, true, true),
        (
            "o4-mini", "o4-mini", "openai", 200_000, 100_000, true, true, true,
        ),
        (
            "gpt-4.1", "GPT-4.1", "openai", 1_000_000, 32_768, true, true, true,
        ),
        (
            "gpt-4.1-mini",
            "GPT-4.1 mini",
            "openai",
            1_000_000,
            32_768,
            true,
            true,
            true,
        ),
        (
            "gpt-4o", "GPT-4o", "openai", 128_000, 16_384, true, true, true,
        ),
        (
            "gpt-4o-mini",
            "GPT-4o mini",
            "openai",
            128_000,
            16_384,
            true,
            true,
            true,
        ),
        (
            "deepseek-chat",
            "DeepSeek Chat",
            "deepseek",
            128_000,
            8_192,
            true,
            false,
            true,
        ),
        (
            "deepseek-reasoner",
            "DeepSeek Reasoner",
            "deepseek",
            128_000,
            8_192,
            false,
            false,
            true,
        ),
        (
            "anthropic/claude-sonnet-4.5",
            "Claude Sonnet 4.5 (gateway)",
            "kilo",
            200_000,
            64_000,
            true,
            true,
            false,
        ),
    ];

    specs
        .into_iter()
        .map(
            |(id, label, provider, context, max_output, tools, vision, json)| {
                let pricing = builtin_price(id);
                ModelRow {
                    id: id.to_string(),
                    label: label.to_string(),
                    provider: provider.to_string(),
                    context,
                    max_output,
                    input_per_mtok: pricing.map(|price| price.input_per_mtok),
                    output_per_mtok: pricing.map(|price| price.output_per_mtok),
                    variants: variants_for(id),
                    tools,
                    vision,
                    json,
                    deprecated: false,
                    live: false,
                    connected: false,
                }
            },
        )
        .collect()
}

/// The effort levels a model's API accepts, if any.
///
/// The predicate is the one the request path uses, so the arrows are offered on
/// exactly the models that will not answer 400 when the field is sent; the list
/// is empty everywhere else.
fn variants_for(id: &str) -> Vec<&'static str> {
    if crate::provider::takes_reasoning_effort(id) {
        vec!["low", "medium", "high"]
    } else {
        Vec::new()
    }
}

/// Which tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Connected,
    Favorites,
    Reasoning,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Connected => "Connected",
            Tab::Favorites => "Favorites",
            Tab::Reasoning => "Reasoning",
        }
    }
}

/// The two arrow zones. `Tabs` answers the horizontal arrows with a tab
/// change; `Table` answers them with the reasoning effort. Vertical arrows
/// cross between the two, so the keyboard reads as one grid rather than two
/// screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusZone {
    Tabs,
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortField {
    #[default]
    Provider,
    Model,
    Context,
    Input,
    Output,
}

impl SortField {
    fn next(self) -> Self {
        match self {
            SortField::Provider => SortField::Model,
            SortField::Model => SortField::Context,
            SortField::Context => SortField::Input,
            SortField::Input => SortField::Output,
            SortField::Output => SortField::Provider,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortField::Provider => "provider",
            SortField::Model => "model",
            SortField::Context => "context",
            SortField::Input => "input price",
            SortField::Output => "output price",
        }
    }
}

#[derive(Debug)]
pub struct ModelsOverlay {
    visible: bool,
    tab: Tab,
    rows: Vec<ModelRow>,
    filtered: Vec<usize>,
    selected: usize,
    table: TableState,
    search: String,
    /// Which zone owns the arrows: the tab bar or the model table. The search
    /// has no zone — typing always searches — so the horizontal arrows belong
    /// to whichever zone is focused, and `tab` moves between them.
    focus: FocusZone,
    /// Last frame's hit areas, so a click can be mapped to a tab or a row.
    tabs_area: Rect,
    table_area: Rect,
    /// The reasoning column's x range, for click-to-cycle.
    effort_col: (u16, u16),
    favorites: Vec<String>,
    /// Effort chosen per model id, seeded from the config for the current one.
    efforts: std::collections::BTreeMap<String, String>,
    sort: SortField,
    descending: bool,
    provider_filter: Option<String>,
    details: Option<usize>,
    compare: Option<usize>,
    note: Option<(bool, String)>,
    /// Provider whose own list has replaced its catalogue rows.
    live_for: Option<String>,
    live_count: usize,
    /// Provider being asked for its list right now.
    loading: Option<String>,
    /// The model the session is using, read once per open. Reading the config
    /// in `render` meant a file and credential-store hit on every frame.
    current: Option<String>,
}

impl ModelsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            tab: Tab::All,
            rows: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            table: TableState::default(),
            search: String::new(),
            focus: FocusZone::Table,
            tabs_area: Rect::default(),
            table_area: Rect::default(),
            effort_col: (0, 0),
            favorites: Vec::new(),
            efforts: std::collections::BTreeMap::new(),
            sort: SortField::Provider,
            descending: false,
            provider_filter: None,
            details: None,
            compare: None,
            note: None,
            live_for: None,
            live_count: 0,
            loading: None,
            current: None,
        }
    }

    /// Fill the browser: catalogue + saved preferences. Offline, so opening the
    /// overlay cannot hang on a provider that is down.
    fn reload(&mut self, app: &App) {
        let settings = Settings::load(&app.config).ok();

        self.favorites = app
            .config
            .ui
            .as_ref()
            .map(|ui| ui.favorite_models.clone())
            .unwrap_or_default();

        self.current = settings.as_ref().map(|settings| settings.model.clone());

        // The effort the session is actually using belongs to the selected
        // model; keeping it in the map means moving the cursor does not forget
        // what was chosen two rows up.
        if let Some(settings) = &settings
            && let Some(effort) = &settings.reasoning_effort
        {
            self.efforts.insert(settings.model.clone(), effort.clone());
        }

        self.rows = catalogue();
        seed_connectivity(&mut self.rows, app, settings.as_ref());
        self.live_for = None;
        self.live_count = 0;

        // With a key, the provider's own list is the truth and it is already on
        // its way; the catalogue above is what fills the screen until it lands.
        match settings.as_ref().filter(|settings| settings.ready()) {
            Some(settings) => {
                let provider = settings
                    .provider_name
                    .clone()
                    .unwrap_or_else(|| settings.kind.name().to_string());
                if self.tab == Tab::All && self.rows.iter().any(|row| row.connected) {
                    self.tab = Tab::Connected;
                }
                self.begin_fetch(
                    app,
                    provider,
                    settings.kind,
                    settings.base_url.clone(),
                    settings.api_key.clone(),
                );
            }
            None => {
                self.loading = None;
                self.note = Some((
                    false,
                    "no provider has a key yet — open /connect, then come back".to_string(),
                ));
            }
        }

        self.apply_filters();
        // The cursor starts on what the session is actually using, not on
        // whichever id sorts first.
        if let Some(current) = self.current.clone() {
            self.focus(&current);
        }
    }

    /// Ask one provider for its own list, on a thread, and report to the host.
    ///
    /// The answer arrives in the event loop and is handed back through
    /// `on_models_loaded`, so the draw never waits on an endpoint.
    fn begin_fetch(
        &mut self,
        app: &App,
        provider: String,
        kind: Kind,
        base_url: String,
        key: Option<String>,
    ) {
        self.loading = Some(provider.clone());
        let tx = app.models_tx.clone();
        std::thread::spawn(move || {
            let result = crate::provider::fetch_provider_models(kind, &base_url, key.as_deref());
            let (ids, error) = match result {
                Ok(mut ids) => {
                    ids.sort();
                    ids.dedup();
                    (ids, None)
                }
                Err(error) => (Vec::new(), Some(format!("{error:#}"))),
            };
            let _ = tx.send(ModelsLoaded {
                provider,
                ids,
                error,
            });
        });
    }

    /// Put the cursor on one id, if it is in the current view.
    fn focus(&mut self, id: &str) {
        if let Some(position) = self
            .filtered
            .iter()
            .position(|index| self.rows[*index].id == id)
        {
            self.selected = position;
            self.table.select(Some(position));
        }
    }

    /// Replace one provider's rows with the list it published.
    ///
    /// Only that provider's rows: the catalogue entries for the others describe
    /// providers this screen has not asked, and deleting them would make the
    /// browser lose its content every time a gateway answered.
    fn replace_live_rows(&mut self, provider: &str, ids: &[String]) {
        let keep = self.selected_id();
        self.rows.retain(|row| row.provider != provider);
        for id in ids {
            let pricing = builtin_price(id);
            self.rows.push(ModelRow {
                id: id.clone(),
                // The id is the label: it is what the endpoint accepts, and a
                // prettified name would be a second spelling to get wrong.
                label: id.clone(),
                provider: provider.to_string(),
                // A models endpoint lists ids, not context windows or prices;
                // `—` is honest about that where a guess would not be.
                context: 0,
                max_output: 0,
                input_per_mtok: pricing.map(|price| price.input_per_mtok),
                output_per_mtok: pricing.map(|price| price.output_per_mtok),
                variants: variants_for(id),
                tools: false,
                vision: false,
                json: false,
                deprecated: false,
                live: true,
                connected: true,
            });
        }
        self.apply_filters();
        // Rows were replaced underneath the cursor; put it back on the same id,
        // or on the model in use, rather than on whatever now sits at that
        // position.
        let back = keep
            .filter(|id| self.rows.iter().any(|row| row.id == *id))
            .or_else(|| self.current.clone());
        if let Some(id) = back {
            self.focus(&id);
        }
    }

    fn selected_id(&self) -> Option<String> {
        self.row().map(|row| row.id.clone())
    }

    fn apply_filters(&mut self) {
        let needle = self.search.trim().to_ascii_lowercase();
        let mut matched: Vec<usize> = Vec::new();

        for (index, row) in self.rows.iter().enumerate() {
            if let Some(provider) = &self.provider_filter
                && &row.provider != provider
            {
                continue;
            }
            if !needle.is_empty()
                && !row.id.to_ascii_lowercase().contains(&needle)
                && !row.label.to_ascii_lowercase().contains(&needle)
                && !row.provider.to_ascii_lowercase().contains(&needle)
            {
                continue;
            }
            let keep = match self.tab {
                Tab::All => true,
                Tab::Connected => row.connected,
                Tab::Favorites => self.favorites.iter().any(|id| id == &row.id),
                Tab::Reasoning => !row.variants.is_empty(),
            };
            if keep {
                matched.push(index);
            }
        }

        let rows = &self.rows;
        matched.sort_by(|left, right| {
            let (a, b) = (&rows[*left], &rows[*right]);
            let order = match self.sort {
                SortField::Provider => a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)),
                SortField::Model => a.id.cmp(&b.id),
                SortField::Context => a.context.cmp(&b.context).then(a.id.cmp(&b.id)),
                SortField::Input => cmp_price(a.input_per_mtok, b.input_per_mtok),
                SortField::Output => cmp_price(a.output_per_mtok, b.output_per_mtok),
            };
            if self.descending {
                order.reverse()
            } else {
                order
            }
        });

        self.filtered = matched;
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        self.table.select(if self.filtered.is_empty() {
            None
        } else {
            Some(self.selected)
        });
    }

    fn row(&self) -> Option<&ModelRow> {
        self.filtered
            .get(self.selected)
            .map(|index| &self.rows[*index])
    }

    fn effort(&self, id: &str, variants: &[&'static str]) -> Option<String> {
        if variants.is_empty() {
            return None;
        }
        self.efforts
            .get(id)
            .cloned()
            .filter(|effort| variants.contains(&effort.as_str()))
            .or_else(|| Some(variants[0].to_string()))
    }

    /// Move the selected row's reasoning effort one level.
    fn cycle_effort(&mut self, forward: bool) {
        let Some(row) = self.row().cloned() else {
            return;
        };
        if row.variants.is_empty() {
            self.note = Some((false, format!("{} takes no reasoning effort", row.id)));
            return;
        }
        let current = self
            .effort(&row.id, &row.variants)
            .and_then(|effort| row.variants.iter().position(|v| *v == effort))
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % row.variants.len()
        } else {
            (current + row.variants.len() - 1) % row.variants.len()
        };
        self.efforts
            .insert(row.id.clone(), row.variants[next].to_string());
    }

    fn toggle_favorite(&mut self, app: &mut App) {
        let Some(row) = self.row().cloned() else {
            return;
        };
        if let Some(position) = self.favorites.iter().position(|id| id == &row.id) {
            self.favorites.remove(position);
        } else {
            self.favorites.push(row.id.clone());
        }
        let mut config = app.config.clone();
        config
            .ui
            .get_or_insert_with(Default::default)
            .favorite_models = self.favorites.clone();
        match config.save(&app.config_path) {
            Ok(()) => {
                app.config = config;
                let verb = if self.favorites.contains(&row.id) {
                    "favorited"
                } else {
                    "unfavorited"
                };
                self.note = Some((true, format!("{verb} {}", row.id)));
            }
            Err(error) => self.note = Some((false, format!("cannot save favorites: {error:#}"))),
        }
        self.apply_filters();
    }

    /// Make the selected model the session's model.
    ///
    /// Everything not being changed is carried over from the config that was
    /// read: `max_tokens`, explicit rates, `base_url`. Switching families also
    /// switches the wire shape and the preset, because a Claude id on an OpenAI
    /// endpoint is a 404, not a preference.
    fn apply_selection(&mut self, app: &mut App) {
        let Some(row) = self.row().cloned() else {
            return;
        };
        // A model on a provider with no key cannot answer a turn. Writing it
        // anyway looked like a switch and failed on the next message, which
        // reads as a model problem instead of a missing connection.
        if !row.connected {
            self.note = Some((
                false,
                format!(
                    "no key for {} — open /connect to add one, then pick here",
                    row.provider
                ),
            ));
            return;
        }
        let mut section = app.config.provider.clone().unwrap_or_default();
        let target = kind_of(&row.provider);
        let family_changed = section
            .kind
            .as_deref()
            .map(|kind| !kind.eq_ignore_ascii_case(target.name()))
            .unwrap_or(true)
            || section
                .name
                .as_deref()
                .map(|name| name != row.provider)
                .unwrap_or(true);

        section.kind = Some(target.name().to_string());
        section.name = Some(row.provider.clone());
        section.model = Some(row.id.clone());
        section.reasoning_effort = self.effort(&row.id, &row.variants);

        if family_changed {
            section.base_url = Some(base_url_for(&row.provider, target));
            // An explicit rate is a statement about the old model; carrying it
            // to a new one would price Sonnet at GPT rates.
            section.input_per_mtok = None;
            section.output_per_mtok = None;
        }

        let effort = section.reasoning_effort.clone();
        let mut config = app.config.clone();
        config.provider = Some(section);
        match config.save(&app.config_path) {
            Ok(()) => {
                app.config = config;
                self.current = Some(row.id.clone());
                let effort = effort
                    .map(|effort| format!(" · reasoning {effort}"))
                    .unwrap_or_default();
                self.note = Some((
                    true,
                    format!("{} · {}{} — active now", row.id, row.provider, effort),
                ));
            }
            Err(error) => {
                self.note = Some((false, format!("cannot save the model: {error:#}")));
            }
        }
    }

    /// Ask the configured provider for its own list again.
    fn refresh_live(&mut self, app: &mut App) {
        let settings = match Settings::load(&app.config) {
            Ok(settings) => settings,
            Err(error) => {
                self.note = Some((false, format!("cannot read the config: {error:#}")));
                return;
            }
        };
        if !settings.ready() {
            self.note = Some((
                false,
                "no provider has a key yet — open /connect to add one".to_string(),
            ));
            return;
        }
        let provider = settings
            .provider_name
            .clone()
            .unwrap_or_else(|| settings.kind.name().to_string());
        self.begin_fetch(
            app,
            provider,
            settings.kind,
            settings.base_url.clone(),
            settings.api_key.clone(),
        );
    }

    fn set_provider_filter(&mut self, provider: &str) {
        self.provider_filter = if self.provider_filter.as_deref() == Some(provider) {
            None
        } else {
            Some(provider.to_string())
        };
        self.apply_filters();
    }

    /// One tab either way, wrapping. The tab change resets the row cursor,
    /// because the list the arrows land on is a different list.
    fn cycle_tab(&mut self, forward: bool) {
        self.tab = match (self.tab, forward) {
            (Tab::All, true) => Tab::Connected,
            (Tab::Connected, true) => Tab::Favorites,
            (Tab::Favorites, true) => Tab::Reasoning,
            (Tab::Reasoning, true) => Tab::All,
            (Tab::All, false) => Tab::Reasoning,
            (Tab::Connected, false) => Tab::All,
            (Tab::Favorites, false) => Tab::Connected,
            (Tab::Reasoning, false) => Tab::Favorites,
        };
        self.selected = 0;
        self.apply_filters();
    }

    /// A touchpad flick is a scroll: three rows, not a page, because the
    /// wheel streams events and a page per event overshoots every shelf.
    fn wheel(&mut self, up: bool) {
        if self.focus == FocusZone::Tabs {
            self.cycle_tab(!up);
            return;
        }
        self.move_by(if up { -3 } else { 3 });
    }

    fn move_by(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let last = self.filtered.len() - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected.saturating_add(delta as usize).min(last)
        };
        self.table.select(Some(self.selected));
    }

    fn close_popups(&mut self) -> bool {
        if self.details.take().is_some() || self.compare.take().is_some() {
            return true;
        }
        false
    }

    /// What the box is showing: the provider's own list, a request in flight,
    /// or the compiled-in fallback. Said in the border rather than in a row of
    /// its own, because it is a caption for the table under it.
    fn title(&self) -> String {
        match (&self.loading, &self.live_for) {
            (Some(provider), _) => format!(" Models — asking {provider} for its list… "),
            (None, Some(provider)) => format!(
                " Models — {provider} · {} from the provider ",
                self.live_count
            ),
            (None, None) => " Models — built-in list ".to_string(),
        }
    }

    /// The status row. Drawn every frame so the table above it never moves.
    fn status(&self, width: u16) -> Line<'static> {
        let room = (width as usize).saturating_sub(2);
        let (marker, color, text) = match &self.note {
            Some((true, text)) => ("✓", Color::Green, text.clone()),
            Some((false, text)) => ("✗", Color::Red, text.clone()),
            None if self.filtered.is_empty() => (
                "›",
                Color::Yellow,
                "no model matches — backspace edits the search, esc clears it, alt-0 clears the provider".to_string(),
            ),
            None => (" ", Color::DarkGray, String::new()),
        };
        Line::from(vec![
            Span::styled(format!("{marker} "), Style::default().fg(color)),
            Span::styled(ellipsize(&text, room), Style::default().fg(color)),
        ])
    }

    /// The hint row: one line, always present, ellipsized rather than wrapped.
    fn hints(&self) -> &'static str {
        match self.focus {
            FocusZone::Tabs => "←→ tabs · ↑↓ models/tabs · tab focus · enter use · esc close",
            FocusZone::Table => {
                "type to filter · ↑↓ models · ←→ reasoning · enter use · tab tabs · esc close"
            }
        }
    }
}

impl Default for ModelsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

fn cmp_price(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        // Unpriced sorts last either way, and never looks like "free".
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// The wire shape a provider preset speaks.
fn kind_of(provider: &str) -> Kind {
    crate::connect::known_providers()
        .iter()
        .find(|info| info.name == provider)
        .map(|info| info.kind)
        .unwrap_or(Kind::OpenAi)
}

/// The endpoint a preset lives at.
fn base_url_for(provider: &str, kind: Kind) -> String {
    crate::connect::known_providers()
        .iter()
        .find(|info| info.name == provider)
        .map(|info| info.default_base_url)
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| kind.default_base_url())
        .to_string()
}

/// Whether a turn could run on this provider right now.
///
/// The same precedence the request path uses: a stored preset key, the account
/// token for Memnoth, then the provider's own environment variable, then the
/// resolved settings for the provider this session is already pointed at.
fn provider_ready(_app: &App, settings: Option<&Settings>, provider: &str) -> bool {
    let preset = crate::connect::known_providers()
        .into_iter()
        .find(|info| info.name == provider);

    if let Some(info) = &preset {
        if !info.env_var.is_empty() && std::env::var(info.env_var).is_ok() {
            return true;
        }
        if let Ok(Some(key)) = crate::connect::get_provider_key(info.name)
            && !key.trim().is_empty()
        {
            return true;
        }
    }

    let Some(settings) = settings.filter(|settings| settings.ready()) else {
        return false;
    };
    if settings.kind == Kind::Memnoth && provider == "memnoth" {
        return true;
    }
    settings.kind == kind_of(provider)
        && settings
            .provider_name
            .as_deref()
            .map(|name| name == provider)
            .unwrap_or(true)
        && settings.api_key.is_some()
}

/// Fill `connected` once per provider, not once per row: reading the credential
/// store is a blocking call, and six of them is already enough per open.
fn seed_connectivity(rows: &mut [ModelRow], app: &App, settings: Option<&Settings>) {
    let mut cache: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for row in rows.iter_mut() {
        let ready = *cache
            .entry(row.provider.clone())
            .or_insert_with(|| provider_ready(app, settings, &row.provider));
        row.connected = ready;
    }
}

fn format_context(tokens: u32) -> String {
    match tokens {
        0 => "—".to_string(),
        tokens if tokens >= 1_000_000 => format!("{}M", tokens / 1_000_000),
        tokens if tokens >= 1_000 => format!("{}k", tokens / 1_000),
        tokens => tokens.to_string(),
    }
}

fn format_price(price: Option<f64>) -> String {
    match price {
        Some(price) if price < 1.0 => format!("{price:.2}"),
        Some(price) => format!("{price:.1}"),
        None => "—".to_string(),
    }
}

impl Overlay for ModelsOverlay {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Models
    }

    fn is_visible(&self) -> bool {
        self.visible
    }

    fn show(&mut self) {
        self.visible = true;
        self.focus = FocusZone::Table;
        self.details = None;
        self.compare = None;
        self.note = None;
    }

    fn on_open(&mut self, app: &mut App) {
        self.reload(app);
    }

    fn on_models_loaded(&mut self, result: &ModelsLoaded) {
        // Another open browser may have asked too; only the answer to the
        // request this screen is waiting on changes what it shows.
        if self.loading.as_deref() != Some(result.provider.as_str()) {
            return;
        }
        self.loading = None;
        match &result.error {
            None => {
                self.replace_live_rows(&result.provider, &result.ids);
                self.live_count = result.ids.len();
                self.live_for = Some(result.provider.clone());
                self.note = Some((
                    true,
                    format!(
                        "{} models listed by {} — enter uses one",
                        result.ids.len(),
                        result.provider
                    ),
                ));
            }
            Some(error) => {
                self.live_for = None;
                self.note = Some((
                    false,
                    format!(
                        "{} did not list its models ({error}) — showing the built-in list",
                        result.provider
                    ),
                ));
            }
        }
    }

    fn hide(&mut self) {
        self.visible = false;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        // A popup owns its dismissal, so the key that closes it cannot also act
        // on the row behind it.
        if self.close_popups() {
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {
                // One esc clears the filter; the next closes. A filter that
                // cannot be cleared with the same key as every other cancel
                // is a trap on a screen whose whole job is narrowing.
                if !self.search.is_empty() {
                    self.search.clear();
                    self.apply_filters();
                } else {
                    return OverlayAction::Close;
                }
            }
            KeyCode::Backspace if !alt => {
                self.search.pop();
                self.apply_filters();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    FocusZone::Tabs => FocusZone::Table,
                    FocusZone::Table => FocusZone::Tabs,
                };
            }
            KeyCode::Up => match self.focus {
                // Up past the first model reaches the tab bar; the grid is one
                // surface, not two screens stacked.
                FocusZone::Table if self.selected == 0 => self.focus = FocusZone::Tabs,
                FocusZone::Table => self.move_by(-1),
                FocusZone::Tabs => self.cycle_tab(false),
            },
            KeyCode::Down => match self.focus {
                FocusZone::Tabs => self.focus = FocusZone::Table,
                FocusZone::Table => self.move_by(1),
            },
            KeyCode::PageUp => {
                if self.focus == FocusZone::Table {
                    self.move_by(-10);
                } else {
                    self.cycle_tab(false);
                }
            }
            KeyCode::PageDown => {
                if self.focus == FocusZone::Table {
                    self.move_by(10);
                } else {
                    self.cycle_tab(true);
                }
            }
            KeyCode::Home => {
                self.selected = 0;
                self.table.select(Some(0));
            }
            KeyCode::End => {
                self.selected = self.filtered.len().saturating_sub(1);
                self.table.select(Some(self.selected));
            }
            // The horizontal arrows belong to the focused zone: tabs on the
            // tab bar, reasoning effort on a model row. The visual difference
            // is in the render, not in a mode the person has to remember.
            KeyCode::Left => match self.focus {
                FocusZone::Tabs => self.cycle_tab(false),
                FocusZone::Table => self.cycle_effort(false),
            },
            KeyCode::Right => match self.focus {
                FocusZone::Tabs => self.cycle_tab(true),
                FocusZone::Table => self.cycle_effort(true),
            },
            KeyCode::Enter => self.apply_selection(app),
            // Typing searches, so the one-letter actions moved behind alt:
            // the letters they used to take are the letters of the models.
            KeyCode::Char(ch) if alt => match ch.to_ascii_lowercase() {
                'f' => self.toggle_favorite(app),
                'c' => self.compare = Some(self.selected),
                'i' => self.details = Some(self.selected),
                's' => {
                    self.sort = self.sort.next();
                    self.apply_filters();
                }
                'r' => self.refresh_live(app),
                '0' => {
                    self.provider_filter = None;
                    self.apply_filters();
                }
                digit @ '1'..='6' => {
                    let index = digit as usize - '1' as usize;
                    let providers = crate::connect::known_providers();
                    if let Some(info) = providers.get(index) {
                        let name = info.name.to_string();
                        self.set_provider_filter(&name);
                    }
                }
                _ => {}
            },
            KeyCode::Char('s') if ctrl => {
                self.descending = !self.descending;
                self.apply_filters();
            }
            KeyCode::Char('r') if ctrl => self.refresh_live(app),
            // Everything else types: the search is on from the moment the
            // screen opens, so finding a model never needs a prefix key.
            KeyCode::Char(ch) if !ctrl && !alt => {
                self.search.push(ch);
                self.apply_filters();
            }
            _ => {}
        }
        OverlayAction::None
    }

    /// A pasted query is a search, not a set of key presses.
    fn handle_paste(&mut self, text: &str) -> bool {
        self.search
            .extend(text.chars().filter(|ch| !ch.is_control()));
        self.apply_filters();
        true
    }

    /// Clicks and the wheel. Tabs are four equal quarters of their bar; a
    /// model row selects, a second click on the selected row uses it, and
    /// the reasoning column steps the effort. Every region the render drew
    /// is clickable, because a row that looks like a button and is not one
    /// is a lie.
    fn handle_mouse(&mut self, mouse: MouseEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }
        let position = mouse.column;
        let row = mouse.row;
        match mouse.kind {
            MouseEventKind::ScrollUp => self.wheel(true),
            MouseEventKind::ScrollDown => self.wheel(false),
            MouseEventKind::Down(MouseButton::Left) => {
                if self.tabs_area.contains(Position::new(position, row)) {
                    let quarter = ((position.saturating_sub(self.tabs_area.x)) as usize) * 4
                        / self.tabs_area.width.max(1) as usize;
                    let tab =
                        [Tab::All, Tab::Connected, Tab::Favorites, Tab::Reasoning][quarter.min(3)];
                    if tab != self.tab {
                        self.tab = tab;
                        self.selected = 0;
                        self.apply_filters();
                    }
                    self.focus = FocusZone::Tabs;
                } else if self.table_area.contains(Position::new(position, row)) {
                    // First content row is under the header; the block adds
                    // one border row above the header.
                    let first = self.table_area.y + 2;
                    if row >= first {
                        let offset = (row - first) as usize;
                        let target = self.table.offset() + offset;
                        if target < self.filtered.len() {
                            let in_effort =
                                position >= self.effort_col.0 && position < self.effort_col.1;
                            if in_effort && target == self.selected {
                                self.cycle_effort(true);
                            } else if in_effort && target != self.selected {
                                self.selected = target;
                                self.table.select(Some(target));
                                self.cycle_effort(true);
                            } else if target == self.selected {
                                self.apply_selection(app);
                            } else {
                                self.selected = target;
                                self.table.select(Some(target));
                            }
                            self.focus = FocusZone::Table;
                        }
                    }
                }
            }
            _ => {}
        }
        OverlayAction::None
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _app: &App) {
        if !self.visible {
            return;
        }
        frame.render_widget(Clear, area);

        // Status and hints are two fixed rows at the bottom. They exist whether
        // or not they have anything to say, so a note cannot move the table —
        // and the hints no longer disappear behind the note that replaced them.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Min(4),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let titles: Vec<&str> = [Tab::All, Tab::Connected, Tab::Favorites, Tab::Reasoning]
            .iter()
            .map(|tab| tab.label())
            .collect();
        let active = [Tab::All, Tab::Connected, Tab::Favorites, Tab::Reasoning]
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);
        // The focused zone carries the bold border: that is what the arrows
        // answer to, and it is also what a click lands on.
        let tabs_border = if self.focus == FocusZone::Tabs {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let tabs = Tabs::new(titles)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(self.title())
                    .border_style(tabs_border),
            )
            .select(active)
            .style(Style::default().fg(Color::Gray))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        self.tabs_area = chunks[0];
        frame.render_widget(tabs, chunks[0]);

        // The search is on from the moment the screen opens: typing filters.
        // The line still says what it is, with a caret parked after the query,
        // and the active filters beside it so a filtered-out model is
        // explainable rather than missing.
        let search = format!("search: {}", self.search);
        let filter = self
            .provider_filter
            .as_ref()
            .map(|provider| format!(" · provider {provider}"))
            .unwrap_or_default();
        let sort = format!(
            " · sort {}{}",
            self.sort.label(),
            if self.descending { " ▼" } else { " ▲" }
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(search, Style::default().fg(Color::Cyan)),
                Span::styled(filter, Style::default().fg(Color::Yellow)),
                Span::styled(sort, Style::default().fg(Color::DarkGray)),
            ])),
            chunks[1],
        );
        // A real caret in the field, not a block glyph that cannot be typed
        // over: ratatui places the terminal's own cursor.
        {
            let prefix = "search: ".chars().count() as u16;
            let typed = self.search.chars().count() as u16;
            let x =
                (chunks[1].x + prefix + typed).min(chunks[1].x + chunks[1].width.saturating_sub(1));
            frame.set_cursor_position(Position::new(x, chunks[1].y));
        }

        let default_model = self.current.clone();
        let mut rows: Vec<Row> = Vec::new();
        for (position, index) in self.filtered.iter().enumerate() {
            let row = &self.rows[*index];
            let selected = position == self.selected;
            let is_default = default_model.as_deref() == Some(row.id.as_str());
            let favorite = self.favorites.iter().any(|id| id == &row.id);
            let effort = self.effort(&row.id, &row.variants);
            let effort_text = match (&effort, row.variants.is_empty()) {
                (Some(effort), _) => effort.clone(),
                (None, true) => "—".to_string(),
                _ => "—".to_string(),
            };
            let status = if row.deprecated {
                "✗ deprecated"
            } else if row.connected {
                "● ready"
            } else {
                "○ no key"
            };

            let mut name = row.label.clone();
            if is_default {
                name.push_str(" ★");
            }
            if favorite {
                name.push_str(" ⭐");
            }
            if row.live {
                name.push_str(" +");
            }

            // The cyan fill belongs to the table zone. With the tab bar
            // focused the selected row is only a marker, so the eye can tell
            // which zone the arrows answer to without reading the hint.
            let style = if selected && self.focus == FocusZone::Table {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            rows.push(
                Row::new(vec![
                    Cell::from(name),
                    Cell::from(row.provider.clone()),
                    Cell::from(format_context(row.context)),
                    Cell::from(format_price(row.input_per_mtok)),
                    Cell::from(format_price(row.output_per_mtok)),
                    Cell::from(effort_text),
                    Cell::from(status),
                ])
                .style(style),
            );
        }

        let header = Row::new(vec![
            Cell::from("model"),
            Cell::from("provider"),
            Cell::from("ctx"),
            Cell::from("$in"),
            Cell::from("$out"),
            Cell::from("reasoning"),
            Cell::from("status"),
        ])
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

        let table = Table::new(
            rows,
            [
                Constraint::Min(28),
                Constraint::Length(11),
                Constraint::Length(6),
                Constraint::Length(7),
                Constraint::Length(7),
                Constraint::Length(10),
                Constraint::Length(12),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} models ", self.filtered.len())),
        )
        .row_highlight_style(Style::default().bg(Color::Cyan).fg(Color::Black))
        .highlight_symbol(" ");

        self.table_area = chunks[2];
        // The reasoning column's x range, for click-to-cycle: one border plus
        // the five columns left of it.
        let effort_x = chunks[2].x.saturating_add(1 + 28 + 11 + 6 + 7 + 7 + 1);
        self.effort_col = (effort_x, effort_x + 10);
        let mut state = self.table.clone();
        state.select(if self.filtered.is_empty() {
            None
        } else {
            Some(self.selected)
        });
        frame.render_stateful_widget(table, chunks[2], &mut state);
        self.table = state;

        frame.render_widget(Paragraph::new(self.status(chunks[3].width)), chunks[3]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                ellipsize(self.hints(), chunks[4].width as usize),
                Style::default().fg(Color::DarkGray),
            ))),
            chunks[4],
        );

        if let Some(index) = self.details
            && let Some(row) = self.filtered.get(index).and_then(|i| self.rows.get(*i))
        {
            self.render_details(frame, area, row);
        }
        if let Some(index) = self.compare
            && let Some(row) = self.filtered.get(index).and_then(|i| self.rows.get(*i))
        {
            self.render_compare(frame, area, row, default_model.as_deref());
        }
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/models", "The models your provider lists (alias: /model)"),
            ("type", "Filter by model id or provider — typing searches"),
            ("↑/↓", "Move through models; ↑ at the top reaches the tabs"),
            ("←/→", "Reasoning on a model row, tabs on the tab bar"),
            ("Enter", "Use this model now, and save it"),
            ("Tab", "Move between the tab bar and the models"),
            ("Alt+1..6, Alt+0", "Filter by provider, alt-0 clears"),
            ("Alt+s, Ctrl+s", "Sort field, direction"),
            ("Alt+f", "Favorite (persisted)"),
            ("Alt+c", "Compare with the current model"),
            ("Alt+i", "Details"),
            ("Alt+r, Ctrl+r", "Ask the provider for its list again"),
            ("wheel / click", "Scroll, pick a tab, pick a model, use it"),
            ("esc", "Clear the filter, then close"),
        ]
    }
}

impl ModelsOverlay {
    fn render_details(&self, frame: &mut Frame, area: Rect, row: &ModelRow) {
        let width = ((area.width as u32 * 4 / 5) as u16).clamp(56, area.width);
        let height = ((area.height as u32 * 3 / 4) as u16).clamp(16, area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);

        let effort = self.effort(&row.id, &row.variants);
        let lines = vec![
            format!("id            {}", row.id),
            format!("provider      {}", row.provider),
            format!(
                "context       {} in · {} out",
                format_context(row.context),
                format_context(row.max_output)
            ),
            format!(
                "price         ${} in · ${} out per Mtok",
                format_price(row.input_per_mtok),
                format_price(row.output_per_mtok)
            ),
            format!(
                "reasoning     {}",
                match (&effort, row.variants.is_empty()) {
                    (Some(effort), _) => format!("{effort} ({})", row.variants.join(" / ")),
                    _ => "not supported".to_string(),
                }
            ),
            format!(
                "capabilities  {}{}{}",
                if row.tools { "tools " } else { "" },
                if row.vision { "vision " } else { "" },
                if row.json { "json" } else { "" }
            ),
            format!(
                "status        {}{}",
                if row.connected {
                    "key present"
                } else {
                    "no key"
                },
                if row.deprecated { " · deprecated" } else { "" }
            ),
            String::new(),
            if row.live {
                "from the provider's own /models; unpriced entries are priced".to_string()
            } else {
                "from the catalogue in this build; Enter makes it active".to_string()
            },
            if row.live {
                "by the built-in table only, so '—' means unknown.".to_string()
            } else {
                "immediately — no restart.".to_string()
            },
        ];

        let text: Vec<Line> = lines
            .into_iter()
            .map(|line| Line::from(Span::raw(line)))
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} — details ", row.label))
            .border_style(Style::default().fg(Color::Cyan));
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(block),
            popup,
        );
    }

    fn render_compare(&self, frame: &mut Frame, area: Rect, row: &ModelRow, default: Option<&str>) {
        let current = default.unwrap_or("(none)");
        let width = ((area.width as u32 * 3 / 4) as u16).clamp(60, area.width);
        let height = 14.min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);

        // 100k in, 4k out: one long session turn, so the numbers are comparable
        // rather than abstract.
        let estimate = |model: &ModelRow| match (model.input_per_mtok, model.output_per_mtok) {
            (Some(input), Some(output)) => {
                format!("${:.2}", input * 0.1 + output * 0.004)
            }
            _ => "unpriced".to_string(),
        };
        let current_row = self.rows.iter().find(|candidate| candidate.id == current);
        let price = |model: Option<&ModelRow>, pick: fn(&ModelRow) -> Option<f64>| match model {
            Some(model) => format_price(pick(model)),
            None => "—".to_string(),
        };

        let lines = vec![
            Line::from(vec![
                Span::styled("                  ", Style::default()),
                Span::styled(
                    format!("{:>16}", "current"),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(
                    format!("{:>18}", "selected"),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(vec![
                Span::styled("model             ", Style::default().fg(Color::Cyan)),
                Span::raw(format!("{:>16}", truncate(current, 16))),
                Span::raw(format!("{:>18}", truncate(&row.id, 18))),
            ]),
            Line::from(vec![
                Span::styled("provider          ", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    "{:>16}",
                    truncate(
                        current_row
                            .map(|model| model.provider.as_str())
                            .unwrap_or("—"),
                        16
                    )
                )),
                Span::raw(format!("{:>18}", row.provider)),
            ]),
            Line::from(vec![
                Span::styled("context           ", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    "{:>16}",
                    format_context(current_row.map(|model| model.context).unwrap_or(0))
                )),
                Span::raw(format!("{:>18}", format_context(row.context))),
            ]),
            Line::from(vec![
                Span::styled("in $/Mtok         ", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    "{:>16}",
                    price(current_row, |model| model.input_per_mtok)
                )),
                Span::raw(format!("{:>18}", format_price(row.input_per_mtok))),
            ]),
            Line::from(vec![
                Span::styled("out $/Mtok        ", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    "{:>16}",
                    price(current_row, |model| model.output_per_mtok)
                )),
                Span::raw(format!("{:>18}", format_price(row.output_per_mtok))),
            ]),
            Line::from(vec![
                Span::styled("100k in + 4k out  ", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    "{:>16}",
                    current_row.map(estimate).unwrap_or_else(|| "—".to_string())
                )),
                Span::raw(format!("{:>18}", estimate(row))),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "enter uses the selected model · esc closes",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" compare ")
            .border_style(Style::default().fg(Color::Cyan));
        frame.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened_overlay() -> ModelsOverlay {
        let mut overlay = ModelsOverlay::new();
        overlay.show();
        overlay.rows = catalogue();
        overlay.apply_filters();
        overlay
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Typing filters without a prefix key: the search is on from the moment
    /// the screen opens.
    #[test]
    fn typing_searches_without_a_slash() {
        let mut overlay = opened_overlay();
        let mut app = App::new("s", false);
        for ch in "deepseek".chars() {
            overlay.handle_key(key(KeyCode::Char(ch)), &mut app);
        }
        assert_eq!(overlay.search, "deepseek");
        assert!(
            overlay.filtered.iter().all(|index| overlay.rows[*index]
                .id
                .to_ascii_lowercase()
                .contains("deepseek")),
            "every row the filter kept names deepseek"
        );
        // One esc clears the filter rather than closing the screen.
        overlay.handle_key(key(KeyCode::Esc), &mut app);
        assert!(overlay.search.is_empty());
        // A second esc closes.
        let action = overlay.handle_key(key(KeyCode::Esc), &mut app);
        assert!(matches!(action, OverlayAction::Close));
    }

    /// The arrows belong to the focused zone: on the tab bar they cycle tabs,
    /// and up past the first model reaches that bar.
    #[test]
    fn the_arrows_answer_to_the_focused_zone() {
        let mut overlay = opened_overlay();
        let mut app = App::new("s", false);
        assert!(overlay.focus == FocusZone::Table);
        overlay.handle_key(key(KeyCode::Up), &mut app);
        assert!(overlay.focus == FocusZone::Tabs, "up past the first row");
        let before = overlay.tab;
        overlay.handle_key(key(KeyCode::Right), &mut app);
        assert_ne!(overlay.tab, before, "right on the tab bar changes tab");
        overlay.handle_key(key(KeyCode::Left), &mut app);
        assert_eq!(overlay.tab, before, "left returns to the first tab");
        overlay.handle_key(key(KeyCode::Down), &mut app);
        assert!(overlay.focus == FocusZone::Table, "down returns to models");
        // On a model row, the same arrows change reasoning effort instead.
        let position = overlay
            .filtered
            .iter()
            .position(|index| !overlay.rows[*index].variants.is_empty())
            .expect("a reasoning model is listed");
        overlay.selected = position;
        let id = overlay.rows[overlay.filtered[position]].id.clone();
        let first = overlay.effort(&id, &overlay.rows[overlay.filtered[position]].variants);
        overlay.handle_key(key(KeyCode::Right), &mut app);
        let next = overlay.effort(&id, &overlay.rows[overlay.filtered[position]].variants);
        assert_ne!(first, next, "right on a model steps the effort");
    }

    /// The wheel moves the selection without a click, and clicking a tab
    /// quarter focuses the tab bar.
    #[test]
    fn wheel_and_click_reach_the_list() {
        let mut overlay = opened_overlay();
        // Pretend a frame was drawn at a known rect.
        overlay.tabs_area = Rect::new(0, 0, 80, 3);
        overlay.table_area = Rect::new(0, 5, 80, 10);
        let mut app = App::new("s", false);
        let mouse = |kind| MouseEvent {
            kind,
            column: 5,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        overlay.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left)), &mut app);
        assert!(overlay.focus == FocusZone::Tabs);
        assert!(overlay.tab == Tab::All, "leftmost quarter is All");
        // The wheel follows the focused zone, so a wheel over the tab bar
        // cycles tabs — and over the models it moves the selection.
        overlay.handle_key(key(KeyCode::Down), &mut app);
        assert!(overlay.focus == FocusZone::Table);
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 10,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };
        overlay.handle_mouse(wheel, &mut app);
        assert!(overlay.selected > 0, "the wheel moved the selection");
    }

    #[test]
    fn the_catalogue_is_priced_from_the_shared_table() {
        let rows = catalogue();
        let sonnet = rows
            .iter()
            .find(|row| row.id == "claude-sonnet-4-5")
            .expect("sonnet is in the catalogue");
        assert_eq!(sonnet.input_per_mtok, Some(3.0));
        assert_eq!(sonnet.output_per_mtok, Some(15.0));
    }

    #[test]
    fn only_reasoning_models_offer_effort_levels() {
        assert!(variants_for("o3").contains(&"high"));
        assert!(variants_for("gpt-5").contains(&"medium"));
        assert!(variants_for("claude-sonnet-4-5").is_empty());
        assert!(variants_for("gpt-4o").is_empty());
    }

    #[test]
    fn an_unknown_model_sorts_last_and_prints_as_unknown() {
        assert_eq!(format_price(None), "—");
        assert_eq!(
            cmp_price(Some(1.0), None),
            std::cmp::Ordering::Less,
            "a priced model outranks an unpriced one"
        );
    }

    #[test]
    fn context_windows_read_as_tokens_people_recognise() {
        assert_eq!(format_context(200_000), "200k");
        assert_eq!(format_context(1_000_000), "1M");
        assert_eq!(format_context(0), "—");
    }
}
