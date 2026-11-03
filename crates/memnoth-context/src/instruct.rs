//! Instructions: who said it, how strongly, and whether it should be said at all.
//!
//! Every harness has an instruction file, most have several, and none of them agree on precedence.
//! The measured consequence is in the config-file study (100 repos, 207 smells): 62% of the smells
//! are rules that restate a linter, 42% are bloat, 35% leak skills. Adding a fifth place to write
//! rules does not fix that; a merge with an explicit precedence order and a linter does.
//!
//! Precedence, strongest first:
//!
//! `Enterprise > Team > Project > User > Session`
//!
//! Within one scope, the later rule wins — that is how a file corrects itself. Across scopes the
//! higher scope wins even if the lower one is newer, because a managed policy is not a suggestion
//! that a project file may overwrite.
//!
//! Duplicates are collapsed by normalised text (case and whitespace insensitive) and the losers
//! are reported, so `memnoth context lint` can show the user which of their four files is dead
//! weight. Lint-equivalent rules are dropped and reported too: a formatter is better at formatting
//! than a sentence in a markdown file is at describing the formatter.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Where a rule came from, in precedence order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Managed policy shipped by an organisation. Cannot be overridden.
    Enterprise,
    /// Shared with a team.
    Team,
    /// Committed with the repository.
    Project,
    /// One developer's machine.
    User,
    /// Typed during this session.
    Session,
}

impl Scope {
    /// Higher wins. The numbers are only an ordering; they are not persisted.
    pub fn precedence(self) -> u8 {
        match self {
            Scope::Enterprise => 5,
            Scope::Team => 4,
            Scope::Project => 3,
            Scope::User => 2,
            Scope::Session => 1,
        }
    }

    /// The file a scope is conventionally read from, if it has one.
    pub fn conventional_file(self) -> Option<&'static str> {
        match self {
            Scope::Enterprise => Some("enterprise-policy.md"),
            Scope::Team => Some("TEAM.md"),
            Scope::Project => Some("AGENTS.md"),
            Scope::User => Some("~/.memnoth/AGENTS.md"),
            Scope::Session => None,
        }
    }
}

/// Whether a rule is a real instruction or a restatement of tooling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleKind {
    Rule,
    Lint,
}

/// One instruction, with enough provenance to explain a decision later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub text: String,
    pub scope: Scope,
    /// Where it was read from. A path, or `"session"` for typed rules.
    pub source: String,
    /// 1-based line in that source, so a report can point at it.
    pub line: usize,
    pub kind: RuleKind,
}

/// Why a rule is not in the effective set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// A higher-precedence rule says the same thing.
    Superseded,
    /// An identical rule in the same scope appeared first.
    Duplicate,
    /// It restates a formatter or linter.
    LintLeak,
}

/// A rule that lost, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dropped {
    pub rule: Rule,
    pub reason: DropReason,
}

/// The effective instruction set and the audit trail of what it excluded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstructionSet {
    pub rules: Vec<Rule>,
    pub dropped: Vec<Dropped>,
}

impl InstructionSet {
    /// Render the effective rules as the Project layer's block.
    ///
    /// Stable by construction: `merge` has already ordered the rules, and this adds no heading,
    /// counter or timestamp, so the same files always produce the same bytes — which is what the
    /// prefix hash depends on.
    pub fn render(&self) -> String {
        self.rules
            .iter()
            .map(|rule| format!("- {}", rule.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Read instruction files and merge them.
///
/// Files are read in the order given. Callers should pass strongest scope first for readability,
/// but the merge does not depend on it: precedence comes from [`Scope`], never from position.
pub fn load_from(files: &[(Scope, PathBuf)]) -> std::io::Result<InstructionSet> {
    let mut layers: Vec<(Scope, Vec<Rule>)> = Vec::with_capacity(files.len());
    for (scope, path) in files {
        // A missing instruction file is a normal state — most projects have no TEAM.md and most
        // machines have no enterprise policy. Only a real read error (permissions, bad encoding)
        // is worth failing the load over.
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let source = path.display().to_string();
        layers.push((*scope, parse(&text, *scope, &source)));
    }
    Ok(merge(layers))
}

/// Parse one instruction document.
///
/// Bullets (`- `, `* `, `+ `) and numbered items (`1. `) are rules; so is a plain non-empty line,
/// because half the real files in the wild are prose. Headings, fences and HTML comments are
/// structure, not instructions.
pub fn parse(text: &str, scope: Scope, source: &str) -> Vec<Rule> {
    // Two passes, in this order: a document that uses bullets is a list, and its prose is
    // explanation. Treating every paragraph of an `AGENTS.md` as a rule is how one file becomes
    // 316 rules and the twelve that matter stop being attended to — the measured bloat failure
    // (42% of config smells) reproduced by our own parser. A prose-only file still works: if the
    // document has no bullets at all, its non-structural lines are the rules.
    let mut bullets: Vec<Rule> = Vec::new();
    let mut plain: Vec<Rule> = Vec::new();
    let mut in_fence = false;
    // A wrapped bullet is one rule, not four. Markdown writers wrap at 100 columns, and without
    // this the first line of every long rule reads as a complete instruction — which is how a
    // "do not merge other branches" rule becomes "do not merge".
    let mut open_bullet: Option<usize> = None;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with("```") {
            in_fence = !in_fence;
            open_bullet = None;
            continue;
        }
        if in_fence {
            continue;
        }
        if line.is_empty() {
            open_bullet = None;
            continue;
        }
        if line.starts_with('#')
            || line.starts_with('|')
            || line.starts_with('>')
            || line.starts_with("<!--")
            || line.starts_with("-->")
        {
            open_bullet = None;
            continue;
        }
        let (body, is_bullet) = split_marker(line);
        let body = body.trim();
        if body.is_empty() || is_rule_continuation(body) {
            continue;
        }

        // An indented line that is not a new bullet continues the previous one.
        let indented = raw.len() > line.len();
        if !is_bullet
            && indented
            && let Some(at) = open_bullet
        {
            bullets[at].text.push(' ');
            bullets[at].text.push_str(body);
            continue;
        }

        let kind = if lint_leak(body).is_some() {
            RuleKind::Lint
        } else {
            RuleKind::Rule
        };
        let rule = Rule {
            text: body.to_string(),
            scope,
            source: source.to_string(),
            line: index + 1,
            kind,
        };
        if is_bullet {
            open_bullet = Some(bullets.len());
            bullets.push(rule);
        } else {
            open_bullet = None;
            plain.push(rule);
        }
    }
    if bullets.is_empty() { plain } else { bullets }
}

/// A horizontal rule is structure, not an instruction (and `---` would otherwise become one).
fn is_rule_continuation(body: &str) -> bool {
    body.chars()
        .all(|c| matches!(c, '-' | '*' | '_' | '=' | ' '))
}

/// Strip a list marker, reporting whether there was one.
fn split_marker(line: &str) -> (&str, bool) {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return (rest, true);
        }
    }
    // `12. text` — a numbered list. Only the first dot, and only if everything before it is
    // digits, so `1.2.3 is the version` is not mistaken for a list item.
    if let Some((head, rest)) = line.split_once(". ")
        && !head.is_empty()
        && head.chars().all(|c| c.is_ascii_digit())
    {
        return (rest, true);
    }
    (line, false)
}

#[allow(dead_code)]
fn strip_marker(line: &str) -> &str {
    split_marker(line).0
}

/// Merge layers into one effective set.
///
/// Rules are emitted strongest scope first, then by source path and line, so two runs over the
/// same files produce the same order — which is what lets the compiled prompt be byte-stable.
pub fn merge(layers: Vec<(Scope, Vec<Rule>)>) -> InstructionSet {
    let mut ordered: Vec<(usize, Rule)> = Vec::new();
    for (_, rules) in layers {
        for rule in rules {
            let index = ordered.len();
            ordered.push((index, rule));
        }
    }

    let mut winners: BTreeMap<String, (usize, Rule)> = BTreeMap::new();
    let mut dropped: Vec<Dropped> = Vec::new();

    for (index, rule) in ordered {
        if rule.kind == RuleKind::Lint {
            dropped.push(Dropped {
                rule,
                reason: DropReason::LintLeak,
            });
            continue;
        }
        let key = normalise(&rule.text);
        let existing = winners
            .get(&key)
            .map(|(seen_at, existing)| (*seen_at, existing.scope));
        match existing {
            None => {
                winners.insert(key, (index, rule));
            }
            Some((seen_at, existing_scope)) => {
                // Stronger scope wins; within one scope, the later rule wins. Never the other
                // way round: a project file may refine a team rule only by being more specific
                // in different words, not by silently overwriting the same sentence.
                let incoming_wins =
                    (rule.scope.precedence(), index) > (existing_scope.precedence(), seen_at);
                let reason = if existing_scope == rule.scope {
                    DropReason::Duplicate
                } else {
                    DropReason::Superseded
                };
                if incoming_wins {
                    if let Some((_, old)) = winners.insert(key, (index, rule)) {
                        dropped.push(Dropped { rule: old, reason });
                    }
                } else {
                    dropped.push(Dropped { rule, reason });
                }
            }
        }
    }

    let mut rules: Vec<Rule> = winners.into_values().map(|(_, rule)| rule).collect();
    rules.sort_by(|a, b| {
        b.scope
            .precedence()
            .cmp(&a.scope.precedence())
            .then(a.source.cmp(&b.source))
            .then(a.line.cmp(&b.line))
    });
    dropped.sort_by(|a, b| {
        b.rule
            .scope
            .precedence()
            .cmp(&a.rule.scope.precedence())
            .then(a.rule.source.cmp(&b.rule.source))
            .then(a.rule.line.cmp(&b.rule.line))
    });

    InstructionSet { rules, dropped }
}

fn normalise(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', ';', ',', ':'])
        .to_lowercase()
}

/// If a rule is really a formatter command in disguise, name the tool.
pub fn lint_leak(text: &str) -> Option<&'static str> {
    const MULTI: [&str; 6] = [
        "cargo fmt",
        "cargo clippy",
        "npm run lint",
        "yarn lint",
        "pnpm lint",
        "pre-commit",
    ];
    const SINGLE: [&str; 9] = [
        "rustfmt", "prettier", "eslint", "gofmt", "black", "ruff", "pylint", "tslint", "biome",
    ];

    let lower = text.to_lowercase();
    for marker in MULTI {
        if lower.contains(marker) {
            return Some(marker);
        }
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    SINGLE
        .iter()
        .find(|marker| words.contains(*marker))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(text: &str, scope: Scope, line: usize) -> Rule {
        Rule {
            text: text.to_string(),
            scope,
            source: "x.md".to_string(),
            line,
            kind: RuleKind::Rule,
        }
    }

    #[test]
    fn an_enterprise_rule_beats_a_newer_project_rule() {
        let merged = merge(vec![
            (Scope::Project, vec![rule("use tabs", Scope::Project, 99)]),
            (
                Scope::Enterprise,
                vec![rule("use spaces", Scope::Enterprise, 1)],
            ),
        ]);
        // Different text, so both survive — precedence only decides conflicts on the same text.
        assert_eq!(merged.rules.len(), 2);
        assert_eq!(merged.rules[0].scope, Scope::Enterprise);
    }

    #[test]
    fn the_same_rule_from_two_scopes_keeps_the_stronger_copy() {
        let merged = merge(vec![
            (
                Scope::Project,
                vec![rule("Never force-push to main", Scope::Project, 4)],
            ),
            (
                Scope::Enterprise,
                vec![rule("never force-push to main", Scope::Enterprise, 2)],
            ),
        ]);
        assert_eq!(merged.rules.len(), 1);
        assert_eq!(merged.rules[0].scope, Scope::Enterprise);
        assert_eq!(merged.dropped.len(), 1);
        assert_eq!(merged.dropped[0].reason, DropReason::Superseded);
    }

    #[test]
    fn a_later_line_in_the_same_scope_supersedes_an_earlier_one() {
        // Same text, written twice: the later line is the correction. A file that says
        // "use tabs" on line 1 and the same sentence on line 9 is not two rules.
        let merged = merge(vec![(
            Scope::Project,
            vec![
                rule("run tests before committing", Scope::Project, 1),
                rule("Run tests before committing.", Scope::Project, 8),
            ],
        )]);
        assert_eq!(merged.rules.len(), 1);
        assert_eq!(merged.rules[0].line, 8, "the later line is the live one");
        assert!(matches!(
            merged.dropped[0].reason,
            DropReason::Duplicate | DropReason::Superseded
        ));
    }

    #[test]
    fn load_from_skips_a_missing_file_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-context-missing-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("AGENTS.md");
        std::fs::write(&present, "- be brief\n").unwrap();

        let set = load_from(&[
            (Scope::Enterprise, dir.join("enterprise-policy.md")),
            (Scope::Project, present),
        ])
        .expect("a missing file is not an error");
        assert_eq!(set.rules.len(), 1);
        assert_eq!(set.rules[0].text, "be brief");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_identical_rule_in_two_files_is_reported_once() {
        let a = Rule {
            source: "a.md".into(),
            ..rule("no unwraps in library code", Scope::Project, 1)
        };
        let b = Rule {
            source: "b.md".into(),
            ..rule("No  unwraps in  library code.", Scope::Project, 2)
        };
        let merged = merge(vec![(Scope::Project, vec![a, b])]);
        assert_eq!(merged.rules.len(), 1);
        assert_eq!(merged.dropped.len(), 1);
        assert_eq!(merged.dropped[0].reason, DropReason::Duplicate);
    }

    #[test]
    fn a_formatter_command_is_dropped_as_lint_leakage() {
        // Through `parse`, which is the path that decides `RuleKind`; building `Rule` literals
        // would skip the linter and the test would assert nothing about production behaviour.
        let merged = merge(vec![(
            Scope::Project,
            parse(
                "- Always run cargo fmt before committing\n- Prefer small functions\n",
                Scope::Project,
                "AGENTS.md",
            ),
        )]);
        assert_eq!(merged.rules.len(), 1);
        assert_eq!(merged.rules[0].text, "Prefer small functions");
        assert_eq!(merged.dropped[0].reason, DropReason::LintLeak);
    }

    #[test]
    fn parsing_strips_bullets_numbers_and_structure() {
        let text = "\
# Rules

- Use `start_line` for big files
* Never guess a path
1. Run the tests
12. Do not commit secrets
1.2.3 is a version, not a list

```sh
cargo fmt
```
";
        let parsed = parse(text, Scope::Project, "AGENTS.md");
        let texts: Vec<&str> = parsed.iter().map(|r| r.text.as_str()).collect();
        assert!(texts.contains(&"Use `start_line` for big files"));
        assert!(texts.contains(&"Never guess a path"));
        assert!(texts.contains(&"Run the tests"));
        assert!(texts.contains(&"Do not commit secrets"));
        assert!(
            !texts.contains(&"1.2.3 is a version, not a list"),
            "a file that uses bullets is a list; its prose is explanation, not a rule"
        );
        assert!(!texts.contains(&"Rules"));
    }

    #[test]
    fn a_wrapped_bullet_is_one_rule() {
        let parsed = parse(
            "- Never merge another lane's branch, and do not\n  cherry-pick code from it either.\n- be brief\n",
            Scope::Project,
            "AGENTS.md",
        );
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert!(
            parsed[0].text.contains("cherry-pick"),
            "the continuation belongs to the rule: {parsed:?}"
        );
        assert!(parsed[0].text.starts_with("Never merge"));
    }

    #[test]
    fn a_prose_only_file_still_yields_rules() {
        let parsed = parse(
            "Use tabs.\nRun the tests before committing.\n",
            Scope::Project,
            "CONVENTIONS.md",
        );
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert_eq!(parsed[0].text, "Use tabs.");
    }

    #[test]
    fn precedence_is_strongest_first_and_independent_of_input_order() {
        let forward = merge(vec![
            (Scope::Enterprise, vec![rule("e", Scope::Enterprise, 1)]),
            (Scope::Project, vec![rule("p", Scope::Project, 1)]),
            (Scope::Session, vec![rule("s", Scope::Session, 1)]),
        ]);
        let backward = merge(vec![
            (Scope::Session, vec![rule("s", Scope::Session, 1)]),
            (Scope::Project, vec![rule("p", Scope::Project, 1)]),
            (Scope::Enterprise, vec![rule("e", Scope::Enterprise, 1)]),
        ]);
        let order: Vec<&str> = forward.rules.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(order, vec!["e", "p", "s"]);
        assert_eq!(forward.rules, backward.rules);
    }

    #[test]
    fn merge_is_idempotent_for_the_same_files() {
        let files = vec![
            (Scope::Project, vec![rule("a", Scope::Project, 1)]),
            (Scope::User, vec![rule("b", Scope::User, 1)]),
        ];
        assert_eq!(merge(files.clone()), merge(files));
    }

    #[test]
    fn load_from_reads_each_file_at_its_scope() {
        let dir = std::env::temp_dir().join(format!(
            "memnoth-context-instruct-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("AGENTS.md");
        let user = dir.join("USER.md");
        std::fs::write(&project, "- run the tests\n- be brief\n").unwrap();
        std::fs::write(&user, "- run the tests\n- use my name\n").unwrap();

        let set = load_from(&[(Scope::Project, project), (Scope::User, user)]).unwrap();
        let texts: Vec<&str> = set.rules.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts.len(), 3);
        assert!(texts.contains(&"be brief"));
        assert!(texts.contains(&"use my name"));
        assert_eq!(
            set.rules[0].scope,
            Scope::Project,
            "project outranks user in the output order"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lint_leak_matches_words_not_substrings() {
        assert_eq!(lint_leak("run prettier on save"), Some("prettier"));
        assert_eq!(lint_leak("no blacklist entries"), None);
        assert_eq!(lint_leak("use cargo clippy"), Some("cargo clippy"));
        assert_eq!(lint_leak("keep functions small"), None);
    }
}
