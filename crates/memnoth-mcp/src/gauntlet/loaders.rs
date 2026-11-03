// ============================================================================
use super::*;

// Loaders
// ============================================================================

/// BEAM's ten memory abilities. Kept as an enum so a new or renamed ability is a
/// compile error rather than a silently unmatched string — the failure mode that
/// would quietly drop a whole class from a published table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BeamAbility {
    Abstention,
    ContradictionResolution,
    EventOrdering,
    InformationExtraction,
    InstructionFollowing,
    KnowledgeUpdate,
    MultiSessionReasoning,
    PreferenceFollowing,
    Summarization,
    TemporalReasoning,
}

impl BeamAbility {
    pub const ALL: [BeamAbility; 10] = [
        BeamAbility::Abstention,
        BeamAbility::ContradictionResolution,
        BeamAbility::EventOrdering,
        BeamAbility::InformationExtraction,
        BeamAbility::InstructionFollowing,
        BeamAbility::KnowledgeUpdate,
        BeamAbility::MultiSessionReasoning,
        BeamAbility::PreferenceFollowing,
        BeamAbility::Summarization,
        BeamAbility::TemporalReasoning,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace([' ', '-'], "_")
                .as_str()
            {
                "abstention" => Self::Abstention,
                "contradiction_resolution" => Self::ContradictionResolution,
                "event_ordering" => Self::EventOrdering,
                "information_extraction" => Self::InformationExtraction,
                "instruction_following" => Self::InstructionFollowing,
                "knowledge_update" => Self::KnowledgeUpdate,
                "multi_session_reasoning" => Self::MultiSessionReasoning,
                "preference_following" => Self::PreferenceFollowing,
                "summarization" => Self::Summarization,
                "temporal_reasoning" => Self::TemporalReasoning,
                _ => return None,
            },
        )
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Abstention => "abstention",
            Self::ContradictionResolution => "contradiction_resolution",
            Self::EventOrdering => "event_ordering",
            Self::InformationExtraction => "information_extraction",
            Self::InstructionFollowing => "instruction_following",
            Self::KnowledgeUpdate => "knowledge_update",
            Self::MultiSessionReasoning => "multi_session_reasoning",
            Self::PreferenceFollowing => "preference_following",
            Self::Summarization => "summarization",
            Self::TemporalReasoning => "temporal_reasoning",
        }
    }

    /// Position in [`BeamAbility::ALL`].
    ///
    /// Used to number the class in the harness's per-class report. Derived from the
    /// array rather than hand-written so the numbering cannot drift from the list.
    pub fn index(&self) -> u32 {
        Self::ALL.iter().position(|a| a == self).unwrap_or(0) as u32
    }

    /// Abilities whose gold is a *rubric* rather than a fact, scored as nuggets.
    pub fn is_rubric(&self) -> bool {
        !matches!(self, Self::EventOrdering)
    }

    /// Abilities that are really questions about **change over time**. These are the
    /// four where the measured field is weakest at 10M and where this vault holds the
    /// data structures nobody else does.
    pub fn is_state_over_time(&self) -> bool {
        matches!(
            self,
            Self::ContradictionResolution
                | Self::KnowledgeUpdate
                | Self::EventOrdering
                | Self::TemporalReasoning
        )
    }
}

/// One BEAM probing question with its rubric.
#[derive(Clone, Debug)]
pub struct BeamQuestion {
    pub question_id: String,
    pub ability: BeamAbility,
    pub question: String,
    /// The rubric nuggets an answer is scored against (0/0.5/1.0 each).
    pub nuggets: Vec<String>,
    /// For `event_ordering`: the gold sequence, in order.
    pub gold_order: Vec<String>,
    pub difficulty: Option<String>,
}

/// A BEAM conversation: the chat to ingest and the questions asked about it.
#[derive(Clone, Debug)]
pub struct BeamConversation {
    pub conversation_id: String,
    pub chat_size: String,
    pub turns: Vec<Span>,
    pub questions: Vec<BeamQuestion>,
}

/// Parse a BEAM record (as the HF dataset ships it: `chat` + `probing_questions`,
/// where `probing_questions` may arrive as a JSON *string*).
pub fn load_beam_record(item: &Value, chat_size: &str, idx: usize) -> Result<BeamConversation> {
    let conversation_id = item
        .get("conversation_id")
        .and_then(scalar_string)
        .unwrap_or_else(|| format!("{chat_size}_{idx}"));

    let mut turns = Vec::new();
    if let Some(chat) = item.get("chat").and_then(Value::as_array) {
        for (t, turn) in chat.iter().enumerate() {
            let text = turn
                .get("content")
                .or_else(|| turn.get("message"))
                .or_else(|| turn.get("text"))
                .and_then(scalar_string);
            let Some(text) = text else { continue };
            let mut span =
                Span::new(&format!("{conversation_id}-t{t}"), &text).session(&conversation_id);
            if let Some(role) = turn
                .get("role")
                .or_else(|| turn.get("speaker"))
                .and_then(scalar_string)
            {
                span = span.speaker(&role);
            }
            span.turn = Some(t);
            turns.push(span);
        }
    }

    // `probing_questions` is a dict of ability -> [question], sometimes a JSON string.
    let raw = item
        .get("probing_questions")
        .cloned()
        .unwrap_or(Value::Null);
    let parsed = coerce_json(raw);
    let mut questions = Vec::new();
    if let Some(map) = parsed.as_object() {
        for (ability_name, list) in map {
            let Some(ability) = BeamAbility::parse(ability_name) else {
                // Unknown ability: refuse rather than silently drop it from a table.
                anyhow::bail!(
                    "BEAM ability `{ability_name}` is not one of the ten known abilities"
                );
            };
            let Some(list) = list.as_array() else {
                continue;
            };
            for (q, entry) in list.iter().enumerate() {
                let question_text = entry
                    .get("question")
                    .or_else(|| entry.get("query"))
                    .and_then(scalar_string)
                    .or_else(|| entry.as_str().map(str::to_string));
                let Some(question_text) = question_text else {
                    continue;
                };

                let mut nuggets = Vec::new();
                let mut gold_order = Vec::new();
                if let Some(rubric) = entry.get("rubric") {
                    // HF stores the rubric as an object, but a JSON-encoded string is
                    // also accepted — the same polymorphism `probing_questions` has.
                    let rubric = coerce_json(rubric.clone());
                    // **Two rubric shapes ship, and reading only the first made the official
                    // checkout refuse to load.** The HF record carries `{"nuggets":
                    // [{"description": …}]}`; the repository's own
                    // `chats/<tier>/<n>/probing_questions/probing_questions.json` carries a plain
                    // **array of strings** that are already the nugget text:
                    //
                    //   "rubric": ["Based on the provided chat, there is no information …", …]
                    //
                    // With the array unread, `nuggets` came back empty for every question and
                    // `load_beam` bailed on its "carries no rubric nuggets and no gold order"
                    // check — so the published data could not be loaded at all. The inner mapping
                    // below already handles a bare string (`n.as_str()`), which is why the fix
                    // belongs here and not there.
                    let array = rubric
                        .get("nuggets")
                        .and_then(Value::as_array)
                        .or_else(|| rubric.as_array());
                    if let Some(arr) = array {
                        for n in arr {
                            let desc = n
                                .get("description")
                                .or_else(|| n.get("nugget"))
                                .and_then(scalar_string)
                                .or_else(|| n.as_str().map(str::to_string));
                            if let Some(d) = desc.filter(|d| !d.trim().is_empty()) {
                                nuggets.push(d);
                            }
                        }
                    }
                }
                // **A rubric-less question is still scoreable when the dataset ships its own
                // reference answer.** The official checkout pairs every rubric with
                // `ideal_response` — the text the graders' rubric was derived from — and for the
                // `abstention` class that reference is the whole gold ("Based on the provided
                // chat, there is no information related to …"). Using it as the single nugget
                // keeps the question in the run instead of dropping it, and it is exactly what a
                // human grader reads first.
                if nuggets.is_empty()
                    && gold_order.is_empty()
                    && let Some(ideal) = entry
                        .get("ideal_response")
                        .or_else(|| entry.get("reference_answer"))
                        .and_then(scalar_string)
                        .filter(|t| !t.trim().is_empty())
                {
                    nuggets.push(ideal);
                }
                if let Some(gold) = entry
                    .get("gold_order")
                    .or_else(|| entry.get("order"))
                    .and_then(Value::as_array)
                {
                    for g in gold {
                        if let Some(s) = scalar_string(g) {
                            gold_order.push(s);
                        }
                    }
                }
                questions.push(BeamQuestion {
                    question_id: format!("{conversation_id}_q{q}_{}", ability.name()),
                    ability,
                    question: question_text,
                    nuggets,
                    gold_order,
                    difficulty: entry.get("difficulty").and_then(scalar_string),
                });
            }
        }
    }

    if turns.is_empty() {
        anyhow::bail!(
            "BEAM record {conversation_id} yielded no chat turns — refusing to score a conversation with an empty haystack"
        );
    }
    Ok(BeamConversation {
        conversation_id,
        chat_size: chat_size.to_string(),
        turns,
        questions,
    })
}

/// A PersonaMem multiple-choice question.
#[derive(Clone, Debug)]
pub struct PersonaMemQuestion {
    pub question_id: String,
    pub question: String,
    pub options: Vec<String>,
    /// Index into `options` of the correct one.
    pub answer_index: usize,
    /// The user whose shared context these questions are asked about.
    pub user_id: String,
    /// **The join key into `shared_contexts_*.jsonl`.** Verified against a working
    /// implementation of this benchmark (MemOS ships one), not inferred from the paper:
    /// each question row names the shared context it is answered against.
    pub shared_context_id: String,
    /// **How many sessions of that context are in scope for this question.**
    ///
    /// The reference truncates per question (`context[:end_index_in_shared_context]`),
    /// which is what makes the 32k and 128k variants different measurements. Ignoring
    /// it would answer every question against the *full* context — a different, and
    /// easier, benchmark wearing the same name.
    pub end_index_in_shared_context: Option<usize>,
    /// Optional class label, used for the per-class report when present.
    pub category: Option<String>,
}

impl PersonaMemQuestion {
    /// Score a response as an option index, under PersonaMem's MCQ rule.
    pub fn is_correct(&self, chosen: Option<usize>) -> bool {
        chosen == Some(self.answer_index)
    }

    /// Parse a letter (`A`-`J`) or digit reply. PersonaMem-v2 is answer-letter based,
    /// and a reader that emits prose must be read for its final letter or the
    /// benchmark measures formatting rather than memory.
    pub fn parse_reply(reply: &str) -> Option<usize> {
        let t = reply.trim();
        if let Ok(n) = t.parse::<usize>() {
            return (n < 26).then_some(n);
        }
        // A leading letter, or an explicit "answer: X".
        let candidate = t.rsplit([':', '\n', ')']).next().unwrap_or(t).trim();
        let first = candidate.chars().next()?;
        if first.is_ascii_alphabetic() {
            Some((first.to_ascii_uppercase() as u8 - b'A') as usize)
        } else {
            None
        }
    }
}

/// Normalise a PersonaMem answer token.
///
/// The released `questions_*.csv` (HF `bowen-upenn/PersonaMem-v1`) writes the answer as
/// `(c)`; the synthetic fixture used a bare `B`. Unwrapping one layer of parentheses lets
/// the existing text -> letter -> index ladder run unchanged for both, without a second
/// code path. Verified against the real file, not inferred: `correct_answer` is always a
/// parenthesised lower-case letter there, and the option list carries `(a) ` markers.
fn personamem_answer_token(a: &str) -> &str {
    let t = a.trim();
    t.strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(t)
}

/// Parse a PersonaMem options cell.
///
/// Two spellings exist and both are accepted: the released `all_options` column is a JSON
/// array (`["(a) ...", "(b) ..."]`, escaped through the CSV), while the fixture joined
/// with `|` or newlines. A leading `(a) ` marker is stripped so the harness renders its own
/// letter instead of printing the dataset's twice; the correct option is located by
/// position, never by this prefix.
fn parse_personamem_options(cell: &str) -> Vec<String> {
    let t = cell.trim();
    if t.starts_with('[')
        && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(t)
    {
        let opts: Vec<String> = items
            .iter()
            .filter_map(scalar_string)
            .map(|s| strip_option_prefix(s.trim()))
            .collect();
        if !opts.is_empty() {
            return opts;
        }
    }
    split_pipe(t)
        .into_iter()
        .map(|s| strip_option_prefix(s.trim()))
        .collect()
}

fn strip_option_prefix(s: &str) -> String {
    let mut chars = s.chars();
    // One let-chain rather than three nested `if`s. The `&&` short-circuits
    // exactly as the nesting did, so a non-alphabetic second character still
    // leaves the iterator untouched (the `)` is only consumed when the letter
    // check passed).
    if chars.next() == Some('(')
        && let Some(letter) = chars.next()
        && letter.is_ascii_alphabetic()
        && chars.next() == Some(')')
    {
        let rest = chars.as_str().trim_start();
        if !rest.is_empty() {
            return rest.to_string();
        }
    }
    s.to_string()
}

/// Load PersonaMem `questions_*.csv` (accepts both the released `question`/`options`/`answer`
/// spelling and the fixture's `user_question_or_message`/`all_options`/`correct_answer`).
///
/// Hand-rolled rather than pulling a CSV crate: the file is small, the format is
/// fixed, and adding a dependency to the MCP crate affects four other members'
/// build graph for one loader.
pub fn load_personamem_csv(text: &str) -> Result<Vec<PersonaMemQuestion>> {
    let mut lines = text.lines();
    let header = lines.next().context("PersonaMem csv has no header row")?;
    let cols: Vec<String> = split_csv(header)
        .iter()
        .map(|c| c.trim().to_ascii_lowercase())
        .collect();
    let idx = |name: &str| cols.iter().position(|c| c == name);
    let (q_col, o_col, a_col) = (
        idx("question")
            .or_else(|| idx("user_question_or_message"))
            .context("PersonaMem csv lacks a `question`/`user_question_or_message` column")?,
        idx("options")
            .or_else(|| idx("all_options"))
            .context("PersonaMem csv lacks an `options`/`all_options` column")?,
        idx("answer")
            .or_else(|| idx("correct_answer"))
            .context("PersonaMem csv lacks an `answer` column")?,
    );
    let u_col = idx("user_id").or_else(|| idx("context_id"));

    let mut out = Vec::new();
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = split_csv(line);
        let Some(q) = fields.get(q_col) else { continue };
        let Some(o) = fields.get(o_col) else { continue };
        let Some(a) = fields.get(a_col) else { continue };
        let options = parse_personamem_options(o);
        // The answer column has three spellings in the wild, and all three are
        // accepted **in this order**: an exact option text (most robust, and it is
        // self-checking), then a single answer letter (`B` — PersonaMem-v2's own
        // convention), then a numeric index.
        //
        // Order matters for one specific input: with options `6am|7am`, the answer
        // `1` is unambiguous as an index but would also parse as a letter-like
        // position. Text is tried first so a *value* always beats a *position* when
        // both readings exist. The numeric reading is taken as 0-based, which is a
        // guess about a file we have not pinned yet — [`load_personamem_csv`] is
        // therefore documented as requiring the real file to be checked against this
        // order before any PersonaMem number is published.
        let answer_index = options
            .iter()
            .position(|opt| opt.eq_ignore_ascii_case(personamem_answer_token(a)))
            .or_else(|| {
                let t = personamem_answer_token(a);
                let mut chars = t.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphabetic() => {
                        Some((c.to_ascii_uppercase() as u8 - b'A') as usize)
                    }
                    _ => None,
                }
            })
            .or_else(|| personamem_answer_token(a).parse::<usize>().ok());
        let Some(answer_index) = answer_index.filter(|i| *i < options.len()) else {
            anyhow::bail!(
                "PersonaMem row {n}: answer `{}` matches no option",
                a.trim()
            );
        };
        let shared_context_id = idx("shared_context_id")
            .and_then(|c| fields.get(c))
            .cloned()
            .unwrap_or_default();
        out.push(PersonaMemQuestion {
            question_id: format!("pm-{n:05}"),
            question: q.trim().to_string(),
            options,
            answer_index,
            user_id: u_col
                .and_then(|c| fields.get(c))
                .cloned()
                .unwrap_or_default(),
            shared_context_id,
            end_index_in_shared_context: idx("end_index_in_shared_context")
                .and_then(|c| fields.get(c))
                .and_then(|v| v.trim().parse::<usize>().ok()),
            category: idx("category")
                .or_else(|| idx("question_type"))
                .and_then(|c| fields.get(c))
                .cloned()
                .filter(|s| !s.trim().is_empty()),
        });
    }
    if out.is_empty() {
        anyhow::bail!(
            "PersonaMem csv produced no questions — refusing to publish an accuracy over nothing"
        );
    }
    Ok(out)
}

/// Parse `shared_contexts_*.jsonl`.
///
/// Each line is one JSON object with a **single key** — the shared context id — whose
/// value is the list of *sessions*, each session a list of `{role, content}` messages.
/// (This shape is taken from a working implementation of the benchmark, which indexes
/// the file by that key and slices the value by the question's `end_index`.)
///
/// Returns id → sessions → spans. The per-session grouping is kept because the
/// benchmark's own truncation unit is a session, so flattening it would make
/// `end_index_in_shared_context` impossible to honour.
pub fn load_personamem_contexts_jsonl(text: &str) -> Result<BTreeMap<String, Vec<Vec<Span>>>> {
    let mut out: BTreeMap<String, Vec<Vec<Span>>> = BTreeMap::new();
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(line)
            .with_context(|| format!("PersonaMem shared_contexts line {line_no} is not JSON"))?;
        let Some(obj) = parsed.as_object() else {
            anyhow::bail!("PersonaMem shared_contexts line {line_no} is not an object");
        };
        for (context_id, sessions_value) in obj {
            let Some(sessions) = sessions_value.as_array() else {
                continue;
            };
            let mut rendered_sessions = Vec::new();
            for (si, session) in sessions.iter().enumerate() {
                let session_label = format!("{context_id}-s{si}");
                let mut spans = Vec::new();
                // A session is a list of messages; tolerate a dict-shaped session too by
                // looking for the one obvious array of turns inside it.
                // Two released shapes, both accepted:
                //  * the fixture / older export: one session object per item, each carrying
                //    its own `messages` array;
                //  * the released `shared_contexts_*.jsonl`: the value is a **flat message
                //    list** (`[{role, content}, ...]`), one dict per message, with no session
                //    nesting at all. Treating each message as its own session keeps
                //    `end_index_in_shared_context` meaningful, because that index is a message
                //    offset in the released file (context 0 has 183 messages and the first
                //    question's end_index is 182). Verified against the real file.
                let messages: Option<Vec<&Value>> = match session {
                    Value::Array(a) => Some(a.iter().collect()),
                    Value::Object(o) if o.contains_key("content") || o.contains_key("text") => {
                        Some(vec![session])
                    }
                    Value::Object(o) => o
                        .get("messages")
                        .or_else(|| o.get("dialogue"))
                        .or_else(|| o.get("session"))
                        .and_then(Value::as_array)
                        .map(|v| v.iter().collect()),
                    _ => None,
                };
                let Some(messages) = messages else { continue };
                for (ti, m) in messages.iter().enumerate() {
                    let content = m
                        .get("content")
                        .or_else(|| m.get("text"))
                        .and_then(scalar_string)
                        .or_else(|| m.as_str().map(str::to_string));
                    let Some(content) = content else { continue };
                    let mut span = Span::new(&format!("{session_label}-t{ti}"), &content)
                        .session(&session_label);
                    if let Some(role) = m.get("role").and_then(scalar_string) {
                        span = span.speaker(&role);
                    }
                    span.turn = Some(ti);
                    spans.push(span);
                }
                if !spans.is_empty() {
                    rendered_sessions.push(spans);
                }
            }
            if !rendered_sessions.is_empty() {
                out.insert(context_id.clone(), rendered_sessions);
            }
        }
    }
    if out.is_empty() {
        anyhow::bail!(
            "PersonaMem shared_contexts yielded no contexts — refusing to answer every question against an empty haystack"
        );
    }
    Ok(out)
}

/// One MemoryArena domain.
///
/// **There are five, not four.** An earlier draft of this module modelled the domains
/// from the MemoryArena *paper's* prose ("web navigation, preference-constrained
/// planning, progressive information searching, sequential formal reasoning") and got
/// four. The released dataset card is authoritative and splits formal reasoning into
/// **math and physics**, giving five configs. The suite score is an equal-weight mean
/// **over the five**, so four domains would have computed a different benchmark.
///
/// The variant names are the dataset's own config names, so a config name can never be
/// reinterpreted by a rename in this file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArenaDomain {
    BundledShopping,
    ProgressiveSearch,
    GroupTravelPlanner,
    FormalReasoningMath,
    FormalReasoningPhys,
}

impl ArenaDomain {
    pub const ALL: [ArenaDomain; 5] = [
        ArenaDomain::BundledShopping,
        ArenaDomain::ProgressiveSearch,
        ArenaDomain::GroupTravelPlanner,
        ArenaDomain::FormalReasoningMath,
        ArenaDomain::FormalReasoningPhys,
    ];

    /// The dataset's own config / directory name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::BundledShopping => "bundled_shopping",
            Self::ProgressiveSearch => "progressive_search",
            Self::GroupTravelPlanner => "group_travel_planner",
            Self::FormalReasoningMath => "formal_reasoning_math",
            Self::FormalReasoningPhys => "formal_reasoning_phys",
        }
    }

    /// Accepts the dataset's config names plus the aliases used in the MemoryLake
    /// matched study, so a number reported by either names the same domain here.
    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s
                .trim()
                .to_ascii_lowercase()
                .replace([' ', '-'], "_")
                .as_str()
            {
                "bundled_shopping" | "web_shopping" | "web_navigation" | "shopping" => {
                    Self::BundledShopping
                }
                "progressive_search"
                | "progressive_information_searching"
                | "progressive_retrieval" => Self::ProgressiveSearch,
                "group_travel_planner" | "travel_planning" | "preference_planning" => {
                    Self::GroupTravelPlanner
                }
                "formal_reasoning_math" | "math" | "physics_math" => Self::FormalReasoningMath,
                "formal_reasoning_phys" | "physics" | "phys" => Self::FormalReasoningPhys,
                _ => return None,
            },
        )
    }

    /// Infer the domain from a dataset *path*, because the release ships one file per
    /// domain (`bundled_shopping/data.jsonl`, …) and the record itself does not always
    /// name its domain.
    pub fn from_path(path: &str) -> Option<Self> {
        let lower = path.to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|d| lower.contains(d.name()))
            .or_else(|| {
                // Tolerate the study's directory spellings.
                for (needle, d) in [
                    ("shopping", Self::BundledShopping),
                    ("progressive", Self::ProgressiveSearch),
                    ("travel", Self::GroupTravelPlanner),
                    ("math", Self::FormalReasoningMath),
                    ("phys", Self::FormalReasoningPhys),
                ] {
                    if lower.contains(needle) {
                        return Some(d);
                    }
                }
                None
            })
    }
}

/// A MemoryArena task record, exactly as the release ships it.
///
/// The distinguishing property is **interdependence**: a later subtask needs what an
/// earlier one established. A memory system that only answers questions scores 0 here;
/// it has to carry state forward, which is why this is a *trajectory* benchmark and not
/// a QA benchmark with long inputs.
#[derive(Clone, Debug)]
pub struct ArenaTask {
    /// The dataset's `id` (an integer in the release; kept as a string so it can join
    /// with a hypotheses file).
    pub task_id: String,
    pub domain: ArenaDomain,
    /// `questions` — the sub-task instructions, in dependency order.
    pub subtasks: Vec<String>,
    /// `answers` — one per sub-task. Element types differ per domain (a string in
    /// shopping, an object with `target_asin` in progressive search, a list of daily
    /// plans in travel, a result in formal reasoning), so each is rendered to text by
    /// [`render_answer`] rather than assumed to be a string.
    pub answers: Vec<String>,
    /// `backgrounds` — absent in shopping and progressive search, a per-sub-task list in
    /// formal reasoning, and (for travel) the base person's details. Empty when the
    /// domain carries none.
    pub backgrounds: Vec<String>,
    /// `paper_name` in formal reasoning; `None` elsewhere.
    pub paper_name: Option<String>,
}

impl ArenaTask {
    pub fn subtask_count(&self) -> usize {
        self.subtasks.len()
    }
}

/// Which domain a resolved sub-task belonged to, and whether it succeeded.
///
/// Separated from [`ArenaTask`] because a task and its outcome are different things:
/// the record is the benchmark's data, the outcome is our run's result, and conflating
/// them makes it impossible to score a superset or a re-run.
#[derive(Clone, Debug)]
pub struct ArenaOutcome {
    pub task_id: String,
    pub domain: ArenaDomain,
    pub success: bool,
}

/// MemoryArena's suite convention: the **equal-weight mean of the per-domain success
/// rates**, over five domains. Not the per-task mean — a domain with 40 tasks would
/// otherwise outvote one with 20, which is not what the benchmark publishes.
///
/// Returns the per-domain rates (zero for a domain with no scored task, so an absent
/// domain is visible rather than silently dropped from the mean) and the suite score.
/// The suite score is `None` when nothing was scored at all.
pub fn arena_suite_score(outcomes: &[ArenaOutcome]) -> (BTreeMap<String, f64>, Option<f64>) {
    let mut by_domain: BTreeMap<String, ClassTally> = BTreeMap::new();
    for d in ArenaDomain::ALL {
        // Pre-seed every domain so a domain we failed to run shows as 0.0 rather than
        // vanishing — an absent domain must lower the mean, not disappear from it.
        by_domain.entry(d.name().to_string()).or_default();
    }
    for o in outcomes {
        by_domain
            .entry(o.domain.name().to_string())
            .or_default()
            .add(o.success, if o.success { 1.0 } else { 0.0 });
    }
    // Equal weight over **all five** domains, with an unrun domain counting as 0.0.
    // A mean over only the domains we happened to run would let a partial suite beat a
    // complete one, which is the opposite of what the benchmark reports.
    if outcomes.is_empty() {
        // Nothing was scored: `None`, not 0.0. A suite score of zero for a benchmark we
        // never ran is the same class of error as an empty corpus scoring 0% recall.
        return (
            by_domain
                .into_iter()
                .map(|(k, v)| (k, v.accuracy().unwrap_or(0.0)))
                .collect(),
            None,
        );
    }
    let rates: Vec<f64> = by_domain
        .values()
        .map(|v| v.accuracy().unwrap_or(0.0))
        .collect();
    (
        by_domain
            .into_iter()
            .map(|(k, v)| (k, v.accuracy().unwrap_or(0.0)))
            .collect(),
        mean(&rates),
    )
}

/// Render one `answers` element to text, whatever shape the domain used.
///
/// Progressive Search ships `{"target_asin": ..., "attributes": [...]}`, travel ships a
/// list of daily plans, formal reasoning ships a string, and a future revision may ship
/// something else. Coercing silently to a string would produce `[object]` as a gold
/// answer, which would then score every response as wrong while looking like a memory
/// failure — so the rendering is explicit and recurses.
pub fn render_answer(v: &Value) -> String {
    match v {
        Value::Array(items) => items
            .iter()
            .map(render_answer)
            .collect::<Vec<_>>()
            .join(" | "),
        Value::Object(map) => map
            .iter()
            .map(|(k, val)| format!("{k}={}", render_answer(val)))
            .collect::<Vec<_>>()
            .join(", "),
        other => scalar_string(other).unwrap_or_default(),
    }
}

/// Parse one MemoryArena record.
///
/// `domain` is supplied by the caller because the release splits domains across files
/// and the record does not always name its own domain; see [`ArenaDomain::from_path`].
pub fn load_memoryarena_task(item: &Value, domain: ArenaDomain) -> Result<ArenaTask> {
    let task_id = item
        .get("id")
        .and_then(scalar_string)
        .context("MemoryArena record has no `id`")?;

    let questions = item
        .get("questions")
        .and_then(Value::as_array)
        .context("MemoryArena record has no `questions` list")?;
    let subtasks: Vec<String> = questions
        .iter()
        .map(|q| scalar_string(q).unwrap_or_default())
        .collect();

    let answers_raw = item
        .get("answers")
        .and_then(Value::as_array)
        .context("MemoryArena record has no `answers` list")?;
    let answers: Vec<String> = answers_raw.iter().map(render_answer).collect();

    if subtasks.len() != answers.len() {
        anyhow::bail!(
            "MemoryArena task {task_id}: {} questions but {} answers — refusing to score a misaligned task",
            subtasks.len(),
            answers.len()
        );
    }
    if subtasks.is_empty() {
        anyhow::bail!("MemoryArena task {task_id} has no subtasks");
    }

    // `backgrounds` is a string in travel, a list in formal reasoning, absent in
    // shopping and progressive search — all three are accepted.
    let backgrounds = match item.get("backgrounds") {
        Some(Value::Array(items)) => items.iter().map(render_answer).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![render_answer(other)],
    };

    Ok(ArenaTask {
        task_id,
        domain,
        subtasks,
        answers,
        backgrounds,
        paper_name: item.get("paper_name").and_then(scalar_string),
    })
}

/// Where a HaluMem memory point came from.
///
/// `Interference` is the load-bearing variant: Halu-Long injects distractor content
/// (unrelated factual QA and math problems) at 1M tokens, and a system that hoovers
/// those up as memory is exhibiting exactly the hallucination the benchmark measures.
/// Folding it into `Secondary` would hide the one source the benchmark added on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HaluSource {
    Primary,
    Secondary,
    Interference,
    System,
    Unknown,
}

impl HaluSource {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "primary" => Self::Primary,
            "secondary" => Self::Secondary,
            "interference" => Self::Interference,
            "system" => Self::System,
            _ => Self::Unknown,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Interference => "interference",
            Self::System => "system",
            Self::Unknown => "unknown",
        }
    }

    /// Should this point ever be *stored* as a memory? Only interference is expected to
    /// be rejected on sight, but it is enough to separate it.
    pub fn is_distractor(&self) -> bool {
        matches!(self, Self::Interference)
    }
}

/// One HaluMem memory point — the ground truth for the extraction and update tasks.
#[derive(Clone, Debug)]
pub struct HaluMemoryPoint {
    pub index: i64,
    pub content: String,
    pub memory_type: String,
    pub source: HaluSource,
    /// True when this point *replaces* earlier ones. The release stores this as the
    /// **string** `"True"` in at least one documented example, so parsing must accept
    /// both a bool and a string — reading only `as_bool` would report that nothing is
    /// ever updated, i.e. exactly the failure the update task is looking for.
    pub is_update: bool,
    /// The earlier memories this point supersedes. This is the gold the update task
    /// scores against, and it is the same shape as our own supersession chains.
    pub original_memories: Vec<String>,
    pub importance: Option<f64>,
    pub timestamp: Option<String>,
}

/// A HaluMem session: the dialogue, its gold memory points, and its QA pairs.
#[derive(Clone, Debug)]
pub struct HaluSession {
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub turns: Vec<Span>,
    pub memory_points: Vec<HaluMemoryPoint>,
    /// QA pairs as `(question, gold answer)`.
    pub questions: Vec<(String, String)>,
    pub token_length: Option<i64>,
}

/// One HaluMem user.
#[derive(Clone, Debug)]
pub struct HaluUser {
    pub uuid: String,
    pub persona_info: Option<String>,
    pub sessions: Vec<HaluSession>,
}

impl HaluUser {
    pub fn memory_points(&self) -> usize {
        self.sessions.iter().map(|s| s.memory_points.len()).sum()
    }

    /// Gold memory points that are *not* distractors — the denominator for extraction
    /// completeness. Counting interference points here would make a system that stored
    /// nothing look complete.
    pub fn real_memory_points(&self) -> usize {
        self.sessions
            .iter()
            .flat_map(|s| s.memory_points.iter())
            .filter(|p| !p.source.is_distractor())
            .count()
    }

    /// Points that supersede earlier ones, i.e. the update task's gold.
    pub fn update_points(&self) -> usize {
        self.sessions
            .iter()
            .flat_map(|s| s.memory_points.iter())
            .filter(|p| p.is_update)
            .count()
    }
}

/// Parse one HaluMem user record (`HaluMem-Medium.jsonl` / `-Long.jsonl`).
pub fn load_halumem_user(item: &Value) -> Result<HaluUser> {
    let uuid = item
        .get("uuid")
        .and_then(scalar_string)
        .context("HaluMem record has no `uuid`")?;
    let persona_info = item.get("persona_info").map(render_answer);
    let sessions_raw = item
        .get("sessions")
        .and_then(Value::as_array)
        .context("HaluMem record has no `sessions` list")?;

    let mut sessions = Vec::new();
    for (si, s) in sessions_raw.iter().enumerate() {
        let session_label = format!("{uuid}-s{si}");
        let mut turns = Vec::new();
        if let Some(dialogue) = s.get("dialogue").and_then(Value::as_array) {
            for (ti, turn) in dialogue.iter().enumerate() {
                let content = turn
                    .get("content")
                    .or_else(|| turn.get("text"))
                    .and_then(scalar_string)
                    .or_else(|| turn.as_str().map(str::to_string));
                let Some(content) = content else { continue };
                let mut span =
                    Span::new(&format!("{session_label}-t{ti}"), &content).session(&session_label);
                if let Some(role) = turn.get("role").and_then(scalar_string) {
                    span = span.speaker(&role);
                }
                span.turn = Some(ti);
                turns.push(span);
            }
        }

        let mut memory_points = Vec::new();
        if let Some(points) = s.get("memory_points").and_then(Value::as_array) {
            for p in points {
                let content = p
                    .get("memory_content")
                    .and_then(scalar_string)
                    .unwrap_or_default();
                if content.trim().is_empty() {
                    continue;
                }
                let original_memories = p
                    .get("original_memories")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(render_answer).collect())
                    .unwrap_or_default();
                memory_points.push(HaluMemoryPoint {
                    index: p.get("index").and_then(Value::as_i64).unwrap_or(0),
                    content,
                    memory_type: p
                        .get("memory_type")
                        .and_then(scalar_string)
                        .unwrap_or_default(),
                    source: p
                        .get("memory_source")
                        .and_then(scalar_string)
                        .map(|v| HaluSource::parse(&v))
                        .unwrap_or(HaluSource::Unknown),
                    is_update: truthy(p.get("is_update")),
                    original_memories,
                    importance: p.get("importance").and_then(Value::as_f64),
                    timestamp: p.get("timestamp").and_then(scalar_string),
                });
            }
        }

        let mut questions = Vec::new();
        if let Some(qs) = s.get("questions").and_then(Value::as_array) {
            for q in qs {
                let question = q
                    .get("question")
                    .or_else(|| q.get("query"))
                    .and_then(scalar_string)
                    .or_else(|| q.as_str().map(str::to_string));
                let Some(question) = question else { continue };
                let answer = q
                    .get("answer")
                    .or_else(|| q.get("gold"))
                    .map(render_answer)
                    .unwrap_or_default();
                questions.push((question, answer));
            }
        }

        sessions.push(HaluSession {
            start_time: s.get("start_time").and_then(scalar_string),
            end_time: s.get("end_time").and_then(scalar_string),
            turns,
            memory_points,
            questions,
            token_length: s.get("dialogue_token_length").and_then(Value::as_i64),
        });
    }

    if sessions.is_empty() {
        anyhow::bail!("HaluMem user {uuid} yielded no sessions");
    }
    Ok(HaluUser {
        uuid,
        persona_info,
        sessions,
    })
}

/// Read a boolean that may be spelled as a bool or as a string.
///
/// Exists for HaluMem's `is_update`, which the release's own example writes as `"True"`.
/// `as_bool` on that returns `None`, which would silently report that no memory point
/// ever updates anything — the exact signal the update task exists to measure.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => {
            matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1")
        }
        Some(Value::Number(n)) => n.as_i64().map(|i| i != 0).unwrap_or(false),
        _ => false,
    }
}

/// HaluMem's three operations. Hallucination is measured **per stage**, not only
/// end-to-end, because the benchmark's finding is that errors are created at
/// extraction and updating and then propagate to QA.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HaluStage {
    Extraction,
    Updating,
    Qa,
}

impl HaluStage {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Extraction => "extraction",
            Self::Updating => "updating",
            Self::Qa => "question_answering",
        }
    }
}

/// Operation-level outcome. `fabricated` is separated from `omitted` on purpose:
/// they are opposite errors with opposite fixes (an extractor that invents facts
/// needs tighter grounding; one that drops them needs recall), and a single
/// "hallucination rate" would average them into a number that names neither.
#[derive(Clone, Copy, Debug, Default)]
pub struct HaluOutcome {
    pub total_memory_points: usize,
    pub correct: usize,
    pub fabricated: usize,
    pub omitted: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HaluTally {
    pub correct: usize,
    pub fabricated: usize,
    pub omitted: usize,
    pub counted: usize,
}

impl HaluTally {
    pub fn add(&mut self, o: HaluOutcome) {
        self.correct += o.correct;
        self.fabricated += o.fabricated;
        self.omitted += o.omitted;
        self.counted += o.total_memory_points;
    }

    pub fn hallucination_rate(&self) -> Option<f64> {
        if self.counted == 0 {
            None
        } else {
            Some(self.fabricated as f64 / self.counted as f64 * 100.0)
        }
    }

    pub fn omission_rate(&self) -> Option<f64> {
        if self.counted == 0 {
            None
        } else {
            Some(self.omitted as f64 / self.counted as f64 * 100.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The released file's columns must parse. The loader was first written against a
    /// synthetic fixture (`question`/`options`/`answer`, pipe-joined options, bare-letter
    /// answers) and the real HF file (`bowen-upenn/PersonaMem-v1`) spells all three
    /// differently (`user_question_or_message` / `all_options` / `correct_answer`). This
    /// pins the real spelling so a future edit cannot silently revert to fixture-only
    /// support and make a PersonaMem run fail at load time.
    #[test]
    fn personamem_reads_the_released_columns_and_json_options() {
        let csv = "persona_id,question_id,question_type,user_question_or_message,correct_answer,all_options,shared_context_id,end_index_in_shared_context\n\
0,q-1,recall_user_shared_facts,Which beat did the user hear?,(c),\"[\"\"(a) rock\"\", \"\"(b) jazz\"\", \"\"(c) modern beats\"\"]\",ctx-a,3\n";
        let qs = load_personamem_csv(csv).expect("released columns must parse");
        assert_eq!(qs.len(), 1);
        let q = &qs[0];
        assert_eq!(q.question, "Which beat did the user hear?");
        assert_eq!(q.options, vec!["rock", "jazz", "modern beats"]);
        assert_eq!(q.answer_index, 2, "`(c)` must resolve to the third option");
        assert_eq!(q.shared_context_id, "ctx-a");
        assert_eq!(q.end_index_in_shared_context, Some(3));
        assert_eq!(q.category.as_deref(), Some("recall_user_shared_facts"));
    }

    /// The fixture spelling keeps working, and the parenthesis handling must not break a
    /// bare letter.
    #[test]
    fn personamem_still_reads_the_fixture_spelling() {
        let csv = "shared_context_id,end_index_in_shared_context,question,options,answer\nctx-a,2,Which cuisine?,Italian|Thai|Indian,B\n";
        let qs = load_personamem_csv(csv).expect("fixture columns must still parse");
        assert_eq!(qs[0].options, vec!["Italian", "Thai", "Indian"]);
        assert_eq!(qs[0].answer_index, 1);
    }
}
