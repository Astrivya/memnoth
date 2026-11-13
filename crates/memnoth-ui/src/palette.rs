//! Palette widget: fuzzy match, shows CLI forms, keybindings.
//!
//! This is extracted from the startup/home screen in `tui/ui.rs::draw_home`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::util::{ellipsize, ellipsize_words, muted_style, normal_style, title_style, wrap};

/// Configuration for a palette item (command + description)
#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub command: String,
    pub description: String,
}

/// Configuration for the palette widget
#[derive(Debug, Clone)]
pub struct PaletteConfig {
    pub title: String,
    pub version: String,
    pub subtitle: String,
    pub rows: Vec<PaletteRow>,
    pub items: Vec<PaletteItem>,
    pub footer_hint: String,
}

#[derive(Debug, Clone)]
pub struct PaletteRow {
    pub label: String,
    pub value: String,
    pub note: String,
}

/// The palette widget state
#[derive(Debug, Default)]
pub struct Palette {
    /// The palette's content, exposed so an overlay can render its own chrome.
    pub config: PaletteConfig,
    filter: String,
    selected_index: usize,
}

impl Palette {
    /// Create a new palette with default configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a palette from a config
    pub fn with_config(config: PaletteConfig) -> Self {
        Self {
            config,
            filter: String::new(),
            selected_index: 0,
        }
    }

    /// Set the filter text for fuzzy matching
    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.selected_index = 0;
    }

    /// The index of the highlighted item.
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Move the highlight to an explicit item, clamped to the filtered list.
    pub fn select(&mut self, index: usize) {
        let len = self.filtered_items().len();
        if len > 0 {
            self.selected_index = index.min(len - 1);
        }
    }

    /// Get the current filter
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Get filtered items based on current filter
    pub fn filtered_items(&self) -> Vec<&PaletteItem> {
        if self.filter.is_empty() {
            self.config.items.iter().collect()
        } else {
            // One rank: the same fuzzy scorer the `/` dropdown uses. A
            // substring test here and a fuzzy one there meant the same query
            // produced two different orders. A command whose *name* matches
            // comes first: someone typing `help` means `/help`, not the row
            // whose description happens to contain those letters in order.
            let needle = self.filter.to_lowercase();
            let mut by_command: Vec<&PaletteItem> = Vec::new();
            let mut by_description: Vec<&PaletteItem> = Vec::new();
            for item in &self.config.items {
                if fuzzy_match(&item.command.to_lowercase(), &needle).is_some() {
                    by_command.push(item);
                } else if fuzzy_match(&item.description.to_lowercase(), &needle).is_some() {
                    by_description.push(item);
                }
            }
            by_command.into_iter().chain(by_description).collect()
        }
    }

    /// Move selection up
    /// How many commands exist before the filter is applied.
    pub fn total(&self) -> usize {
        self.config.items.len()
    }

    /// Move the highlight to the first item.
    pub fn select_first(&mut self) {
        self.select(0);
    }

    /// Move the highlight to the last item, or to nothing when the filter
    /// matched no row.
    pub fn select_last(&mut self) {
        let len = self.filtered_items().len();
        if len > 0 {
            self.select(len - 1);
        }
    }

    pub fn select_previous(&mut self) {
        let items = self.filtered_items();
        if !items.is_empty() {
            self.selected_index = self.selected_index.saturating_sub(1);
        }
    }

    /// Move selection down
    pub fn select_next(&mut self) {
        let items = self.filtered_items();
        if !items.is_empty() {
            self.selected_index = (self.selected_index + 1).min(items.len() - 1);
        }
    }

    /// Get the currently selected item
    pub fn selected_item(&self) -> Option<&PaletteItem> {
        self.filtered_items().get(self.selected_index).copied()
    }

    /// Set the palette rows (label/value/note)
    pub fn set_rows(&mut self, rows: Vec<PaletteRow>) {
        self.config.rows = rows;
    }

    /// Set the palette items (commands)
    pub fn set_items(&mut self, items: Vec<PaletteItem>) {
        self.config.items = items;
        self.selected_index = 0;
    }

    /// Set the title
    pub fn set_title(&mut self, title: String, version: String) {
        self.config.title = title;
        self.config.version = version;
    }

    /// Set the subtitle
    pub fn set_subtitle(&mut self, subtitle: String) {
        self.config.subtitle = subtitle;
    }

    /// Set the footer hint
    pub fn set_footer_hint(&mut self, hint: String) {
        self.config.footer_hint = hint;
    }

    /// Render the palette widget
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 20 || area.height < 6 {
            return;
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(muted_style())
            .title(Line::from(Span::styled(" palette ", muted_style())));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let width = inner.width as usize;
        let mut lines: Vec<Line> = Vec::new();

        // Title row: wordmark on the left, version on the right
        let version = self.config.version.trim();
        let pad = width
            .saturating_sub(1 + self.config.title.len() + version.chars().count() + 1)
            .max(1);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(self.config.title.clone(), title_style()),
            Span::raw(" ".repeat(pad)),
            Span::styled(format!("{version} "), muted_style()),
        ]));

        // Rule
        lines.push(Line::from(Span::styled(
            format!(" {}", "─".repeat(width.saturating_sub(1))),
            muted_style(),
        )));

        // Subtitle
        lines.push(Line::from(Span::styled(
            ellipsize(&self.config.subtitle, width),
            normal_style().fg(Color::Gray),
        )));
        lines.push(Line::from(""));

        // Rows
        const LABEL_WIDTH: usize = 11;
        for row in &self.config.rows {
            let room = width.saturating_sub(1 + LABEL_WIDTH);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(format!("{:<LABEL_WIDTH$}", row.label), muted_style()),
                Span::styled(ellipsize(&row.value, room), normal_style()),
            ];
            let used = 1 + LABEL_WIDTH + row.value.chars().count();
            if !row.note.is_empty() {
                let separator = "  ·  ";
                let remaining = width.saturating_sub(used + separator.len());
                if remaining >= 8 {
                    spans.push(Span::styled(separator, muted_style()));
                    spans.push(Span::styled(
                        ellipsize_words(&row.note, remaining),
                        muted_style(),
                    ));
                }
            }
            lines.push(Line::from(spans));
        }

        // Items section
        if !self.config.items.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " commands",
                Style::default().fg(Color::White),
            )));

            let items = self.filtered_items();
            const COMMAND_COLUMN: usize = 16;
            for (idx, item) in items.iter().enumerate() {
                let room = width.saturating_sub(2 + COMMAND_COLUMN);
                let is_selected = idx == self.selected_index;
                let mut spans = vec![Span::raw("  ")];

                if is_selected {
                    spans.push(Span::styled(
                        format!("{:<COMMAND_COLUMN$}", item.command),
                        Style::default().fg(Color::Black).bg(Color::Cyan),
                    ));
                    spans.push(Span::styled(
                        ellipsize_words(&item.description, room),
                        Style::default().fg(Color::Black).bg(Color::Cyan),
                    ));
                } else {
                    spans.push(Span::styled(
                        format!("{:<COMMAND_COLUMN$}", item.command),
                        Style::default().fg(Color::Cyan),
                    ));
                    spans.push(Span::styled(
                        ellipsize_words(&item.description, room),
                        normal_style().fg(Color::Gray),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }

        // Footer hint pinned to bottom
        let hint = Line::from(vec![
            Span::raw(" "),
            Span::styled(&self.config.footer_hint, muted_style()),
        ]);
        let height = inner.height as usize;
        let content_height = lines.len();
        if content_height < height {
            for _ in 0..height - content_height - 1 {
                lines.push(Line::from(""));
            }
        }
        lines.push(hint);

        frame.render_widget(Paragraph::new(lines).wrap(wrap()), inner);
    }

    /// Render as a popup (for command palette overlay)
    pub fn render_popup(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 40 || area.height < 10 {
            return;
        }
        let width = ((area.width as u32 * 3 / 4) as u16).clamp(40, area.width);
        let height = ((area.height as u32 * 3 / 4) as u16).clamp(10, area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        self.render(frame, popup);
    }
}

impl Default for PaletteConfig {
    fn default() -> Self {
        Self {
            title: "memnoth".to_string(),
            version: "0.1.0".to_string(),
            subtitle: "terminal agent · temporal vault · code graph".to_string(),
            rows: Vec::new(),
            items: Vec::new(),
            footer_hint: "enter select · ↑↓ navigate · type to filter · esc close".to_string(),
        }
    }
}

/// Fuzzy-match `needle` against `haystack`, returning the positions the
/// needle landed on when every one of its characters appears in order.
///
/// One scorer for every surface that offers a command. It lived in the TUI,
/// where the palette could not reach it, so `Palette` fell back to a
/// substring test and the two surfaces ranked the same query differently.
pub fn fuzzy_match(haystack: &str, needle: &str) -> Option<Vec<usize>> {
    let mut hits = Vec::with_capacity(needle.len());
    let mut wanted = needle.chars();
    let mut next = wanted.next();
    for (index, ch) in haystack.chars().enumerate() {
        let Some(want) = next else { break };
        if ch == want {
            hits.push(index);
            next = wanted.next();
        }
    }
    if next.is_some() { None } else { Some(hits) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_palette(palette: &mut Palette, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| palette.render(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn palette_renders_title() {
        let mut palette = Palette::new();
        palette.set_title("memnoth".to_string(), "0.1.0".to_string());
        let text = render_palette(&mut palette, 80, 20);
        assert!(text.contains("memnoth"));
        assert!(text.contains("0.1.0"));
    }

    #[test]
    fn palette_renders_rows() {
        let mut palette = Palette::new();
        palette.set_rows(vec![
            PaletteRow {
                label: "workspace".to_string(),
                value: "E:/ws".to_string(),
                note: "".to_string(),
            },
            PaletteRow {
                label: "vault".to_string(),
                value: "E:/state/vault.db".to_string(),
                note: "not initialised".to_string(),
            },
        ]);
        let text = render_palette(&mut palette, 80, 20);
        assert!(text.contains("workspace"));
        assert!(text.contains("E:/ws"));
        assert!(text.contains("vault"));
        assert!(text.contains("not initialised"));
    }

    #[test]
    fn palette_filters_items() {
        let mut palette = Palette::new();
        palette.set_items(vec![
            PaletteItem {
                command: "/vault init".to_string(),
                description: "create the temporal vault".to_string(),
            },
            PaletteItem {
                command: "/search".to_string(),
                description: "search the vault and graph".to_string(),
            },
        ]);
        palette.set_filter("init".to_string());
        let filtered = palette.filtered_items();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].command, "/vault init");
    }
}
