//! Table widget: sortable, filterable, keybindings.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};

use crate::util::{ellipsize, muted_style, normal_style, title_style};

/// A column definition for the table
#[derive(Debug, Clone)]
pub struct Column {
    pub header: String,
    pub width: Option<usize>,
    pub sortable: bool,
    pub sort_index: Option<usize>,
    pub style: Option<Style>,
}

impl Column {
    pub fn new(header: impl Into<String>) -> Self {
        Self {
            header: header.into(),
            width: None,
            sortable: false,
            sort_index: None,
            style: None,
        }
    }

    pub fn width(mut self, width: usize) -> Self {
        self.width = Some(width);
        self
    }

    pub fn sortable(mut self) -> Self {
        self.sortable = true;
        self
    }

    pub fn sorted(mut self, index: usize) -> Self {
        self.sort_index = Some(index);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }
}

/// A row in the table
#[derive(Debug, Clone)]
pub struct Row {
    pub cells: Vec<String>,
    pub style: Option<Style>,
    pub selected: bool,
}

impl Row {
    pub fn new(cells: Vec<String>) -> Self {
        Self {
            cells,
            style: None,
            selected: false,
        }
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }

    pub fn selected(mut self) -> Self {
        self.selected = true;
        self
    }
}

/// Sort direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDirection {
    #[default]
    Ascending,
    Descending,
}

/// Table widget state
#[derive(Debug, Default)]
pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Row>,
    state: ListState,
    filter: String,
    sorted_by: Option<usize>,
    sort_direction: SortDirection,
    show_header: bool,
    zebra_stripes: bool,
}

impl Table {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_columns(&mut self, columns: Vec<Column>) {
        self.columns = columns;
    }

    pub fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.state
            .select(if self.rows.is_empty() { None } else { Some(0) });
    }

    pub fn add_row(&mut self, row: Row) {
        self.rows.push(row);
    }

    pub fn clear_rows(&mut self) {
        self.rows.clear();
        self.state.select(None);
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn show_header(mut self, show: bool) -> Self {
        self.show_header = show;
        self
    }

    pub fn zebra_stripes(mut self, enable: bool) -> Self {
        self.zebra_stripes = enable;
        self
    }

    /// Get filtered rows
    pub fn filtered_rows(&self) -> Vec<&Row> {
        if self.filter.is_empty() {
            self.rows.iter().collect()
        } else {
            self.rows
                .iter()
                .filter(|row| {
                    row.cells
                        .iter()
                        .any(|cell| cell.to_lowercase().contains(&self.filter.to_lowercase()))
                })
                .collect()
        }
    }

    /// Sort by column index
    pub fn sort_by(&mut self, column_index: usize) {
        if self.sorted_by == Some(column_index) {
            self.sort_direction = match self.sort_direction {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
        } else {
            self.sorted_by = Some(column_index);
            self.sort_direction = SortDirection::Ascending;
        }
        self.sort_rows();
    }

    fn sort_rows(&mut self) {
        if let Some(col_idx) = self.sorted_by {
            let asc = matches!(self.sort_direction, SortDirection::Ascending);
            self.rows.sort_by(|a, b| {
                let a_val = a.cells.get(col_idx).map(String::as_str).unwrap_or("");
                let b_val = b.cells.get(col_idx).map(String::as_str).unwrap_or("");
                if asc {
                    a_val.cmp(b_val)
                } else {
                    b_val.cmp(a_val)
                }
            });
        }
    }

    pub fn select_next(&mut self) {
        let filtered = self.filtered_rows();
        let len = filtered.len();
        if len == 0 {
            return;
        }
        let next = self
            .state
            .selected()
            .map(|s| (s + 1).min(len - 1))
            .unwrap_or(0);
        self.state.select(Some(next));
    }

    pub fn select_previous(&mut self) {
        let filtered = self.filtered_rows();
        let len = filtered.len();
        if len == 0 {
            return;
        }
        let prev = self
            .state
            .selected()
            .map(|s| s.saturating_sub(1))
            .unwrap_or(0);
        self.state.select(Some(prev));
    }

    pub fn select_first(&mut self) {
        if !self.filtered_rows().is_empty() {
            self.state.select(Some(0));
        }
    }

    pub fn select_last(&mut self) {
        let filtered = self.filtered_rows();
        if !filtered.is_empty() {
            self.state.select(Some(filtered.len() - 1));
        }
    }

    pub fn selected(&self) -> Option<&Row> {
        self.state
            .selected()
            .and_then(|i| self.filtered_rows().get(i).copied())
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.state.selected()
    }

    pub fn row_count(&self) -> usize {
        self.filtered_rows().len()
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 10 || area.height < 3 {
            return;
        }

        let visible_rows = self.filtered_rows();

        // Build header
        let mut header_lines: Vec<Line> = Vec::new();
        if self.show_header {
            for col in &self.columns {
                let mut header_text = col.header.clone();
                if col.sortable {
                    let col_idx = self.columns.iter().position(|c| c.header == col.header);
                    let marker = match (self.sorted_by, self.sort_direction, col_idx) {
                        (Some(i), SortDirection::Ascending, Some(j)) if i == j => " ▲",
                        (Some(i), SortDirection::Descending, Some(j)) if i == j => " ▼",
                        _ => "",
                    };
                    header_text = format!("{}{}", header_text, marker);
                }
                header_lines.push(Line::from(Span::styled(header_text, title_style())));
            }
        }

        // Build rows
        let mut rows: Vec<ListItem> = Vec::new();
        for (idx, row) in visible_rows.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            for (cell_idx, cell) in row.cells.iter().enumerate() {
                let col_style = self
                    .columns
                    .get(cell_idx)
                    .and_then(|c| c.style)
                    .unwrap_or(normal_style());
                let is_selected = self.state.selected() == Some(idx);
                let style = if is_selected {
                    col_style.bg(Color::Cyan).fg(Color::Black)
                } else if self.zebra_stripes && idx % 2 == 1 {
                    col_style.bg(Color::DarkGray)
                } else {
                    col_style
                };
                spans.push(Span::styled(ellipsize(cell, area.width as usize), style));
                if cell_idx < self.columns.len() - 1 {
                    spans.push(Span::styled(" │ ", muted_style()));
                }
            }
            rows.push(ListItem::new(Line::from(spans)));
        }

        let list = List::new(rows)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(muted_style())
                    .title(Line::from(Span::styled(" table ", muted_style()))),
            )
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));

        frame.render_stateful_widget(list, area, &mut self.state);

        // Render header above list area (simplified: header is part of first row visually)
        // In a real implementation, this would use Layout to split header/body
        let _ = header_lines;
    }

    pub fn render_popup(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 60 || area.height < 15 {
            return;
        }
        let width = ((area.width as u32 * 4 / 5) as u16).clamp(60, area.width);
        let height = ((area.height as u32 * 4 / 5) as u16).clamp(15, area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        self.render(frame, popup);
    }

    /// Keyboard event handling (returns action if one should be taken)
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<TableAction> {
        use crossterm::event::{KeyCode, KeyModifiers};
        match key.code {
            KeyCode::Down => {
                self.select_next();
                None
            }
            KeyCode::Up => {
                self.select_previous();
                None
            }
            KeyCode::Home => {
                self.select_first();
                None
            }
            KeyCode::End => {
                self.select_last();
                None
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(TableAction::Find)
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(TableAction::Sort)
            }
            KeyCode::Enter => Some(TableAction::Select),
            KeyCode::Esc => Some(TableAction::Cancel),
            _ => None,
        }
    }
}

/// Actions the table can request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAction {
    Select,
    Cancel,
    Find,
    Sort,
    SortBy(usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_table(table: &mut Table, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| table.render(frame, frame.area()))
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
    fn table_renders_rows() {
        let mut table = Table::new();
        table.set_columns(vec![
            Column::new("Name").width(20),
            Column::new("Type").width(15),
        ]);
        table.set_rows(vec![
            Row::new(vec!["main.rs".into(), "file".into()]),
            Row::new(vec!["config.rs".into(), "file".into()]),
        ]);
        let text = render_table(&mut table, 80, 20);
        assert!(text.contains("main.rs"));
        assert!(text.contains("config.rs"));
    }

    #[test]
    fn table_filters() {
        let mut table = Table::new();
        table.set_columns(vec![Column::new("Name")]);
        table.set_rows(vec![
            Row::new(vec!["main.rs".into()]),
            Row::new(vec!["config.rs".into()]),
        ]);
        table.set_filter("main".into());
        assert_eq!(table.row_count(), 1);
        assert_eq!(table.selected().unwrap().cells[0], "main.rs");
    }

    #[test]
    fn table_sorts() {
        let mut table = Table::new();
        table.set_columns(vec![Column::new("Name").sortable()]);
        table.set_rows(vec![
            Row::new(vec!["zebra".into()]),
            Row::new(vec!["alpha".into()]),
        ]);
        table.sort_by(0);
        let first = table.filtered_rows()[0].cells[0].clone();
        assert_eq!(first, "alpha");
    }
}
