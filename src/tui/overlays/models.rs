//! ModelsOverlay: stunning model browser with tabs, filtering, sorting, and actions.
//!
//! Tabs: All / Connected / Favorites / Recommended.
//! Columns: Model (name+provider), Context, Pricing (input/output),
//! Capabilities (tools/vision/json), Speed, Status.
//! Filter: provider, capability, price range, context.
//! Sort: pricing, context, speed.
//! Actions: Enter=select/set default, c=compare, f=favorite, i=details, /=search.
//! Shows current default with ★.
//! --json with full metadata.

use crate::context::Config;
use crate::provider::{Pricing, Settings};
use crate::tui::app::App;
use crate::tui::overlays::{Overlay, OverlayAction, OverlayKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use memnoth_auth::get_credential_sync;
use memnoth_cloud_client::{CloudClient, ModelInfo};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
/// Tab variants for the model browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelTab {
    All,
    Connected,
    Favorites,
    Recommended,
}

/// Speed rating for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SpeedRating {
    Slow,
    Medium,
    Fast,
}

/// Status of a model (connected / available / deprecated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelStatus {
    Connected,
    Available,
    Deprecated,
}

/// A single model entry with full metadata.
#[derive(Debug, Clone)]
struct ModelEntry {
    id: String,
    name: String,
    provider: String,
    family: String,
    context_window: u32,
    max_output_tokens: u32,
    input_cost_per_million_micros: i64,
    output_cost_per_million_micros: i64,
    supports_streaming: bool,
    supports_tools: bool,
    supports_vision: bool,
    supports_json: bool,
    capabilities: Vec<String>,
    deprecated: bool,
    speed: SpeedRating,
    status: ModelStatus,
    is_default: bool,
    is_favorite: bool,
}

impl serde::Serialize for ModelEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("ModelEntry", 15)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("name", &self.name)?;
        s.serialize_field("provider", &self.provider)?;
        s.serialize_field("family", &self.family)?;
        s.serialize_field("context_window", &self.context_window)?;
        s.serialize_field("max_output_tokens", &self.max_output_tokens)?;
        s.serialize_field(
            "input_cost_per_million_micros",
            &self.input_cost_per_million_micros,
        )?;
        s.serialize_field(
            "output_cost_per_million_micros",
            &self.output_cost_per_million_micros,
        )?;
        s.serialize_field("supports_streaming", &self.supports_streaming)?;
        s.serialize_field("supports_tools", &self.supports_tools)?;
        s.serialize_field("supports_vision", &self.supports_vision)?;
        s.serialize_field("supports_json", &self.supports_json)?;
        s.serialize_field("capabilities", &self.capabilities)?;
        s.serialize_field("deprecated", &self.deprecated)?;
        s.serialize_field("speed", &speed_label(self.speed))?;
        s.serialize_field("status", &status_text(self.status))?;
        s.serialize_field("is_default", &self.is_default)?;
        s.serialize_field("is_favorite", &self.is_favorite)?;
        s.end()
    }
}

/// Sort fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortField {
    #[default]
    Pricing,
    Context,
    Speed,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

/// Filter state for the model list.
#[derive(Debug, Clone, Default)]
struct ModelFilter {
    provider: Option<String>,
    capability: Option<String>,
    min_price: Option<f64>,
    max_price: Option<f64>,
    min_context: Option<u32>,
    search: String,
}

#[derive(Debug, Default)]
pub struct ModelsOverlay {
    visible: bool,
    tabs: Vec<ModelTab>,
    active_tab: usize,
    models: Vec<ModelEntry>,
    filtered: Vec<ModelEntry>,
    selected: usize,
    filter: ModelFilter,
    sort_field: SortField,
    sort_direction: SortDirection,
    details_popup: Option<ModelEntry>,
    json_mode: bool,
}

impl ModelsOverlay {
    /// The tab the overlay is showing.
    fn tab(&self) -> ModelTab {
        self.tabs
            .get(self.active_tab)
            .cloned()
            .unwrap_or(ModelTab::All)
    }

    pub fn new() -> Self {
        let mut tabs = Vec::new();
        tabs.push(ModelTab::All);
        tabs.push(ModelTab::Connected);
        tabs.push(ModelTab::Favorites);
        tabs.push(ModelTab::Recommended);

        Self {
            tabs,
            models: Vec::new(),
            filtered: Vec::new(),
            filter: ModelFilter::default(),
            sort_field: SortField::Pricing,
            sort_direction: SortDirection::Ascending,
            ..Default::default()
        }
    }

    /// Fill the browser from the cloud catalogue and the built-in fallback.
    ///
    /// Public because the host is what knows when the overlay is pushed and it
    /// holds the `App` the catalogue is read against — `show()` has neither.
    pub fn load_models(&mut self, app: &mut App) {
        self.models = self.fetch_available_models(app);
        self.apply_filters();
    }

    fn fetch_available_models(&mut self, app: &mut App) -> Vec<ModelEntry> {
        // The catalogue lives at the API base, which is not the provider base:
        // `CloudClient` joins `models` onto it.
        let base_url = std::env::var("MEMNOTH_API_URL")
            .unwrap_or_else(|_| "https://memnoth.astrivya.ai/api/v1".to_string());
        let api_key = std::env::var("MEMNOTH_API_KEY").ok();

        // The cloud answers with the OpenAI-shaped list (id, name, description).
        // Pricing and capabilities come from the catalogue this build ships, so a
        // model the cloud knows but we do not still appears — unpriced, not hidden.
        let mut fetched = self.builtin_models_info();
        if let Ok(mut client) = CloudClient::with_base_url(&base_url) {
            if let Some(key) = api_key {
                client.set_auth_token(key);
            }
            if let Ok(list) = client.models() {
                for entry in list.data {
                    if fetched.iter().any(|m| m.id == entry.id) {
                        continue;
                    }
                    fetched.push(ModelInfo {
                        id: entry.id,
                        name: entry.name,
                        provider: "memnoth".to_string(),
                        family: String::new(),
                        context_window: 0,
                        max_output_tokens: 0,
                        input_cost_per_million_micros: -1,
                        output_cost_per_million_micros: -1,
                        supports_streaming: true,
                        supports_tools: false,
                        supports_vision: false,
                        capabilities: None,
                        deprecated: Some(false),
                        deprecation_date: None,
                    });
                }
            }
        }

        let default_model = self.get_default_model(app);

        let mut entries: Vec<ModelEntry> = Vec::new();
        for info in fetched {
            let capabilities = info.capabilities.clone().unwrap_or_default();
            let supports_json = capabilities.iter().any(|c| c == "json" || c == "json_mode");
            let speed = infer_speed(&info.family);

            let is_default = default_model.as_ref().map_or(false, |dm| dm == &info.id);
            let status = if info.deprecated.unwrap_or(false) {
                ModelStatus::Deprecated
            } else if self.is_provider_connected(&info.provider, app) {
                ModelStatus::Connected
            } else {
                ModelStatus::Available
            };

            entries.push(ModelEntry {
                id: info.id.clone(),
                name: info.name.clone(),
                provider: info.provider.clone(),
                family: info.family.clone(),
                context_window: info.context_window,
                max_output_tokens: info.max_output_tokens,
                input_cost_per_million_micros: info.input_cost_per_million_micros,
                output_cost_per_million_micros: info.output_cost_per_million_micros,
                supports_streaming: info.supports_streaming,
                supports_tools: info.supports_tools,
                supports_vision: info.supports_vision,
                supports_json,
                capabilities,
                deprecated: info.deprecated.unwrap_or(false),
                speed,
                status,
                is_default,
                is_favorite: false,
            });
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    fn builtin_models_info(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "gpt-4o".to_string(),
                name: "GPT-4o".to_string(),
                provider: "openai".to_string(),
                family: "gpt-4".to_string(),
                context_window: 128000,
                max_output_tokens: 4096,
                input_cost_per_million_micros: 2500,
                output_cost_per_million_micros: 10000,
                supports_streaming: true,
                supports_tools: true,
                supports_vision: true,
                capabilities: Some(vec!["tools".into(), "vision".into(), "json".into()]),
                deprecated: Some(false),
                deprecation_date: None,
            },
            ModelInfo {
                id: "gpt-4o-mini".to_string(),
                name: "GPT-4o Mini".to_string(),
                provider: "openai".to_string(),
                family: "gpt-4".to_string(),
                context_window: 128000,
                max_output_tokens: 4096,
                input_cost_per_million_micros: 150,
                output_cost_per_million_micros: 600,
                supports_streaming: true,
                supports_tools: true,
                supports_vision: true,
                capabilities: Some(vec!["tools".into(), "vision".into(), "json".into()]),
                deprecated: Some(false),
                deprecation_date: None,
            },
            ModelInfo {
                id: "claude-3-5-sonnet-20241022".to_string(),
                name: "Claude 3.5 Sonnet".to_string(),
                provider: "anthropic".to_string(),
                family: "claude-3".to_string(),
                context_window: 200000,
                max_output_tokens: 8192,
                input_cost_per_million_micros: 3000,
                output_cost_per_million_micros: 15000,
                supports_streaming: true,
                supports_tools: true,
                supports_vision: true,
                capabilities: Some(vec!["tools".into(), "vision".into(), "json".into()]),
                deprecated: Some(false),
                deprecation_date: None,
            },
            ModelInfo {
                id: "claude-3-haiku-20240307".to_string(),
                name: "Claude 3 Haiku".to_string(),
                provider: "anthropic".to_string(),
                family: "claude-3".to_string(),
                context_window: 200000,
                max_output_tokens: 4096,
                input_cost_per_million_micros: 250,
                output_cost_per_million_micros: 1250,
                supports_streaming: true,
                supports_tools: true,
                supports_vision: true,
                capabilities: Some(vec!["tools".into(), "vision".into(), "json".into()]),
                deprecated: Some(false),
                deprecation_date: None,
            },
        ]
    }

    fn get_default_model(&self, app: &App) -> Option<String> {
        Settings::load(&app.config).ok().map(|s| s.model)
    }

    fn is_provider_connected(&self, provider: &str, _app: &App) -> bool {
        match provider {
            "anthropic" => std::env::var("ANTHROPIC_API_KEY").is_ok(),
            "openai" => std::env::var("OPENAI_API_KEY").is_ok(),
            "memnoth" => {
                get_credential_sync("memnoth").is_ok() || std::env::var("MEMNOTH_API_KEY").is_ok()
            }
            _ => false,
        }
    }

    fn apply_filters(&mut self) {
        self.filtered = self
            .models
            .iter()
            .filter(|m| {
                // Tab filter
                match self.tab() {
                    ModelTab::All => {}
                    ModelTab::Connected => {
                        if m.status != ModelStatus::Connected {
                            return false;
                        }
                    }
                    ModelTab::Favorites => {
                        if !m.is_favorite {
                            return false;
                        }
                    }
                    ModelTab::Recommended => {
                        if m.speed != SpeedRating::Fast {
                            return false;
                        }
                    }
                }

                // Provider filter
                if let Some(ref p) = self.filter.provider {
                    if !m.provider.to_lowercase().contains(&p.to_lowercase()) {
                        return false;
                    }
                }

                // Capability filter
                if let Some(ref c) = self.filter.capability {
                    if !m
                        .capabilities
                        .iter()
                        .any(|cap| cap.to_lowercase().contains(&c.to_lowercase()))
                    {
                        return false;
                    }
                }

                // Price range filter
                if let Some(min) = self.filter.min_price {
                    let price = m.input_cost_per_million_micros as f64 / 1_000_000.0;
                    if price < min {
                        return false;
                    }
                }
                if let Some(max) = self.filter.max_price {
                    let price = m.input_cost_per_million_micros as f64 / 1_000_000.0;
                    if price > max {
                        return false;
                    }
                }

                // Context filter
                if let Some(min_ctx) = self.filter.min_context {
                    if m.context_window < min_ctx {
                        return false;
                    }
                }

                // Search filter (matches name, provider, family)
                if !self.filter.search.is_empty() {
                    let q = self.filter.search.to_lowercase();
                    if !m.name.to_lowercase().contains(&q)
                        && !m.provider.to_lowercase().contains(&q)
                        && !m.family.to_lowercase().contains(&q)
                    {
                        return false;
                    }
                }

                true
            })
            .cloned()
            .collect();

        self.sort_models();
    }

    fn sort_models(&mut self) {
        match self.sort_field {
            SortField::Pricing => {
                self.filtered.sort_by(|a, b| {
                    let a_price = a.input_cost_per_million_micros;
                    let b_price = b.input_cost_per_million_micros;
                    if self.sort_direction == SortDirection::Ascending {
                        a_price.cmp(&b_price)
                    } else {
                        b_price.cmp(&a_price)
                    }
                });
            }
            SortField::Context => {
                self.filtered.sort_by(|a, b| {
                    if self.sort_direction == SortDirection::Ascending {
                        a.context_window.cmp(&b.context_window)
                    } else {
                        b.context_window.cmp(&a.context_window)
                    }
                });
            }
            SortField::Speed => {
                self.filtered.sort_by(|a, b| {
                    if self.sort_direction == SortDirection::Ascending {
                        a.speed.cmp(&b.speed)
                    } else {
                        b.speed.cmp(&a.speed)
                    }
                });
            }
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    fn selected_model(&self) -> Option<&ModelEntry> {
        self.filtered.get(self.selected)
    }

    fn toggle_favorite(&mut self) {
        if let Some(m) = self.selected_model() {
            let id = m.id.clone();
            if let Some(entry) = self.models.iter_mut().find(|e| e.id == id) {
                entry.is_favorite = !entry.is_favorite;
            }
            self.apply_filters();
        }
    }

    fn set_as_default(&mut self, app: &mut App) {
        // The selection lives in `filtered`, so take the identity out before the
        // list is mutated — otherwise the borrow and the edit overlap.
        let Some((id, name)) = self
            .selected_model()
            .map(|m| (m.id.clone(), m.name.clone()))
        else {
            return;
        };

        for entry in &mut self.models {
            entry.is_default = entry.id == id;
        }

        // Only the `[provider]` section this overlay owns is rewritten; every
        // other section is carried over from the config that was read.
        let mut config = app.config.clone();
        if let Ok(settings) = Settings::load(&app.config) {
            config.provider = Some(crate::provider::ProviderConfig {
                kind: Some(settings.kind.name().to_string()),
                model: Some(id.clone()),
                base_url: Some(settings.base_url.clone()),
                max_tokens: Some(settings.max_tokens),
                input_per_mtok: settings.pricing.as_ref().map(|p| p.input_per_mtok),
                output_per_mtok: settings.pricing.as_ref().map(|p| p.output_per_mtok),
            });
            let _ = config.save(&app.config_path);
        }

        self.apply_filters();
        app.push(
            crate::tui::app::EntryRole::Info,
            format!("Set {name} as default model"),
        );
    }

    fn toggle_tab(&mut self) {
        self.active_tab = (self.active_tab + 1) % self.tabs.len();
        self.selected = 0;
        self.apply_filters();
    }

    fn toggle_sort(&mut self) {
        match self.sort_field {
            SortField::Pricing => self.sort_field = SortField::Context,
            SortField::Context => self.sort_field = SortField::Speed,
            SortField::Speed => self.sort_field = SortField::Pricing,
        }
        self.apply_filters();
    }

    fn toggle_sort_direction(&mut self) {
        self.sort_direction = match self.sort_direction {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => SortDirection::Ascending,
        };
        self.apply_filters();
    }

    fn set_search(&mut self, search: String) {
        self.filter.search = search;
        self.apply_filters();
    }

    fn clear_search(&mut self) {
        self.filter.search.clear();
        self.apply_filters();
    }

    fn set_provider_filter(&mut self, provider: String) {
        if provider.is_empty() {
            self.filter.provider = None;
        } else {
            self.filter.provider = Some(provider);
        }
        self.apply_filters();
    }

    fn set_capability_filter(&mut self, capability: String) {
        if capability.is_empty() {
            self.filter.capability = None;
        } else {
            self.filter.capability = Some(capability);
        }
        self.apply_filters();
    }

    fn set_min_price(&mut self, price: f64) {
        self.filter.min_price = Some(price);
        self.apply_filters();
    }

    fn set_max_price(&mut self, price: f64) {
        self.filter.max_price = Some(price);
        self.apply_filters();
    }

    fn set_min_context(&mut self, context: u32) {
        self.filter.min_context = Some(context);
        self.apply_filters();
    }

    fn clear_filters(&mut self) {
        self.filter = ModelFilter::default();
        self.apply_filters();
    }

    fn input_price(&self, m: &ModelEntry) -> f64 {
        m.input_cost_per_million_micros as f64 / 1_000_000.0
    }

    fn output_price(&self, m: &ModelEntry) -> f64 {
        m.output_cost_per_million_micros as f64 / 1_000_000.0
    }

    fn format_price(micros: i64) -> String {
        if micros < 0 {
            "N/A".to_string()
        } else {
            format!("${:.4}/M", micros as f64 / 1_000_000.0)
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let tab_titles: Vec<&str> = self
            .tabs
            .iter()
            .map(|t| match t {
                ModelTab::All => "All",
                ModelTab::Connected => "Connected",
                ModelTab::Favorites => "Favorites",
                ModelTab::Recommended => "Recommended",
            })
            .collect();

        let tabs = ratatui::widgets::Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::ALL).title(" Models "))
            .select(self.active_tab)
            .style(Style::default().fg(Color::Gray))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        frame.render_widget(tabs, area);
    }

    fn render_table(&mut self, frame: &mut Frame, area: Rect) {
        let mut rows: Vec<ratatui::widgets::ListItem> = Vec::new();
        for (idx, m) in self.filtered.iter().enumerate() {
            let is_selected = self.selected == idx;
            let default_marker = if m.is_default { " ★" } else { "" };
            let favorite_marker = if m.is_favorite { " ⭐" } else { "" };

            let model_line = Line::from(vec![
                Span::styled(
                    format!("{}{}{}", m.name, default_marker, favorite_marker),
                    if is_selected {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else if m.is_default {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!(" │ {}", m.provider),
                    if is_selected {
                        Style::default().fg(Color::Black).bg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
            ]);

            let context_line = Line::from(vec![Span::styled(
                format!("{}k", m.context_window / 1000),
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default()
                },
            )]);

            let price_line = Line::from(vec![Span::styled(
                format!("${:.4} / ${:.4}", self.input_price(m), self.output_price(m)),
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default()
                },
            )]);

            let caps_line = Line::from(vec![Span::styled(
                format!(
                    "{} {} {}",
                    if m.supports_tools { "🔧" } else { "⚪" },
                    if m.supports_vision { "👁️" } else { "⚪" },
                    if m.supports_json { "JSON" } else { "  " },
                ),
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default()
                },
            )]);

            let speed_style = match m.speed {
                SpeedRating::Slow => Color::Red,
                SpeedRating::Medium => Color::Yellow,
                SpeedRating::Fast => Color::Green,
            };
            let speed_line = Line::from(vec![Span::styled(
                speed_label(m.speed),
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(speed_style)
                },
            )]);

            let status_style = match m.status {
                ModelStatus::Connected => Color::Green,
                ModelStatus::Available => Color::Cyan,
                ModelStatus::Deprecated => Color::Red,
            };
            let status_label = match m.status {
                ModelStatus::Connected => "●",
                ModelStatus::Available => "○",
                ModelStatus::Deprecated => "✗",
            };
            let status_line = Line::from(vec![Span::styled(
                format!("{} {}", status_label, status_text(m.status)),
                if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(status_style)
                },
            )]);

            let combined = Line::from(vec![
                Span::raw(model_line.spans[0].content.clone()),
                Span::raw(" │ "),
                Span::raw(context_line.spans[0].content.clone()),
                Span::raw(" │ "),
                Span::raw(price_line.spans[0].content.clone()),
                Span::raw(" │ "),
                Span::raw(caps_line.spans[0].content.clone()),
                Span::raw(" │ "),
                Span::raw(speed_line.spans[0].content.clone()),
                Span::raw(" │ "),
                Span::raw(status_line.spans[0].content.clone()),
            ]);

            let row_style = if is_selected {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else if idx % 2 == 1 {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            rows.push(ratatui::widgets::ListItem::new(combined).style(row_style));
        }

        let list = ratatui::widgets::List::new(rows)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} models ", self.filtered.len()))
                    .border_style(Style::default().fg(Color::Gray)),
            )
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));

        let mut list_state = ratatui::widgets::ListState::default();
        if !self.filtered.is_empty() {
            list_state.select(Some(self.selected.min(self.filtered.len() - 1)));
        }

        frame.render_stateful_widget(list, area, &mut list_state);
    }

    fn render_footer(&mut self, frame: &mut Frame, area: Rect) {
        let filter_hint = if !self.filter.search.is_empty() {
            format!("search: {} │ ", self.filter.search)
        } else {
            String::new()
        };

        let sort_hint = match self.sort_field {
            SortField::Pricing => "sort: price",
            SortField::Context => "sort: context",
            SortField::Speed => "sort: speed",
        };

        let direction_hint = match self.sort_direction {
            SortDirection::Ascending => " ▲",
            SortDirection::Descending => " ▼",
        };

        let tab_hint = match self.tab() {
            ModelTab::All => "tab: all",
            ModelTab::Connected => "tab: connected",
            ModelTab::Favorites => "tab: favorites",
            ModelTab::Recommended => "tab: recommended",
        };

        let help_text = format!(
            "{} {} {} │ {} │ ↑↓: nav · enter: default · c: compare · f: favorite · i: details · /: search · tab: next · s: sort · esc: close",
            filter_hint, sort_hint, direction_hint, tab_hint
        );

        let help = Paragraph::new(help_text)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(help, area);
    }

    fn render_details_popup(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(ref m) = self.details_popup {
            let width = ((area.width as u32 * 4 / 5) as u16).clamp(60, area.width);
            let height = ((area.height as u32 * 3 / 4) as u16).clamp(20, area.height);
            let popup = Rect::new(
                area.x + (area.width - width) / 2,
                area.y + (area.height - height) / 2,
                width,
                height,
            );

            frame.render_widget(Clear, popup);

            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} — Details ", m.name))
                .border_style(Style::default().fg(Color::Cyan));
            frame.render_widget(block, popup);

            let inner = popup.inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 1,
            });

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Min(0),
                ])
                .split(inner);

            let lines: Vec<Line> = vec![
                Line::from(vec![
                    Span::styled(
                        "ID: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&m.id),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Provider: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&m.provider),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Family: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&m.family),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Context: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{}k tokens", m.context_window / 1000)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Max Output: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{} tokens", m.max_output_tokens)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Input Price: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(Self::format_price(m.input_cost_per_million_micros)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Output Price: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(Self::format_price(m.output_cost_per_million_micros)),
                ]),
                Line::from(vec![
                    Span::styled(
                        "Capabilities: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(m.capabilities.join(", ")),
                ]),
            ];

            for (i, line) in lines.iter().enumerate() {
                if i < chunks.len() {
                    frame.render_widget(Paragraph::new(line.clone()), chunks[i]);
                }
            }

            let hint = Paragraph::new("press any key to close")
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(hint, area);
        }
    }

    fn render_json(&self, frame: &mut Frame, area: Rect) {
        if self.json_mode {
            if let Some(m) = self.selected_model() {
                let json = serde_json::to_string_pretty(m).unwrap_or_default();
                let popup_width = ((area.width as u32 * 9 / 10) as u16).clamp(80, area.width);
                let popup_height = ((area.height as u32 * 8 / 10) as u16).clamp(20, area.height);
                let popup = Rect::new(
                    area.x + (area.width - popup_width) / 2,
                    area.y + (area.height - popup_height) / 2,
                    popup_width,
                    popup_height,
                );

                frame.render_widget(Clear, popup);

                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(" Model Metadata (JSON) ")
                    .border_style(Style::default().fg(Color::Cyan));
                frame.render_widget(block, popup);

                let inner = popup.inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 1,
                });

                let paragraph = Paragraph::new(json)
                    .block(Block::default().borders(Borders::NONE))
                    .wrap(Wrap { trim: false });
                frame.render_widget(paragraph, inner);
            }
        }
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
        // load_models will be called by the app when it has a valid App context
        self.active_tab = 0;
        self.selected = 0;
    }

    fn hide(&mut self) {
        self.visible = false;
        self.details_popup = None;
    }

    fn handle_key(&mut self, key: KeyEvent, app: &mut App) -> OverlayAction {
        if !self.visible {
            return OverlayAction::None;
        }

        // Details popup takes priority
        if self.details_popup.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.details_popup = None;
                }
                _ => {}
            }
            return OverlayAction::None;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Esc => return OverlayAction::Close,
            KeyCode::Char('q') => return OverlayAction::Close,
            KeyCode::Tab => {
                self.toggle_tab();
            }
            KeyCode::Char('s') => {
                self.toggle_sort();
            }
            KeyCode::Char('S') if ctrl => {
                self.toggle_sort_direction();
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Down => {
                if self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Home => {
                self.selected = 0;
            }
            KeyCode::End => {
                self.selected = self.filtered.len().saturating_sub(1);
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.filtered.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                self.set_as_default(app);
            }
            KeyCode::Char('c') => {
                if let Some(m) = self.selected_model() {
                    app.push(
                        crate::tui::app::EntryRole::Info,
                        format!("Compare: {} vs default", m.name),
                    );
                }
            }
            KeyCode::Char('f') => {
                self.toggle_favorite();
            }
            KeyCode::Char('i') => {
                if let Some(m) = self.selected_model() {
                    self.details_popup = Some(m.clone());
                }
            }
            KeyCode::Char('/') => {
                app.push(
                    crate::tui::app::EntryRole::Info,
                    "Type search query...".to_string(),
                );
            }
            KeyCode::Char('1') => {
                self.set_provider_filter("openai".to_string());
            }
            KeyCode::Char('2') => {
                self.set_provider_filter("anthropic".to_string());
            }
            KeyCode::Char('3') => {
                self.set_provider_filter("memnoth".to_string());
            }
            KeyCode::Char('r') => {
                self.clear_filters();
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

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Tabs
                Constraint::Min(0),    // Table
                Constraint::Length(3), // Footer
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_table(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);

        if self.details_popup.is_some() {
            self.render_details_popup(frame, area);
        }

        if self.json_mode {
            self.render_json(frame, area);
        }
    }

    fn help_text(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("/models", "Open model browser overlay"),
            ("Tab", "Switch tabs (All/Connected/Favorites/Recommended)"),
            ("↑/↓", "Navigate models"),
            ("PgUp/PgDn", "Page navigation"),
            ("Home/End", "Jump to start/end"),
            ("Enter", "Set as default model"),
            ("C", "Compare with default"),
            ("F", "Toggle favorite"),
            ("I", "Show model details"),
            ("/", "Search models"),
            ("1/2/3", "Filter by provider (openai/anthropic/memnoth)"),
            ("R", "Clear filters"),
            ("S", "Cycle sort (price/context/speed)"),
            ("Ctrl+S", "Toggle sort direction"),
            ("Esc", "Close overlay"),
        ]
    }
}

fn speed_label(speed: SpeedRating) -> &'static str {
    match speed {
        SpeedRating::Slow => "Slow",
        SpeedRating::Medium => "Med",
        SpeedRating::Fast => "Fast",
    }
}

fn status_text(status: ModelStatus) -> &'static str {
    match status {
        ModelStatus::Connected => "Connected",
        ModelStatus::Available => "Available",
        ModelStatus::Deprecated => "Deprecated",
    }
}

fn infer_speed(family: &str) -> SpeedRating {
    let f = family.to_lowercase();
    if f.contains("haiku") || f.contains("mini") || f.contains("flash") {
        SpeedRating::Fast
    } else if f.contains("opus") || f.contains("sonnet") || f.contains("4o") || f.contains("70b") {
        SpeedRating::Medium
    } else {
        SpeedRating::Slow
    }
}
