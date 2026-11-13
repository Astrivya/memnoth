//! `memnoth demo` answers the whole "why is this code like this?" question in
//! one offline command, and its text and `--json` must be the same answer.
//!
//! The regression test seeds the bundled scenario into a real vault: removing
//! the contradiction from the scenario has to change the output, and the dates
//! have to stay ordered. A demo whose story is hand-written twice would pass a
//! "contains a string" test and still lie to half its readers.

use std::path::PathBuf;
use std::process::Command;

use memnoth::demo::{Scenario, SideState, Story, render, seed};
use memnoth_engine::{MemoryEngine, now_ms};

fn bundled() -> Scenario {
    Scenario::bundled().expect("the bundled demo scenario parses")
}

fn seeded(scenario: &Scenario) -> Story {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    seed(&engine, scenario, now_ms()).expect("seed the demo story")
}

fn json(story: &Story) -> serde_json::Value {
    serde_json::to_value(story).expect("the story serializes")
}

/// Every string a JSON value carries, at any depth.
fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                strings(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            for field in fields.values() {
                strings(field, out);
            }
        }
        _ => {}
    }
}

#[test]
fn the_bundled_scenario_seeds_all_five_elements() {
    let story = seeded(&bundled());
    let text = render(&story);

    // 1. The decision, and 2. when it was believed.
    assert!(
        text.contains(&story.decision.belief),
        "the decision must be named:\n{text}"
    );
    assert!(
        text.contains(&story.decision.believed_at.rfc3339),
        "the believed-at date must be named:\n{text}"
    );

    // 3. What superseded it, with the reason.
    assert!(
        text.contains(&story.superseded_by.belief),
        "the superseding belief must be named:\n{text}"
    );
    assert!(
        text.contains(&story.superseded_by.reason),
        "the reason it changed must be named:\n{text}"
    );

    // 4. The source that introduced it.
    assert!(
        text.contains(&story.source_commit.entity) && text.contains(&story.source_commit.message),
        "the commit must be named:\n{text}"
    );

    // 5. Both sides of the contradiction, each with its own date.
    assert_eq!(
        story.contradiction.sides.len(),
        2,
        "the scenario is a two-sided contradiction"
    );
    for side in &story.contradiction.sides {
        assert!(
            text.contains(&side.label) && text.contains(&side.belief),
            "side {:?} must be named:\n{text}",
            side.label
        );
        assert!(
            text.contains(&side.believed_at.rfc3339),
            "side {:?} must carry its date:\n{text}",
            side.label
        );
    }
    assert!(
        story
            .contradiction
            .sides
            .iter()
            .any(|side| side.state == SideState::Revoked)
            && story
                .contradiction
                .sides
                .iter()
                .any(|side| side.state == SideState::Current),
        "a contradiction keeps both a revoked side and the current one"
    );
}

#[test]
fn text_and_json_are_the_same_answer() {
    let story = seeded(&bundled());
    let text = render(&story);
    let mut value = json(&story);
    // `vault` is filled in by the command, and `next_commands` are printed by
    // the shared text renderer rather than by the story's body.
    let object = value.as_object_mut().expect("the story is an object");
    object.remove("vault");
    object.remove("next_commands");

    let mut carried = Vec::new();
    strings(&value, &mut carried);
    assert!(!carried.is_empty(), "the JSON half must carry the story");
    for item in carried {
        assert!(
            text.contains(&item),
            "the text must render {item:?} from the same story:\n{text}"
        );
    }
}

#[test]
fn dates_stay_ordered() {
    let now = now_ms();
    let scenario = bundled();
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let story = seed(&engine, &scenario, now).expect("seed the demo story");

    let decision = story.decision.believed_at.epoch_ms;
    let superseded = story.superseded_by.at.epoch_ms;
    assert!(
        decision < superseded,
        "the correction must be later than the belief it replaced: {decision} vs {superseded}"
    );
    assert!(
        superseded <= now,
        "no part of the story may be in the future: {superseded} vs {now}"
    );
    assert!(
        story.source_commit.at.epoch_ms <= decision,
        "the commit cannot postdate the decision it introduced"
    );

    let revoked = story
        .contradiction
        .sides
        .iter()
        .find(|side| side.state == SideState::Revoked)
        .expect("one side is revoked");
    let current = story
        .contradiction
        .sides
        .iter()
        .find(|side| side.state == SideState::Current)
        .expect("one side is current");
    assert!(
        revoked.believed_at.epoch_ms < current.believed_at.epoch_ms,
        "the contradicted side must be the older belief"
    );
    assert_eq!(
        revoked
            .revoked_at
            .as_ref()
            .expect("a revoked side carries the date it was contradicted")
            .epoch_ms,
        current.believed_at.epoch_ms,
        "a side is revoked when the other side is believed"
    );
    assert!(
        current.revoked_at.is_none(),
        "the current side is not revoked"
    );
}

#[test]
fn removing_the_contradiction_changes_the_answer() {
    let mut scenario = bundled();
    let full = seeded(&scenario);

    scenario.contradiction.sides.clear();
    let without = seeded(&scenario);

    assert_ne!(
        render(&full),
        render(&without),
        "removing the contradiction must change the text"
    );
    assert_ne!(
        json(&full)["contradiction"],
        json(&without)["contradiction"],
        "removing the contradiction must change the JSON"
    );
    let text = render(&without);
    assert!(
        text.contains("nothing contradicts") || text.contains("none recorded"),
        "an absent contradiction must be stated, not omitted:\n{text}"
    );
    assert!(
        text.contains(&without.decision.belief) && text.contains(&without.superseded_by.belief),
        "the other four elements must survive:\n{text}"
    );
    assert_eq!(
        json(&without)["contradiction"]["sides"]
            .as_array()
            .expect("sides is an array")
            .len(),
        0
    );
}

#[test]
fn re_running_does_not_grow_the_history() {
    let engine = MemoryEngine::open_in_memory().expect("in-memory vault");
    let scenario = bundled();
    let now = now_ms();
    let first = seed(&engine, &scenario, now).expect("first seed");

    let rows = |table: &str| -> i64 {
        engine
            .connection()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count rows")
    };
    let entities = rows("entities");
    let relations = rows("relations");

    let second = seed(&engine, &scenario, now).expect("second seed");
    assert_eq!(rows("entities"), entities, "no duplicated entities");
    assert_eq!(rows("relations"), relations, "no duplicated relations");
    assert_eq!(
        json(&first),
        json(&second),
        "the second run must report the first run's story"
    );
}

fn fresh_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("memnoth-demo-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    home
}

fn run_demo(home: &PathBuf, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_memnoth"))
        .arg("demo")
        .args(extra)
        .env("MEMNOTH_HOME", home)
        .env_remove("MEMNOTH_VAULT")
        .output()
        .expect("run memnoth demo")
}

#[test]
fn one_command_answers_from_a_fresh_state() {
    let home = fresh_home("text");
    let out = run_demo(&home, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "decision",
        "believed at",
        "superseded by",
        "source",
        "contradiction",
        "side A",
        "side B",
    ] {
        assert!(text.contains(expected), "missing {expected:?}:\n{text}");
    }
}

#[test]
fn json_form_is_the_shared_envelope() {
    let home = fresh_home("json");
    let out = run_demo(&home, &["--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("demo --json must emit JSON");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "demo");
    let data = &envelope["data"];
    assert!(data["decision"]["believed_at"]["rfc3339"].is_string());
    assert!(data["superseded_by"]["at"]["rfc3339"].is_string());
    assert!(data["source_commit"]["entity"].is_string());
    assert_eq!(
        data["contradiction"]["sides"]
            .as_array()
            .expect("sides is an array")
            .len(),
        2
    );
}
