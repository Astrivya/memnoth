//! ALG20 policy-evaluation acceptance tests.
//!
//! Pure, in-process checks over `memnoth::policy_engine`: no network, no model,
//! no filesystem. Every requirement in the audit row is exercised: deny
//! precedence, group DAG inheritance, explainability, cache invalidation,
//! fail-closed-on-malformed, and diamond-graph (two-path) correctness.

use memnoth::policy_engine::{
    Action, AttrOp, AttrValue, Condition, Decision, Evaluator, Mode, Policy, PolicyBuilder,
    Request, Rule, Target,
};

fn command_request(subject: &str, cmd: &str) -> Request {
    Request::new(subject)
        .with_action(Action::Execute)
        .with_command(cmd)
}

#[test]
fn deny_beats_allow_regardless_of_declaration_order() {
    // The allow rule is declared first; deny must still win.
    let policy = PolicyBuilder::new()
        .member("devs", "alice")
        .rule(Rule::new(
            "allow-cargo",
            "devs",
            Target::CommandPrefix("cargo".into()),
            Mode::Allow,
        ))
        .rule(Rule::new(
            "deny-cargo-publish",
            "devs",
            Target::CommandPrefix("cargo publish".into()),
            Mode::Forbid,
        ))
        .build();
    let mut engine = Evaluator::new(policy);

    let decision = engine.evaluate(&command_request("alice", "cargo publish --token xyz"));
    assert_eq!(decision.mode, Mode::Forbid);
    assert_eq!(decision.rule_id, "deny-cargo-publish");

    // A non-denied command under the same prefix still gets the allow.
    let ok = engine.evaluate(&command_request("alice", "cargo test --all"));
    assert_eq!(ok.mode, Mode::Allow);
    assert_eq!(ok.rule_id, "allow-cargo");

    // Reversing declaration order cannot let the allow win the tie-break:
    // mode precedence is applied before declaration index.
    let reversed = PolicyBuilder::new()
        .member("devs", "alice")
        .rule(Rule::new(
            "deny-cargo-publish",
            "devs",
            Target::CommandPrefix("cargo publish".into()),
            Mode::Forbid,
        ))
        .rule(Rule::new(
            "allow-cargo",
            "devs",
            Target::CommandPrefix("cargo".into()),
            Mode::Allow,
        ))
        .build();
    let mut engine = Evaluator::new(reversed);
    assert_eq!(
        engine
            .evaluate(&command_request("alice", "cargo publish"))
            .rule_id,
        "deny-cargo-publish"
    );
}

#[test]
fn deny_beats_allow_on_file_globs_too() {
    let policy = PolicyBuilder::new()
        .rule(Rule::new(
            "allow-rs",
            "alice",
            Target::FileGlob("src/**/*.rs".into()),
            Mode::Allow,
        ))
        .rule(Rule::new(
            "deny-env",
            "alice",
            Target::FileGlob("**/*.env".into()),
            Mode::Forbid,
        ))
        .build();
    let mut engine = Evaluator::new(policy);

    // `**/` matches zero directories, so this hits both patterns and deny wins.
    let d = engine.evaluate(
        &Request::new("alice")
            .with_action(Action::Write)
            .with_path("src/config.env"),
    );
    assert_eq!(d.mode, Mode::Forbid);
    assert_eq!(d.rule_id, "deny-env");

    let ok = engine.evaluate(
        &Request::new("alice")
            .with_action(Action::Read)
            .with_path("src/deep/nested/main.rs"),
    );
    assert_eq!(ok.mode, Mode::Allow);
    assert_eq!(ok.rule_id, "allow-rs");
}

#[test]
fn group_inheritance_applies_ancestor_rules() {
    // all > senior > devs, with alice a direct member of devs.
    let policy = PolicyBuilder::new()
        .member("devs", "alice")
        .edge("senior", "devs")
        .edge("all", "senior")
        .rule(Rule::new(
            "deploy-senior",
            "senior",
            Target::CommandPrefix("deploy".into()),
            Mode::Allow,
        ))
        .rule(Rule::new(
            "read-all",
            "all",
            Target::CommandPrefix("repo status".into()),
            Mode::Allow,
        ))
        .build();
    let mut engine = Evaluator::new(policy);

    assert_eq!(
        engine
            .evaluate(&command_request("alice", "deploy prod"))
            .rule_id,
        "deploy-senior"
    );
    assert_eq!(
        engine
            .evaluate(&command_request("alice", "repo status"))
            .rule_id,
        "read-all"
    );
    // A non-member does not inherit.
    assert_eq!(
        engine.evaluate(&command_request("bob", "deploy prod")).mode,
        Mode::Prompt // default
    );
}

#[test]
fn explanation_names_the_deciding_rule() {
    let policy = PolicyBuilder::new()
        .member("devs", "alice")
        .rule(Rule::new(
            "no-rm-rf",
            "devs",
            Target::CommandPrefix("rm -rf".into()),
            Mode::Forbid,
        ))
        .build();
    let mut engine = Evaluator::new(policy);

    let d: Decision = engine.evaluate(&command_request("alice", "rm -rf /"));
    assert_eq!(d.rule_id, "no-rm-rf");
    assert!(
        d.explanation.contains("no-rm-rf"),
        "explanation must name the rule: {}",
        d.explanation
    );
    assert!(d.explanation.contains("forbid"));
    assert!(
        d.considered
            .iter()
            .any(|step| step.rule_id == "no-rm-rf" && step.matched)
    );
}

#[test]
fn cache_invalidates_on_membership_change() {
    let policy = PolicyBuilder::new()
        .member("devs", "alice")
        .default_mode(Mode::Forbid)
        .rule(Rule::new(
            "allow-cargo",
            "devs",
            Target::CommandPrefix("cargo".into()),
            Mode::Allow,
        ))
        .build();
    let mut engine = Evaluator::new(policy);
    let req = command_request("alice", "cargo test");

    assert_eq!(engine.evaluate(&req).mode, Mode::Allow);
    assert_eq!(engine.evaluate(&req).mode, Mode::Allow);
    assert_eq!(
        engine.stats().hits,
        1,
        "second identical call should hit cache"
    );
    assert_eq!(engine.cache_len(), 1);

    // Remove alice from the group: the cached Allow must not survive.
    engine.remove_member("devs", "alice");
    assert_eq!(engine.generation(), 1);
    assert_eq!(engine.cache_len(), 0, "membership change clears the cache");
    let changed = engine.evaluate(&req);
    assert_eq!(
        changed.mode,
        Mode::Forbid,
        "no longer a member -> default deny"
    );
    assert_eq!(changed.rule_id, "<default>");
    assert_eq!(engine.stats().misses, 2, "re-evaluated after invalidation");

    // Adding membership back flips the decision again.
    engine.set_member("devs", "alice");
    assert_eq!(engine.evaluate(&req).mode, Mode::Allow);
    assert_eq!(engine.generation(), 2);
}

#[test]
fn malformed_policy_fails_closed_for_mutating_actions_but_allows_reads() {
    let mut engine = Evaluator::new(Policy::malformed("unsupported rule grammar at line 7"));
    assert!(engine.policy().is_malformed());

    // Mutating actions are forbidden.
    for action in [
        Action::Write,
        Action::Execute,
        Action::Delete,
        Action::Network,
    ] {
        let d = engine.evaluate(&Request::new("alice").with_action(action));
        assert_eq!(d.mode, Mode::Forbid);
        assert_eq!(d.rule_id, "<malformed-policy:fail-closed>");
        assert!(d.explanation.contains("unsupported rule grammar at line 7"));
    }

    // A pure read is still allowed so inspection commands keep working.
    let read = engine.evaluate(&Request::new("alice").with_action(Action::Read));
    assert_eq!(read.mode, Mode::Allow);
    assert_eq!(read.rule_id, "<malformed-policy:read-fallback>");

    // Malformed decisions are not cached.
    assert_eq!(engine.cache_len(), 0);
}

#[test]
fn policy_validation_rejects_duplicate_ids_and_cycles() {
    let duplicate = PolicyBuilder::new()
        .rule(Rule::new(
            "same",
            "alice",
            Target::CommandPrefix("a".into()),
            Mode::Allow,
        ))
        .rule(Rule::new(
            "same",
            "alice",
            Target::CommandPrefix("b".into()),
            Mode::Forbid,
        ))
        .build();
    assert!(duplicate.is_malformed());
    assert!(
        duplicate
            .malformed_reason()
            .unwrap()
            .contains("duplicate rule id")
    );

    let cyclic = PolicyBuilder::new().edge("a", "b").edge("b", "a").build();
    assert!(cyclic.is_malformed());
    assert!(cyclic.malformed_reason().unwrap().contains("cycle"));

    // A diamond is a valid DAG, not a cycle.
    let diamond = PolicyBuilder::new()
        .edge("root", "a")
        .edge("root", "b")
        .edge("a", "shared")
        .edge("b", "shared")
        .build();
    assert!(!diamond.is_malformed());
}

#[test]
fn rule_reachable_two_ways_is_not_bypassed() {
    // Diamond: root contains a and b; both contain shared. Bob is a direct
    // member of shared, so shared/a/b/root are each reachable by two paths.
    let policy = PolicyBuilder::new()
        .member("shared", "bob")
        .edge("root", "a")
        .edge("root", "b")
        .edge("a", "shared")
        .edge("b", "shared")
        .rule(Rule::new(
            "deny-from-shared",
            "shared",
            Target::CommandPrefix("rm".into()),
            Mode::Forbid,
        ))
        .rule(Rule::new(
            "deny-from-root",
            "root",
            Target::CommandPrefix("curl".into()),
            Mode::Forbid,
        ))
        .rule(Rule::new(
            "deny-from-a",
            "a",
            Target::CommandPrefix("ssh".into()),
            Mode::Forbid,
        ))
        .default_mode(Mode::Allow)
        .build();
    assert!(!policy.is_malformed());
    let mut engine = Evaluator::new(policy);

    for (cmd, expected_rule) in [
        ("rm -rf /", "deny-from-shared"),
        ("curl http://example", "deny-from-root"),
        ("ssh host", "deny-from-a"),
    ] {
        let d = engine.evaluate(&command_request("bob", cmd));
        assert_eq!(d.mode, Mode::Forbid, "command {cmd} must be forbidden");
        assert_eq!(d.rule_id, expected_rule);
        // Reachable twice, but considered exactly once: no duplication that
        // could mask the winner or change the outcome.
        assert_eq!(
            d.considered
                .iter()
                .filter(|step| step.rule_id == expected_rule)
                .count(),
            1,
            "rule {expected_rule} must be evaluated once"
        );
    }
}

#[test]
fn attribute_conditions_are_fail_closed_and_support_operators() {
    let policy = PolicyBuilder::new()
        .rule(
            Rule::new(
                "prod-only",
                "alice",
                Target::CommandPrefix("deploy".into()),
                Mode::Allow,
            )
            .with_condition(Condition::new(
                "env",
                AttrOp::Eq,
                AttrValue::str_value("prod"),
            )),
        )
        .rule(
            Rule::new(
                "clearance",
                "alice",
                Target::CommandPrefix("export".into()),
                Mode::Allow,
            )
            .with_condition(Condition::new(
                "level",
                AttrOp::Ge,
                AttrValue::int_value(3),
            )),
        )
        .default_mode(Mode::Forbid)
        .build();
    let mut engine = Evaluator::new(policy);

    // Right attribute value: allowed.
    let ok = engine.evaluate(
        &command_request("alice", "deploy now").with_attr("env", AttrValue::str_value("prod")),
    );
    assert_eq!(ok.mode, Mode::Allow);
    assert_eq!(ok.rule_id, "prod-only");

    // Wrong value: condition fails, default deny.
    let bad = engine.evaluate(
        &command_request("alice", "deploy now").with_attr("env", AttrValue::str_value("dev")),
    );
    assert_eq!(bad.mode, Mode::Forbid);

    // Missing attribute is false even for Ne — the rule cannot widen by absent
    // data.
    let missing = engine.evaluate(&command_request("alice", "deploy now"));
    assert_eq!(missing.mode, Mode::Forbid);

    // Numeric operator: 3 >= 3 passes, 2 fails.
    let ge_ok = engine.evaluate(
        &command_request("alice", "export data").with_attr("level", AttrValue::int_value(3)),
    );
    assert_eq!(ge_ok.rule_id, "clearance");
    let ge_bad = engine.evaluate(
        &command_request("alice", "export data").with_attr("level", AttrValue::int_value(2)),
    );
    assert_eq!(ge_bad.mode, Mode::Forbid);
}

#[test]
fn command_prefix_does_not_match_a_longer_word() {
    let policy = PolicyBuilder::new()
        .rule(Rule::new(
            "allow-cargo-test",
            "alice",
            Target::CommandPrefix("cargo test".into()),
            Mode::Allow,
        ))
        .default_mode(Mode::Forbid)
        .build();
    let mut engine = Evaluator::new(policy);

    assert_eq!(
        engine
            .evaluate(&command_request("alice", "cargo test --all"))
            .mode,
        Mode::Allow
    );
    // "cargo testify" is a different word, not the prefix + arguments.
    assert_eq!(
        engine
            .evaluate(&command_request("alice", "cargo testify"))
            .mode,
        Mode::Forbid
    );
}

#[test]
fn adding_a_deny_rule_invalidates_a_previously_cached_allow() {
    let policy = PolicyBuilder::new()
        .rule(Rule::new(
            "allow-all",
            "alice",
            Target::CommandPrefix("tool".into()),
            Mode::Allow,
        ))
        .build();
    let mut engine = Evaluator::new(policy);
    let req = command_request("alice", "tool run");
    assert_eq!(engine.evaluate(&req).mode, Mode::Allow);
    assert_eq!(engine.evaluate(&req).mode, Mode::Allow);

    engine.add_rule(Rule::new(
        "deny-tool-run",
        "alice",
        Target::CommandPrefix("tool run".into()),
        Mode::Forbid,
    ));
    assert_eq!(engine.cache_len(), 0);
    let changed = engine.evaluate(&req);
    assert_eq!(changed.mode, Mode::Forbid);
    assert_eq!(changed.rule_id, "deny-tool-run");
}
