use super::*;
use serde_json::json;

// ---- metrics -----------------------------------------------------------

#[test]
fn nugget_thresholds_match_the_benchmark() {
    // The three points of BEAM's scale, and the boundaries themselves.
    assert_eq!(nugget_score(1.0), 1.0);
    assert_eq!(nugget_score(0.75), 1.0);
    assert_eq!(nugget_score(0.74), 0.5);
    assert_eq!(nugget_score(0.25), 0.5);
    assert_eq!(nugget_score(0.24), 0.0);
    // A non-finite judge reply must not become a correct nugget.
    assert_eq!(nugget_score(f64::NAN), 0.0);
}

#[test]
fn mean_of_nothing_is_none_not_zero() {
    assert_eq!(mean(&[]), None);
    assert_eq!(mean(&[f64::NAN]), None);
    assert_eq!(mean(&[1.0, 2.0]), Some(1.5));
}

#[test]
fn class_report_is_worst_first() {
    let mut t: BTreeMap<String, ClassTally> = BTreeMap::new();
    let (mut good, mut bad) = (ClassTally::default(), ClassTally::default());
    good.add(true, 1.0);
    bad.add(false, 0.0);
    t.insert("aaa".into(), good);
    t.insert("zzz".into(), bad);
    let report = class_report(&t);
    assert!(report.find("zzz").unwrap() < report.find("aaa").unwrap());
}

// ---- kendall -----------------------------------------------------------

#[test]
fn kendall_perfect_and_reversed() {
    let gold = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert_eq!(kendall_tau_b(&gold, &gold), Some(1.0));
    let reversed = vec!["c".to_string(), "b".to_string(), "a".to_string()];
    assert_eq!(kendall_tau_b(&reversed, &gold), Some(-1.0));
    // A single adjacent swap out of three must land strictly between.
    let swapped = vec!["b".to_string(), "a".to_string(), "c".to_string()];
    let t = kendall_tau_b(&swapped, &gold).unwrap();
    assert!(
        t > 0.0 && t < 1.0,
        "adjacent swap should be partial credit: {t}"
    );
}

#[test]
fn kendall_needs_two_shared_items() {
    let gold = vec!["a".to_string(), "b".to_string()];
    assert_eq!(kendall_tau_b(&["a".to_string()], &gold), None);
    assert_eq!(
        kendall_tau_b(&["x".to_string(), "y".to_string()], &gold),
        None
    );
}

#[test]
fn kendall_ignores_items_missing_from_the_prediction() {
    let gold = vec![
        "a".to_string(),
        "b".to_string(),
        "c".to_string(),
        "d".to_string(),
    ];
    // Two of four, in the right order: the extra gold items must not penalise.
    let pred = vec!["a".to_string(), "b".to_string()];
    assert_eq!(kendall_tau_b(&pred, &gold), Some(1.0));
}

// ---- temporal ----------------------------------------------------------

#[test]
fn durations_in_digits_and_words() {
    assert_eq!(parse_duration("3 days"), Some(3 * 86_400));
    assert_eq!(parse_duration("a week"), Some(604_800));
    assert_eq!(parse_duration("two years"), Some(2 * 31_536_000));
    assert_eq!(parse_duration("45 minutes"), Some(45 * 60));
    assert_eq!(parse_duration("half an hour"), Some(1_800));
    assert_eq!(parse_duration("no idea"), None);
}

#[test]
fn tolerated_date_and_duration_windows() {
    let t0 = 1_700_000_000i64;
    assert!(date_within_tolerance(t0 + 13 * 86_400, t0));
    assert!(!date_within_tolerance(t0 + 20 * 86_400, t0));
    // A duration within half of the gold is credit; beyond it is not.
    assert!(duration_within_tolerance(60, 100));
    assert!(!duration_within_tolerance(40, 100));
}

#[test]
fn interval_overlap_is_symmetric_and_handles_points() {
    assert!(intervals_overlap((10, 20), (20, 30)));
    assert!(!intervals_overlap((10, 20), (21, 30)));
    assert!(intervals_overlap((15, 15), (10, 20)));
    assert!(intervals_overlap((30, 10), (20, 25)));
}

// ---- reconciliation ---------------------------------------------------

fn facts() -> Vec<FactRecord> {
    vec![
        FactRecord::new("f1", "user lives in Berlin", 1_000),
        FactRecord::new("f2", "user lives in Munich", 2_000).superseded_by("f3"),
        FactRecord::new("f3", "user lives in Lisbon", 3_000),
    ]
}

#[test]
fn belief_at_picks_the_newest_and_names_the_reason() {
    let b = belief_at(&facts(), 3_500).unwrap();
    assert_eq!(b.fact_id, "f3");
    assert_eq!(b.reason, ReconcileReason::Current);
    // Before the move, the first belief is the right one, and it is not "current".
    let old = belief_at(&facts(), 1_500).unwrap();
    assert_eq!(old.fact_id, "f1");
}

#[test]
fn revocation_reason_survives_when_nothing_is_believed() {
    let f = vec![FactRecord::new("f9", "card expires 2024", 1_000).revoked(5_000, "card replaced")];
    let b = belief_at(&f, 6_000).unwrap();
    assert_eq!(
        b.reason,
        ReconcileReason::Revoked {
            reason: "card replaced".into()
        },
        "the reason is the evidence a FAMA-style scorer reads"
    );
    // The same fact is still believed before the revocation.
    assert!(belief_at(&f, 4_000).unwrap().fact_id == "f9");
}

#[test]
fn asks_about_a_time_before_a_fact_existed() {
    let f = vec![FactRecord::new("f1", "new policy", 9_000)];
    let b = belief_at(&f, 1_000).unwrap();
    assert_eq!(b.reason, ReconcileReason::NotYetValid { at: 9_000 });
}

#[test]
fn fama_does_not_reward_a_right_answer_reached_wrongly() {
    let f = facts();
    let invalid = FactRecord::new("f2b", "user lives in Munich", 2_000).revoked(2_500, "moved");
    let mut all = f.clone();
    all.push(invalid);

    let right_way = FamaOutcome {
        correct: true,
        relied_on_invalid: false,
    };
    let wrong_way = FamaOutcome {
        correct: true,
        relied_on_invalid: true,
    };
    assert_eq!(right_way.score(), 1.0);
    assert_eq!(wrong_way.score(), 0.0);
    assert!(wrong_way.lucky_but_invalid());

    // And the detector agrees, given the citation.
    assert!(relies_on_invalid(&all, &["f2b".to_string()], 3_000));
    assert!(!relies_on_invalid(&all, &["f3".to_string()], 3_000));
    // A citation to a fact we do not hold is not stale-belief evidence.
    assert!(!relies_on_invalid(&all, &["nope".to_string()], 3_000));
}

#[test]
fn fama_tally_reports_the_cost_of_forgetting() {
    let mut t = FamaTally::default();
    t.add(FamaOutcome {
        correct: true,
        relied_on_invalid: false,
    });
    t.add(FamaOutcome {
        correct: true,
        relied_on_invalid: true,
    });
    assert_eq!(t.accuracy(), Some(100.0));
    assert_eq!(t.fama(), Some(50.0));
    assert_eq!(t.forgetting_cost(), Some(50.0));
    assert_eq!(t.lucky_but_invalid, 1);
}

// ---- verbatim ---------------------------------------------------------

#[test]
fn span_render_carries_provenance() {
    let mut s = Span::new("s1", "I shipped the migration on Friday")
        .speaker("Caroline")
        .at(1_683_000_000)
        .session("conv-26");
    s.turn = Some(4);
    let r = s.render();
    assert!(r.contains("Caroline"), "{r}");
    assert!(r.contains("conv-26#4"), "{r}");
    assert!(r.contains("I shipped the migration on Friday"), "{r}");
}

#[test]
fn span_coverage_detects_paraphrase() {
    let spans = vec![Span::new("s1", "the user prefers PostgreSQL over MySQL")];
    // A faithful fact quotes the source.
    assert!(span_coverage("user prefers PostgreSQL", &spans) > 0.9);
    // A paraphrase about a subject not in the source is the hallucination risk.
    assert!(span_coverage("user prefers MongoDB sharding", &spans) < 0.6);
}

#[test]
fn verbatim_assembly_is_newest_first_and_never_cuts_a_span() {
    let spans = vec![
        Span::new("a", "older").at(1_000),
        Span::new("b", "newer").at(2_000),
    ];
    let (out, used) = assemble_verbatim(&spans, 10_000);
    assert!(out.find("newer").unwrap() < out.find("older").unwrap());
    assert!(used > 0);

    // A span that does not fit is skipped whole, not truncated.
    let long = vec![Span::new("c", &"x".repeat(500)).at(3_000)];
    let (out, used) = assemble_verbatim(&long, 100);
    assert_eq!(out, "");
    assert_eq!(used, 0);
}

// ---- loaders ----------------------------------------------------------

#[test]
fn ability_parsing_covers_all_ten_and_rejects_strangers() {
    assert_eq!(BeamAbility::ALL.len(), 10);
    for a in BeamAbility::ALL {
        assert_eq!(BeamAbility::parse(a.name()), Some(a));
    }
    assert_eq!(
        BeamAbility::parse("contradiction resolution"),
        Some(BeamAbility::ContradictionResolution)
    );
    assert_eq!(BeamAbility::parse("vibes"), None);
    // The four state-over-time abilities are the ones the field is worst at.
    let stateful: Vec<&str> = BeamAbility::ALL
        .iter()
        .filter(|a| a.is_state_over_time())
        .map(|a| a.name())
        .collect();
    assert_eq!(stateful.len(), 4);
    assert!(stateful.contains(&"contradiction_resolution"));
    assert!(stateful.contains(&"event_ordering"));
}

#[test]
fn beam_record_with_string_encoded_questions_loads() {
    let item = json!({
        "conversation_id": "1M_7",
        "chat": [
            {"role": "user", "content": "I moved to Lisbon in March"},
            {"role": "assistant", "content": "Noted."}
        ],
        // HF ships this as a JSON *string*, not an object.
        "probing_questions": "{\"contradiction_resolution\": [{\"question\": \"Where does the user live?\", \"rubric\": {\"nuggets\": [{\"description\": \"Lisbon\"}]}}]}"
    });
    let conv = load_beam_record(&item, "1M", 7).unwrap();
    assert_eq!(conv.turns.len(), 2);
    assert_eq!(conv.questions.len(), 1);
    assert_eq!(
        conv.questions[0].ability,
        BeamAbility::ContradictionResolution
    );
    assert_eq!(conv.questions[0].nuggets, vec!["Lisbon".to_string()]);
}

/// **The shape the official checkout actually ships**, and the reason the published data could
/// not be loaded: `rubric` is a plain array of strings, not `{"nuggets": [{"description": …}]}`.
/// Read as an object it yields zero nuggets, and `load_beam` then refuses every question — a
/// benchmark that will not load at all rather than one that scores 0.
#[test]
fn beam_official_rubric_is_an_array_of_strings() {
    let item = json!({
        "conversation_id": "1M_1",
        "chat": [
            {"role": "user", "content": "I studied psychology"},
            {"role": "assistant", "content": "Noted."}
        ],
        "probing_questions": {
            "abstention": [{
                "question": "Where did I study psychology?",
                "ideal_response": "Based on the provided chat, there is no information related to your educational background.",
                "difficulty": "easy",
                "abstention_type": "missing_detail",
                "rubric": [
                    "Based on the provided chat, there is no information related to your educational background",
                    "Based on the provided chat, there is no information related to your specialization in psychology"
                ]
            }]
        }
    });
    let conv = load_beam_record(&item, "1M", 0).unwrap();
    assert_eq!(conv.questions.len(), 1);
    assert_eq!(conv.questions[0].ability, BeamAbility::Abstention);
    assert_eq!(conv.questions[0].nuggets.len(), 2);
    assert!(conv.questions[0].nuggets[0].ends_with("educational background"));
    assert_eq!(conv.questions[0].difficulty.as_deref(), Some("easy"));
}

/// A rubric-less question is still scoreable when the record ships `ideal_response` — the
/// reference text the rubric was derived from. Dropping such a question would shrink the
/// benchmark silently, and `load_beam` errors on an empty gold rather than dropping it, so this
/// fallback is what keeps the `abstention` class in the run.
#[test]
fn beam_falls_back_to_ideal_response_when_a_rubric_is_absent() {
    let item = json!({
        "conversation_id": "1M_2",
        "chat": [{"role": "user", "content": "hi"}],
        "probing_questions": {
            "summarization": [{
                "question": "Summarise the conversation.",
                "ideal_response": "The user said hi."
            }]
        }
    });
    let conv = load_beam_record(&item, "1M", 1).unwrap();
    assert_eq!(
        conv.questions[0].nuggets,
        vec!["The user said hi.".to_string()]
    );
}

/// AMA-Bench's turn-shaped step: one object, two steps of *different kinds*, and two distinct
/// step numbers so the rendered documents cannot collide.
#[test]
fn ama_turn_shaped_step_expands_into_action_then_observation() {
    let item = json!({
        "episode_id": 7,
        "trajectory": [
            {"turn_idx": 0, "action": "left", "observation": "Active rules: baba is you"},
            {"turn_idx": 1, "action": "up", "observation": "door 3 steps down"}
        ]
    });
    let traj = load_trajectory_record(&item, 0).unwrap();
    assert_eq!(traj.task_id, "7");
    let kinds: Vec<StepKind> = traj.steps.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        vec![
            StepKind::Action,
            StepKind::Observation,
            StepKind::Action,
            StepKind::Observation
        ]
    );
    // Turn order preserved, turn clock carried, indices unique.
    assert_eq!(traj.steps[0].at, Some(0));
    assert_eq!(traj.steps[3].at, Some(1));
    let mut indices: Vec<usize> = traj.steps.iter().map(|s| s.index).collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(indices.len(), 4);
    assert!(traj.steps[0].render().contains("step 0 action: left"));
    assert!(traj.steps[1].render().contains("observation: Active rules"));
    // **Both steps of a turn are labelled with the turn, not with the synthetic id.**
    // AMA-Bench's questions say "the observation after the `up` action at Step 8" — the number
    // they quote is `turn_idx`. The observation of turn 1 carries index 3 (so the two steps of
    // one turn stay uniquely addressable for the causal edges), and rendering that index would
    // have shown the reader `step 3` for a line every question calls step 1.
    assert_eq!(traj.steps[1].turn, Some(0));
    assert_eq!(traj.steps[2].index, 1);
    assert_eq!(
        traj.steps[3].index, 3,
        "the observation's id must stay unique"
    );
    assert_eq!(traj.steps[3].turn, Some(1));
    assert!(
        traj.steps[3]
            .render()
            .starts_with("step 1 observation: door 3 steps down"),
        "got {:?}",
        traj.steps[3].render()
    );
    // A shape with no clock renders exactly as it did before this field existed.
    let plain = load_trajectory_record(&json!({"trajectory": ["action: ls"]}), 0).unwrap();
    assert_eq!(plain.steps[0].turn, None);
    assert_eq!(plain.steps[0].render(), "step 0 action: ls");
}

/// **The placeholder turn must not fail the load.** AMA-Bench's `spider2` episodes open with
/// two `{turn_idx, action: null, observation: null}` steps — 102 of the published set's 14 944.
/// The shape test used to fire on the *values* (`action.is_some() || observation.is_some()`),
/// so those turns fell through to the kind branch and the entire 208-episode file refused to
/// load with "trajectory step 0 carries no kind/type/role". A shape test that fires on the
/// values turns a valid dataset into an unreadable one; this pins the keys-not-values rule.
#[test]
fn ama_placeholder_turn_loads_and_is_counted_rather_than_failing_the_file() {
    let item = json!({
        "episode_id": 90,
        "trajectory": [
            {"turn_idx": 0, "action": null, "observation": null},
            {"turn_idx": 1, "action": null, "observation": null},
            {"turn_idx": 2, "action": "SELECT 1", "observation": "1 row"}
        ]
    });
    let traj = load_trajectory_record(&item, 0)
        .expect("a placeholder turn carries no text, not an unknown kind");
    assert_eq!(traj.empty_turns, 2, "both placeholders counted");
    assert_eq!(
        traj.steps.len(),
        2,
        "each real turn still yields action + observation"
    );
    // The turn clock survives the placeholder gap: the first *real* turn is turn 2, and the
    // reader's step numbering has to say so or every step-reference in the question is off.
    assert_eq!(traj.steps[0].turn, Some(2));
    assert!(
        traj.steps[0]
            .render()
            .starts_with("step 2 action: SELECT 1")
    );
    assert_eq!(traj.steps[1].render(), "step 2 observation: 1 row");
}

#[test]
fn beam_refuses_an_unknown_ability_rather_than_dropping_the_class() {
    let item = json!({
        "conversation_id": "x",
        "chat": [{"role": "user", "content": "hi"}],
        "probing_questions": {"teleportation": [{"question": "?"}]}
    });
    let err = load_beam_record(&item, "1M", 0).unwrap_err().to_string();
    assert!(err.contains("teleportation"), "{err}");
}

#[test]
fn beam_refuses_an_empty_haystack() {
    let item = json!({"conversation_id": "x", "chat": [], "probing_questions": {}});
    assert!(load_beam_record(&item, "1M", 0).is_err());
}

#[test]
fn personamem_mcq_parses_and_scores() {
    let csv = "user_id,question,options,answer\n\
u1,Which cuisine does the user prefer?,Italian|Thai|Indian,B\n\
u1,What time do they usually exercise?,6am|7am,1\n";
    let qs = load_personamem_csv(csv).unwrap();
    assert_eq!(qs.len(), 2);
    assert_eq!(qs[0].options.len(), 3);
    assert_eq!(qs[0].answer_index, 1);
    assert!(qs[0].is_correct(Some(1)));
    assert!(!qs[0].is_correct(Some(0)));
    // A digit answer and a letter answer both resolve.
    assert!(qs[1].is_correct(Some(1)));
    // Free-text replies are read for their answer letter, not their length.
    assert_eq!(PersonaMemQuestion::parse_reply("B"), Some(1));
    assert_eq!(PersonaMemQuestion::parse_reply("answer: C"), Some(2));
    assert_eq!(PersonaMemQuestion::parse_reply("2"), Some(2));
}

#[test]
fn personamem_refuses_a_csv_with_no_questions() {
    assert!(load_personamem_csv("user_id,question,options,answer\n").is_err());
    assert!(load_personamem_csv("nope\n").is_err());
}

#[test]
fn persona_answer_out_of_range_is_an_error_not_a_silent_zero() {
    let csv = "question,options,answer\nq,a|b,9\n";
    assert!(load_personamem_csv(csv).is_err());
}

fn outcome(domain: ArenaDomain, ok: bool) -> ArenaOutcome {
    ArenaOutcome {
        task_id: "t".into(),
        domain,
        success: ok,
    }
}

#[test]
fn arena_suite_score_is_the_equal_weight_mean_over_five_domains() {
    let outcomes = vec![
        outcome(ArenaDomain::BundledShopping, true),
        outcome(ArenaDomain::BundledShopping, false),
        outcome(ArenaDomain::ProgressiveSearch, true),
        outcome(ArenaDomain::ProgressiveSearch, true),
    ];
    let (by_domain, suite) = arena_suite_score(&outcomes);
    assert_eq!(by_domain["bundled_shopping"], 50.0);
    assert_eq!(by_domain["progressive_search"], 100.0);
    // The three domains we did not run count as 0.0, so the mean is over five:
    // (50 + 100 + 0 + 0 + 0) / 5. A mean over the two run domains would say 75.0.
    assert_eq!(suite, Some(30.0));
}

#[test]
fn arena_suite_score_of_nothing_is_none_not_zero() {
    let (by_domain, suite) = arena_suite_score(&[]);
    assert_eq!(suite, None);
    // Every domain is still named, so an unrun domain is visible in the table.
    assert_eq!(by_domain.len(), 5);
}

#[test]
fn there_are_five_arena_domains_named_as_the_dataset_names_them() {
    assert_eq!(ArenaDomain::ALL.len(), 5);
    for d in ArenaDomain::ALL {
        assert_eq!(ArenaDomain::parse(d.name()), Some(d));
        assert_eq!(
            ArenaDomain::from_path(&format!("{}/data.jsonl", d.name())),
            Some(d)
        );
    }
    // The study's spellings map onto the same five.
    assert_eq!(
        ArenaDomain::parse("web_shopping"),
        Some(ArenaDomain::BundledShopping)
    );
    assert_eq!(
        ArenaDomain::parse("travel planning"),
        Some(ArenaDomain::GroupTravelPlanner)
    );
    assert_eq!(
        ArenaDomain::parse("math"),
        Some(ArenaDomain::FormalReasoningMath)
    );
    assert_eq!(
        ArenaDomain::parse("physics"),
        Some(ArenaDomain::FormalReasoningPhys)
    );
    assert_eq!(ArenaDomain::parse("unknown"), None);
}

#[test]
fn memoryarena_record_loads_with_per_domain_answer_shapes() {
    // Progressive Search: answers are objects, not strings.
    let ps = json!({
        "id": 0,
        "questions": ["buy item 1", "buy item 2"],
        "answers": [
            {"target_asin": "B00TUDFEW2", "attributes": ["Almond Flour"]},
            {"target_asin": "B08957C9ZH", "attributes": []}
        ]
    });
    let t = load_memoryarena_task(&ps, ArenaDomain::ProgressiveSearch).unwrap();
    assert_eq!(t.subtasks.len(), 2);
    assert_eq!(t.subtask_count(), 2);
    assert!(t.answers[0].contains("B00TUDFEW2"), "{:?}", t.answers[0]);
    assert!(
        t.backgrounds.is_empty(),
        "progressive search ships no backgrounds"
    );

    // Travel: a string background plus list-shaped answers.
    let travel = json!({
        "id": 4,
        "backgrounds": "Jennifer: 3 days from St. Petersburg to Rockford",
        "questions": ["I am Eric, joining Jennifer"],
        "answers": [[{"days": 1, "current_city": "Rockford"}]]
    });
    let t = load_memoryarena_task(&travel, ArenaDomain::GroupTravelPlanner).unwrap();
    assert_eq!(t.backgrounds.len(), 1);
    assert!(t.answers[0].contains("Rockford"), "{:?}", t.answers[0]);

    // Formal reasoning: per-subtask backgrounds and a paper name.
    let math = json!({
        "id": 7,
        "paper_name": "paper_42",
        "backgrounds": ["defn A", "defn B"],
        "questions": ["q1", "q2"],
        "answers": ["42", "7"]
    });
    let t = load_memoryarena_task(&math, ArenaDomain::FormalReasoningMath).unwrap();
    assert_eq!(t.backgrounds.len(), 2);
    assert_eq!(t.paper_name.as_deref(), Some("paper_42"));
    assert_eq!(t.answers, vec!["42".to_string(), "7".to_string()]);
}

#[test]
fn memoryarena_refuses_a_misaligned_task() {
    // 2 questions, 1 answer: scoring this would silently mark one subtask wrong.
    let bad = json!({"id": 1, "questions": ["a", "b"], "answers": ["only one"]});
    let err = load_memoryarena_task(&bad, ArenaDomain::BundledShopping)
        .unwrap_err()
        .to_string();
    assert!(err.contains("misaligned"), "{err}");
}

#[test]
fn halumem_is_update_accepts_the_string_spelling_the_release_uses() {
    // The documented example writes "True" as a *string*. `as_bool` returns None on
    // it, which would report that nothing is ever updated.
    let user = json!({
        "uuid": "u-1",
        "persona_info": {"name": "Martin Mark"},
        "sessions": [{
            "start_time": "Dec 15, 2025, 08:41:23",
            "dialogue": [
                {"role": "user", "content": "I am considering a career change"},
                {"role": "assistant", "content": "Tell me more."}
            ],
            "memory_points": [
                {"index": 1, "memory_content": "Martin considers a career change",
                 "memory_type": "Event Memory", "memory_source": "secondary",
                 "is_update": "True",
                 "original_memories": ["Martin is happy in his role"],
                 "importance": 0.75},
                {"index": 2, "memory_content": "42 x 7 = 294",
                 "memory_source": "interference", "is_update": "False"}
            ],
            "questions": [{"question": "What is Martin considering?", "answer": "a career change"}],
            "dialogue_token_length": 1200
        }]
    });
    let u = load_halumem_user(&user).unwrap();
    assert_eq!(u.uuid, "u-1");
    assert_eq!(u.sessions.len(), 1);
    assert_eq!(u.sessions[0].turns.len(), 2);
    assert_eq!(u.sessions[0].turns[0].speaker.as_deref(), Some("user"));
    assert_eq!(u.memory_points(), 2);
    // The interference point is excluded from the completeness denominator.
    assert_eq!(u.real_memory_points(), 1);
    assert_eq!(u.update_points(), 1);
    let p = &u.sessions[0].memory_points[0];
    assert!(p.is_update, "the string form \"True\" must read as true");
    assert_eq!(p.source, HaluSource::Secondary);
    assert_eq!(
        p.original_memories,
        vec!["Martin is happy in his role".to_string()]
    );
    assert_eq!(p.importance, Some(0.75));
    assert!(u.sessions[0].memory_points[1].source.is_distractor());
    assert_eq!(u.sessions[0].questions.len(), 1);
    assert_eq!(u.sessions[0].token_length, Some(1200));
}

#[test]
fn halumem_rejects_a_record_without_a_uuid_or_sessions() {
    assert!(load_halumem_user(&json!({"sessions": []})).is_err());
    assert!(load_halumem_user(&json!({"uuid": "u"})).is_err());
    assert!(load_halumem_user(&json!({"uuid": "u", "sessions": []})).is_err());
}

#[test]
fn personamem_contexts_jsonl_loads_and_groups_by_session() {
    let jsonl = concat!(
        r#"{"ctx-a": [[{"role": "user", "content": "I love Thai food"}], [{"role": "assistant", "content": "Noted"}]]}"#,
        "\n",
        r#"{"ctx-b": [[{"role": "user", "content": "I run at 6am"}]]}"#,
        "\n"
    );
    let contexts = load_personamem_contexts_jsonl(jsonl).unwrap();
    assert_eq!(contexts.len(), 2);
    // Two sessions in ctx-a, one in ctx-b — the truncation unit is a session.
    assert_eq!(contexts["ctx-a"].len(), 2);
    assert_eq!(contexts["ctx-b"].len(), 1);
    assert_eq!(contexts["ctx-a"][0][0].speaker.as_deref(), Some("user"));
    assert!(contexts["ctx-a"][0][0].render().contains("Thai food"));
}

#[test]
fn personamem_contexts_refuses_an_empty_file() {
    assert!(load_personamem_contexts_jsonl("").is_err());
    assert!(load_personamem_contexts_jsonl("\n\n").is_err());
}

#[test]
fn personamem_csv_carries_the_context_join_key_and_truncation_point() {
    let csv = "user_id,shared_context_id,end_index_in_shared_context,category,question,options,answer\n\
u1,ctx-a,3,preference,Which cuisine?,Italian|Thai,B\n";
    let qs = load_personamem_csv(csv).unwrap();
    assert_eq!(qs.len(), 1);
    assert_eq!(qs[0].shared_context_id, "ctx-a");
    assert_eq!(qs[0].end_index_in_shared_context, Some(3));
    assert_eq!(qs[0].category.as_deref(), Some("preference"));
}

#[test]
fn render_answer_handles_every_shipped_shape() {
    assert_eq!(render_answer(&json!("plain")), "plain");
    assert_eq!(render_answer(&json!(["a", "b"])), "a | b");
    let obj = render_answer(&json!({"target_asin": "X1"}));
    assert!(obj.contains("target_asin=X1"), "{obj}");
    // An integer gold must render as its digits, not vanish.
    assert_eq!(render_answer(&json!(2014)), "2014");
}

#[test]
fn halumem_separates_fabrication_from_omission() {
    let mut t = HaluTally::default();
    t.add(HaluOutcome {
        total_memory_points: 10,
        correct: 7,
        fabricated: 2,
        omitted: 1,
    });
    assert_eq!(t.hallucination_rate(), Some(20.0));
    assert_eq!(t.omission_rate(), Some(10.0));
    assert_eq!(HaluTally::default().hallucination_rate(), None);
}

// ---- the bug this module exists to prevent ----------------------------

#[test]
fn scalar_golds_are_read_including_integers() {
    // The LongMemEval bug in one assertion: 32 golds are JSON integers and
    // `as_str` returned None for every one of them.
    assert_eq!(scalar_string(&json!(2014)), Some("2014".to_string()));
    assert_eq!(scalar_string(&json!(38)), Some("38".to_string()));
    assert_eq!(scalar_string(&json!("2014")), Some("2014".to_string()));
    assert_eq!(scalar_string(&json!(null)), None);
    // And a float is not truncated into a different answer.
    assert_eq!(scalar_string(&json!(2.5)), Some("2.5".to_string()));
}

#[test]
fn csv_splitter_handles_quoted_commas() {
    let f = split_csv(r#"a,"b, with comma",c"#);
    assert_eq!(f, vec!["a", "b, with comma", "c"]);
    let g = split_csv(r#"x,"he said ""hi""",y"#);
    assert_eq!(g, vec!["x", r#"he said "hi""#, "y"]);
}

// ---- intrinsic scoring (the instruments that had no caller) --------------

/// The property that makes the ordering arm a measurement: a complete-but-shuffled order must
/// score above a half-mentioned one, which is only true if unmentioned events stay in the
/// comparison instead of being dropped from it.
#[test]
fn an_unmentioned_event_is_placed_last_not_dropped() {
    let gold: Vec<String> = ["alpha one", "bravo two", "charlie three"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (predicted, mentioned) =
        order_from_text("alpha one happened, and later charlie three", &gold);
    assert_eq!(mentioned, 2);
    assert_eq!(predicted, vec!["alpha one", "charlie three", "bravo two"]);
    // All three are present, so the correlation covers the whole sequence.
    assert_eq!(predicted.len(), gold.len());
    let scored = ordering_score(&predicted, &gold).unwrap();
    let complete = ordering_score(&gold, &gold).unwrap();
    assert!(
        scored < complete,
        "half the events named must not score full marks"
    );
}

#[test]
fn ordering_score_is_normalised_to_a_credit() {
    let gold: Vec<String> = ["a one", "b two", "c three"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(ordering_score(&gold, &gold), Some(1.0));
    let reversed: Vec<String> = gold.iter().rev().cloned().collect();
    // τ = -1 normalised to 0.0, not averaged in as a negative credit for other questions.
    assert_eq!(ordering_score(&reversed, &gold), Some(0.0));
    // One shared item cannot support a correlation, so it is unmeasured rather than zero.
    assert_eq!(ordering_score(&gold[..1], &gold), None);
}

/// The clamp runs **per nugget**, then averages — and the two orders genuinely disagree here.
///
/// CORRECTION (this assertion was wrong when first written, and the first run said so): the
/// threshold is `>= 0.25` for partial, so `0.4` clamps to **0.5**, not 0.0. The old case
/// `[0.4, 0.4]` was therefore wrong twice — it expected the wrong number, and both clamp
/// orders produce 0.5 there, so it asserted a disagreement the input did not contain. It would
/// have passed for the wrong reason had the expectation matched the implementation.
///
/// `[0.2, 0.2, 0.9]` is the case that actually separates the rules: per nugget it is
/// (0.0 + 0.0 + 1.0) / 3 = 1/3, while clamping the mean once gives 0.5.
#[test]
fn rubric_scores_clamp_per_nugget_not_once_over_the_mean() {
    let per_nugget = rubric_score(&[0.2, 0.2, 0.9]).expect("three grades");
    assert!((per_nugget - 1.0 / 3.0).abs() < 1e-9, "got {per_nugget}");
    // The rejected rule, computed rather than described, so the two are compared and the
    // difference is asserted instead of claimed in a comment.
    let once = nugget_score((0.2 + 0.2 + 0.9) / 3.0);
    assert_eq!(once, 0.5);
    assert_ne!(per_nugget, once, "the two clamp orders must disagree here");
    // The thresholds are the benchmark's, not ours, and they are pinned individually.
    assert_eq!(nugget_score(0.4), 0.5);
    assert_eq!(nugget_score(0.24), 0.0);
    assert_eq!(nugget_score(0.75), 1.0);
    assert_eq!(rubric_score(&[1.0, 0.9]), Some(1.0));
    // An empty rubric is unmeasured, not a zero.
    assert_eq!(rubric_score(&[]), None);
    // A non-finite grade becomes 0.0 rather than poisoning the mean to NaN.
    assert_eq!(rubric_score(&[f64::NAN]), Some(0.0));
}

#[test]
fn parse_date_reads_the_forms_the_datasets_use() {
    // 2024-03-15 00:00 UTC, the civil-days algorithm checked against a known instant.
    assert_eq!(parse_date("2024-03-15"), Some(1_710_460_800));
    assert_eq!(parse_date("1970-01-01"), Some(0));
    assert_eq!(parse_date("2024/03/15"), Some(1_710_460_800));
    assert_eq!(parse_date("March 15, 2024"), Some(1_710_460_800));
    assert_eq!(parse_date("15 March 2024"), Some(1_710_460_800));
    assert_eq!(parse_date("Mar 15 2024"), Some(1_710_460_800));
    assert_eq!(parse_date("no date in here"), None);
    // A bare number is not a date: it must not silently parse as one.
    assert_eq!(parse_date("38"), None);
}

/// Both tolerance comparisons must **decline** what they cannot parse, because `None` routes
/// the question to the judge and `false` would record a wrong answer the memory did not
/// cause.
#[test]
fn the_tolerance_comparisons_refuse_what_they_cannot_parse() {
    assert_eq!(date_agreement("2024-03-15", "2024-03-20"), Some(true));
    assert_eq!(date_agreement("2024-03-15", "2024-05-15"), Some(false));
    assert_eq!(date_agreement("2024-03-15", "sometime in spring"), None);
    assert_eq!(duration_agreement("3 days", "4 days"), Some(true));
    assert_eq!(duration_agreement("3 days", "20 days"), Some(false));
    assert_eq!(duration_agreement("3 days", "a while"), None);
}

// ---- the agentic spine --------------------------------------------------

#[test]
fn an_unknown_step_kind_is_refused_rather_than_defaulted() {
    assert_eq!(StepKind::parse("action"), Some(StepKind::Action));
    assert_eq!(StepKind::parse("tool-output"), Some(StepKind::ToolOutput));
    assert_eq!(StepKind::parse("nonsense"), None);
    // A prose clause is not a kind, so it is not silently typed as a state step.
    assert!(split_kind_prefix("Note: the agent failed").is_none());
    // A colon inside the step body survives.
    let (kind, text) = split_kind_prefix("action: curl http://x").unwrap();
    assert_eq!(kind, StepKind::Action);
    assert_eq!(text, "curl http://x");
    // Exactly the agent-side kinds can originate an edge.
    assert!(StepKind::Action.is_cause_capable());
    assert!(StepKind::Plan.is_cause_capable());
    assert!(!StepKind::Observation.is_cause_capable());
    assert!(!StepKind::Outcome.is_cause_capable());
}

#[test]
fn a_trajectory_round_trips_from_both_shapes() {
    let object_shaped = json!({
        "task_id": "t1",
        "steps": [
            {"kind": "state", "text": "empty room"},
            {"kind": "action", "text": "open door"},
            {"kind": "observation", "text": "a key is on the table"},
        ],
        "success": true
    });
    let t = load_trajectory_record(&object_shaped, 0).unwrap();
    assert_eq!(t.task_id, "t1");
    assert_eq!(t.steps.len(), 3);
    assert_eq!(t.steps[1].kind, StepKind::Action);
    assert_eq!(t.steps[1].index, 1);
    assert_eq!(t.success, Some(true));
    assert!(t.steps[1].render().contains("step 1 action: open door"));

    let string_shaped = json!({
        "id": "t2",
        "steps": ["state: empty room", "action: open door"]
    });
    let s = load_trajectory_record(&string_shaped, 1).unwrap();
    assert_eq!(s.steps.len(), 2);
    assert_eq!(s.steps[1].text, "open door");
    // No recorded outcome is None, not a failure.
    assert_eq!(s.success, None);

    // A record with no steps must fail rather than contribute an empty episode.
    assert!(load_trajectory_record(&json!({"task_id": "t3", "steps": []}), 2).is_err());
    assert!(load_trajectory_record(&json!({"task_id": "t3"}), 3).is_err());
}

#[test]
fn success_rate_and_forgetting_are_absent_rather_than_zero_when_unmeasured() {
    assert_eq!(success_rate(&[]), None);
    assert_eq!(success_rate(&[true, true, false, true]), Some(0.75));
    assert_eq!(selective_forgetting_score(0, 0, 0), None);
    assert_eq!(selective_forgetting_score(4, 4, 0), Some(1.0));
    assert_eq!(selective_forgetting_score(4, 0, 0), Some(0.0));
    // Wrongly keeping memories costs, so "forget nothing" cannot look neutral.
    assert_eq!(selective_forgetting_score(4, 0, 2), Some(-0.5));
}

// ---- the correctness spine's attribution half ----------------------------

#[test]
fn citations_are_read_only_from_the_declared_marker() {
    let answer = "She moved to Lisbon. [[memory:s1]] Then she switched jobs \
                      [[memory:s2]] and [[memory:s1]] again.";
    assert_eq!(cited_memory_ids(answer), vec!["s1", "s2"]);
    // An unterminated marker yields nothing rather than a guess.
    assert!(cited_memory_ids("see [[memory:s1 and that is all").is_empty());
    assert!(cited_memory_ids("no citations here").is_empty());
    // The clause and the parser must describe the same syntax.
    assert!(CITATION_CLAUSE.contains(CITATION_OPEN));
    assert!(CITATION_CLAUSE.contains(CITATION_CLOSE));
}

/// The FAMA cell that no retrieval or match score can express: right answer, invalid memory.
#[test]
fn a_right_answer_reached_through_a_revoked_fact_is_penalised() {
    let facts = vec![
        FactRecord::new("f1", "lives in Berlin", 0).revoked(100, "she moved"),
        FactRecord::new("f2", "lives in Lisbon", 100),
    ];
    // Cited the revoked fact: invalid reliance at t=200, even though the answer may be right.
    assert!(relies_on_invalid(&facts, &["f1".to_string()], 200));
    assert!(!relies_on_invalid(&facts, &["f2".to_string()], 200));
    // At t=50 the first fact was still believed, so citing it was correct then.
    assert!(!relies_on_invalid(&facts, &["f1".to_string()], 50));
    // A citation we do not hold is a citation error, not evidence of a stale belief.
    assert!(!relies_on_invalid(&facts, &["nope".to_string()], 200));
    let lucky = FamaOutcome {
        correct: true,
        relied_on_invalid: true,
    };
    assert_eq!(lucky.score(), 0.0);
    assert!(lucky.lucky_but_invalid());
}

/// The primary figures must exclude distractors and keep them in their own bucket. Folding
/// them in would let a corpus with many injected distractors move the very number that is
/// supposed to describe the extraction ceiling — and Halu-Long injects them on purpose.
#[test]
fn extraction_coverage_splits_distractors_from_the_ceiling() {
    let turns = vec![
        Span::new(
            "t0",
            "I adopted a golden retriever named Biscuit last spring",
        ),
        Span::new("t1", "my sister's birthday is on the fourteenth of March"),
    ];
    let point = |idx: i64, content: &str, source: HaluSource| HaluMemoryPoint {
        index: idx,
        content: content.to_string(),
        memory_type: "fact".to_string(),
        source,
        is_update: false,
        original_memories: Vec::new(),
        importance: None,
        timestamp: None,
    };
    let user = HaluUser {
        uuid: "u1".to_string(),
        persona_info: None,
        sessions: vec![HaluSession {
            start_time: None,
            end_time: None,
            turns,
            // One verbatim point, one mostly-paraphrased point, one injected distractor.
            memory_points: vec![
                point(
                    0,
                    "adopted a golden retriever named Biscuit",
                    HaluSource::Primary,
                ),
                point(1, "adopted a cat instead of a dog", HaluSource::Secondary),
                point(
                    2,
                    "solve for x: 4x plus 7 equals 19",
                    HaluSource::Interference,
                ),
            ],
            questions: Vec::new(),
            token_length: None,
        }],
    };
    let c = extraction_coverage(&[user]);
    assert_eq!(
        c.n, 2,
        "the distractor must not inflate the measured point count"
    );
    assert_eq!(c.distractor_n, 1);
    assert_eq!(
        c.paraphrased, 1,
        "only the paraphrased point is below the zone"
    );
    assert!(
        c.p50 > 0.6,
        "the verbatim point must sit above the paraphrase zone, got {}",
        c.p50
    );
    assert!(
        c.min <= c.p10 && c.p10 <= c.p50,
        "percentiles must be ordered: min {} p10 {} p50 {}",
        c.min,
        c.p10,
        c.p50
    );
    assert!(c.mean < 1.0 && c.mean > 0.0, "mean {} out of range", c.mean);
    assert_eq!(
        c.distractor_mean, 0.0,
        "text absent from the session must score zero"
    );
}

/// The causal chain must come from the step kinds, must be a **chain** (one direct cause per
/// effect, not a clique), and must never invent a prevented alternative.
#[test]
fn causal_edges_are_derived_from_step_kinds_and_never_invent_a_prevented_alternative() {
    let step = |kind: StepKind, idx: usize| TrajectoryStep {
        kind,
        text: format!("step {idx}"),
        index: idx,
        turn: None,
        at: None,
        session: None,
    };
    // plan, action, observation, action, outcome
    let steps = vec![
        step(StepKind::Plan, 0),
        step(StepKind::Action, 1),
        step(StepKind::Observation, 2),
        step(StepKind::Action, 3),
        step(StepKind::Outcome, 4),
    ];
    let edges = derive_causal_edges(&steps);
    use CausalKind::{Causes, Enables};
    // The plan enables the two actions it schedules, and nothing else.
    assert!(edges.contains(&CausalEdge {
        from: 0,
        to: 1,
        kind: Enables
    }));
    assert!(edges.contains(&CausalEdge {
        from: 0,
        to: 3,
        kind: Enables
    }));
    // The nearer action — not the plan — causes the observation.
    assert!(edges.contains(&CausalEdge {
        from: 1,
        to: 2,
        kind: Causes
    }));
    assert!(!edges.contains(&CausalEdge {
        from: 0,
        to: 2,
        kind: Causes
    }));
    // The last action causes the outcome.
    assert!(edges.contains(&CausalEdge {
        from: 3,
        to: 4,
        kind: Causes
    }));
    assert_eq!(
        edges.iter().filter(|e| e.to == 4).count(),
        1,
        "an effect must have one direct cause, not a clique of them"
    );
    assert!(
        edges.iter().all(|e| e.kind.is_derivable()),
        "a prevented alternative leaves no step to point at, so it can never be derived"
    );
    assert!(
        !edges.iter().any(|e| e.kind == CausalKind::Prevents),
        "Prevents must never appear without the dataset saying so"
    );
}

/// An annotated trace keeps its **own** edges — including a `Prevents`, which derivation
/// cannot reach — and ours are not merged in. Merging would make the resulting number
/// attributable to neither source.
#[test]
fn causal_edges_prefer_the_records_annotation_and_do_not_merge_in_our_derivation() {
    let steps = vec![
        TrajectoryStep {
            kind: StepKind::Action,
            text: "rm -rf".into(),
            index: 0,
            turn: None,
            at: None,
            session: None,
        },
        TrajectoryStep {
            kind: StepKind::Observation,
            text: "permission denied".into(),
            index: 1,
            turn: None,
            at: None,
            session: None,
        },
    ];
    let mut t = Trajectory {
        task_id: "t".into(),
        steps,
        outcome: None,
        success: None,
        causal: Vec::new(),
        empty_turns: 0,
    };
    assert!(!t.causal_is_annotated());
    assert_eq!(
        t.causal_edges().len(),
        1,
        "a silent record derives one edge"
    );
    // Now the record states an edge derivation could never reach.
    t.causal = vec![CausalEdge {
        from: 1,
        to: 0,
        kind: CausalKind::Prevents,
    }];
    assert!(t.causal_is_annotated());
    assert_eq!(
        t.causal_edges().len(),
        1,
        "annotated edges must not be merged with derived ones"
    );
    assert_eq!(t.causal_edges()[0].kind, CausalKind::Prevents);
    // The join to the engine: the kind string the graph stores.
    assert_eq!(t.causal_relations(), vec![(1usize, 0usize, "prevents")]);
}

/// The vocabulary must refuse an unrecognised kind and **round-trip** through the string the
/// graph actually stores, or an edge written under an alias is invisible to the traversal that
/// has to follow it — an alias that works at ingest and silently fails at retrieval.
#[test]
fn causal_kind_refuses_the_unknown_and_round_trips_through_the_relation_kind() {
    assert_eq!(CausalKind::parse("leads to"), Some(CausalKind::Causes));
    assert_eq!(CausalKind::parse("Prevented"), Some(CausalKind::Prevents));
    assert_eq!(CausalKind::parse("enables"), Some(CausalKind::Enables));
    assert_eq!(CausalKind::parse("causality"), Some(CausalKind::Causes));
    assert_eq!(CausalKind::parse("correlates_with"), None);
    for k in CausalKind::ALL {
        assert_eq!(CausalKind::from_relation_kind(k.relation_kind()), Some(k));
        assert!(is_causal_relation_kind(k.relation_kind()));
    }
    assert!(
        !is_causal_relation_kind("supersedes"),
        "a supersession is a revision, not a causal edge"
    );
    assert!(!is_causal_relation_kind("relates_to"));
    // Aliases must be followable, not merely parseable at ingest.
    assert!(is_causal_relation_kind("blocks"));
}

/// A malformed or unknown-kind edge is dropped rather than defaulted, and dropping all of them
/// must fall back to the structural derivation — not to an empty causal graph, which would
/// read as "this trace has no causal structure".
#[test]
fn unparseable_causal_edges_are_dropped_rather_than_defaulted() {
    let rec = json!({
        "task_id": "t",
        "steps": ["action: open the door", "observation: it is locked"],
        "causal": [
            { "from": 0, "to": 1, "kind": "correlates_with" },
            { "from": 0, "kind": "causes" }
        ]
    });
    let t = load_trajectory_record(&rec, 0).unwrap();
    assert!(!t.causal_is_annotated(), "no well-formed edge was stated");
    assert_eq!(t.causal_edges().len(), 1, "falls back to the derivation");
}

/// An empty input must report zeroes rather than panic, and specifically must not report a
/// mean of 1.0 — a system that measured nothing has not demonstrated faithful extraction.
#[test]
fn extraction_coverage_of_nothing_is_zero_not_one() {
    let c = extraction_coverage(&[]);
    assert_eq!(c.n, 0);
    assert_eq!(c.mean, 0.0);
    assert_eq!(c.p10, 0.0);
    assert_eq!(c.p50, 0.0);
    assert_eq!(c.min, 0.0);
    assert_eq!(c.paraphrased, 0);
    assert_eq!(c.distractor_n, 0);
}
