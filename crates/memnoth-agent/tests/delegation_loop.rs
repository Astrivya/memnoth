//! Gate G10 / M5: the delegation loop.
//!
//! These tests exercise the five observed gate checks: three agents return
//! summaries rather than transcripts, a `/kill` at a tool boundary leaves git
//! clean, a restart surfaces an orphan instead of a completion, an over-budget
//! run is refused before the request, and a bad spec names its file and line.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use memnoth_agent::result::AgentResult;
use memnoth_agent::spec::SpecSet;
use memnoth_agent::{
    AgentPhase, AgentRun, AgentSpec, Budget, BudgetLedger, CancelToken, Cost, Delegator, ErrorCode,
    Evidence, Executor, Exit, Finding, Layer, ProbeOutcome, ResultRef, RunIdentity, SpawnScope,
    Step, last_phases, orphan_candidates, recover, replay_run_journal,
};

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("mn-g10-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("scratch dir");
    path
}

fn run_git(dir: &PathBuf, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be available for the worktree-hygiene check");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn porcelain(dir: &PathBuf) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .expect("git status");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn three_agents_return_summaries_not_transcripts() {
    let dir = scratch("three-agents");
    let mut tree = Delegator::with_caps(dir.join("logs"), 1, 8, 16);
    let root = tree.root(Some("s1"), "lead", "ship it").expect("root");
    let tasks = [("a.rs", "worker"), ("b.rs", "worker"), ("c.rs", "critic")];
    let mut children = Vec::new();
    for (file, agent) in tasks {
        let child = tree
            .spawn(Some(&root.agent_id), Some("s1"), agent, file)
            .expect("child");
        children.push((child.agent_id, agent.to_string(), file.to_string()));
    }

    let mut parent_messages: Vec<String> = Vec::new();
    let mut references: Vec<ResultRef> = Vec::new();
    let mut changed = Vec::new();
    {
        let mut executor = Executor::without_hooks(&mut tree);
        for (id, agent, file) in &children {
            let result: AgentResult = executor
                .run(id, &[Step::with_detail("write_file", file)], |_, _| Ok(()))
                .expect("child result");
            assert_eq!(result.exit, Exit::Completed);
            // In a real run the model fills these; set them to model the three
            // disjoint files the gate asks for.
            let mut result = result;
            result.summary = format!("{agent} finished {file}");
            result.changed_files = vec![file.clone()];
            changed.push(file.clone());
            let reference = result.reference(
                id.clone(),
                agent.clone(),
                dir.join("logs")
                    .join(format!("{id}.jsonl"))
                    .to_string_lossy(),
            );
            parent_messages.push(reference.summary.clone());
            references.push(reference);
        }
    }

    assert_eq!(parent_messages.len(), 3);
    assert_eq!(references.len(), 3);
    changed.sort();
    changed.dedup();
    assert_eq!(changed.len(), 3, "the three agents touched disjoint files");

    // The parent's message list holds summaries and references only. It does
    // not contain any of the child-log event kinds: the transcripts stay in
    // their own JSONL files for a Trace pane to open on demand.
    let wire = serde_json::to_string(&references).expect("serialize references");
    for kind in ["tool.use", "run.start", "run.done"] {
        assert!(!wire.contains(kind), "parent got a transcript row: {kind}");
    }
    for (id, _, _) in &children {
        let events = tree.read_log(id).expect("child log");
        assert!(
            events.iter().any(|event| event.event == "tool.use"),
            "the transcript exists in the child's own log"
        );
    }
    assert!(references.iter().all(|r| !r.summary.is_empty()));
    assert_eq!(
        references
            .iter()
            .map(|r| &r.record)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "each summary names a distinct record"
    );
}

#[test]
fn kill_at_a_tool_boundary_leaves_git_clean() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("git is unavailable; skipping the worktree-hygiene check");
        return;
    }

    let root = scratch("kill-git");
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, &["init", "-q"]);
    run_git(&repo, &["config", "user.email", "test@example.invalid"]);
    run_git(&repo, &["config", "user.name", "Gate Test"]);
    std::fs::write(repo.join("tracked.txt"), "base\n").expect("tracked file");
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "base"]);
    assert_eq!(porcelain(&repo), "", "baseline must be clean");

    let mut tree = Delegator::with_caps(root.join("logs"), 1, 4, 4);
    let parent = tree.root(None, "lead", "root").expect("root");
    let child = tree
        .spawn(Some(&parent.agent_id), None, "worker", "edit")
        .expect("child");
    let token = tree
        .arm_cancellation(&child.agent_id, CancelToken::new())
        .expect("arm token");

    let kill = token.clone();
    let mut writes = 0;
    let error = Executor::without_hooks(&mut tree)
        .run(
            &child.agent_id,
            &[Step::new("read_tracked"), Step::new("write_new")],
            |step, _| {
                if step.tool == "read_tracked" {
                    // `/kill` arrives while the first tool is in flight. It is
                    // cooperative: the next boundary is where work stops.
                    kill.cancel();
                } else {
                    writes += 1;
                    std::fs::write(repo.join("injected.txt"), "bad").expect("would-be write");
                }
                Ok(())
            },
        )
        .expect_err("the next boundary must refuse the cancelled run");

    assert!(error.to_string().contains("cancelled"), "{error}");
    assert_eq!(writes, 0, "the write after /kill must never run");
    assert!(!repo.join("injected.txt").exists());
    assert_eq!(porcelain(&repo), "", "/kill left the parent worktree dirty");
}

#[test]
fn a_restart_shows_orphaned_not_completed() {
    let dir = scratch("restart");
    let journal = dir.join("sessions/s1/runs.jsonl");
    let mut run = AgentRun::enqueue("r-1", Some("s1"), None, "worker", &journal).expect("enqueue");
    run.start().expect("start");
    run.wait_for_approval("write the file").expect("approval");
    run.approve().expect("approved");
    // The process vanishes here: no completion edge is ever appended.

    let recorded = RunIdentity {
        pid: 4242,
        process_start_time: 12_345,
        boot_id: "boot-1".to_string(),
    };
    let mut identities = BTreeMap::new();
    identities.insert("r-1".to_string(), Some(recorded));
    let report = recover(&journal, &identities, |_| None).expect("recovery");

    assert_eq!(report.orphaned, vec!["r-1"]);
    assert!(report.adopted.is_empty());
    assert_eq!(
        last_phases(&journal).expect("phases").get("r-1"),
        Some(&AgentPhase::Orphaned),
        "a vanished run is orphaned, never completed"
    );
    assert!(orphan_candidates(&journal).expect("candidates").is_empty());
    let rows = replay_run_journal(&journal).expect("journal");
    assert_eq!(rows.last().expect("last row").phase, AgentPhase::Orphaned);
    assert!(!rows.iter().any(|row| row.phase == AgentPhase::Completed));
}

#[test]
fn a_proven_process_is_adopted_not_orphaned() {
    let dir = scratch("adopt");
    let journal = dir.join("runs.jsonl");
    let mut run = AgentRun::enqueue("r-2", None, None, "worker", &journal).expect("enqueue");
    run.start().expect("start");

    let recorded = RunIdentity {
        pid: 99,
        process_start_time: 777,
        boot_id: "boot-2".to_string(),
    };
    let mut identities = BTreeMap::new();
    identities.insert("r-2".to_string(), Some(recorded.clone()));
    // The observer sees the same triple: deterministic adopt.
    let report =
        recover(&journal, &identities, |observed| Some(observed.clone())).expect("recovery");
    assert_eq!(report.adopted, vec!["r-2"]);
    assert!(report.orphaned.is_empty());
    assert_eq!(
        last_phases(&journal).expect("phases").get("r-2"),
        Some(&AgentPhase::Running)
    );
}

#[test]
fn an_over_budget_run_is_refused_before_the_request() {
    let dir = scratch("budget");
    let mut tree = Delegator::new(dir.join("logs"));
    let child = tree.root(None, "worker", "two calls").expect("child");
    let mut ledger = BudgetLedger::new(Budget {
        tokens: Some(1_000),
        usd: None,
    });
    let mut called = Vec::new();

    let result = Executor::without_hooks(&mut tree)
        .run_budgeted(
            &child.agent_id,
            &[Step::new("first"), Step::new("second")],
            &mut ledger,
            |step| {
                if step.tool == "second" {
                    (2_000, 0.0)
                } else {
                    (10, 0.0)
                }
            },
            |step, _| {
                called.push(step.tool.clone());
                Ok(())
            },
        )
        .expect("a budget refusal is a result, not a panic");

    assert_eq!(called, vec!["first"], "the over-budget tool never ran");
    assert_eq!(result.exit, Exit::BudgetRefused);
    assert_eq!(
        result.cost,
        Cost {
            tokens: 10,
            usd: 0.0
        }
    );
    assert_eq!(ledger.usage(), (10, 0.0));
}

#[test]
fn a_bad_spec_names_its_file_and_line() {
    let dir = scratch("bad-spec");
    let path = dir.join("agents/bad.md");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("agents dir");
    // `maxTurn` is a typo for `maxTurns` and sits on line 4.
    let text = "---\nname: scout\ndescription: Explore the code\nmaxTurn: 7\n---\nDo it.\n";
    std::fs::write(&path, text).expect("spec file");

    let error = AgentSpec::from_file(&path).expect_err("unknown key must refuse");
    assert_eq!(error.code, ErrorCode::UnknownField);
    assert_eq!(error.file.as_deref(), Some(path.as_path()));
    assert_eq!(error.line, Some(4));
    let rendered = error.to_string();
    assert!(
        rendered.contains(&format!("{}:4", path.display())),
        "{rendered}"
    );
    assert!(rendered.contains("E_AGENT_UNKNOWN_FIELD"), "{rendered}");
    assert!(rendered.contains("maxturn"), "{rendered}");
}

#[test]
fn a_spawn_cycle_and_an_outside_whitelist_are_both_refused() {
    let mut set = SpecSet::new();
    for (name, child) in [("a", "b"), ("b", "a")] {
        let text = format!(
            "---\nname: {name}\ndescription: Do {name}\ntools: [Agent({child})]\n---\nbody\n"
        );
        set.insert(
            Layer::Project,
            AgentSpec::from_markdown(None, &text).expect("spec"),
        )
        .expect("insert");
    }
    let cycle = set.check_cycles().expect_err("a <-> b is a cycle");
    assert_eq!(cycle.code, ErrorCode::Cycle);
    assert!(cycle.message.contains("a -> b -> a"), "{cycle}");

    let dir = scratch("whitelist");
    let mut tree = Delegator::new(dir.join("logs"));
    let root = tree.root(None, "scout", "map").expect("root");
    let whitelist = SpawnScope::Only(vec!["worker".to_string()]);
    let refused = tree
        .spawn_allowed(&root.agent_id, None, "critic", "review", &whitelist)
        .expect_err("critic is not in the whitelist");
    assert!(refused.to_string().contains("spawn whitelist"), "{refused}");
}

#[test]
fn unevidenced_findings_are_dropped_before_the_parent_sees_them() {
    let dir = scratch("findings");
    let mut tree = Delegator::new(dir.join("logs"));
    let child = tree.root(None, "explore", "look").expect("child");
    let mut result = Executor::without_hooks(&mut tree)
        .run(&child.agent_id, &[], |_, _| Ok(()))
        .expect("result");
    result.findings = vec![
        Finding {
            claim: "the handler is here".to_string(),
            evidence: Evidence::Path {
                path: "src/router.rs".to_string(),
                line: 42,
            },
            confidence: 0.9,
        },
        Finding {
            claim: "vibes".to_string(),
            evidence: Evidence::Path {
                path: String::new(),
                line: 0,
            },
            confidence: 0.5,
        },
    ];
    let dropped = result.drop_unevidenced_findings();
    assert_eq!(dropped.len(), 1);
    assert_eq!(dropped[0].code, ErrorCode::FindingUnevidenced);
    assert_eq!(dropped[0].code_str(), "E_FINDING_UNEVIDENCED");
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].claim, "the handler is here");

    // A missing process identity can never be "alive" by accident.
    let probe = memnoth_agent::OrphanProbe::new();
    assert_eq!(probe.probe_run(), ProbeOutcome::Orphaned);
}
