//! Tree widget: kg symbols, vault entities with hierarchical display.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};

use crate::util::{ellipsize, muted_style, normal_style};

/// A node in the tree
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub id: String,
    pub label: String,
    pub children: Vec<TreeNode>,
    pub expanded: bool,
    pub icon: String,
    pub style: Option<Style>,
    pub metadata: Option<serde_json::Value>,
}

impl TreeNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            children: Vec::new(),
            expanded: false,
            icon: "▸".to_string(),
            style: None,
            metadata: None,
        }
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn add_child(&mut self, child: TreeNode) {
        self.children.push(child);
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
        if expanded {
            self.icon = "▾".to_string();
        } else {
            self.icon = "▸".to_string();
        }
    }

    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
        if self.expanded {
            self.icon = "▾".to_string();
        } else {
            self.icon = "▸".to_string();
        }
    }
}

/// Tree widget configuration
#[derive(Debug, Clone)]
pub struct TreeConfig {
    pub title: String,
    pub show_icons: bool,
    pub indent_size: usize,
    pub highlight_style: Style,
    pub normal_style: Style,
}

impl Default for TreeConfig {
    fn default() -> Self {
        Self {
            title: "tree".to_string(),
            show_icons: true,
            indent_size: 2,
            highlight_style: Style::default().fg(Color::Black).bg(Color::Cyan),
            normal_style: normal_style(),
        }
    }
}

/// Flattened tree entry for rendering
#[derive(Debug, Clone)]
pub struct FlatEntry {
    depth: usize,
    id: String,
    label: String,
    icon: String,
    style: Style,
}

/// Tree widget state
#[derive(Debug, Default)]
pub struct Tree {
    config: TreeConfig,
    roots: Vec<TreeNode>,
    state: ListState,
    flattened: Vec<FlatEntry>,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: TreeConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    pub fn set_roots(&mut self, roots: Vec<TreeNode>) {
        self.roots = roots;
        self.rebuild_flattened();
        self.state.select(if self.flattened.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    pub fn set_title(&mut self, title: String) {
        self.config.title = title;
    }

    fn rebuild_flattened(&mut self) {
        self.flattened.clear();
        // Clone roots to avoid moving them, then flatten
        let roots = self.roots.clone();
        for root in roots {
            self.flatten_node_owned(root, 0);
        }
    }

    /// Flatten a node and its expanded children into the flat list.
    fn flatten_node_owned(&mut self, node: TreeNode, depth: usize) {
        let style = node.style.unwrap_or(self.config.normal_style);
        let icon = node.icon.clone();
        let label = node.label.clone();
        let id = node.id.clone();
        self.flattened.push(FlatEntry {
            depth,
            id,
            label,
            icon,
            style,
        });
        if node.expanded {
            for child in node.children {
                self.flatten_node_owned(child, depth + 1);
            }
        }
    }

    /// Toggle expansion of the currently selected node
    pub fn toggle_selected(&mut self) {
        if let Some(selected) = self.state.selected()
            && let Some(id) = self.flattened.get(selected).map(|e| e.id.clone())
            && let Some(node) = find_node_mut(&mut self.roots, &id)
        {
            node.toggle();
            self.rebuild_flattened();
        }
    }

    pub fn select_next(&mut self) {
        let len = self.flattened.len();
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
        let len = self.flattened.len();
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
        if !self.flattened.is_empty() {
            self.state.select(Some(0));
        }
    }

    pub fn select_last(&mut self) {
        if !self.flattened.is_empty() {
            self.state.select(Some(self.flattened.len() - 1));
        }
    }

    pub fn selected(&self) -> Option<&FlatEntry> {
        self.state.selected().and_then(|i| self.flattened.get(i))
    }

    pub fn selected_id(&self) -> Option<String> {
        self.selected().map(|e| e.id.clone())
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.width < 10 || area.height < 3 {
            return;
        }

        let items: Vec<ListItem> = self
            .flattened
            .iter()
            .map(|entry| {
                let indent = " ".repeat(entry.depth * self.config.indent_size);
                let content = if self.config.show_icons && !entry.icon.is_empty() {
                    format!("{indent}{} {}", entry.icon, entry.label)
                } else {
                    format!("{indent}{}", entry.label)
                };
                ListItem::new(Line::from(Span::styled(
                    ellipsize(&content, area.width as usize),
                    entry.style,
                )))
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(muted_style())
                    .title(Line::from(Span::styled(
                        format!(" {} ", self.config.title),
                        muted_style(),
                    ))),
            )
            .highlight_style(self.config.highlight_style);

        frame.render_stateful_widget(list, area, &mut self.state);
        let inner = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        if self.flattened.len() > inner.height as usize {
            let mut scrollbar_state = ScrollbarState::new(self.flattened.len())
                .position(self.state.selected().unwrap_or(0))
                .viewport_content_length(inner.height as usize);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight),
                inner,
                &mut scrollbar_state,
            );
        }
    }

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

    /// Helper to build a tree from KG symbols
    pub fn from_kg_symbols(symbols: &[KgSymbol]) -> Vec<TreeNode> {
        let mut by_file: std::collections::HashMap<String, Vec<&KgSymbol>> =
            std::collections::HashMap::new();

        for sym in symbols {
            by_file.entry(sym.file.clone()).or_default().push(sym);
        }

        let mut roots = Vec::new();
        for (file, syms) in by_file {
            let mut file_node = TreeNode::new(file.clone(), file.clone())
                .with_icon("📄".to_string())
                .with_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );

            for sym in syms {
                let kind_icon = match sym.kind.as_str() {
                    "function" => "ƒ",
                    "struct" => "𝑆",
                    "enum" => "ℰ",
                    "trait" => "𝑇",
                    "mod" => "𝑀",
                    "const" => "𝐶",
                    "static" => "𝑆",
                    _ => "●",
                };
                let sym_node = TreeNode::new(sym.id.clone(), sym.name.clone())
                    .with_icon(kind_icon.to_string())
                    .with_style(Style::default().fg(Color::White))
                    .with_metadata(serde_json::json!({
                        "kind": sym.kind,
                        "line": sym.line,
                        "file": sym.file,
                    }));
                file_node.add_child(sym_node);
            }
            file_node.set_expanded(true);
            roots.push(file_node);
        }

        roots
    }

    /// Helper to build a tree from vault entities
    pub fn from_vault_entities(entities: &[VaultEntity]) -> Vec<TreeNode> {
        let mut by_namespace: std::collections::HashMap<String, Vec<&VaultEntity>> =
            std::collections::HashMap::new();

        for entity in entities {
            by_namespace
                .entry(entity.namespace.clone())
                .or_default()
                .push(entity);
        }

        let mut roots = Vec::new();
        for (namespace, entities) in by_namespace {
            let mut ns_node = TreeNode::new(namespace.clone(), namespace.clone())
                .with_icon("📦".to_string())
                .with_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                );

            for entity in entities {
                let kind_icon = match entity.kind.as_str() {
                    "fact" => "✦",
                    "decision" => "⌬",
                    "lesson" => "✎",
                    "gotcha" => "⚠",
                    "correction" => "↻",
                    _ => "●",
                };
                let entity_node = TreeNode::new(entity.id.clone(), entity.name.clone())
                    .with_icon(kind_icon.to_string())
                    .with_style(Style::default().fg(Color::White))
                    .with_metadata(serde_json::json!({
                        "kind": entity.kind,
                        "created": entity.created,
                        "modified": entity.modified,
                    }));
                ns_node.add_child(entity_node);
            }
            ns_node.set_expanded(true);
            roots.push(ns_node);
        }

        roots
    }
}

/// Find a mutable node by id in a slice of nodes
fn find_node_mut<'a>(nodes: &'a mut [TreeNode], id: &str) -> Option<&'a mut TreeNode> {
    for node in nodes.iter_mut() {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find_node_mut(&mut node.children, id) {
            return Some(found);
        }
    }
    None
}

/// KG Symbol for tree building
#[derive(Debug, Clone)]
pub struct KgSymbol {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
}

/// Vault Entity for tree building
#[derive(Debug, Clone)]
pub struct VaultEntity {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub namespace: String,
    pub created: i64,
    pub modified: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_tree(tree: &mut Tree, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| tree.render(frame, frame.area()))
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
    fn tree_renders_roots() {
        let mut tree = Tree::new();
        tree.set_roots(vec![
            TreeNode::new("1", "root1").with_icon("📁".to_string()),
            TreeNode::new("2", "root2").with_icon("📁".to_string()),
        ]);
        let text = render_tree(&mut tree, 60, 20);
        assert!(text.contains("root1"));
        assert!(text.contains("root2"));
    }

    #[test]
    fn tree_expands_node() {
        let mut tree = Tree::new();
        let mut root = TreeNode::new("1", "root").with_icon("📁".to_string());
        root.add_child(TreeNode::new("2", "child"));
        tree.set_roots(vec![root]);

        // Initially collapsed
        let text = render_tree(&mut tree, 60, 20);
        assert!(text.contains("root"));
        assert!(!text.contains("child"));

        // Expand
        tree.toggle_selected();
        let text = render_tree(&mut tree, 60, 20);
        assert!(text.contains("child"));
    }

    #[test]
    fn tree_from_kg_symbols() {
        let symbols = vec![
            KgSymbol {
                id: "1".into(),
                name: "main".into(),
                kind: "function".into(),
                file: "main.rs".into(),
                line: 1,
            },
            KgSymbol {
                id: "2".into(),
                name: "Config".into(),
                kind: "struct".into(),
                file: "config.rs".into(),
                line: 10,
            },
        ];
        let roots = Tree::from_kg_symbols(&symbols);
        assert_eq!(roots.len(), 2);
        let labels: std::collections::HashSet<_> = roots.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains("main.rs"));
        assert!(labels.contains("config.rs"));
        let main_rs = roots.iter().find(|r| r.label == "main.rs").unwrap();
        assert_eq!(main_rs.children.len(), 1);
        assert_eq!(main_rs.children[0].label, "main");
    }
}
