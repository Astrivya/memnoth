// ============================================================================
use super::*;

// The agentic spine — memory that acts (AMA-Bench, MemoryAgentBench, MemoryArena)
// ============================================================================
//
// WHY THIS IS A SEPARATE INGEST SHAPE and not just more conversation. These three benchmarks do
// not ask what a user said. They ask what an agent **did**: which action it took in which state,
// what the environment returned, and whether the outcome followed from the earlier steps.
// Ingesting a trace as ordinary turns loses precisely the fields the questions are about — the
// kind of each step and the chain between them — so a benchmark whose entire subject is causality
// would be entered with a corpus that contains no causes.
//
// AMA-Bench's own SOTA states the recipe: make the trajectory first-class and retrieve
// graph-first. This section supplies the shape and the ingest rendering. The causal edge kinds
// that consume it live in the engine (KG lane), which is why the step kind is carried as data
// rather than inferred by the reader — the two lanes meet at this type.

/// What one step of a trajectory **is**.
///
/// First-class rather than a text prefix, because "which action caused the failure" cannot be
/// answered over a corpus where every step is a string beginning `"Action: "`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StepKind {
    State,
    Action,
    Observation,
    ToolOutput,
    Plan,
    Outcome,
}

impl StepKind {
    pub const ALL: [StepKind; 6] = [
        StepKind::State,
        StepKind::Action,
        StepKind::Observation,
        StepKind::ToolOutput,
        StepKind::Plan,
        StepKind::Outcome,
    ];

    /// Parse a step kind.
    ///
    /// **Refuses an unrecognised kind** rather than defaulting to `Observation`, matching the
    /// BEAM ability loader: a renamed or newly-introduced kind must be loud. Silently typing an
    /// action as an observation would move the causal chain without changing any count, which is
    /// the kind of error that surfaces as a plausible score.
    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace([' ', '-'], "_")
                .as_str()
            {
                "state" | "context" | "situation" => Self::State,
                "action" | "act" | "command" | "tool_call" => Self::Action,
                "observation" | "obs" | "result" | "feedback" => Self::Observation,
                "tool_output" | "tool_result" | "output" => Self::ToolOutput,
                "plan" | "thought" | "reasoning" | "reflection" => Self::Plan,
                "outcome" | "reward" | "final" | "score" => Self::Outcome,
                _ => return None,
            },
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Action => "action",
            Self::Observation => "observation",
            Self::ToolOutput => "tool_output",
            Self::Plan => "plan",
            Self::Outcome => "outcome",
        }
    }

    /// Whether a step of this kind can *cause* a later one.
    ///
    /// The three agent-side kinds can; state, environment observations and the terminal outcome
    /// are things that happen *to* the agent. This is the predicate a causal-edge construction
    /// reads, kept here so "cause-capable" is one definition rather than a `matches!` repeated
    /// in three places — the mistake that once let two fusion arms silently change meaning
    /// between arena runs.
    pub fn is_cause_capable(&self) -> bool {
        matches!(self, Self::Action | Self::Plan | Self::ToolOutput)
    }
}

/// One step of a trajectory, with its provenance.
#[derive(Clone, Debug)]
pub struct TrajectoryStep {
    pub kind: StepKind,
    pub text: String,
    /// Position within its own trajectory — the causal order the trace recorded. Kept because
    /// "which came first" is half of every ordering question these benchmarks ask.
    pub index: usize,
    /// The dataset's **own step clock** for this step, when it ships one (AMA-Bench's `turn_idx`).
    ///
    /// This is the number the questions quote, and that is the whole reason it is a field. AMA-Bench
    /// asks about "the action at Step 8" and "the observation from Step 6", and those numbers are
    /// `turn_idx` — not positions in the flattened step list. One turn produces *two* steps here (the
    /// action the agent chose, the observation the environment returned), so the turn cannot be the
    /// id — `index` stays unique — but if the reader is never shown the turn then the question's own
    /// vocabulary appears nowhere in the evidence it is asked to search. `None` for every shape that
    /// ships no clock, which keeps their rendering byte-identical.
    pub turn: Option<i64>,
    pub at: Option<i64>,
    pub session: Option<String>,
}

impl TrajectoryStep {
    /// The document text this step is ingested as.
    ///
    /// The kind appears as a **prefix here as well as in the type**, deliberately: the structured
    /// kind is what the harness and the graph read, and this rendering exists so a purely lexical
    /// arm can still tell an action from an observation — the cheapest useful thing to preserve
    /// when the arm under measurement has no graph.
    pub fn render(&self) -> String {
        // The visible step number is the **turn the dataset wrote** when it ships one, and the
        // positional index otherwise. A question that says "at Step 8" must be able to find a line
        // in the evidence that says step 8; before this, AMA's observation for turn 7 was rendered
        // as `step 107` (a synthetic id) and every step-reference in every question pointed at the
        // wrong line. Falls back to `index` so every dataset that ships no clock renders exactly as
        // it did before.
        let step_no = self.turn.unwrap_or(self.index as i64);
        match &self.session {
            Some(s) => format!("{s} step {step_no} {}: {}", self.kind.name(), self.text),
            None => format!("step {step_no} {}: {}", self.kind.name(), self.text),
        }
    }
}

/// A full trajectory plus its terminal outcome.
#[derive(Clone, Debug)]
pub struct Trajectory {
    pub task_id: String,
    pub steps: Vec<TrajectoryStep>,
    /// The trace's own statement of how it ended, when it carries one.
    pub outcome: Option<String>,
    /// Ground-truth success, when the dataset provides it. `None` is **not** `false`: a trace
    /// with no recorded outcome must not be counted as a failure.
    pub success: Option<bool>,
    /// Causal edges **the dataset states**. Empty when it states none, in which case
    /// [`Trajectory::causal_edges`] derives them from the step kinds instead. Kept as a field
    /// rather than computed on demand because the two sources are not interchangeable — an
    /// annotated `Prevents` cannot be derived, and silently mixing derived edges into an
    /// annotated trace would attribute causes the benchmark never claimed.
    pub causal: Vec<CausalEdge>,
    /// Turn-shaped steps the dataset shipped **empty on both sides** — AMA-Bench's `spider2`
    /// episodes open with two `{turn_idx, action: null, observation: null}` placeholders (102 of
    /// its 14 944 steps). They carry no text under any key, so there is nothing to index and they
    /// are skipped rather than turned into empty documents.
    ///
    /// Counted rather than dropped silently because the count is the only evidence that the loader
    /// read the file it thinks it did: a run whose placeholder count is 0 is either a different
    /// dataset version or a shape this loader no longer recognises, and both change what the score
    /// means.
    pub empty_turns: usize,
}

/// What one causal edge **means**.
///
/// The three kinds AMA-Bench's winning recipe is built on, and they are not interchangeable: a
/// system that collapses them into one "related" edge can retrieve the right steps and still
/// answer "what caused the failure" wrong, because *enabled* and *prevented* point in opposite
/// directions from the same antecedent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CausalKind {
    Causes,
    Enables,
    Prevents,
}

impl CausalKind {
    pub const ALL: [CausalKind; 3] = [Self::Causes, Self::Enables, Self::Prevents];

    /// Parse a causal kind.
    ///
    /// **Refuses an unrecognised kind** rather than defaulting to `Causes`, matching
    /// [`StepKind::parse`]. `Causes` is the wrong default in both directions here: it invents a
    /// causal claim the record did not make, and it hides a `Prevents`, which is the one kind
    /// whose semantics invert.
    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace([' ', '-'], "_")
                .as_str()
            {
                "causes" | "cause" | "caused" | "causality" | "leads_to" => Self::Causes,
                "enables" | "enable" | "enabled" | "facilitates" | "supports" => Self::Enables,
                "prevents" | "prevent" | "prevented" | "blocks" | "inhibits" => Self::Prevents,
                _ => return None,
            },
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Causes => "causes",
            Self::Enables => "enables",
            Self::Prevents => "prevents",
        }
    }

    /// The relation kind written into the graph.
    ///
    /// One definition, read by the ingest side and the traversal side alike — a causal edge a
    /// reader cannot follow is not a capability, it is a label. The engine stores relation kinds as
    /// free strings (`add_relation(.., kind: &str, ..)`), so the vocabulary lives here rather than
    /// in an engine enum: this lane owns it, and neither lane has to wait for the other.
    pub fn relation_kind(&self) -> &'static str {
        self.name()
    }

    /// Whether a relation kind coming **out of the graph** is one of ours.
    ///
    /// Parses rather than string-compares, so an alias the loader accepts is not invisible to the
    /// traversal that has to follow it — the failure mode that makes an alias half-supported.
    pub fn from_relation_kind(kind: &str) -> Option<Self> {
        Self::parse(kind)
    }

    /// Whether this kind can ever be **derived** from step kinds alone. Only `Prevents` cannot:
    /// a prevented alternative leaves no step of its own in the trace, so inferring it would mean
    /// inventing an event. It is read from the dataset or not claimed at all.
    pub fn is_derivable(&self) -> bool {
        matches!(self, Self::Causes | Self::Enables)
    }
}

/// Whether a relation kind names a causal edge — the predicate a graph traversal filters on.
///
/// Exposed as a free function as well as a method because the retrieval side holds a `&str` off a
/// [`Relation`] row and should not have to construct the enum to ask the question.
pub fn is_causal_relation_kind(kind: &str) -> bool {
    CausalKind::parse(kind).is_some()
}

/// One causal edge: `from` ⟶ `to`, both **indices into `Trajectory::steps`**.
///
/// Indices rather than ids: the steps are not ingested as entities yet, and a stable per-trace
/// index is the only identity a trace step has. Resolving them to vault entity ids is the
/// ingest's job, and keeping the mismatch explicit here is what stops a step index being mistaken
/// for a global id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CausalEdge {
    pub from: usize,
    pub to: usize,
    pub kind: CausalKind,
}

/// Derive causal edges from step kinds alone.
///
/// This is the caller [`StepKind::is_cause_capable`] was written for. The rule, stated because a
/// causal graph whose rule is implicit cannot be argued with:
///
/// - a `Plan` **enables** the cause-capable steps that follow it until the next plan — a plan
///   schedules specific actions, not the whole remainder of the trace, and treating the remainder
///   as its consequence is how a causal graph quietly becomes a chronological one;
/// - a cause-capable step (action, plan, tool output) **causes** each following step that is not
///   itself cause-capable (state, observation, outcome) — the nearer cause wins, so a later action
///   displaces an earlier one rather than both claiming the same effect;
/// - `Prevents` is **never** derived. A prevented alternative leaves no step behind to point at,
///   so any `Prevents` edge here would be an invented event.
///
/// The result is a chain, not a clique: an effect has at most one direct cause, which is what
/// makes "which action caused the failure" answerable by walking backwards.
pub fn derive_causal_edges(steps: &[TrajectoryStep]) -> Vec<CausalEdge> {
    let mut out: Vec<CausalEdge> = Vec::new();
    let mut plan: Option<usize> = None;
    // The cause-capable step currently awaiting an effect. Replaced, never cleared, so a trace of
    // one action followed by three observations links all three to that action.
    let mut cause: Option<usize> = None;
    for (i, s) in steps.iter().enumerate() {
        if s.kind == StepKind::Plan {
            plan = Some(i);
            cause = Some(i);
            continue;
        }
        if s.kind.is_cause_capable() {
            if let Some(p) = plan
                && p != i
            {
                out.push(CausalEdge {
                    from: p,
                    to: i,
                    kind: CausalKind::Enables,
                });
            }
            cause = Some(i);
            continue;
        }
        if let Some(c) = cause {
            out.push(CausalEdge {
                from: c,
                to: i,
                kind: CausalKind::Causes,
            });
        }
    }
    out
}

/// Read the causal edges a record states, if any.
///
/// Tolerant about the field name and the endpoint spelling (three datasets, three conventions),
/// strict about the kind. An edge missing an endpoint or carrying an unrecognised kind is
/// **dropped** rather than defaulted — a guessed causal edge is a claim the dataset did not make,
/// and dropping it leaves the trace to be derived structurally instead, which is at least a rule
/// we can state.
fn parse_causal_edges(item: &Value) -> Vec<CausalEdge> {
    let raw = item
        .get("causal_edges")
        .or_else(|| item.get("causal"))
        .or_else(|| item.get("causal_relations"));
    let Some(arr) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<CausalEdge> = Vec::new();
    for e in arr {
        let from = e
            .get("from")
            .or_else(|| e.get("source"))
            .or_else(|| e.get("cause"))
            .and_then(int_or_string);
        let to = e
            .get("to")
            .or_else(|| e.get("target"))
            .or_else(|| e.get("effect"))
            .and_then(int_or_string);
        let kind = e
            .get("kind")
            .or_else(|| e.get("type"))
            .or_else(|| e.get("relation"))
            .and_then(scalar_string)
            .and_then(|k| CausalKind::parse(&k));
        if let (Some(from), Some(to), Some(kind)) = (from, to, kind)
            && from >= 0
            && to >= 0
        {
            out.push(CausalEdge {
                from: from as usize,
                to: to as usize,
                kind,
            });
        }
    }
    out
}

impl Trajectory {
    /// The causal edges for this trace — the record's own when it states them, derived from the
    /// step kinds when it does not.
    ///
    /// Deliberately **not** a union. Merging the two would let our derivation add an edge to a
    /// trace that already declared its causal structure, and the resulting number could not be
    /// attributed to either source.
    pub fn causal_edges(&self) -> Vec<CausalEdge> {
        if self.causal.is_empty() {
            derive_causal_edges(&self.steps)
        } else {
            self.causal.clone()
        }
    }

    /// True when [`Trajectory::causal_edges`] returns the dataset's edges rather than our
    /// derivation — reported so a score is never quoted without knowing which produced it.
    pub fn causal_is_annotated(&self) -> bool {
        !self.causal.is_empty()
    }

    /// The `(from, to, kind)` triples to write with `MemoryEngine::add_relation`.
    ///
    /// This is the join between the two lanes: this lane constructs the vocabulary and the
    /// traversal filter, and the engine stores the edge without either side owning the other's
    /// type.
    pub fn causal_relations(&self) -> Vec<(usize, usize, &'static str)> {
        self.causal_edges()
            .into_iter()
            .map(|e| (e.from, e.to, e.kind.relation_kind()))
            .collect()
    }
}

/// Read an integer that may arrive as a number or as a numeric string.
fn int_or_string(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
}

/// Parse a trajectory record from the shapes these benchmarks ship.
///
/// Tolerant about *where* the steps live (`steps`, `trajectory`, `episode`, or a bare array), and
/// about whether a step is an object with a `kind`, an object carrying AMA-Bench's
/// `{action, observation}` pair, or a string whose kind is a prefix — because each of the three
/// datasets does it differently. Strict about the kind itself: an unknown kind is an error, so a
/// schema change is loud instead of silently re-typing the causal chain.
///
/// | dataset | step shape | becomes |
/// |---|---|---|
/// | MemoryArena / generic | `{kind, text}` or `"action: ls"` | one step |
/// | **AMA-Bench** | `{turn_idx, action, observation}` | **two steps** — `Action`, then `Observation` |
/// | **AMA-Bench placeholder** | `{turn_idx, action: null, observation: null}` | zero steps, counted in `empty_turns` |
/// | scalar | `"text"` | one `Observation` step |
///
/// The AMA-Bench expansion is the important one and it is not cosmetic: that benchmark's questions
/// ask what the agent *did* and why the environment answered as it did, and its winning recipe
/// retrieves the action that caused an observation. Collapsing the pair into one step would keep
/// every word and destroy exactly the structure the retrieval reads.
pub fn load_trajectory_record(item: &Value, idx: usize) -> Result<Trajectory> {
    // The record's own identity, in the order the three datasets spell it. **`episode_id` matters
    // for AMA-Bench specifically**: its result file is keyed by that number
    // (`{"episode_id": …, "answer_list": […], "reasoning_trace": …}`), so a group named
    // anything else cannot be joined back to a submission or to their judge.
    let task_id = item
        .get("task_id")
        .or_else(|| item.get("id"))
        .or_else(|| item.get("instance_id"))
        .or_else(|| item.get("episode_id"))
        .and_then(scalar_string)
        .unwrap_or_else(|| format!("traj-{idx}"));
    let raw = item
        .get("steps")
        .or_else(|| item.get("trajectory"))
        .or_else(|| item.get("episode"))
        .and_then(Value::as_array)
        .or_else(|| item.as_array())
        .context("trajectory record carries no steps/trajectory/episode array")?;

    // **A turn-shaped record yields two steps, and the second one needs its own number.**
    // AMA-Bench ships `{turn_idx, action, observation}` — one object carrying two facts of
    // *different kinds* — so the element index is not unique among the steps it produces. The
    // observation is therefore numbered above every element index (`raw.len() + index`): unique for
    // any record, and unable to collide with a dataset-annotated causal edge, which addresses
    // steps by the element index the dataset wrote. The rendering the reader sees is unchanged in
    // every other shape, so a trace that does not expand is byte-identical to before.
    let pair_offset = raw.len();
    let mut steps = Vec::with_capacity(raw.len());
    let mut empty_turns = 0usize;
    for (index, step) in raw.iter().enumerate() {
        let mut at = None;
        let mut session = None;
        // One element can produce two steps; everything else produces one.
        let mut produced: Vec<(usize, StepKind, String, Option<i64>)> = Vec::new();
        match step {
            Value::String(s) => {
                let (kind, text) = split_kind_prefix(s).with_context(|| {
                    format!("trajectory step {index} has no recognisable '<kind>:' prefix")
                })?;
                produced.push((index, kind, text, None));
            }
            Value::Object(_) => {
                at = step
                    .get("at")
                    .or_else(|| step.get("timestamp"))
                    .and_then(int_or_string);
                session = step
                    .get("session")
                    .or_else(|| step.get("session_id"))
                    .and_then(scalar_string);
                let kind_raw = step
                    .get("kind")
                    .or_else(|| step.get("type"))
                    .or_else(|| step.get("role"))
                    .and_then(scalar_string);
                let action = step.get("action").and_then(scalar_string);
                let observation = step.get("observation").and_then(scalar_string);
                // **The shape test reads the KEYS, not the values.** AMA-Bench ships placeholder
                // turns whose `action` and `observation` are both `null` (spider2's opening two, in
                // all 51 of its episodes). Testing `action.is_some() || observation.is_some()`
                // therefore sent those turns down the *other* branch and the whole file failed to
                // load with "carries no kind/type/role" — a shape test that fires on the values
                // turns a valid dataset into an unreadable one. Keying on the presence of any of the
                // three AMA keys recognises the shape whatever the values are, and an empty turn
                // then contributes zero steps instead of an error.
                let carries_ama_keys = step.get("action").is_some()
                    || step.get("observation").is_some()
                    || step.get("turn_idx").is_some();
                if kind_raw.is_none() && carries_ama_keys {
                    // **AMA-Bench's shape.** A single step typed `Observation` (or `Action`) would
                    // lose the distinction the agentic spine is built on: an action is what the
                    // agent chose, an observation is what the environment returned, and the whole
                    // retrieval recipe here is "find the action that caused this observation".
                    // `turn_idx`, when present, is the turn's own clock — kept so a turn's pair
                    // cannot drift apart in a temporal re-sort.
                    let turn = step.get("turn_idx").and_then(int_or_string);
                    if at.is_none() {
                        at = turn;
                    }
                    if let Some(a) = action.filter(|a| !a.trim().is_empty()) {
                        produced.push((index, StepKind::Action, a, turn));
                    }
                    if let Some(o) = observation.filter(|o| !o.trim().is_empty()) {
                        produced.push((pair_offset + index, StepKind::Observation, o, turn));
                    }
                    if produced.is_empty() {
                        // Both sides empty (or whitespace): a placeholder turn. Counted, skipped.
                        empty_turns += 1;
                    }
                } else {
                    let kind_raw = kind_raw.with_context(|| {
                        format!("trajectory step {index} carries no kind/type/role")
                    })?;
                    let kind = StepKind::parse(&kind_raw).with_context(|| {
                        format!("trajectory step {index} has unknown kind `{kind_raw}`")
                    })?;
                    let text = step
                        .get("text")
                        .or_else(|| step.get("content"))
                        .or_else(|| step.get("observation"))
                        .or_else(|| step.get("message"))
                        .and_then(scalar_string)
                        .unwrap_or_default();
                    produced.push((index, kind, text, None));
                }
            }
            // A bare scalar step has no kind to lose, so it is read as the environment's own
            // voice rather than dropped — dropping it would silently shorten every trace.
            other => produced.push((
                index,
                StepKind::Observation,
                scalar_string(other).unwrap_or_default(),
                None,
            )),
        };
        for (step_index, kind, text, turn) in produced {
            if text.trim().is_empty() {
                continue;
            }
            steps.push(TrajectoryStep {
                kind,
                text,
                index: step_index,
                turn,
                at,
                session: session.clone(),
            });
        }
    }
    if steps.is_empty() {
        anyhow::bail!("trajectory {task_id} produced no usable steps");
    }
    Ok(Trajectory {
        task_id,
        steps,
        causal: parse_causal_edges(item),
        outcome: item.get("outcome").and_then(scalar_string),
        success: item.get("success").and_then(Value::as_bool),
        empty_turns,
    })
}

/// Split `"action: ls -la"` into its kind and its text.
///
/// Only the first `:` is considered, and only when the text before it parses as a kind — so
/// `"Note: the agent failed"` is not silently turned into a `State` step, and a command
/// containing a colon (`"action: curl http://x"`) keeps the rest intact.
pub(crate) fn split_kind_prefix(s: &str) -> Option<(StepKind, String)> {
    let (head, rest) = s.split_once(':')?;
    let kind = StepKind::parse(head)?;
    Some((kind, rest.trim().to_string()))
}

/// Recover a step's kind from the **rendered** document text it was ingested as.
///
/// [`TrajectoryStep::render`] is the only thing that survives into the vault, so a caller that
/// needs the step kinds again *after* ingest — writing the causal edges, for instance — has to read
/// them back out of that rendering rather than from the `TrajectoryStep` it no longer holds.
///
/// Strict on purpose, and delegated to [`parse_rendered_header`] so the kind and the number are read
/// by one parser: a document that merely *mentions* a kind cannot be mistaken for a step, and an
/// unrecognised kind returns `None` rather than defaulting to `Observation` — the same refusal
/// `StepKind::parse` makes, kept here so the recovery path cannot be the one place that silently
/// re-types a causal chain.
///
/// `(step number, kind)` from a **rendered** step header — the one parser both recoverers share.
///
/// **Why one parser and not two.** These two facts were read back from the vault by two separate
/// functions, and they diverged exactly as the earlier note warned they would: both searched for the
/// literal `" step "` — with a leading space — which only matches [`TrajectoryStep::render`]'s
/// `Some(session)` arm. AMA-Bench's steps carry no `session`/`session_id` key, so `session` is `None`,
/// so its documents render as `"step 20 action: …"` and **neither function matched anything**. The
/// symptom was not an error: `kind_from_rendered` returning `None` for every document makes
/// `build_group_causal_edges` take its `steps.len() < 2` early return, so an ability the loader
/// advertises (AMA's causal edges) ran, produced nothing, and counted nothing.
///
/// So the shape is parsed from the **end of the header instead of the start of the line**: take
/// everything before the first `": "`, then read the last three whitespace-separated tokens as
/// `… step {n} {kind}`. That is independent of any prefix, which is the property that matters — the
/// session prefix is optional by construction and a parser that assumes it is not is a parser that
/// silently does nothing on half the corpora.
///
/// Strict on the parts that matter, so a *mention* of a step cannot be read as one:
/// * the `": "` is required, so prose with no kind field cannot match;
/// * the last token must be a recognised kind — `"run step 3"` fails there;
/// * the token before the number must be the literal `step`;
/// * the number must be an integer. `"step 1 observation: the command printed step 3"` yields **1**:
///   only the header before the first colon is considered, so the mention inside the observation is
///   never mistaken for the step being described.
pub(crate) fn parse_rendered_header(text: &str) -> Option<(i64, StepKind)> {
    let (head, _) = text.split_once(": ")?;
    let mut parts = head.split_whitespace().rev();
    let kind = StepKind::parse(parts.next()?)?;
    let index = parts.next()?.parse::<i64>().ok()?;
    // `"step"` itself, so `"5 action: …"` is not a step header.
    if parts.next()? != "step" {
        return None;
    }
    Some((index, kind))
}

pub fn kind_from_rendered(text: &str) -> Option<StepKind> {
    parse_rendered_header(text).map(|(_, kind)| kind)
}

/// The step number a rendered document **shows**, read back out of the ingested text.
///
/// [`TrajectoryStep::render`] prints `self.turn.unwrap_or(self.index)` — the dataset's own clock when
/// it ships one, the positional index otherwise — and AMA-Bench's questions quote exactly that number
/// (`"the observation from Step 6"`). The document *id* carries the positional index instead, and the
/// two differ by construction: one turn produces two steps (the action and the observation), so the
/// observation of turn 7 is `index = 2*7+1` while rendering as `step 7`.
///
/// That divergence is why routing a step reference by **document id** cannot work and why this reads
/// the number off the text instead — the same reasoning [`kind_from_rendered`] uses for kinds, and the
/// same one `write_group_causal_edges` uses to rebuild structure after ingest: the rendering is the
/// only thing that survives into the vault, so it is what has to be parsed.
pub fn rendered_step_no(text: &str) -> Option<i64> {
    parse_rendered_header(text).map(|(n, _)| n)
}

/// Success rate — MemoryArena's and AMA-Bench's headline number.
///
/// `None` on an empty set rather than 0.0, so a run that evaluated nothing cannot print a figure.
/// An unscored trace is excluded by the caller rather than counted as a failure: a benchmark that
/// reports outcomes only for a subset must not have the rest silently counted against us.
pub fn success_rate(successes: &[bool]) -> Option<f64> {
    if successes.is_empty() {
        return None;
    }
    let won = successes.iter().filter(|s| **s).count();
    Some(won as f64 / successes.len() as f64)
}

/// Selective-forgetting score for MemoryAgentBench: of the memories that **should** have been
/// forgotten, how many were — minus the ones wrongly kept.
///
/// Both halves are required or the metric is trivially gamed. A system that forgets everything
/// scores 1.0 on the first half and is useless; one that forgets nothing scores 0.0 on it and
/// loses nothing. The difference is the score, and the two component rates are reported
/// alongside so a zero cannot hide which half is failing.
pub fn selective_forgetting_score(
    should_forget: usize,
    forgotten_correctly: usize,
    kept_wrongly: usize,
) -> Option<f64> {
    if should_forget == 0 {
        return None;
    }
    Some((forgotten_correctly as f64 - kept_wrongly as f64) / should_forget as f64)
}

// ============================================================================
// Step-reference pinning — AMA-Agent's Stage 1b, ported
// ============================================================================
//
// WHY THIS EXISTS. AMA-Bench's questions name steps: *"At step 71, what did the agent do?"*,
// *"Between step 71 and step 76, …"*, *"which actions did the agent take by step 26?"*. The
// reference system (AMA-Agent, 57.22% average) has a dedicated retrieval stage for exactly this
// and it is the first thing its `memory_retrieve` does after similarity search
// (`src/method/ama_agent_core/retrieve.py`, *Stage 1b: Pin turns explicitly referenced by
// step/turn number*), with this rationale in its own synthesis step:
//
//   "The compressed state memory often disagrees with the raw turn — for point-lookup questions
//    ('at step N, what action…?') the raw turn is authoritative."
//
// Ours has no such stage. It has a *cue list* (`looks_like_ordering_question`) that decides whether
// to add an ordering sentence to the prompt, which is a different job: it changes what the reader is
// told, not what the reader is given. So a question quoting "step 71" competes on lexical overlap
// against every other row in the episode, and when it loses, the row that holds the answer is
// simply absent. The measured symptom is AMA's `spider2` class at 25% (9/36) with a *diagnosable*
// n, and the diagnosis was step references (BENCH-BUS §3).
//
// This is a port of their extractor, not a guess at one. Their forms, all supported here:
//
//   single  "step 5" / "turn 5"
//   range   "between step 5 and step 10" · "from step 5 to step 10" · "steps 5 to 10" ·
//            "steps 5-10" · "steps 5–10" · "turns 5..10"
//   prefix  "by step 26" · "until step 26" · "up to step 26" · "through step 26" ·
//            "before step 26"  ->  0..=N, **only** when the trajectory's own maximum is known,
//            because otherwise the upper bound of "by step 26" is unrepresentable
//   list    "steps 3, 7, 12" · "steps 3 and 7"
//
// Hand-rolled deliberately: this crate has no `regex` dependency and adding one is a workspace
// dependency change for a function that is a hundred lines of character scanning.

/// A token of the little scanner, enough to see connectors between numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepTok<'a> {
    Word(&'a str),
    Num(i64),
    /// `-` or `–`, which is a range inside `steps 5-10`.
    Dash,
    /// `..`, the range form in `turns 5..10`.
    DotDot,
    Comma,
    /// A clause boundary (`.` `?` `!` `;` `:`), so "step 5. What happened" cannot join into a range.
    Stop,
}

fn step_tokens(question: &str) -> Vec<StepTok<'_>> {
    let mut toks = Vec::new();
    let bytes = question.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                i += 1;
            }
            if let Ok(n) = question[start..i].parse::<i64>() {
                toks.push(StepTok::Num(n));
            }
            continue;
        }
        if c.is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && (bytes[i] as char).is_ascii_alphabetic() {
                i += 1;
            }
            toks.push(StepTok::Word(&question[start..i]));
            continue;
        }
        match c {
            '-' | '\u{2013}' | '\u{2014}' => toks.push(StepTok::Dash),
            '.' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'.' {
                    toks.push(StepTok::DotDot);
                    i += 2;
                    continue;
                }
                toks.push(StepTok::Stop);
            }
            '?' | '!' | ';' | ':' => toks.push(StepTok::Stop),
            ',' => toks.push(StepTok::Comma),
            _ => {}
        }
        i += 1;
    }
    toks
}

fn is_step_word(w: &str) -> bool {
    matches!(w, "step" | "steps" | "turn" | "turns")
}

const STEP_PIN_CAP: usize = 256;

/// Every step/turn number `question` explicitly references.
///
/// `max_turn` is the trajectory's own highest `turn` when the dataset ships one. It is required only
/// by the prefix forms: *"by step 26"* means `0..=26` when 26 is a legal step and is unrepresentable
/// when the maximum is unknown, so with `None` the prefix cue falls back to the literal `0..=26`
/// clamped later by whichever steps exist.
///
/// The cap is not cosmetic. A question containing "steps 1-100000" would otherwise allocate a
/// hundred thousand entries in the retrieval path, which is a denial-of-service in a benchmark
/// harness that reads public data.
/// What a connector between two step numbers *means*.
///
/// The distinction is load-bearing and the wording alone cannot make it: `and` is a **list**
/// connector in "steps 3 and 7 were run" and a **range** connector in "between step 71 and step 76".
/// The introducer is the only thing that separates the two readings, which is why the connector's kind
/// and `range_mode` are consulted *together* below.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conn {
    List,
    Range,
}

pub fn referenced_steps(question: &str, max_turn: Option<i64>) -> Vec<i64> {
    let lower = question.to_ascii_lowercase();
    let toks = step_tokens(&lower);
    let mut out: Vec<i64> = Vec::new();
    let push = |n: i64, out: &mut Vec<i64>| {
        if n >= 0 && !out.contains(&n) && out.len() < STEP_PIN_CAP {
            out.push(n);
        }
    };

    let mut i = 0usize;
    while i < toks.len() {
        let StepTok::Word(w) = toks[i] else {
            i += 1;
            continue;
        };
        if !is_step_word(w) {
            i += 1;
            continue;
        }
        // The introducer's own cue: "between step 5 and …" and "by step 26" both put the cue
        // *before* the step word, so it has to be read from the tokens already passed.
        let mut range_mode = false;
        let mut prefix_mode = false;
        if i > 0
            && let StepTok::Word(p) = toks[i - 1]
        {
            range_mode = matches!(p, "between" | "from");
            prefix_mode = matches!(p, "by" | "until" | "through" | "before");
            if p == "to"
                && i > 1
                && let StepTok::Word(pp) = toks[i - 2]
            {
                prefix_mode = pp == "up";
            }
        }

        // Collect the numbers and connectors that follow, up to the clause boundary.
        let mut nums: Vec<i64> = Vec::new();
        let mut joined_by_range = false;
        let mut pending: Option<Conn> = None;
        let mut j = i + 1;
        while j < toks.len() {
            match toks[j] {
                StepTok::Num(n) => {
                    // `and` only joins a span when the introducer asked for a span; otherwise it
                    // joins a list, and expanding a list into a span would pin every step between two
                    // numbers the question merely enumerated side by side.
                    if let Some(kind) = pending.take()
                        && !nums.is_empty()
                        && (kind == Conn::Range || (range_mode && kind == Conn::List))
                    {
                        joined_by_range = true;
                    }
                    nums.push(n);
                }
                StepTok::Comma => pending = Some(Conn::List),
                StepTok::Dash | StepTok::DotDot => pending = Some(Conn::Range),
                StepTok::Word("and" | "or") => pending = Some(Conn::List),
                StepTok::Word("to" | "through" | "until" | "than") => pending = Some(Conn::Range),
                // Another step word normally ends this reference and starts its own **except** under a
                // range introducer, where the upper bound is spelled with one: breaking at
                // "between step 71 **and step** 76" left `nums` as the single value 71, so the span was
                // read as its two loose endpoints and the sixteen steps between them — the ones such a
                // question is actually about — were never pinned.
                StepTok::Word(c) if is_step_word(c) => {
                    if !range_mode {
                        break;
                    }
                }
                StepTok::Stop => break,
                StepTok::Word(_) if !nums.is_empty() => break,
                StepTok::Word(_) => {}
            }
            j += 1;
        }
        i = j.max(i + 1);

        if nums.is_empty() {
            continue;
        }
        if range_mode && nums.len() >= 2 {
            let (mut lo, mut hi) = (nums[0], nums[nums.len() - 1]);
            if lo > hi {
                std::mem::swap(&mut lo, &mut hi);
            }
            for n in lo..=hi.min(lo.saturating_add(STEP_PIN_CAP as i64)) {
                push(n, &mut out);
            }
        } else if prefix_mode {
            let hi = match max_turn {
                Some(m) => nums[0].min(m),
                None => nums[0],
            };
            for n in 0..=hi.max(0) {
                if out.len() >= STEP_PIN_CAP {
                    break;
                }
                push(n, &mut out);
            }
        } else if joined_by_range && nums.len() >= 2 {
            let (mut lo, mut hi) = (nums[0], nums[nums.len() - 1]);
            if lo > hi {
                std::mem::swap(&mut lo, &mut hi);
            }
            for n in lo..=hi.min(lo.saturating_add(STEP_PIN_CAP as i64)) {
                push(n, &mut out);
            }
        } else {
            for n in nums {
                push(n, &mut out);
            }
        }
    }
    out.sort_unstable();
    out
}

/// The label AMA-Agent puts above pinned turns, kept verbatim so a diff against their pipeline is
/// readable — including the parenthetical, which is the instruction that does the work.
const PINNED_TURNS_HEADER: &str =
    "# Pinned Turns (referenced explicitly in the question — TRUST THIS OVER STATE MEMORY)";

impl Trajectory {
    /// The steps this question names, rendered whole, as a labelled section.
    ///
    /// `None` when the question names none, or names only steps the trajectory does not contain —
    /// so a caller can prepend the result unconditionally without inventing an empty section, and a
    /// question that references nothing is byte-identical to before this existed.
    ///
    /// **Whole text, no truncation.** That is the reference's choice and it is load-bearing: the
    /// pinned turns exist precisely for point-lookup questions whose answer is a detail
    /// ("at step 71, look"), and a truncated observation is the case where the detail is what gets
    /// cut. Their `_format_chunks(pinned_sorted, max_obs_chars=20000)` is the same idea with a
    /// generous per-item cap; this uses the step's own `render()`, which is what the corpus holds,
    /// so the pinned section cannot disagree with the indexed text.
    pub fn pinned_section(&self, question: &str) -> Option<String> {
        let max_turn = self.steps.iter().filter_map(|s| s.turn).max();
        let wanted = referenced_steps(question, max_turn);
        if wanted.is_empty() {
            return None;
        }
        let mut hits: Vec<&TrajectoryStep> = self
            .steps
            .iter()
            .filter(|s| {
                // Match on the number the dataset wrote when it ships one (that is the number the
                // question quotes), and on the positional index otherwise — the same fallback
                // `render()` uses, so the pinned text always contains the number the question used.
                let n = s.turn.unwrap_or(s.index as i64);
                wanted.contains(&n)
            })
            .collect();
        if hits.is_empty() {
            return None;
        }
        hits.sort_by_key(|s| s.turn.unwrap_or(s.index as i64));
        let mut body = String::new();
        for s in hits {
            body.push_str(&s.render());
            body.push('\n');
        }
        Some(format!("{PINNED_TURNS_HEADER}\n{body}"))
    }
}

#[cfg(test)]
mod step_reference_tests {
    use super::*;

    fn steps(q: &str, max: Option<i64>) -> Vec<i64> {
        referenced_steps(q, max)
    }

    #[test]
    fn every_step_reference_form_ama_agent_handles_is_extracted() {
        // The five forms, each taken from the shapes AMA's own questions use.
        assert_eq!(steps("What did the agent do at step 71?", None), vec![71]);
        assert_eq!(steps("What happened at turn 4", None), vec![4]);
        assert_eq!(
            steps("Between step 71 and step 76, what did the agent do?", None),
            (71..=76).collect::<Vec<i64>>()
        );
        assert_eq!(
            steps("From step 5 to step 8, describe the actions", None),
            (5..=8).collect::<Vec<i64>>()
        );
        assert_eq!(
            steps("List the actions in steps 5-8", None),
            (5..=8).collect::<Vec<i64>>()
        );
        assert_eq!(
            steps("Summarise turns 5..8", None),
            (5..=8).collect::<Vec<i64>>()
        );
        assert_eq!(
            steps("Steps 3, 7, 12 were run — what were they?", None),
            vec![3, 7, 12]
        );
        assert_eq!(steps("What did steps 3 and 7 do?", None), vec![3, 7]);
        // Prefix form: bounded by the trajectory when the maximum is known, literal when not.
        assert_eq!(
            steps("What had the agent accomplished by step 26?", Some(40)),
            (0..=26).collect::<Vec<i64>>()
        );
        assert_eq!(
            steps("Which tasks were finished until step 3?", Some(40)),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            steps("up to step 4", Some(40)),
            (0..=4).collect::<Vec<i64>>()
        );
        // `by step 26` when the trajectory ends at 10 clamps, because 11..26 do not exist.
        assert_eq!(
            steps("by step 26", Some(10)),
            (0..=10).collect::<Vec<i64>>()
        );
    }

    #[test]
    fn a_question_that_names_no_step_extracts_nothing_and_a_long_span_is_capped() {
        // The load-bearing negatives: these must not pin anything, or the section would be added to
        // every agentic question and the clause would stop meaning anything.
        for q in [
            "Summarise the whole trajectory.",
            "Which command failed and why?",
            "How many steps did the agent take?",
            "What was the final outcome of the task?",
        ] {
            assert!(steps(q, Some(100)).is_empty(), "must extract nothing: {q}");
        }
        // "How many steps" names the word but no number; a bare "2" elsewhere in the sentence about
        // a different noun must not become a step reference.
        assert!(steps("The agent retried 2 times. Summarise it.", Some(100)).is_empty());
        // A clause boundary stops a range: "step 5. What happened at step 9" is two references, not
        // the span 5..9.
        assert_eq!(
            steps("At step 5. What happened at step 9?", None),
            vec![5, 9]
        );
        // The DoS guard: a span wider than the cap is clipped, never expanded in full.
        assert!(steps("steps 1-100000", None).len() <= STEP_PIN_CAP);
    }

    #[test]
    fn a_pinned_section_carries_the_number_the_question_quoted_and_nothing_else() {
        let traj = Trajectory {
            task_id: "t".into(),
            steps: vec![
                TrajectoryStep {
                    kind: StepKind::Action,
                    text: "look".into(),
                    index: 0,
                    turn: Some(71),
                    at: None,
                    session: None,
                },
                TrajectoryStep {
                    kind: StepKind::ToolOutput,
                    text: "cabinet 2 contains the key".into(),
                    index: 1,
                    turn: Some(72),
                    at: None,
                    session: None,
                },
            ],
            outcome: None,
            success: None,
            causal: Vec::new(),
            empty_turns: 0,
        };
        let section = traj
            .pinned_section("Between step 71 and step 72, what did the agent do?")
            .expect("must pin");
        assert!(section.starts_with(PINNED_TURNS_HEADER), "{section}");
        // The question's own vocabulary must appear in the pinned text, or the reader cannot join
        // the question to the turn — that is the whole reason `turn` is a field.
        assert!(section.contains("step 71"), "{section}");
        assert!(section.contains("step 72"), "{section}");
        assert!(section.contains("cabinet 2 contains the key"), "{section}");
        // A question naming a step the trajectory does not contain pins nothing rather than
        // producing an empty section.
        assert!(traj.pinned_section("What happened at step 400?").is_none());
        assert!(traj.pinned_section("Summarise the trajectory.").is_none());
    }
}

/// Tests for reading a step back out of the **rendered** text — the path that has to survive ingest.
///
/// The defect these guard is not a wrong number, it is a parser that matched *nothing*: both
/// recoverers searched for `" step "` with a leading space, which is only how the `Some(session)` arm
/// of `render()` writes. AMA-Bench sets no per-step `session`, so every one of its documents renders
/// without the prefix, `kind_from_rendered` returned `None` for all of them, and
/// `write_group_causal_edges` took its `steps.len() < 2` early return — a capability that ran, produced
/// nothing, and was counted nowhere.
#[cfg(test)]
mod rendered_header_tests {
    use super::*;

    fn step(
        kind: StepKind,
        text: &str,
        index: usize,
        turn: Option<i64>,
        session: Option<&str>,
    ) -> TrajectoryStep {
        TrajectoryStep {
            kind,
            text: text.into(),
            index,
            turn,
            at: None,
            session: session.map(str::to_string),
        }
    }

    /// **The regression.** No session prefix — the shape every AMA-Bench document has — must parse.
    #[test]
    fn a_header_with_no_session_prefix_still_parses() {
        let s = step(StepKind::Action, "right 1 lines", 20, Some(20), None);
        assert_eq!(rendered_step_no(&s.render()), Some(20));
        assert_eq!(kind_from_rendered(&s.render()), Some(StepKind::Action));
        assert!(
            s.render().starts_with("step 20 "),
            "no prefix: {}",
            s.render()
        );
    }

    /// The prefixed shape must keep working — it is what every earlier figure was produced on.
    #[test]
    fn a_header_with_a_session_prefix_still_parses() {
        let s = step(StepKind::Observation, "moved", 21, Some(20), Some("0"));
        assert_eq!(rendered_step_no(&s.render()), Some(20));
        assert_eq!(kind_from_rendered(&s.render()), Some(StepKind::Observation));
    }

    /// The two recoverers must agree, because a disagreement is a causal edge pointing at the wrong
    /// step — the failure mode the whole "two sources of the same number" note exists to avoid.
    #[test]
    fn the_number_and_the_kind_come_from_the_same_header() {
        for (kind, turn, session) in [
            (StepKind::Action, Some(7), None),
            (StepKind::Observation, Some(7), None),
            (StepKind::Action, None, Some("s1")),
            (StepKind::State, Some(0), None),
        ] {
            let s = step(kind, "payload", 3, turn, session);
            let r = s.render();
            assert_eq!(rendered_step_no(&r), Some(turn.unwrap_or(3)), "{r}");
            assert_eq!(kind_from_rendered(&r), Some(kind), "{r}");
        }
    }

    /// A *mention* of a step is still not a step: only the header before the first `": "` counts, so an
    /// observation quoting another step keeps its own number.
    #[test]
    fn a_mention_inside_the_text_is_not_the_header() {
        let s = step(
            StepKind::Observation,
            "the command printed step 3",
            2,
            Some(1),
            None,
        );
        let r = s.render();
        assert_eq!(rendered_step_no(&r), Some(1), "{r}");
    }

    /// Prose and near-misses must be refused rather than guessed at, because a false positive here
    /// silently re-types or re-orders a causal chain.
    #[test]
    fn prose_is_not_a_rendered_header() {
        for t in [
            "Note: the agent failed",
            "run step 3",
            "5 action: move",
            "step seven action: move",
            "step 4: no kind",
            "",
        ] {
            assert_eq!(rendered_step_no(t), None, "should not parse: {t:?}");
            assert_eq!(kind_from_rendered(t), None, "should not parse: {t:?}");
        }
    }
}
