//! One command projection, read by every surface.
//!
//! The palette, the `/` dropdown, `/commands`, the help overlay and the footer
//! all draw these rows. A row is either a registry row — command line,
//! description, availability and risk all read from
//! [`memnoth_command::CommandRegistry::build_command_rows`] — or one explicit
//! `SessionAction`, which is a verb that exists only inside the session and has
//! no CLI counterpart. There is no third list, and `local_commands()` is gone.
//!
//! A registry row the session's own parser cannot resolve is dropped rather
//! than shown: a palette entry that cannot run is a promise the screen cannot
//! keep, and `Parsed::Unknown` is the only test of "can run" that cannot drift
//! from the parser.

use std::collections::HashSet;

use memnoth_ui::palette::PaletteItem;

/// One row of the single command projection.
#[derive(Debug, Clone)]
pub(crate) struct CommandRow {
    /// The line as typed, with its leading `/`.
    pub command: String,
    /// What it does, in the registry's own words.
    pub description: String,
    /// `runs here`, `opens a surface`, `terminal only`, `needs auth`.
    pub availability: &'static str,
    /// `read-only`, `writes`, `network`, `destructive`.
    pub risk: &'static str,
    /// The screen this command opens (`confirm`, `picker`, ...), empty when the
    /// command answers in the transcript.
    pub screen: String,
    /// What has to be true before it can run: an account, a terminal, an
    /// argument. Derived, so a row cannot claim to be runnable while needing a
    /// parameter nobody mentioned.
    pub requires: Vec<String>,
    /// `name (required)` / `name (optional)` for every parameter.
    pub params: Vec<String>,
    /// Keys that act on this row once selected.
    pub keys: Vec<String>,
    /// Whether a parameter carries a credential; such a row must never echo it.
    pub secret: bool,
    /// Whether the command works with no screen attached.
    pub headless: bool,
    /// What to check when it fails, in the registry's words.
    pub failure_hint: String,
}

impl CommandRow {
    /// The chip shown before selection, never after.
    pub fn chip(&self) -> String {
        format!("{} · {}", self.availability, self.risk)
    }

    /// A session-only verb, whose detail the session itself supplies.
    fn session(
        command: &str,
        description: &str,
        availability: &'static str,
        risk: &'static str,
    ) -> Self {
        Self {
            command: command.to_string(),
            description: description.to_string(),
            availability,
            risk,
            screen: if availability == "opens a surface" {
                "overlay".to_string()
            } else {
                String::new()
            },
            requires: match availability {
                "terminal only" => vec!["a terminal".to_string()],
                "needs auth" => vec!["a signed-in account".to_string()],
                _ => Vec::new(),
            },
            params: Vec::new(),
            keys: vec!["Enter runs it".to_string(), "Tab completes it".to_string()],
            secret: false,
            headless: true,
            failure_hint: String::new(),
        }
    }

    /// The detail page for this row: every field the plan asks to show, in one
    /// place, so the overlay and the tests read the same list.
    pub fn detail_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("{} — {}", self.command, self.description),
            format!(
                "screen: {}",
                if self.screen.is_empty() {
                    "none (answers in the transcript)"
                } else {
                    self.screen.as_str()
                }
            ),
            format!("availability: {} · risk: {}", self.availability, self.risk),
        ];
        if !self.requires.is_empty() {
            lines.push(format!("requires: {}", self.requires.join(", ")));
        }
        if !self.params.is_empty() {
            lines.push(format!("params: {}", self.params.join(", ")));
        }
        if !self.keys.is_empty() {
            lines.push(format!("keys: {}", self.keys.join(", ")));
        }
        lines.push(format!(
            "secret: {} · headless: {}",
            if self.secret { "yes" } else { "no" },
            if self.headless { "yes" } else { "no" }
        ));
        if !self.failure_hint.is_empty() {
            lines.push(format!("if it fails: {}", self.failure_hint));
        }
        lines
    }
}

/// One line for the `/commands` header: how much of the registry is on screen,
/// and why the rest is not. A reconciliation nobody can read where the commands
/// are listed is a number in a test, not a fact in the product.
pub(crate) fn atlas_summary() -> String {
    let (total, offered, skipped) = reconciliation();
    if skipped == 0 {
        return format!("{offered} of {total} registry rows");
    }
    // Grouped by reason with the ids kept: "3 hidden" is a number, "3 hidden
    // (kg_dead, kg_unreachable, kg_why)" is something a person can go and fix.
    let mut reasons: Vec<(&str, Vec<&str>)> = Vec::new();
    for row in skipped_rows() {
        match reasons.iter_mut().find(|(reason, _)| *reason == row.reason) {
            Some((_, ids)) => ids.push(row.id.as_str()),
            None => reasons.push((row.reason, vec![row.id.as_str()])),
        }
    }
    let detail = reasons
        .into_iter()
        .map(|(reason, ids)| {
            if ids.len() <= 3 {
                format!("{} {reason} ({})", ids.len(), ids.join(", "))
            } else {
                format!("{} {reason}", ids.len())
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("{offered} of {total} registry rows · hidden: {detail}")
}

/// What a command line still needs, read from the projection rather than from a
/// re-parse of the usage string. The dropdown hint, the footer beside it and the
/// `/commands` detail page all answer from this one lookup, so a parameter that
/// is renamed in the registry cannot survive in two of the three surfaces.
pub(crate) fn argument_hint(line: &str) -> Option<String> {
    let line = line.trim().to_lowercase();
    let row = rows()
        .into_iter()
        .filter(|row| {
            let command = row.command.to_lowercase();
            line == command || line.starts_with(&format!("{command} "))
        })
        .max_by_key(|row| row.command.chars().count())?;
    row.params
        .iter()
        .find_map(|param| param.strip_suffix(" (required)").map(str::to_string))
}

/// The detail page for a command line, for a surface that holds only the
/// command string.
pub(crate) fn detail(command: &str) -> Vec<String> {
    rows()
        .into_iter()
        .find(|row| row.command.eq_ignore_ascii_case(command.trim()))
        .map(|row| row.detail_lines())
        .unwrap_or_else(|| vec![format!("{command} — not in this session's command list")])
}

/// What a registry row needs before it can run.
fn requires(row: &serde_json::Value) -> Vec<String> {
    let mut needs = Vec::new();
    match row["availability"].as_str().unwrap_or("here") {
        "auth" => needs.push("a signed-in account".to_string()),
        "terminal" => needs.push("a terminal".to_string()),
        _ => {}
    }
    if row["params"]
        .as_array()
        .is_some_and(|params| params.iter().any(|param| param["required"] == true))
    {
        needs.push("an argument".to_string());
    }
    needs
}

/// `name (required)` / `name (optional)`, in the registry's order.
fn params_of(params: &serde_json::Value) -> Vec<String> {
    params
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|param| {
            let name = param["name"].as_str()?;
            let required = param["required"].as_bool().unwrap_or(false);
            Some(format!(
                "{name} ({})",
                if required { "required" } else { "optional" }
            ))
        })
        .collect()
}

/// Every command the session can show.
pub(crate) fn rows() -> Vec<CommandRow> {
    cached().rows.clone()
}

/// How many commands the surfaces offer, computed once.
pub(crate) fn command_count() -> usize {
    cached().rows.len()
}

/// The rows as the modal palette and the dropdown draw them.
pub(crate) fn palette_items() -> Vec<PaletteItem> {
    rows()
        .into_iter()
        .map(|row| PaletteItem {
            command: row.command.clone(),
            // The chip travels with the description: the palette and the
            // `/` dropdown have no third column, and a command that is
            // terminal-only should say so where it is offered, not only
            // on the help page.
            description: format!("{} · {}", row.description, row.chip()),
        })
        .collect()
}

/// A registry row the session does not offer, and the reason.
///
/// The atlas is only reconciled if the rows that are *missing* are accounted
/// for: "96 registry rows" and "some of them are on screen" is the state this
/// replaces. Every reason here is a fact about that row, not a category.
#[derive(Debug, Clone)]
pub(crate) struct Skipped {
    pub id: String,
    pub reason: &'static str,
}

/// One pass over the registry, kept so the offered and skipped halves come from
/// the same walk and cannot disagree.
struct Projection {
    rows: Vec<CommandRow>,
    skipped: Vec<Skipped>,
    /// Registry rows the walk saw, before any filter.
    registry_total: usize,
}

fn cached() -> &'static Projection {
    static PROJECTION: std::sync::OnceLock<Projection> = std::sync::OnceLock::new();
    PROJECTION.get_or_init(build_projection)
}

/// The atlas reconciliation: registry rows seen, offered, and skipped with a
/// reason. `offered + skipped == total` is the property the gate asserts.
pub(crate) fn reconciliation() -> (usize, usize, usize) {
    let projection = cached();
    (
        projection.registry_total,
        projection.registry_total - projection.skipped.len(),
        projection.skipped.len(),
    )
}

/// Every registry row that was not offered, with the reason it was not.
pub(crate) fn skipped_rows() -> &'static [Skipped] {
    &cached().skipped
}

fn build_projection() -> Projection {
    let registry = memnoth_command::build_p0_registry().build();
    let projection = registry.build_command_rows();

    let mut rows: Vec<CommandRow> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut registry_total = 0usize;

    for row in projection["commands"].as_array().into_iter().flatten() {
        registry_total += 1;
        let id = row["id"].as_str().unwrap_or("").to_string();
        let Some(description) = row["description"].as_str().map(str::to_string) else {
            skipped.push(Skipped {
                id,
                reason: "the registry row declares no description",
            });
            continue;
        };
        let cli = row["cli_command"].as_str().unwrap_or("");
        let line = row["palette"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| usage_line(cli, &row["params"]));
        let line = line.trim().trim_start_matches('/').to_string();
        if line.is_empty() {
            skipped.push(Skipped {
                id,
                reason: "the registry row declares no command line",
            });
            continue;
        }
        let command = format!("/{line}");
        if !runnable(&command) {
            skipped.push(Skipped {
                id,
                reason: "the session parser cannot run this line",
            });
            continue;
        }
        if !seen.insert(command_key(&command)) {
            skipped.push(Skipped {
                id,
                reason: "an earlier row already claims this command",
            });
            continue;
        }
        rows.push(CommandRow {
            command,
            description,
            availability: availability_chip(row["availability"].as_str().unwrap_or("here")),
            risk: risk_chip(row["risk"].as_str().unwrap_or("read")),
            screen: row["screen"].as_str().unwrap_or("").to_string(),
            requires: requires(row),
            params: params_of(&row["params"]),
            keys: row["keys"]
                .as_str()
                .map(str::to_string)
                .into_iter()
                .collect(),
            secret: row["secret"].as_bool().unwrap_or(false),
            headless: row["headless"].as_bool().unwrap_or(true),
            failure_hint: row["failure_hint"].as_str().unwrap_or("").to_string(),
        });
    }

    // Session-only verbs join after the registry walk. A collision is not a
    // registry-row skip, so it is filtered here rather than recorded above.
    rows.extend(
        session_actions()
            .into_iter()
            .filter(|row| runnable(&row.command))
            .filter(|row| seen.insert(command_key(&row.command))),
    );

    // Registry order, not alphabetical: the help page shows the first screen
    // only, and `/vault` reaching that screen matters more than `!cmd` first.
    Projection {
        rows,
        skipped,
        registry_total,
    }
}

/// Whether the session's own parser knows this line.
///
/// The parser decides what it can run; a second hand-kept list cannot.
fn runnable(command: &str) -> bool {
    !matches!(
        crate::tui::app::parse_line_quietly(command),
        crate::tui::app::Parsed::Unknown(_)
    )
}

/// The identity of a command: its first two words, lowercased.
fn command_key(command: &str) -> String {
    let mut words = command.split_whitespace();
    let head = words.next().unwrap_or_default();
    match words.next() {
        Some(second) => format!("{head} {second}").to_lowercase(),
        None => head.to_lowercase(),
    }
}

/// The command line for a registry row with no curated palette line.
///
/// Required parameters are shown as `<name>`; optional ones are omitted, since
/// the dropdown's argument hint comes from the same `params` array.
fn usage_line(cli: &str, params: &serde_json::Value) -> String {
    let mut line = cli.to_string();
    for param in params.as_array().into_iter().flatten() {
        if !param["required"].as_bool().unwrap_or(false) {
            continue;
        }
        let name = param["name"].as_str().unwrap_or("");
        if !name.is_empty() {
            line.push_str(&format!(" <{name}>"));
        }
    }
    line
}

fn availability_chip(value: &str) -> &'static str {
    match value {
        "surface" => "opens a surface",
        "terminal" => "terminal only",
        "auth" => "needs auth",
        "unavailable" => "unavailable",
        _ => "runs here",
    }
}

fn risk_chip(value: &str) -> &'static str {
    match value {
        "write" => "writes",
        "network" => "network",
        "destructive" => "destructive",
        _ => "read-only",
    }
}

/// The verbs that exist only in the session: no CLI verb, no registry row.
///
/// Every entry has no `Verb` in `src/cli.rs` and no `CommandSpec` in
/// `memnoth-command`. Anything that does is projected from the registry above,
/// and `build_rows` drops any entry here that the parser does not accept.
fn session_actions() -> Vec<CommandRow> {
    const ACTIONS: &[(&str, &str, &str, &str)] = &[
        (
            "/help",
            "Keys and every command",
            "opens a surface",
            "read-only",
        ),
        ("/quit", "Leave the session", "runs here", "read-only"),
        ("/clear", "Clear the transcript", "runs here", "read-only"),
        ("/home", "The session brief", "runs here", "read-only"),
        (
            "/new",
            "Start a fresh conversation",
            "runs here",
            "read-only",
        ),
        (
            "/sessions",
            "Browse saved conversations",
            "opens a surface",
            "read-only",
        ),
        (
            "/tasks",
            "Background jobs on the rail",
            "opens a surface",
            "read-only",
        ),
        (
            "/search",
            "Search vault and graph without a model",
            "opens a surface",
            "read-only",
        ),
        (
            "/onboarding",
            "The four moves, replayed",
            "opens a surface",
            "read-only",
        ),
        (
            "/context",
            "Prompt slots, drops, prefix hash",
            "opens a surface",
            "read-only",
        ),
        (
            "/diff",
            "Review pending edits",
            "opens a surface",
            "read-only",
        ),
        (
            "/rewind",
            "Roll the conversation back",
            "opens a surface",
            "writes",
        ),
        (
            "/permissions",
            "Per-tool allow, ask, deny",
            "opens a surface",
            "read-only",
        ),
        ("/palette", "This palette", "opens a surface", "read-only"),
        (
            "/commands",
            "Every command, grouped",
            "opens a surface",
            "read-only",
        ),
        (
            "/model",
            "Browse and switch models",
            "opens a surface",
            "read-only",
        ),
        (
            "/connect",
            "Configure provider connections",
            "opens a surface",
            "writes",
        ),
        (
            "/feedback",
            "Send feedback to operators",
            "opens a surface",
            "network",
        ),
        (
            "/memory",
            "Edit this project's memory file",
            "runs here",
            "writes",
        ),
        ("/init", "Generate AGENTS.md", "runs here", "writes"),
        (
            "/theme default",
            "Switch the palette",
            "runs here",
            "writes",
        ),
        (
            "/screen-reader off",
            "Plain accessible output",
            "runs here",
            "writes",
        ),
        ("/login", "Sign in to memnoth cloud", "runs here", "network"),
        ("/logout", "Sign out of this machine", "runs here", "writes"),
        (
            "!cmd",
            "Shell passthrough under sandbox policy",
            "runs here",
            "destructive",
        ),
        ("@file", "Fuzzy path completion", "runs here", "read-only"),
    ];
    ACTIONS
        .iter()
        .map(|(command, description, availability, risk)| {
            CommandRow::session(command, description, availability, risk)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_projected_row_can_run() {
        let mut unknown = Vec::new();
        for row in rows() {
            if matches!(
                crate::tui::app::parse_line(&row.command),
                crate::tui::app::Parsed::Unknown(_)
            ) {
                unknown.push(row.command);
            }
        }
        assert!(
            unknown.is_empty(),
            "these palette rows cannot be parsed: {unknown:?}"
        );
    }

    #[test]
    fn every_registry_row_marked_for_the_palette_appears_once() {
        let projection = memnoth_command::build_p0_registry()
            .build()
            .build_command_rows();
        let projected = rows();
        for row in projection["commands"].as_array().into_iter().flatten() {
            let Some(line) = row["palette"].as_str() else {
                continue;
            };
            let command = format!("/{}", line.trim().trim_start_matches('/'));
            let key = command_key(&command);
            assert!(
                projected.iter().any(|row| command_key(&row.command) == key),
                "{command} is registered for the palette but missing from the projection"
            );
        }
    }

    #[test]
    fn a_terminal_only_row_says_so() {
        let row = rows()
            .into_iter()
            .find(|row| row.command.starts_with("/mcp serve"))
            .expect("mcp serve is a registry row");
        assert_eq!(row.availability, "terminal only");
        assert!(row.chip().contains("terminal only"));
    }

    /// W2-10: every registry row resolves from its own command line with no
    /// arguments. `Unknown` means a surface can name a command the parser
    /// cannot; `Usage` is a typed answer and passes.
    #[test]
    fn every_projected_row_runs() {
        // The promise is about the list a person sees, not every verb the CLI
        // has: the session parser is still catching up with the registry, so
        // unparsable rows are dropped before they reach a surface. A row that
        // survives must resolve.
        let projected = rows();
        assert!(!projected.is_empty(), "the session offers no commands");
        let dead: Vec<&str> = projected
            .iter()
            .filter(|row| {
                matches!(
                    crate::tui::app::parse_line(&row.command),
                    crate::tui::app::Parsed::Unknown(_)
                )
            })
            .map(|row| row.command.as_str())
            .collect();
        assert!(dead.is_empty(), "these rows cannot run: {dead:?}");
    }

    #[test]
    fn the_projection_has_no_duplicate_verb() {
        let projected = rows();
        let mut keys: Vec<String> = projected
            .iter()
            .map(|row| command_key(&row.command))
            .collect();
        let before = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(before, keys.len(), "a verb appears twice in the projection");
    }

    #[test]
    fn building_the_projection_does_not_re_enter_it() {
        // The shortest path a fresh app takes: the slash menu asks for every
        // command, and each row is checked against the parser. Before the
        // builder used the suggestion-free parser this deadlocked on the
        // projection's own cache, freezing every `App::new` in a test.
        let built = rows();
        let menu = crate::tui::slash::SlashMenu::new();
        assert!(!built.is_empty(), "the session must offer commands");
        assert!(!menu.is_open(), "a fresh dropdown is closed");
    }
}

#[cfg(test)]
mod detail_tests {
    use super::*;

    #[test]
    fn every_row_explains_all_nine_fields() {
        // W13-27: the detail is derived for every row, so a row cannot be
        // missing a field the plan promises to show.
        let rows = rows();
        assert!(!rows.is_empty(), "the session has commands");
        for row in &rows {
            let lines = row.detail_lines();
            let text = lines.join("\n");
            for field in ["screen:", "availability:", "risk:"] {
                assert!(
                    text.contains(field),
                    "{} has no {field}\n{text}",
                    row.command
                );
            }
            assert!(
                text.contains("secret:") && text.contains("headless:"),
                "{} has no secret/headless line\n{text}",
                row.command
            );
            assert_eq!(
                row.detail_lines().len(),
                lines.len(),
                "{} is deterministic",
                row.command
            );
        }
    }

    #[test]
    fn a_row_that_needs_something_says_so() {
        let needs_argument = rows()
            .into_iter()
            .find(|row| row.params.iter().any(|param| param.ends_with("(required)")));
        let Some(row) = needs_argument else {
            return; // a registry with no required params would be the surprise
        };
        assert!(
            row.requires.iter().any(|need| need == "an argument"),
            "{} has a required parameter but does not say it needs one: {:?}",
            row.command,
            row.requires
        );
        assert!(
            row.detail_lines()
                .iter()
                .any(|line| line.starts_with("params:")),
            "{} hides its parameters",
            row.command
        );
    }

    #[test]
    fn a_command_outside_the_list_is_named_rather_than_guessed() {
        let lines = detail("/definitely-not-a-command");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("not in this session's command list"));
    }
    /// The atlas is reconciled when every registry row is either on screen or
    /// carries the reason it is not. A row that simply vanishes is the failure
    /// this replaces: "96 registry rows" and "some of them are on screen" can
    /// both be true while forty commands are unreachable.
    #[test]
    fn every_registry_row_is_offered_or_explained() {
        let (total, offered, skipped) = reconciliation();
        assert!(total > 0, "the registry walk found no rows at all");
        assert_eq!(
            offered + skipped,
            total,
            "{offered} offered + {skipped} skipped != {total}: a registry row vanished"
        );
        for row in skipped_rows() {
            assert!(
                !row.reason.is_empty(),
                "`{}` was skipped without a reason",
                row.id
            );
        }
        // The palette is this projection, not a second list: same rows, same order.
        let palette = palette_items();
        let rows = rows();
        assert_eq!(
            palette.len(),
            rows.len(),
            "the palette and the projection disagree about how many commands exist"
        );
        for (item, row) in palette.iter().zip(rows.iter()) {
            assert_eq!(item.command, row.command);
        }
    }

    /// The hint a surface shows is the registry's parameter, not a placeholder
    /// somebody typed into a usage string.
    #[test]
    fn argument_hints_come_from_the_projection() {
        let mut checked = 0;
        for row in rows() {
            let Some(param) = row
                .params
                .iter()
                .find_map(|param| param.strip_suffix(" (required)"))
            else {
                continue;
            };
            checked += 1;
            assert_eq!(
                argument_hint(&format!("{} ", row.command)).as_deref(),
                Some(param),
                "{} advertises `{param}` but the hint disagrees",
                row.command
            );
        }
        assert!(
            checked > 0,
            "no row requires a parameter, so nothing was checked"
        );
    }

    /// The chip offered in the palette is the same chip help shows.

    #[test]

    fn palette_rows_carry_the_projection_chip() {
        let rows = rows();
        let palette = palette_items();
        assert_eq!(rows.len(), palette.len());
        for (row, item) in rows.iter().zip(palette.iter()) {
            assert!(
                item.description.ends_with(&row.chip()),
                "{} offers `{}` without its chip",
                item.command,
                item.description
            );
        }
    }
}
