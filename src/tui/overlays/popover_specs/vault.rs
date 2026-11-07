//! Windows for `vault …` answers.
//!
//! Every row here is a field the verb already put in its JSON payload, and every
//! action is a line the CLI really accepts — `vault history <name>`,
//! `vault asof --at <iso> --name <name>`, `vault merge <a> --into <b> --reason …`.
//! A window that offered `vault show` would be worse than the table it replaced.
//!
//! What stays text: `vault remember`, `vault link`, `vault forget`. Those answer
//! with the one thing that changed, and a modal around one line is a box that has
//! to be closed before work continues.

use serde_json::Value;

use crate::tui::overlays::popover::{
    PopoverRow, PopoverSpec, follow_buttons, list, present, string,
};

/// The bespoke spec for a `vault …` answer, or `None` for a command this module
/// does not own.
pub(crate) fn spec(command: &str, data: &Value) -> Option<PopoverSpec> {
    match command {
        "vault.recall" => Some(recall(data)),
        "vault.history" => Some(history(data)),
        "vault.timeline" | "vault.audit" => Some(activity(data, command)),
        "vault.duplicates" => Some(duplicates(data)),
        "vault.asof" => Some(asof(data)),
        "vault.top" => Some(top(data)),
        "vault.explain" => Some(explain(data)),
        "vault.graph" => Some(graph(data)),
        "vault.stats" => Some(stats(data)),
        _ => None,
    }
}

/// The first line of a long sentence, clipped, because a row is one line.
fn one_line(text: &str, max: usize) -> String {
    let first = text.lines().next().unwrap_or("").trim();
    if first.chars().count() <= max {
        return first.to_string();
    }
    let cut: String = first.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// `2.13` when the score is a number, nothing when it is not.
fn score(item: &Value) -> Option<String> {
    item.get("score")
        .and_then(Value::as_f64)
        .map(|score| format!("{score:.2}"))
}

/// A confidence as two decimals, for the rows that carry one.
fn confidence(item: &Value) -> Option<String> {
    item.get("confidence")
        .and_then(Value::as_f64)
        .map(|value| format!("confidence {value:.2}"))
}

fn recall(data: &Value) -> PopoverSpec {
    let hits = list(data, "hits");
    let rows = hits
        .iter()
        .map(|hit| {
            let name = string(hit, "name");
            let mut parts = vec![string(hit, "kind")];
            parts.extend(score(hit));
            if let Some(summary) = present(hit, "summary") {
                parts.push(one_line(&summary, 56));
            }
            let detail = parts
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let mut row = PopoverRow::new(name.clone(), detail);
            if !name.is_empty() {
                row = row.running(format!("/vault history {name}"));
            }
            row
        })
        .collect::<Vec<_>>();

    let query = string(data, "query");
    let mut spec = PopoverSpec::new(
        format!("Recall ({})", rows.len()),
        if query.is_empty() {
            "vault search".to_string()
        } else {
            format!("vault search for {query:?}")
        },
    )
    .rows(rows);
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

fn history(data: &Value) -> PopoverSpec {
    let name = string(data, "name");
    let versions = list(data, "versions");
    let rows = versions
        .iter()
        .map(|version| {
            let id = version
                .get("entity_id")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let via = present(version, "via").unwrap_or_else(|| "name".to_string());
            let from = present(version, "valid_from_iso")
                .or_else(|| present(version, "valid_from"))
                .unwrap_or_default();
            let window = match present(version, "valid_to_iso") {
                Some(to) => format!("{from} → {to}"),
                None => format!("{from} → now"),
            };
            let mut detail = window;
            if let Some(reason) = present(version, "change_reason") {
                detail.push_str(&format!(" · {reason}"));
            }
            let mut row = PopoverRow::new(format!("#{id} {via}"), detail);
            // Every version has an instant, and `asof` is the verb that answers
            // "what did we believe then" — so Enter goes there instead of
            // restating the row.
            if !from.is_empty() && !name.is_empty() {
                row = row.running(format!("/vault asof --at {from} --name {name}"));
            }
            row
        })
        .collect::<Vec<_>>();

    let title = if name.is_empty() {
        "History".to_string()
    } else {
        format!("History of {name} ({})", rows.len())
    };
    let mut spec = PopoverSpec::new(title, "each version, and the instant it began").rows(rows);
    if versions.is_empty() {
        spec = spec.note("no entity by that name has ever been recorded");
    }
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

/// `vault timeline` and `vault audit` answer with the same feed under two ids, so
/// they share one window rather than two that can drift apart.
fn activity(data: &Value, command: &str) -> PopoverSpec {
    let facts = list(data, "facts");
    let observations = list(data, "observations");
    // `vault audit` calls the same array `entries`.
    let entries = if facts.is_empty() {
        list(data, "entries")
    } else {
        facts
    };

    let mut rows = Vec::new();
    if !entries.is_empty() {
        rows.push(PopoverRow::section(format!("entities ({})", entries.len())));
        for fact in entries {
            let name = string(fact, "name");
            let kind = present(fact, "kind").unwrap_or_else(|| "unknown".to_string());
            let from = present(fact, "valid_from_iso")
                .or_else(|| present(fact, "valid_from"))
                .unwrap_or_default();
            let window = match present(fact, "valid_to_iso") {
                Some(to) => format!("{from} → {to}"),
                None => format!("{from} → now"),
            };
            // The same three states the verb's own text computes: closed by a
            // newer version is `superseded`, closed for another reason is
            // `closed`, and an open window is `active`.
            let state = if present(fact, "superseded_by").is_some() {
                "superseded"
            } else if fact.get("valid_to").is_some_and(|value| !value.is_null()) {
                "closed"
            } else {
                "active"
            };
            let mut detail = format!("[{kind}] {window} · {state}");
            if let Some(summary) = present(fact, "summary") {
                detail.push_str(&format!(" · {}", one_line(&summary, 40)));
            }
            let mut row = PopoverRow::new(name.clone(), detail);
            if !name.is_empty() {
                row = row.running(format!("/vault history {name}"));
            }
            rows.push(row);
        }
    }

    if !observations.is_empty() {
        rows.push(PopoverRow::section(format!(
            "observations ({})",
            observations.len()
        )));
        for observation in observations {
            let entity = string(observation, "entity");
            let kind = present(observation, "kind").unwrap_or_else(|| "fact".to_string());
            let at = present(observation, "valid_from_iso")
                .or_else(|| present(observation, "valid_from"))
                .unwrap_or_default();
            let content = present(observation, "content")
                .map(|text| one_line(&text, 48))
                .unwrap_or_default();
            let mut row = PopoverRow::new(
                entity.clone(),
                format!("[{kind}] {at} · {content}").trim_end().to_string(),
            );
            if !entity.is_empty() {
                row = row.running(format!("/vault explain {entity}"));
            }
            rows.push(row);
        }
    }

    let subtitle = if command == "vault.audit" {
        "chronological feed of vault activity"
    } else {
        "what the vault recorded, newest first"
    };
    let mut spec = PopoverSpec::new(
        format!(
            "Activity ({})",
            rows.iter().filter(|row| !row.header).count()
        ),
        subtitle,
    )
    .rows(rows);
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

fn duplicates(data: &Value) -> PopoverSpec {
    let groups = list(data, "groups");
    let rows = groups
        .iter()
        .map(|group| {
            let folded = string(group, "folded");
            let entities = group
                .get("entities")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let names: Vec<String> = entities
                .iter()
                .map(|entity| string(entity, "name"))
                .collect();
            let detail = if names.is_empty() {
                "no members".to_string()
            } else {
                names.join(" | ")
            };
            let mut row = PopoverRow::new(folded, detail);
            // `vault merge` needs a source, a target and a reason; the rows supply
            // the two names, so the reason is the only thing a person adds.
            if names.len() >= 2 {
                row = row.running(format!(
                    "/vault merge {} --into {} --reason duplicate",
                    names[0], names[1]
                ));
            }
            row
        })
        .collect::<Vec<_>>();

    let mut spec = PopoverSpec::new(
        format!("Duplicate candidates ({})", rows.len()),
        "names that fold together, as of the instant asked for",
    )
    .rows(rows);
    if let Some(at) = present(data, "at_iso") {
        spec = spec.note(format!("as of {at}"));
    }
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

fn asof(data: &Value) -> PopoverSpec {
    let entities = list(data, "entities");
    let rows = entities
        .iter()
        .map(|entity| {
            let name = string(entity, "name");
            let mut parts = vec![string(entity, "kind")];
            parts.extend(confidence(entity));
            if let Some(summary) = present(entity, "summary") {
                parts.push(one_line(&summary, 56));
            }
            let detail = parts
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let mut row = PopoverRow::new(name.clone(), detail);
            if !name.is_empty() {
                row = row.running(format!("/vault history {name}"));
            }
            row
        })
        .collect::<Vec<_>>();

    let title = match present(data, "name") {
        Some(name) => format!("As of then: {name}"),
        None => format!("As of then ({})", rows.len()),
    };
    let mut spec = PopoverSpec::new(title, "what the vault believed at that instant").rows(rows);
    if let (Some(count), Some(total)) = (
        data.get("count").and_then(Value::as_i64),
        data.get("total").and_then(Value::as_i64),
    ) && total > count
    {
        spec = spec.note(format!("showing {count} of {total} live then"));
    }
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

fn top(data: &Value) -> PopoverSpec {
    let entries = list(data, "entries");
    let rows = entries
        .iter()
        .map(|entry| {
            let name = string(entry, "name");
            let relations = entry.get("relations").and_then(Value::as_i64).unwrap_or(0);
            let detail = format!(
                "{} · {} relation(s)",
                present(entry, "kind").unwrap_or_default(),
                relations
            );
            let mut row = PopoverRow::new(name.clone(), detail);
            if !name.is_empty() {
                row = row.running(format!("/vault explain {name}"));
            }
            row
        })
        .collect::<Vec<_>>();

    let mut spec = PopoverSpec::new(
        format!("Top entities ({})", rows.len()),
        "ranked by the relations that reach them",
    )
    .rows(rows);
    if let Some(at) = present(data, "at_iso") {
        spec = spec.note(format!("as of {at}"));
    }
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

fn explain(data: &Value) -> PopoverSpec {
    let entity = data.get("entity").cloned().unwrap_or(Value::Null);
    let name = string(&entity, "name");
    let mut rows = Vec::new();

    let mut head = vec![string(&entity, "kind")];
    head.extend(confidence(&entity));
    if let Some(summary) = present(&entity, "summary") {
        head.push(one_line(&summary, 56));
    }
    let head_detail = head
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if !name.is_empty() {
        rows.push(
            PopoverRow::new(name.clone(), head_detail).running(format!("/vault history {name}")),
        );
    }

    let observations = list(data, "observations");
    if !observations.is_empty() {
        rows.push(PopoverRow::section(format!(
            "observations ({})",
            observations.len()
        )));
        for observation in observations {
            let kind = present(observation, "kind").unwrap_or_else(|| "fact".to_string());
            let at = present(observation, "valid_from_iso").unwrap_or_default();
            let content = present(observation, "content")
                .map(|text| one_line(&text, 52))
                .unwrap_or_default();
            rows.push(PopoverRow::new(
                format!("[{kind}] {at}"),
                content.trim_end().to_string(),
            ));
        }
    }

    let outgoing = list(data, "outgoing");
    if !outgoing.is_empty() {
        rows.push(PopoverRow::section(format!(
            "outgoing ({})",
            outgoing.len()
        )));
        for relation in outgoing {
            let target = string(relation, "target");
            let kind = present(relation, "kind").unwrap_or_default();
            let confidence = relation
                .get("confidence")
                .and_then(Value::as_f64)
                .map(|value| format!(" · {value:.2}"))
                .unwrap_or_default();
            let mut row = PopoverRow::new(format!("→ {target}"), format!("{kind}{confidence}"));
            if !target.is_empty() {
                row = row.running(format!("/vault explain {target}"));
            }
            rows.push(row);
        }
    }

    let incoming = list(data, "incoming");
    if !incoming.is_empty() {
        rows.push(PopoverRow::section(format!(
            "incoming ({})",
            incoming.len()
        )));
        for relation in incoming {
            let source = string(relation, "source");
            let kind = present(relation, "kind").unwrap_or_default();
            let mut row = PopoverRow::new(format!("← {source}"), kind);
            if !source.is_empty() {
                row = row.running(format!("/vault explain {source}"));
            }
            rows.push(row);
        }
    }

    let mut spec = PopoverSpec::new(
        if name.is_empty() {
            "Entity".to_string()
        } else {
            format!("Entity: {name}")
        },
        "everything recorded about one belief",
    )
    .rows(rows);
    if let Some(aliases) = data.get("aliases").and_then(Value::as_array) {
        let names: Vec<String> = aliases
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !names.is_empty() {
            spec = spec.note(format!("also known as {}", names.join(", ")));
        }
    }
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

fn graph(data: &Value) -> PopoverSpec {
    let edges = list(data, "edges");
    let rows = edges
        .iter()
        .map(|edge| {
            let source = string(edge, "source");
            let target = string(edge, "target");
            let kind = present(edge, "kind").unwrap_or_default();
            let mut row = PopoverRow::new(format!("{source} → {target}"), kind);
            if !target.is_empty() {
                row = row.running(format!("/vault explain {target}"));
            }
            row
        })
        .collect::<Vec<_>>();

    let root = present(data, "root").unwrap_or_else(|| "the vault".to_string());
    let spec = PopoverSpec::new(
        format!("Graph around {root}"),
        "relations within the depth that was asked for",
    )
    .rows(rows);
    let nodes = data.get("node_count").and_then(Value::as_i64).unwrap_or(0);
    let edge_count = data.get("edge_count").and_then(Value::as_i64).unwrap_or(0);
    let mut note = format!("{nodes} node(s) · {edge_count} edge(s)");
    if data
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        note.push_str(" · truncated — raise --depth or narrow the entity");
    }
    spec.note(note)
}

fn stats(data: &Value) -> PopoverSpec {
    let rows = vec![
        PopoverRow::new(
            "entities",
            format!(
                "{} current · {} total",
                data.get("entities_current")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                data.get("entities_total")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            ),
        ),
        PopoverRow::new(
            "relations",
            format!(
                "{} current · {} total",
                data.get("relations_current")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                data.get("relations_total")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            ),
        ),
        PopoverRow::new(
            "observations",
            data.get("observations_total")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .to_string(),
        ),
        PopoverRow::new(
            "bytes",
            data.get("bytes")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .to_string(),
        ),
    ];

    let mut spec = PopoverSpec::new("Vault stats", "what this vault holds, by count").rows(rows);
    let growth = list(data, "growth");
    if !growth.is_empty() {
        spec = spec.note(format!(
            "{} day(s) of growth recorded — `vault stats --growth` prints the series",
            growth.len()
        ));
    }
    for button in follow_buttons(data) {
        spec = spec.button(button);
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recall_rows_are_entities_and_enter_asks_for_history() {
        let data = json!({
            "query": "auth",
            "hits": [
                { "id": 3, "name": "auth", "kind": "decision", "score": 0.91, "summary": "uses httpOnly cookies" },
                { "id": 4, "name": "session", "kind": "concept", "score": 0.5 }
            ],
            "count": 2
        });
        let spec = spec("vault.recall", &data).expect("a window");
        assert_eq!(spec.title, "Recall (2)");
        assert_eq!(spec.rows[0].title, "auth");
        assert_eq!(
            spec.rows[0].detail,
            "decision · 0.91 · uses httpOnly cookies"
        );
        assert_eq!(
            spec.rows[0].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/vault history auth".into()
            ))
        );
        // A hit with no summary says less instead of inventing one.
        assert_eq!(spec.rows[1].detail, "concept · 0.50");
    }

    #[test]
    fn a_version_row_names_the_instant_it_began() {
        let data = json!({
            "name": "auth",
            "versions": [
                { "entity_id": 7, "valid_from": 1_700_000_000_000i64,
                  "valid_from_iso": "2023-11-14T22:13:20Z", "via": "name",
                  "change_reason": "contradicted by the RFC" }
            ]
        });
        let spec = spec("vault.history", &data).expect("a window");
        assert_eq!(spec.title, "History of auth (1)");
        assert_eq!(spec.rows[0].title, "#7 name");
        assert!(spec.rows[0].detail.contains("2023-11-14T22:13:20Z → now"));
        assert!(spec.rows[0].detail.contains("contradicted by the RFC"));
        assert_eq!(
            spec.rows[0].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/vault asof --at 2023-11-14T22:13:20Z --name auth".into()
            ))
        );
    }

    #[test]
    fn a_duplicate_group_offers_the_merge_line_the_verb_prints() {
        let data = json!({
            "groups": [{
                "folded": "auth",
                "entities": [{ "name": "auth" }, { "name": "Auth" }]
            }],
            "count": 1,
            "at_iso": "2026-01-01T00:00:00Z"
        });
        let spec = spec("vault.duplicates", &data).expect("a window");
        assert_eq!(spec.title, "Duplicate candidates (1)");
        assert_eq!(spec.rows[0].detail, "auth | Auth");
        assert_eq!(
            spec.rows[0].action,
            Some(crate::tui::overlays::popover::PopoverAction::Run(
                "/vault merge auth --into Auth --reason duplicate".into()
            ))
        );
    }

    #[test]
    fn a_graph_names_its_counts_and_says_when_it_was_cut() {
        let data = json!({
            "root": "auth",
            "edges": [{ "source": "auth", "kind": "uses", "target": "session" }],
            "node_count": 12,
            "edge_count": 11,
            "truncated": true
        });
        let spec = spec("vault.graph", &data).expect("a window");
        assert_eq!(spec.rows[0].title, "auth → session");
        let note = spec.note.expect("a note");
        assert!(note.contains("12 node(s)"));
        assert!(note.contains("truncated"));
    }

    /// A command this module does not own must fall through to the generic
    /// window, not get a near-empty one from here.
    #[test]
    fn an_unknown_id_is_not_claimed() {
        assert!(spec("kg.refs", &json!({ "references": [] })).is_none());
        assert!(spec("vault.remember", &json!({ "name": "auth" })).is_none());
    }
}
