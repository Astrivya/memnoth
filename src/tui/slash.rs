//! The slash-command dropdown: what the list under the prompt knows.
//!
//! It exists because a command nobody can see is a command nobody uses.
//! Remembering that `/vault contradictions` exists is a documentation problem;
//! typing `/` and reading three lines is not. This module holds the list and
//! which row is highlighted — nothing about drawing it — so the behaviour is
//! testable without a terminal, and the frame stays a pure function of state.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use memnoth_ui::palette::{PaletteItem, fuzzy_match};

use crate::tui::overlays::palette::{commands, insertable, needs_arguments};

/// How many rows one page key moves. Small enough to stay oriented in a list
/// this long, large enough to skip a whole group.
const PAGE: usize = 5;

/// One line of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The command as the list writes it, without its usage tail.
    pub name: String,
    /// The line beside it. Never wrapped: one row is one command.
    pub description: String,
    /// Which characters of `name` the query matched, for the highlight.
    pub hits: Vec<usize>,
}

/// The command as a person types it: everything before the first placeholder or
/// flag. `/vault link <source> <target> --reason <why>` is the command
/// `/vault link` with four answers still missing, and the list shows the first
/// part because that is the part you pick.
pub(crate) fn display_name(command: &str) -> &str {
    let mut cut = None;
    let mut offset = 0usize;
    for token in command.split(' ') {
        if token.starts_with(['<', '[', '-']) {
            cut = Some(offset.saturating_sub(1));
            break;
        }
        offset += token.len() + 1;
    }
    match cut {
        Some(end) if end > 0 => &command[..end],
        _ => command,
    }
}

/// The description without the usage tail it so often repeats.
///
/// Half the registry entries read `What it does - /command <arg>`. On one line
/// the command column already says `/command`, so that tail is the first thing
/// to go: what is left is the part that explains the command.
fn tidy_description(command: &str, description: &str) -> String {
    let name = display_name(command);
    if let Some(at) = description.rfind(" - ")
        && description[at + 3..].starts_with(name)
    {
        return description[..at].trim_end().to_string();
    }
    description.to_string()
}

/// How many commands are remembered as recently run. Long enough that ordinary
/// work floats to the top, short enough that stale ones fall away.
const RECENT: usize = 12;

/// What this machine has run, kept across sessions.
///
/// Two facts share one file because they are two views of the same history:
/// `recent` is the order the dropdown offers, `runs` is how often each command
/// was run, which the `/commands` browser shows beside the row. The count is
/// not capped by [`RECENT`] — a command that has fallen off the shortlist is
/// still one you have run forty times.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Command names (as the list spells them), most recent first.
    #[serde(default)]
    pub recent: Vec<String>,
    /// Command name → times run.
    #[serde(default)]
    pub runs: BTreeMap<String, usize>,
}

/// Read the usage file, or an empty history when it is missing or unreadable.
///
/// A broken stats file must never stop a session from starting: the worst it
/// costs is one unpromoted dropdown, and a person can still type the command.
pub fn load_usage(path: &Path) -> Usage {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write the usage file, creating its directory.
pub fn save_usage(path: &Path, usage: &Usage) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(usage)?;
    std::fs::write(path, text)
}

/// A line that is past the command name and into its arguments.
#[derive(Debug, Clone)]
struct Argument {
    /// Everything before the value being typed, spaces included, so a pick can
    /// rebuild the line.
    head: String,
    /// The partial value being typed.
    word: String,
    /// The placeholder this position expects, as the usage string spells it.
    needs: Option<String>,
}

/// The open dropdown: what it is filtering, which row is highlighted, and —
/// after a command and a space — the values that command expects.
#[derive(Debug)]
pub struct SlashMenu {
    open: bool,
    /// The command filter without its slash; in argument mode, the partial
    /// value.
    query: String,
    selected: usize,
    items: Vec<PaletteItem>,
    /// Ranked indices shared by rendering and navigation until the prompt edits.
    ranked_cache: Vec<(usize, Vec<usize>)>,
    /// What the command still needs, from the shared projection.
    /// The per-line parse below stays as a fallback for a line the
    /// projection does not list.
    hint: Option<String>,
    /// The command being given arguments, when the line is past its name.
    /// `None` means the dropdown is choosing a command.
    argument: Option<Argument>,
    /// Parent command path whose immediate subcommands are being browsed.
    /// For example, `mcp profile` makes the menu show its children only.
    subcommand_prefix: Option<String>,
    /// Rows for the argument, built by `sync` because they read the directory
    /// listing and the frame must not.
    values: Vec<Row>,
    /// Commands run recently, most recent first: what `/` offers before a
    /// filter is typed. Restored from disk at startup, so the ranking survives
    /// a restart.
    recent: Vec<String>,
    /// How often each command has been run, for the `/commands` browser. Kept
    /// past the [`RECENT`] window so a count does not reset when a command
    /// falls off the shortlist.
    runs: BTreeMap<String, usize>,
}

impl Default for SlashMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl SlashMenu {
    /// A closed dropdown holding every command.
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
            items: commands(),
            ranked_cache: Vec::new(),
            argument: None,
            subcommand_prefix: None,
            hint: None,
            values: Vec::new(),
            recent: Vec::new(),
            runs: BTreeMap::new(),
        }
    }

    /// Take the history saved by an earlier session.
    pub fn adopt(&mut self, usage: Usage) {
        self.recent = usage.recent;
        self.recent.truncate(RECENT);
        self.runs = usage.runs;
    }

    /// The history as it is written to disk.
    pub fn usage(&self) -> Usage {
        Usage {
            recent: self.recent.clone(),
            runs: self.runs.clone(),
        }
    }

    /// How often each command has been run, keyed by the name the list shows.
    pub fn runs(&self) -> &BTreeMap<String, usize> {
        &self.runs
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// What the command still expects, for the hint under the prompt.
    pub fn needs(&self) -> Option<&str> {
        let argument = self.argument.as_ref()?;
        self.hint.as_deref().or(argument.needs.as_deref())
    }

    /// Whether the list is choosing a command rather than filling in its
    /// arguments. Enter runs the highlighted row in the first case and runs the
    /// line in the second.
    pub fn choosing_command(&self) -> bool {
        self.argument.is_none()
    }

    /// Close without touching the selected row, so reopening the same query
    /// lands where it was left.
    pub fn close(&mut self) {
        self.open = false;
        self.argument = None;
        self.subcommand_prefix = None;
        self.hint = None;
        self.values.clear();
        self.ranked_cache.clear();
    }

    /// Remember that a line was run, so `/` offers it first next time.
    pub fn record(&mut self, line: &str) {
        let Some(rest) = line.trim().strip_prefix('/') else {
            return;
        };
        let typed = format!("/{rest}").to_lowercase();
        let name = self
            .items
            .iter()
            .map(|item| display_name(&item.command))
            .filter(|name| {
                let name = name.to_lowercase();
                typed == name || typed.starts_with(&format!("{name} "))
            })
            .max_by_key(|name| name.len())
            .map(str::to_string);
        let Some(name) = name else {
            return;
        };
        // The count first: `/commands` reads it even for a command that is
        // about to fall out of the recent window.
        *self.runs.entry(name.clone()).or_insert(0) += 1;
        self.recent.retain(|seen| seen != &name);
        self.recent.insert(0, name);
        self.recent.truncate(RECENT);
    }

    /// Return a parent path and the partial child name when the input is
    /// navigating the command tree instead of supplying a leaf's arguments.
    fn subcommand_stage(&self, rest: &str) -> Option<(String, String)> {
        let trailing_space = rest.ends_with(' ');
        let path = rest.trim_end();
        let path_name = format!("/{path}");
        // A bare line that is itself a complete, runnable command is not a
        // prefix to drill into. `/org` opens the organization overview; the
        // dropdown must not rewrite it to the first child `/org show` on Enter
        // (which needs auth and hides the local, read-only overview). Families
        // whose root has no runnable action resolve to Usage/Unknown and keep
        // their child navigation.
        let root_resolves = matches!(
            crate::tui::app::parse_line(&path_name),
            crate::tui::app::Parsed::Run(_)
                | crate::tui::app::Parsed::OrganizationOverlay
                | crate::tui::app::Parsed::Help
        );
        if !trailing_space && root_resolves {
            return None;
        }
        let path_prefix = format!("{path_name} ");
        let split = path.rsplit_once(' ');
        let parent_prefix = split.map(|(parent, _)| format!("/{parent} "));
        let (mut complete_leaf, mut path_has_children, mut parent_has_children) =
            (false, false, false);
        for item in &self.items {
            let name = display_name(&item.command);
            complete_leaf |= name.eq_ignore_ascii_case(&path_name);
            path_has_children |= name.starts_with(&path_prefix);
            if let Some(prefix) = &parent_prefix {
                parent_has_children |= name.starts_with(prefix);
            }
            if complete_leaf && path_has_children && parent_has_children {
                break;
            }
        }

        // A trailing space after a complete leaf belongs to its argument
        // picker (or closes for a zero-argument leaf).
        if trailing_space && complete_leaf {
            return None;
        }

        // Typing a complete parent drills into its immediate children.
        if !trailing_space && path_has_children {
            return Some((path.to_string(), String::new()));
        }

        // A trailing space after an intermediate family enters the next
        // level. Check this before splitting the path into parent + last word;
        // otherwise `/root child ` incorrectly reopens `root` filtered by
        // the already-completed word `child`.
        if trailing_space && !complete_leaf && path_has_children {
            return Some((path.to_string(), String::new()));
        }

        // Otherwise the last token filters the children of the preceding path.
        if let Some((parent, child)) = split.filter(|_| parent_has_children) {
            return Some((parent.to_string(), child.to_string()));
        }

        // `/root ` opens the first level when root itself is not a command.
        None
    }

    /// Recompute the root, subcommand, or argument menu from the prompt line.
    /// Cheap enough to call after every keystroke.
    pub fn sync(&mut self, input: &str, cursor: usize) {
        let chars: Vec<char> = input.chars().collect();
        let cursor = cursor.min(chars.len());
        let before: String = chars[..cursor].iter().collect();

        let Some(rest) = before.strip_prefix('/') else {
            self.close();
            return;
        };
        let typed = format!("/{rest}");
        let lower = typed.to_lowercase();
        // Capture where the highlight points before the prompt edit rebuilds the
        // list. A keystroke that merely narrows the results must not drag the
        // highlight back to the top while the same command is still on offer.
        let anchor = self.selected_name();
        let anchor_mode = self.selection_mode();
        self.subcommand_prefix.take();
        // Navigate the command tree one level at a time. At `/`, only roots
        // are shown. Typing a root (or a child prefix) opens its children;
        // once a complete leaf is followed by a space, normal argument
        // completion takes over.
        if let Some((prefix, query)) = self.subcommand_stage(rest) {
            self.argument = None;
            self.values.clear();
            self.hint = None;
            self.subcommand_prefix = Some(prefix);
            self.query = query;
            self.open = true;
            self.ranked_cache = self.compute_ranked();
            self.restore_selection(anchor, anchor_mode);
            return;
        }

        // A complete command is not a prefix to narrow. `/models` opens the
        // model browser, so listing `/models list` and `/models use` under the
        // line that was just completed says the same thing twice. A row that
        // matches exactly stays listed: it is what Enter will accept.
        let exact = self.items.iter().any(|item| item.command == typed);
        if !exact
            && !matches!(
                crate::tui::app::parse_line(&typed),
                crate::tui::app::Parsed::Unknown(_)
                    | crate::tui::app::Parsed::Usage(_)
                    | crate::tui::app::Parsed::Empty
            )
        {
            self.close();
            return;
        }

        // A line past a command name is that command's arguments. The longest
        // name wins, so `/vault history ` is history's argument and not
        // something vault's. Only a command whose usage string names arguments
        // qualifies: `/search how do retries work` is a line, not a form.
        let resolved = self
            .items
            .iter()
            .filter(|item| {
                let name = display_name(&item.command);
                lower
                    .get(..name.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(name))
                    && lower.as_bytes().get(name.len()) == Some(&b' ')
                    && !usage_of(item).trim().is_empty()
            })
            .max_by_key(|item| display_name(&item.command).chars().count());
        let Some(item) = resolved else {
            // Choosing a command. The filter is fuzzy, so a space is part of it:
            // `/vault s` narrows to `/vault status`.
            self.argument = None;
            self.values.clear();
            self.hint = None;
            self.query = rest.to_string();
            self.open = true;
            self.ranked_cache = self.compute_ranked();
            self.restore_selection(anchor, anchor_mode);
            return;
        };

        // Only argument entry needs a missing-value hint. Avoid scanning the
        // command projection while the user is browsing roots/subcommands.
        self.hint = crate::tui::overlays::command_rows::argument_hint(&lower);

        let name = display_name(&item.command);
        let after = &typed[name.len() + 1..];
        let (head_tail, word) = match after.rfind(' ') {
            Some(at) => (&after[..=at], &after[at + 1..]),
            None => ("", after),
        };
        let args = usage_args(&usage_of(item));

        // Which value this word is. A flag takes a value of its own, so only the
        // words that do not start with `-` move the position along.
        let position = head_tail
            .split_whitespace()
            .filter(|word| !word.starts_with('-'))
            .count();
        let placeholder = args
            .iter()
            .filter(|arg| !arg.trim_start_matches('[').starts_with('-'))
            .nth(position);

        let mut values: Vec<Row> = Vec::new();
        if placeholder.is_some_and(|arg| is_path(arg)) {
            values.extend(path_rows(word));
        }
        values.extend(flag_rows(&args, word));

        // Nothing to offer and nothing missing: the line is a complete command
        // with a value of its own, and the list has no business standing over
        // it.
        if values.is_empty() && placeholder.is_none() {
            self.close();
            return;
        }

        self.query = word.to_string();
        self.argument = Some(Argument {
            head: format!("{name} {head_tail}"),
            word: word.to_string(),
            needs: placeholder.map(|arg| arg.to_string()),
        });
        self.values = values;
        self.open = true;
        self.ranked_cache.clear();
        self.restore_selection(anchor, anchor_mode);
    }

    /// The filter, without the leading slash.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// How many trailing prompt characters are the active filter.
    ///
    /// `None` when there is nothing to clear. The caller owns the prompt, so it
    /// is the caller that deletes these characters when Esc is to clear the
    /// filter before it closes the list.
    pub fn filter_len(&self) -> Option<usize> {
        (self.open && !self.query.is_empty()).then(|| self.query.chars().count())
    }

    /// The rows that match the query, best first.
    pub fn matches(&self) -> Vec<&PaletteItem> {
        self.ranked_cache
            .iter()
            .map(|(index, _)| &self.items[*index])
            .collect()
    }

    /// The rows the list draws: the name, the line beside it, and where the
    /// query landed in the name so the match can be shown.
    pub fn rows(&self) -> Vec<Row> {
        if self.argument.is_some() {
            return self.values.clone();
        }
        let roots_with_children = if self.subcommand_prefix.is_none() {
            let mut roots = HashMap::new();
            for item in &self.items {
                let path = display_name(&item.command).trim_start_matches('/');
                if let Some((root, _)) = path.split_once(' ') {
                    roots.insert(root.to_string(), true);
                }
            }
            Some(roots)
        } else {
            None
        };
        self.ranked_cache
            .iter()
            .map(|(index, hits)| {
                let item = &self.items[*index];
                let root = command_root(&item.command);
                let name = if let Some(prefix) = &self.subcommand_prefix {
                    immediate_child(prefix, &item.command).unwrap_or_default()
                } else {
                    format!("/{root}")
                };
                Row {
                    name,
                    description: if self.subcommand_prefix.is_some() {
                        item.description.clone()
                    } else if roots_with_children
                        .as_ref()
                        .is_some_and(|roots| roots.contains_key(&root))
                    {
                        "Browse subcommands".to_string()
                    } else {
                        tidy_description(&item.command, &item.description)
                    },
                    hits: hits.clone(),
                }
            })
            .collect()
    }

    /// The width the command column is padded to.
    ///
    /// Taken from the current level of the command tree and capped, so a long
    /// nested path does not waste space in a short root list.
    pub fn name_column(&self) -> usize {
        if self.argument.is_some() {
            // A flag gets a column; a file path does not, because a path is as
            // long as it is and the path itself is the only thing worth
            // reading.
            return 12;
        }
        // Measure the whole level, not just the rows the filter happens to
        // leave: a column that tracks the matches shifts the descriptions
        // sideways with every keystroke, which reads as the list jittering.
        let longest = if let Some(parent) = &self.subcommand_prefix {
            self.items
                .iter()
                .filter_map(|item| immediate_child(parent, &item.command))
                .map(|name| name.chars().count())
                .max()
                .unwrap_or(0)
        } else {
            self.items
                .iter()
                .map(|item| command_root(&item.command).chars().count() + 1)
                .max()
                .unwrap_or(0)
        };
        longest.clamp(10, 22)
    }

    /// The line already spells the highlighted command out, so a one-row list
    /// would only repeat what is on the prompt.
    pub fn exact(&self) -> bool {
        if let Some(argument) = &self.argument {
            return argument.word.eq_ignore_ascii_case(&self.query);
        }
        let Some((index, _)) = self.ranked_cache.get(self.selected_index()) else {
            return false;
        };
        let (name, typed) = if let Some(parent) = &self.subcommand_prefix {
            (
                immediate_child(parent, &self.items[*index].command).unwrap_or_default(),
                self.query.as_str(),
            )
        } else {
            (
                command_root(&self.items[*index].command),
                self.query.strip_prefix('/').unwrap_or(&self.query),
            )
        };
        name.eq_ignore_ascii_case(typed)
    }

    /// The matching rows, best first, with the positions the query landed on.
    ///
    /// Matching is fuzzy — `/vhi` finds `/vault history` — because that is how a
    /// command is actually remembered: the letters, not the spelling. Ranked by
    /// where the first letter lands, then by how tightly the letters pack, so a
    /// prefix always comes before a match in the middle. Ties keep registry
    /// order, which is grouped by area already.
    ///
    /// Only the name is searched. The usage tail (`<symbol>`, `--at WHEN`) is on
    /// half the commands, so matching it would make one common word match
    /// everything and the ranking meaningless.
    fn compute_ranked(&self) -> Vec<(usize, Vec<usize>)> {
        if let Some(parent) = &self.subcommand_prefix {
            let query = self.query.to_lowercase();
            let mut grouped: Vec<(usize, Vec<usize>, String, usize)> = Vec::new();
            let mut positions: HashMap<String, usize> = HashMap::new();
            for (index, item) in self.items.iter().enumerate() {
                let Some(child) = immediate_child(parent, &item.command) else {
                    continue;
                };
                let hits = if query.is_empty() {
                    Vec::new()
                } else if let Some(hits) = fuzzy_match(&child.to_lowercase(), &query) {
                    hits
                } else {
                    continue;
                };
                let recent = self.recency(index);
                if let Some(position) = positions.get(&child).copied() {
                    grouped[position].3 = grouped[position].3.min(recent);
                } else {
                    positions.insert(child.clone(), grouped.len());
                    grouped.push((index, hits, child, recent));
                }
            }
            grouped.sort_by_key(|(_, _, _, recent)| *recent);
            return grouped
                .into_iter()
                .map(|(index, hits, _, _)| (index, hits))
                .collect();
        }

        // At the top level, collapse every command family to its root. The
        // full registry remains available as the user types into that family.
        if !self.query.contains(' ') {
            let query = self.query.to_lowercase();
            let mut roots: Vec<(usize, Vec<usize>, String, usize)> = Vec::new();
            let mut positions: HashMap<String, usize> = HashMap::new();
            for (index, item) in self.items.iter().enumerate() {
                let root = command_root(&item.command);
                let hits = if query.is_empty() {
                    Vec::new()
                } else if let Some(hits) = fuzzy_match(&format!("/{root}").to_lowercase(), &query) {
                    hits
                } else {
                    continue;
                };
                let recent = self.recency(index);
                if let Some(position) = positions.get(&root).copied() {
                    roots[position].3 = roots[position].3.min(recent);
                } else {
                    positions.insert(root.clone(), roots.len());
                    roots.push((index, hits, root, recent));
                }
            }
            roots.sort_by_key(|(_, _, root, recent)| {
                let prefix_match = !root.to_lowercase().starts_with(&query);
                (prefix_match, *recent)
            });
            return roots
                .into_iter()
                .map(|(index, hits, _, _)| (index, hits))
                .collect();
        }

        let query = self.query.to_lowercase();
        let mut hits: Vec<(usize, Vec<usize>)> = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let name = display_name(&item.command).to_lowercase();
            if let Some(found) = fuzzy_match(&name, &query) {
                hits.push((index, found));
            }
        }
        hits.sort_by_key(|(index, found)| {
            let first = found.first().copied().unwrap_or(0);
            let span = found.last().copied().unwrap_or(first).saturating_sub(first);
            (first, span, self.recency(*index))
        });
        hits
    }

    /// How recently a command was run: `0` is the last one, `usize::MAX` never.
    fn recency(&self, index: usize) -> usize {
        let name = display_name(&self.items[index].command);
        self.recent
            .iter()
            .position(|seen| seen == name)
            .unwrap_or(usize::MAX)
    }

    /// The highlighted row, clamped to the current matches.
    pub fn selected_index(&self) -> usize {
        self.selected.min(self.total().saturating_sub(1))
    }

    /// How many rows are on offer: commands, or the values an argument takes.
    pub fn total(&self) -> usize {
        if self.argument.is_some() {
            self.values.len()
        } else {
            self.ranked_cache.len()
        }
    }

    /// One row up, wrapping to the bottom: a list this short has no reason to
    /// make a person hit End before they can come back down the other side.
    pub fn select_previous(&mut self) {
        let last = self.total().saturating_sub(1);
        self.selected = match self.selected_index().checked_sub(1) {
            Some(index) => index,
            None => last,
        };
    }

    /// One row down, wrapping to the top.
    pub fn select_next(&mut self) {
        let last = self.total().saturating_sub(1);
        self.selected = if self.selected_index() >= last {
            0
        } else {
            self.selected_index() + 1
        };
    }

    pub fn page_up(&mut self) {
        self.selected = self.selected_index().saturating_sub(PAGE);
    }

    pub fn page_down(&mut self) {
        let last = self.total().saturating_sub(1);
        self.selected = (self.selected_index() + PAGE).min(last);
    }

    pub fn first(&mut self) {
        self.selected = 0;
    }

    pub fn last(&mut self) {
        self.selected = self.total().saturating_sub(1);
    }

    /// The first row to draw so the highlight stays inside `height` rows.
    ///
    /// The highlight is kept at the bottom of the window once it walks off it,
    /// which is what a list does: the row you are moving to arrives from below
    /// instead of the list jumping a page.
    pub fn window_start(&self, height: usize) -> usize {
        let height = height.max(1);
        let total = self.total();
        if total <= height {
            return 0;
        }
        let selected = self.selected_index();
        selected
            .saturating_sub(height - 1)
            .min(total.saturating_sub(height))
    }

    /// Take the highlighted command.
    ///
    /// `None` when nothing matches: the caller then treats Enter as it always
    /// did, so a half-remembered spelling is still submitted and still gets an
    /// answer. A command that takes an argument is inserted unsent, with room
    /// for the argument, instead of being rejected for the one it is missing.
    pub fn accept(&mut self) -> Option<(String, bool)> {
        if let Some(argument) = self.argument.clone() {
            let row = self.values.get(self.selected_index())?.clone();
            self.close();
            // A directory is half a path, so the line stays open on it. Anything
            // else is a whole value, and a space follows for the next one.
            let line = if row.name.ends_with('/') {
                format!("{}{}", argument.head, row.name)
            } else {
                format!("{}{} ", argument.head, row.name)
            };
            return Some((line, false));
        }
        let item = self
            .matches()
            .get(self.selected_index())
            .map(|item| (**item).clone())?;
        if let Some(parent) = &self.subcommand_prefix {
            let child = immediate_child(parent, &item.command)?;
            let path = format!("{parent} {child}");
            if has_children(&self.items, &path) {
                self.close();
                return Some((format!("/{path} "), false));
            }
        } else {
            let root = command_root(&item.command);
            if has_children(&self.items, &root) {
                self.close();
                return Some((format!("/{root} "), false));
            }
        }
        let (command, needs_more) = (insertable(&item.command), needs_arguments(&item));
        self.close();
        // The space that leaves room for the argument is part of the pick, so
        // every caller gets the same line.
        if needs_more {
            Some((format!("{command} "), false))
        } else {
            Some((command, true))
        }
    }

    fn clamp(&mut self) {
        let last = self.total().saturating_sub(1);
        if self.selected > last {
            self.selected = last;
        }
    }

    /// The displayed name of the highlighted row, before a re-filter rebuilds
    /// the list. Rows are named the same way at every level (`/vault`, `status`,
    /// or a value), so the name is a stable identity even when the representative
    /// registry item behind a collapsed root changes with recency.
    fn selected_name(&self) -> Option<String> {
        self.rows()
            .get(self.selected_index())
            .map(|row| row.name.clone())
    }

    /// Which list the highlight belongs to. Moving between roots, children and
    /// argument values is a change of list, not a re-filter: there is nothing
    /// to preserve across it, so the selection starts at the top.
    fn selection_mode(&self) -> (bool, Option<String>, Option<String>) {
        (
            self.argument.is_some(),
            self.subcommand_prefix.clone(),
            self.argument.as_ref().map(|argument| argument.head.clone()),
        )
    }

    /// Put the highlight back on the row it was on when the filter still
    /// matches it; fall back to a clamped index when that row is gone.
    fn restore_selection(
        &mut self,
        anchor: Option<String>,
        anchor_mode: (bool, Option<String>, Option<String>),
    ) {
        if anchor_mode != self.selection_mode() {
            self.selected = 0;
            return;
        }
        if let Some(name) = anchor
            && let Some(position) = self.rows().iter().position(|row| row.name == name)
        {
            self.selected = position;
            return;
        }
        self.clamp();
    }
}

/// What a command expects after its name.
///
/// The usage string lives in one of two places: in the command itself
/// (`/vault history <name>`) or, more often, at the end of the description
/// (`Every version of one belief - /vault history <name>`). Reading both keeps
/// one source of truth — the registry — and no second list to drift.
fn usage_of(item: &PaletteItem) -> String {
    let name = display_name(&item.command);
    let tail = item.command[name.len()..].trim();
    if !tail.is_empty() {
        return tail.to_string();
    }
    if let Some(at) = item.description.rfind(" - ")
        && item.description[at + 3..].starts_with(name)
    {
        return item.description[at + 3 + name.len()..].trim().to_string();
    }
    String::new()
}

/// The first command token, without its leading slash.
fn command_root(command: &str) -> String {
    display_name(command)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string()
}

/// Whether the command projection has a deeper path below `parent`.
fn has_children(items: &[PaletteItem], parent: &str) -> bool {
    let parent = format!("/{}", parent.trim_start_matches('/'));
    items
        .iter()
        .any(|item| display_name(&item.command).starts_with(&format!("{parent} ")))
}

/// The next command token after `parent`, when this item belongs under it.
fn immediate_child(parent: &str, command: &str) -> Option<String> {
    let parent = format!("/{} ", parent.trim_start_matches('/'));
    display_name(command)
        .strip_prefix(&parent)
        .and_then(|tail| tail.split_whitespace().next())
        .map(str::to_string)
}

/// The arguments a usage string names, in order: `<name>`, `[name]`, `--flag`,
/// and `[--flag VALUE]` as one argument.
fn usage_args(usage: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut open: Option<String> = None;
    for token in usage.split_whitespace() {
        match &mut open {
            Some(group) => {
                group.push(' ');
                group.push_str(token);
                if token.ends_with(']') {
                    args.push(group.clone());
                    open = None;
                }
            }
            None if token.starts_with('[') && !token.ends_with(']') => {
                open = Some(token.to_string())
            }
            None => args.push(token.to_string()),
        }
    }
    if let Some(group) = open {
        args.push(group);
    }
    args
}

/// Whether this placeholder names something on disk, where there is a list to
/// offer instead of a placeholder to read.
fn is_path(arg: &str) -> bool {
    let name = arg.trim_matches(['<', '>', '[', ']']).to_lowercase();
    matches!(
        name.as_str(),
        "file" | "files" | "path" | "dir" | "directory" | "folder" | "snapshot" | "out" | "output"
    )
}

/// Files and directories under the prefix being typed, for a path argument.
///
/// One directory, not a walk: a completion is a menu, and a menu that takes a
/// second to appear is worse than one that shows the neighbours.
fn path_rows(word: &str) -> Vec<Row> {
    let (dir, prefix) = match word.rfind(['/', '\\']) {
        Some(at) => (&word[..=at], &word[at + 1..]),
        None => ("", word),
    };
    let base = if dir.is_empty() { "." } else { dir };
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut rows: Vec<(bool, Row)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // A dotfile only when it is asked for by name: the list is a menu of
        // what is next, not a dump of everything.
        if name.starts_with('.') && !prefix.starts_with('.') {
            continue;
        }
        if !name.to_lowercase().starts_with(&prefix.to_lowercase()) {
            continue;
        }
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        rows.push((
            is_dir,
            Row {
                name: format!("{dir}{name}{}", if is_dir { "/" } else { "" }),
                description: String::new(),
                hits: Vec::new(),
            },
        ));
        if rows.len() >= 32 {
            break;
        }
    }
    // Directories first: the next keystroke usually goes into one.
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// The flags this command still takes, with the value each expects.
fn flag_rows(args: &[String], word: &str) -> Vec<Row> {
    // A value that does not start with `-` is not a flag being typed, so a flag
    // would only be in the way of it.
    if !word.is_empty() && !word.starts_with('-') {
        return Vec::new();
    }
    args.iter()
        .filter_map(|arg| {
            let inner = arg.trim_matches(['[', ']']);
            let (flag, value) = match inner.split_once(' ') {
                Some((flag, value)) => (flag, value.trim_matches(['<', '>'])),
                None => (inner, ""),
            };
            if !flag.starts_with('-') || !flag.starts_with(word) {
                return None;
            }
            Some(Row {
                name: flag.to_string(),
                description: value.to_string(),
                hits: Vec::new(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(line: &str) -> SlashMenu {
        let mut menu = SlashMenu::new();
        menu.sync(line, line.chars().count());
        menu
    }

    #[test]
    fn a_complete_root_command_is_not_rewritten_to_its_first_child() {
        // `/org` is a complete command opening the local, read-only overview;
        // the dropdown must not enter subcommand mode and rewrite it to
        // `/org show` on Enter (which needs auth). Families whose root has no
        // runnable action (e.g. `/vault`) still drill into their children.
        let menu = SlashMenu::new();
        assert_eq!(menu.subcommand_stage("org"), None);
        assert!(menu.subcommand_stage("vault").is_some());
    }

    #[test]
    fn a_slash_opens_only_main_command_roots() {
        let menu = opened("/");
        assert!(menu.is_open());
        assert_eq!(menu.query(), "");
        assert!(menu.total() < commands().len());
        assert!(menu.rows().iter().all(|row| !row.name[1..].contains(' ')));
    }

    #[test]
    fn a_command_that_was_run_is_offered_first() {
        // Recent children promote their root without flattening the subcommands.
        let mut menu = SlashMenu::new();
        menu.record("/vault status");
        menu.sync("/", 1);
        assert_eq!(menu.rows()[0].name, "/vault");

        // Running the same command again moves nothing: one entry, still first.
        menu.record("/vault status");
        assert_eq!(menu.rows()[0].name, "/vault");
        assert!(
            menu.total() < commands().len(),
            "subcommands stay collapsed"
        );
    }

    #[test]
    fn the_history_is_written_and_read_back_across_sessions() {
        let dir = std::env::temp_dir().join(format!("memnoth-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("commands.json");

        // A missing file is no history, never a failure to start.
        assert!(load_usage(&path).recent.is_empty());

        let mut menu = SlashMenu::new();
        for line in ["/vault status", "/kg status", "/vault status"] {
            menu.record(line);
        }
        save_usage(&path, &menu.usage()).unwrap();

        // A new session starts with what the last one ran: the most recent is
        // first in the dropdown, and the counts are what the browser shows.
        let mut restored = SlashMenu::new();
        restored.adopt(load_usage(&path));
        restored.sync("/", 1);
        assert_eq!(restored.rows()[0].name, "/vault");
        assert_eq!(restored.runs().get("/vault status"), Some(&2));
        assert_eq!(restored.runs().get("/kg status"), Some(&1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_list_shows_the_command_without_its_usage_tail() {
        // The name is what you pick; the tail is what you still have to answer.
        assert_eq!(
            display_name("/vault link <source> <target> --kind <kind>"),
            "/vault link"
        );
        assert_eq!(display_name("/vault asof [--at WHEN]"), "/vault asof");
        assert_eq!(display_name("/vault status"), "/vault status");
        assert_eq!(display_name("!cmd"), "!cmd");
        assert_eq!(display_name("@file"), "@file");
    }

    #[test]
    fn the_description_drops_the_usage_it_repeats() {
        assert_eq!(
            tidy_description(
                "/vault recall",
                "Search vault (FTS + graph + temporal) - /vault recall <query>"
            ),
            "Search vault (FTS + graph + temporal)"
        );
        assert_eq!(
            tidy_description("/vault status", "Vault path, size, namespace, counts"),
            "Vault path, size, namespace, counts",
            "a description with no tail is left alone"
        );
        // The tail is only dropped when it is this command's own tail.
        assert_eq!(
            tidy_description("/kg diff-impact", "Changed files - blast radius"),
            "Changed files - blast radius"
        );
    }

    #[test]
    fn subcommands_appear_after_the_main_command_is_typed() {
        let mut menu = opened("/telemetry o");
        let rows = menu.rows();
        assert!(rows.iter().any(|row| row.name == "on"));
        assert!(rows.iter().all(|row| !row.name.contains(' ')));
        assert_eq!(menu.accept(), Some(("/telemetry on".into(), true)));
    }

    #[test]
    fn nested_command_families_expand_one_level_at_a_time() {
        let mut menu = SlashMenu::new();
        menu.items = vec![
            PaletteItem {
                command: "/alpha beta gamma".into(),
                description: "A nested command".into(),
            },
            PaletteItem {
                command: "/alpha beta delta".into(),
                description: "Another nested command".into(),
            },
        ];
        menu.sync("/alpha beta ", "/alpha beta ".chars().count());
        assert!(menu.rows().iter().any(|row| row.name == "gamma"));
        assert!(menu.rows().iter().all(|row| !row.name.contains(' ')));
    }

    #[test]
    fn selecting_a_root_inserts_it_and_opens_its_children() {
        let mut menu = opened("/");
        let telemetry = menu
            .rows()
            .iter()
            .position(|row| row.name == "/telemetry")
            .expect("telemetry is a top-level command");
        for _ in 0..telemetry {
            menu.select_next();
        }
        assert_eq!(menu.accept(), Some(("/telemetry ".into(), false)));
        let children = opened("/telemetry ");
        assert!(children.rows().iter().any(|row| row.name == "on"));
        assert!(children.rows().iter().any(|row| row.name == "off"));
    }

    #[test]
    fn a_prefix_filters_roots_without_exposing_unentered_subcommands() {
        let menu = opened("/re");
        let rows = menu.rows();
        assert!(rows[0].name.starts_with("/re"), "{}", rows[0].name);
        assert!(rows.iter().all(|row| !row.name[1..].contains(' ')));
    }

    #[test]
    fn an_exact_line_needs_no_list() {
        assert!(opened("/help").exact(), "the line is the command");
        assert!(!opened("/vau").exact(), "a prefix is not the command");
    }

    #[test]
    fn the_command_column_does_not_move_with_the_query() {
        for query in ["/", "/v", "/vault", "/mcp profile"] {
            let column = opened(query).name_column();
            assert!((10..=22).contains(&column), "{query}: {column}");
        }
    }

    #[test]
    fn a_partial_word_narrows_the_list() {
        let menu = opened("/vault");
        assert!(menu.is_open());
        assert_eq!(menu.query(), "");
        let rows = menu.matches();
        assert!(!rows.is_empty());
        assert!(
            rows.len() < commands().len(),
            "only this root's subcommands are shown"
        );
        assert!(
            rows.iter().all(|item| item.command.contains("vault")),
            "every row carries the query"
        );
        assert!(
            rows[0].command.starts_with("/vault"),
            "only the selected root is shown: {}",
            rows[0].command
        );
    }

    #[test]
    fn a_question_is_not_a_command() {
        for line in ["what changed?", "!ls", "@src/main.rs", ""] {
            let menu = opened(line);
            assert!(!menu.is_open(), "{line:?} must not open the dropdown");
        }
    }

    #[test]
    fn an_argument_switches_the_list_to_what_the_command_expects() {
        let menu = opened("/vault recall ");
        assert!(menu.is_open(), "the values are what is being chosen now");
        assert!(!menu.choosing_command());
        assert_eq!(menu.needs(), Some("<query>"));
    }

    #[test]
    fn the_arrows_wrap_at_both_ends() {
        let mut menu = opened("/vault");
        assert!(menu.total() > 2);
        menu.select_previous();
        assert_eq!(
            menu.selected_index(),
            menu.total() - 1,
            "up from the first row arrives at the last"
        );
        menu.select_next();
        assert_eq!(
            menu.selected_index(),
            0,
            "and down arrives back at the first"
        );

        // Repeated presses keep cycling; no press is silently swallowed.
        for _ in 0..(2 * menu.total() + 1) {
            menu.select_next();
        }
        assert_eq!(menu.selected_index(), 1);
        menu.select_previous();
        assert_eq!(menu.selected_index(), 0);
    }

    #[test]
    fn a_page_key_moves_five_rows_and_stops_at_the_end() {
        let mut menu = opened("/");
        menu.page_down();
        assert_eq!(menu.selected_index(), 5);
        menu.page_up();
        assert_eq!(menu.selected_index(), 0);
        menu.page_up();
        assert_eq!(menu.selected_index(), 0, "the top does not wrap");
        menu.page_down();
        menu.page_down();
        menu.page_down();
        menu.last();
        menu.page_down();
        assert_eq!(menu.selected_index(), menu.total() - 1);
    }

    #[test]
    fn the_window_follows_a_selection_far_down_the_list() {
        let mut menu = opened("/");
        menu.last();
        let height = 6;
        let start = menu.window_start(height);
        let selected = menu.selected_index();
        assert!(selected >= start, "the highlight is inside the window");
        assert!(selected < start + height, "the highlight is on screen");
        assert!(
            start + height <= menu.total(),
            "the window never runs past the list"
        );
    }

    #[test]
    fn accepting_a_plain_command_asks_for_it_to_be_run() {
        let mut menu = opened("/help");
        let (line, submit) = menu.accept().expect("a row is highlighted");
        assert_eq!(line, "/help");
        assert!(submit);
        assert!(!menu.is_open());
    }

    #[test]
    fn accepting_a_subcommand_with_arguments_leaves_room_to_type_them() {
        let mut menu = opened("/vault rec");
        let recall = menu
            .rows()
            .iter()
            .position(|row| row.name == "recall")
            .expect("recall is a vault subcommand");
        for _ in 0..recall {
            menu.select_next();
        }
        let (line, submit) = menu.accept().expect("a row is highlighted");
        assert_eq!(
            line, "/vault recall ",
            "the space that leaves room for the argument is part of the pick"
        );
        assert!(!submit, "the argument is still missing");
    }

    #[test]
    fn accepting_nothing_matches_returns_nothing() {
        let mut menu = opened("/zzz");
        assert_eq!(menu.total(), 0);
        assert!(menu.accept().is_none());
        assert!(menu.is_open(), "the list stays up for a correction");
    }

    /// Roots without children so `sync` stays in the flat command list: these
    /// assertions are about the highlight, not about drilling into a family.
    fn flat_menu() -> SlashMenu {
        let mut menu = SlashMenu::new();
        menu.items = vec![
            PaletteItem {
                command: "/alpha".into(),
                description: "first".into(),
            },
            PaletteItem {
                command: "/bravo".into(),
                description: "second".into(),
            },
            PaletteItem {
                command: "/charlie".into(),
                description: "third".into(),
            },
        ];
        menu.sync("/", 1);
        menu
    }

    #[test]
    fn a_narrowing_query_keeps_the_highlighted_command() {
        let mut menu = flat_menu();
        assert_eq!(menu.total(), 3);
        menu.select_next();
        assert_eq!(menu.rows()[menu.selected_index()].name, "/bravo");

        menu.sync("/br", 3);
        assert_eq!(menu.rows()[menu.selected_index()].name, "/bravo");
        menu.sync("/bra", 4);
        assert_eq!(menu.rows()[menu.selected_index()].name, "/bravo");

        // Once the highlighted command drops out, a valid row is still selected.
        menu.sync("/ch", 3);
        assert!(menu.selected_index() < menu.total());
        assert_eq!(menu.rows()[menu.selected_index()].name, "/charlie");
    }

    #[test]
    fn a_mode_change_starts_at_the_top() {
        // A child in the subcommand list is not the same row as a value in the
        // argument list, even if both happen to sit at the same index.
        let mut menu = opened("/vault rec");
        assert!(menu.subcommand_prefix.is_some());
        menu.select_next();
        menu.sync("/vault recall ", 14);
        assert!(
            !menu.choosing_command(),
            "the line is now asking for a value"
        );
        assert_eq!(menu.selected_index(), 0);
    }

    #[test]
    fn the_same_query_keeps_the_highlight() {
        let mut menu = opened("/va");
        menu.select_next();
        let selected = menu.selected_index();
        menu.sync("/va", 3);
        assert_eq!(menu.selected_index(), selected);
    }

    #[test]
    fn the_command_column_does_not_shift_with_the_filter() {
        let mut menu = flat_menu();
        let all = menu.name_column();
        menu.sync("/br", 3);
        assert_eq!(menu.name_column(), all);
        menu.sync("/cha", 4);
        assert_eq!(menu.name_column(), all);
    }

    #[test]
    fn the_filter_reports_its_length_for_a_caller_to_clear() {
        assert_eq!(opened("/").filter_len(), None, "an empty filter is not one");
        assert_eq!(opened("/vau").filter_len(), Some(3));
        assert_eq!(
            opened("/vault recor").filter_len(),
            Some(5),
            "the partial subcommand, not the whole line"
        );
        let mut closed = opened("/vau");
        closed.close();
        assert_eq!(
            closed.filter_len(),
            None,
            "a closed list has nothing to clear"
        );
    }

    #[test]
    fn accepting_uses_the_clamped_highlight() {
        let mut menu = opened("/help");
        // A stale index must not pick a different row or refuse the pick: every
        // other view of the highlight is clamped, and Enter must agree with it.
        menu.selected = usize::MAX;
        let (line, submit) = menu.accept().expect("the clamped row is still there");
        assert_eq!(line, "/help");
        assert!(submit);
    }
}
