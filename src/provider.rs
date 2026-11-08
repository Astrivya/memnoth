//! The model: one turn of conversation against a provider, with the vault and
//! the code graph as tools.
//!
//! The session is the product's front door, and until this module existed it
//! only knew how to search two local stores. That is honest but it is not an
//! agent: a question like "what breaks if I change `MemoryEngine`" needs the
//! model and the graph in the same turn. So this module owns exactly three
//! things:
//!
//! * **Resolution.** Which provider, model and endpoint this machine is
//!   configured for, and whether a key is present. Configuration is TOML; the
//!   key is an environment variable, never a file, for the same reason
//!   `MEMNOTH_MCP_TOKEN` is: a config file gets copied, committed and pasted
//!   into issues.
//! * **Transport.** Two wire shapes are enough to reach the field — Anthropic's
//!   `/v1/messages`, and the OpenAI-compatible `/chat/completions` that every
//!   gateway, local server and hosted provider also speaks.
//! * **The tool loop.** The model asks for a tool, the tool is one of the same
//!   [`crate::commands::execute`] verbs a slash command runs, and its text comes
//!   back to the model. There is no second implementation of `vault recall` for
//!   agents; that is the point.
//!
//! What it deliberately does not own: the screen, the session file, and
//! redrawing. [`run_turn`] is synchronous and reports progress through a
//! callback, so the TUI can run it on a thread and stay responsive while a turn
//! is in flight. Tests drive the same function against a socket of their own.
//!
//! Two tools change state: `vault_remember` records a belief and `edit_file`
//! changes a file. Neither can run silently. [`Toolbox`] carries an
//! [`Approver`] — the screen — and a tool that writes stops there until a person
//! says yes; with no approver the write is refused. That is deliberately not
//! something the model can talk its way around: the gate lives in the tool, not
//! in the instructions.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use memnoth_context::assemble::{Assembly, Layer, Piece, assemble};
use memnoth_context::budget::{Budget, Slot};
use memnoth_session::{Role, WriteReceipt, now_ms as session_now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{KgArgs, KgCommand, VaultArgs, VaultCommand, Verb};
use crate::context::Config;
use memnoth_auth::{account_key_sync, get_credential_sync};

/// Tokens a single reply may spend. Enough for a real answer, small enough that
/// a runaway turn is cheap.
pub const DEFAULT_MAX_TOKENS: u32 = 1600;

/// Rounds of tool calls before the turn gives up.
///
/// A model that has not answered after eight rounds of recall/search is
/// looping, not investigating. Stopping is honest; the alternative is a session
/// that burns tokens until the user notices.
pub const MAX_ROUNDS: usize = 8;

/// Characters of one tool result the model is allowed to see.
///
/// `kg impact` on a real symbol returns thousands of references; pasting all of
/// them crowds out the question. The truncation is stated to the model, which
/// can narrow the query instead of pretending it saw everything.
pub const TOOL_RESULT_CHARS: usize = 4000;

/// Request timeout, in seconds. Generous enough for a long completion over a
/// slow link, short enough that a wedged connection is a message rather than a
/// terminal that never comes back.
const TIMEOUT_SECONDS: u64 = 120;

/// Which wire shape to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Anthropic's `/v1/messages`.
    Anthropic,
    /// OpenAI's `/chat/completions`, and everything that copies it.
    OpenAi,
    /// Memnoth managed inference: `/api/v1/chat/completions` with account token auth.
    Memnoth,
    /// Kilo's AI Gateway. OpenAI-compatible, with vendor-prefixed model ids
    /// (`anthropic/claude-sonnet-4.5`) behind one key.
    Kilo,
}

impl Kind {
    /// The name written in `config.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Anthropic => "anthropic",
            Kind::OpenAi => "openai",
            Kind::Memnoth => "memnoth",
            Kind::Kilo => "kilo",
        }
    }

    /// The provider's name, for reports and tables.
    pub fn display_name(self) -> &'static str {
        self.name()
    }

    /// Where the provider lives when nothing says otherwise.
    pub fn default_base_url(self) -> &'static str {
        match self {
            Kind::Anthropic => "https://api.anthropic.com",
            Kind::OpenAi => "https://api.openai.com/v1",
            Kind::Memnoth => "https://memnoth.astrivya.ai/api/v1",
            Kind::Kilo => "https://ai-gateway.kilo.ai/api/v1",
        }
    }

    /// The model a first run uses. Overridable, and named in `doctor` so a
    /// stale default is visible rather than mysterious.
    pub fn default_model(self) -> &'static str {
        match self {
            Kind::Anthropic => "claude-sonnet-4-5",
            Kind::OpenAi => "gpt-4.1",
            Kind::Memnoth => "memnoth-default",
            // The gateway names a model as `<vendor>/<model>`; this is the one
            // its own quickstart uses.
            Kind::Kilo => "anthropic/claude-sonnet-4.5",
        }
    }

    /// The env var the provider's own SDKs use, as a fallback for
    /// `MEMNOTH_API_KEY`.
    pub fn key_env(self) -> &'static str {
        match self {
            Kind::Anthropic => "ANTHROPIC_API_KEY",
            Kind::OpenAi => "OPENAI_API_KEY",
            Kind::Memnoth => "MEMNOTH_API_KEY",
            Kind::Kilo => "KILO_API_KEY",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Ok(Kind::Anthropic),
            "openai" | "open-ai" | "openai-compatible" | "compatible" => Ok(Kind::OpenAi),
            "memnoth" | "managed" => Ok(Kind::Memnoth),
            "kilo" | "kilo-code" | "kilocode" | "kilo-gateway" | "gateway" => Ok(Kind::Kilo),
            other => bail!(
                "unknown provider kind {other:?} — use `anthropic`, `openai`, `memnoth`, or `kilo` \
                 (set it in `provider.kind` or MEMNOTH_PROVIDER)"
            ),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Whether a model's API accepts `reasoning_effort`.
///
/// The wire shape says nothing about the model: a gateway (Kilo, OpenRouter)
/// routes many vendors behind one OpenAI-compatible endpoint, so a Claude or
/// Llama id sent `reasoning_effort` answers 400 — the parameter is not a no-op
/// there, it is an error. Only the OpenAI reasoning families document it, and
/// the models screen offers the arrows on exactly these ids.
pub(crate) fn takes_reasoning_effort(model: &str) -> bool {
    let id = model.to_ascii_lowercase();
    id.starts_with("o1")
        || id.starts_with("o3")
        || id.starts_with("o4")
        || id.contains("gpt-5")
        || id.contains("/o3")
        || id.contains("/o4")
        || id.contains("/gpt-5")
}

/// The `[provider]` table in `~/.memnoth/config.toml`.
///
/// Every field is optional so a config can name only the model and inherit the
/// endpoint, or only the key's provider and inherit the model. It never holds
/// the key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// `anthropic` or `openai`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The preset this came from (`anthropic`, `openrouter`, `kilo`, …).
    ///
    /// `kind` is the wire shape, and every OpenAI-compatible gateway collapses
    /// to `openai`. This is the identity the API key is stored under, so it is
    /// what lets two compatible gateways coexist on one machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Model id sent to the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Endpoint base; a gateway or a local server goes here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Reply budget, in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// USD per million input tokens, for the session's cost estimate. Either
    /// this and `output_per_mtok` are set, or the built-in list price for the
    /// model is used, or the cost is reported as unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_per_mtok: Option<f64>,
    /// USD per million output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_per_mtok: Option<f64>,
    /// Sampling temperature, sent on the wire when set.
    ///
    /// Absent is not `0.0`: it means "the provider's own default", which is
    /// the right answer for anyone who never opened the setting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus sampling (`top_p`), sent on the wire when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Reasoning effort for the models that take one (`low`/`medium`/`high`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// What a million tokens cost, in USD.
///
/// Used only to answer "what has this session spent", and always labelled an
/// estimate: a price is a fact about a provider's page on a day, so the config
/// can override it and `doctor`/`/models` name the rates they used.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    /// USD per million input tokens.
    pub input_per_mtok: f64,
    /// USD per million output tokens.
    pub output_per_mtok: f64,
}

impl Pricing {
    /// A model that costs nothing: the gateway's free tier, or a local model
    /// behind a paid endpoint. Distinct from *unpriced*, which is a rate nobody
    /// knows — a zero here is a number the screen may print.
    pub const FREE: Self = Self {
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
    };

    /// Whether both rates are zero, so a bill of `$0.00` is the truth rather
    /// than a rounding accident.
    pub fn is_free(&self) -> bool {
        self.input_per_mtok == 0.0 && self.output_per_mtok == 0.0
    }

    /// Cost in micro-dollars, rounded. `None` would mean unpriced, which is why
    /// this returns `u64`: callers only hold a `Pricing` when they have rates.
    pub fn cost_micros(&self, input_tokens: u64, output_tokens: u64) -> u64 {
        let micros = (input_tokens as f64 * self.input_per_mtok
            + output_tokens as f64 * self.output_per_mtok)
            .round();
        micros.max(0.0).min(u64::MAX as f64) as u64
    }
}

/// The list price for a model, when one is known.
///
/// The table is the published first-party rate for the model families this
/// build offers in `mn models`, keyed by family rather than by dated id: a
/// provider renames `claude-sonnet-4.5` to `claude-sonnet-4-5-20250929` and the
/// price does not move with the spelling. Anything absent is reported as an
/// unknown cost rather than guessed, and `provider.input_per_mtok` /
/// `output_per_mtok` make it exact for whatever this deployment actually pays.
///
/// Reseller-hosted ids are deliberately absent: Llama on Groq, Together and
/// Fireworks is three different prices for one model name, so a table keyed by
/// the model alone would print a number the host did not charge.
pub(crate) fn builtin_price(model: &str) -> Option<Pricing> {
    // A gateway names the model `vendor/model`; the vendor prefix is routing,
    // not part of the price. `anthropic/claude-sonnet-4.5` is a Sonnet. Dots
    // become dashes so `gpt-4.1-mini` and `gpt-4-1-mini` are one family.
    let model = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase()
        .replace('.', "-");
    // `kilo-auto/free`, `openrouter/free`, `ling-3.0-flash-vl:free`: the id
    // already says what the gateway charges, so the price is a fact of the name
    // rather than a guess. Leaving it unpriced printed "cost unknown" for a
    // free model, which is the one case where the screen did know.
    if model == "free" || model.ends_with("-free") || model.ends_with(":free") {
        return Some(Pricing::FREE);
    }
    // USD per million tokens, as printed on the provider's pricing page.
    const TABLE: &[(&str, f64, f64)] = &[
        // Anthropic. The `claude-3-…` ids predate the `claude-<tier>-<n>`
        // spelling and are pinned separately because their tiers are not the
        // current ones: 3 Haiku is a quarter the price of 3.5 Haiku.
        ("claude-opus-4", 15.0, 75.0),
        ("claude-3-opus", 15.0, 75.0),
        ("claude-opus", 15.0, 75.0),
        ("claude-sonnet-4", 3.0, 15.0),
        ("claude-3-7-sonnet", 3.0, 15.0),
        ("claude-3-5-sonnet", 3.0, 15.0),
        ("claude-3-sonnet", 3.0, 15.0),
        ("claude-sonnet", 3.0, 15.0),
        ("claude-haiku-4", 1.0, 5.0),
        ("claude-3-5-haiku", 0.8, 4.0),
        ("claude-3-haiku", 0.25, 1.25),
        ("claude-haiku", 1.0, 5.0),
        // OpenAI.
        ("gpt-4-1-nano", 0.1, 0.4),
        ("gpt-4-1-mini", 0.4, 1.6),
        ("gpt-4-1", 2.0, 8.0),
        ("gpt-4o-mini", 0.15, 0.6),
        ("gpt-4o", 2.5, 10.0),
        ("gpt-4-turbo", 10.0, 30.0),
        ("gpt-3-5-turbo", 0.5, 1.5),
        ("o1-mini", 1.1, 4.4),
        ("o1-preview", 15.0, 60.0),
        ("o3-mini", 1.1, 4.4),
        ("o1", 15.0, 60.0),
        // Google. `gemini-flash-1-5` is the gateway spelling of 1.5 Flash.
        ("gemini-1-5-pro", 1.25, 5.0),
        ("gemini-1-5-flash", 0.075, 0.3),
        ("gemini-flash-1-5", 0.075, 0.3),
        ("gemini-1-0-pro", 0.5, 1.5),
        ("gemini-2-0-flash", 0.1, 0.4),
        ("gemini-2-5-pro", 1.25, 10.0),
        ("gemini-2-5-flash", 0.3, 2.5),
        // DeepSeek, Mistral, Cohere, Perplexity: first-party list prices.
        ("deepseek-chat", 0.27, 1.1),
        ("deepseek-coder", 0.27, 1.1),
        ("deepseek-reasoner", 0.55, 2.19),
        ("mistral-large", 2.0, 6.0),
        ("mistral-small", 0.2, 0.6),
        ("codestral", 0.3, 0.9),
        ("pixtral-12b", 0.15, 0.15),
        ("command-r-plus", 2.5, 10.0),
        ("command-r", 0.15, 0.6),
        ("sonar-pro", 3.0, 15.0),
        ("sonar", 1.0, 1.0),
    ];
    // Longest family wins, so `gpt-4.1-mini` is not billed as `gpt-4.1`, and a
    // dated id (`claude-3-5-sonnet-20241022`) still finds its family.
    let mut best: Option<Pricing> = None;
    let mut best_len = 0; // Sentinel: every family is longer than zero characters.
    for &(family, input, output) in TABLE {
        if family.len() > best_len && contains_token(&model, family) {
            best = Some(Pricing {
                input_per_mtok: input,
                output_per_mtok: output,
            });
            best_len = family.len();
        }
    }
    best
}

/// Whether `needle` appears in `haystack` as one whole family name.
///
/// Model ids put the family in the middle (`llama-3-1-sonar-large-128k`), so a
/// prefix test is not enough — but a bare `contains` would price `gpt-40` as
/// `gpt-4`. The characters either side have to be separators.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before && after {
            return true;
        }
        from = start + 1;
    }
    false
}

/// What this machine is actually configured for, after flags, env and defaults
/// have been reconciled.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Wire shape.
    pub kind: Kind,
    /// Model id.
    pub model: String,
    /// Endpoint base, without a trailing slash.
    pub base_url: String,
    /// Reply budget.
    pub max_tokens: u32,
    /// `MEMNOTH_API_KEY`, then the provider's own variable. `None` means the
    /// session keeps answering from the stores and says why.
    pub api_key: Option<String>,
    /// Rates for the cost estimate, when the model is priced.
    pub pricing: Option<Pricing>,
    /// The preset the key is stored under, when the config names one.
    pub provider_name: Option<String>,
    /// Sampling temperature for this turn, when the config sets one.
    pub temperature: Option<f32>,
    /// Nucleus sampling for this turn, when the config sets one.
    pub top_p: Option<f32>,
    /// Reasoning effort for this turn, when the config sets one.
    pub reasoning_effort: Option<String>,
    /// Where the key came from, for the screens that explain a connection.
    pub key_source: Option<String>,
}

impl Settings {
    /// Resolve the session's model settings.
    ///
    /// Precedence, highest first: environment, `[provider]` in the config, the
    /// provider's default. `MEMNOTH_PROVIDER` makes an experiment possible
    /// without editing a config that the rest of the machine reads.
    pub fn load(config: &Config) -> Result<Self> {
        let section = config.provider.clone().unwrap_or_default();

        let env_kind = env_string("MEMNOTH_PROVIDER");
        let kind = match env_kind.clone().or(section.kind) {
            Some(raw) => Kind::parse(&raw)?,
            // A key in the environment is the strongest signal of intent there
            // is: someone who exported ANTHROPIC_API_KEY wants to use it.
            None => {
                if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
                    Kind::Anthropic
                } else if std::env::var_os("OPENAI_API_KEY").is_some() {
                    Kind::OpenAi
                } else if std::env::var_os("KILO_API_KEY").is_some() {
                    Kind::Kilo
                } else {
                    Kind::Anthropic
                }
            }
        };

        let base_url = env_string("MEMNOTH_PROVIDER_BASE_URL")
            .or(section.base_url)
            .unwrap_or_else(|| kind.default_base_url().to_string());
        let model = env_string("MEMNOTH_MODEL")
            .or(section.model)
            .unwrap_or_else(|| kind.default_model().to_string());
        let max_tokens = section
            .max_tokens
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_TOKENS);

        // The key is resolved once, here, so every screen that reports a
        // connection reports the same one a turn would use. An environment
        // override means "not the configured preset", so its stored key is not
        // the one this turn should use.
        let provider_name = if env_kind.is_some() {
            None
        } else {
            section.name.clone()
        };
        let (api_key, key_source) = resolve_api_key(kind, provider_name.as_deref());

        // An explicit pair of rates is a statement about what this deployment
        // pays, so it wins over the built-in list price. One rate alone is not
        // a price; it is a mistake worth ignoring rather than half-applying.
        let pricing = match (section.input_per_mtok, section.output_per_mtok) {
            (Some(input), Some(output)) if input >= 0.0 && output >= 0.0 => Some(Pricing {
                input_per_mtok: input,
                output_per_mtok: output,
            }),
            _ => builtin_price(&model),
        };

        Ok(Self {
            kind,
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            max_tokens,
            api_key,
            pricing,
            provider_name,
            temperature: section.temperature,
            top_p: section.top_p,
            reasoning_effort: section
                .reasoning_effort
                .as_deref()
                .map(str::trim)
                .filter(|effort| matches!(*effort, "minimal" | "low" | "medium" | "high" | "off"))
                .map(str::to_string),
            key_source,
        })
    }

    /// Whether a turn can run.
    pub fn ready(&self) -> bool {
        // For Memnoth provider, we can run without an explicit API key
        // because it can work with mn login (token from credential store)
        // and will give a helpful error if credits are exhausted.
        match self.kind {
            Kind::Memnoth => true,
            _ => self.api_key.is_some(),
        }
    }

    /// The env var to name when the key is missing.
    pub fn key_hint(&self) -> String {
        match self.kind {
            Kind::Memnoth => {
                "run `mn login` to authenticate, or export MEMNOTH_API_KEY".to_string()
            }
            Kind::Kilo => "KILO_API_KEY (or KILOCODE_API_KEY)".to_string(),
            _ => format!("MEMNOTH_API_KEY (or {})", self.kind.key_env()),
        }
    }

    /// The cost of one turn, in micro-dollars, when the model is priced.
    pub fn cost_micros(&self, input_tokens: u64, output_tokens: u64) -> Option<u64> {
        self.pricing
            .map(|pricing| pricing.cost_micros(input_tokens, output_tokens))
    }

    /// One line for `doctor`: what would answer, not what might.
    pub fn describe(&self) -> String {
        let key = if self.ready() {
            "key set".to_string()
        } else {
            format!("no key — export {}", self.key_hint())
        };
        format!(
            "{} · {} · {} · {key}",
            self.kind.name(),
            self.model,
            self.base_url
        )
    }
}

/// The key a turn would use, and where it came from.
///
/// A key stored for the *preset* wins: someone who typed it into `/connect` (or
/// ran `mn connect add`) has just said which provider they mean, and the
/// collapsed `kind` — every OpenAI-compatible gateway is `openai` — cannot tell
/// them apart. After that the account token and the environment, then the key
/// stored under the kind's own name.
fn resolve_api_key(kind: Kind, provider_name: Option<&str>) -> (Option<String>, Option<String>) {
    let stored = |name: &str| {
        get_credential_sync(&format!("provider_api_key_{name}"))
            .ok()
            .flatten()
            .filter(|key| !key.trim().is_empty())
    };

    if let Some(name) = provider_name
        && let Some(key) = stored(name)
    {
        return (Some(key), Some(format!("stored for {name}")));
    }

    if matches!(kind, Kind::Memnoth)
        && let Some(key) = account_key_sync()
    {
        return (Some(key), Some("account sign-in".to_string()));
    }

    if let Some(key) = env_string("MEMNOTH_API_KEY") {
        return (Some(key), Some("MEMNOTH_API_KEY".to_string()));
    }

    if matches!(kind, Kind::Kilo) {
        // The gateway documents KILO_API_KEY; Kilo Code's own tooling has long
        // exported KILOCODE_API_KEY, so both are read.
        for variable in ["KILO_API_KEY", "KILOCODE_API_KEY"] {
            if let Some(key) = env_string(variable) {
                return (Some(key), Some(variable.to_string()));
            }
        }
    } else if let Some(key) = env_string(kind.key_env()) {
        return (Some(key), Some(kind.key_env().to_string()));
    }

    if let Some(key) = stored(kind.name()) {
        return (Some(key), Some(format!("stored for {}", kind.name())));
    }

    (None, None)
}

/// Fetch available models from the Memnoth API.
/// Returns a list of model IDs, or an error if the request fails.
pub fn fetch_memnoth_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("memnoth/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("cannot build the HTTP client")?;

    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = client.get(&url);
    if let Some(key) = api_key
        && !key.trim().is_empty()
    {
        request = request.header("authorization", format!("Bearer {}", key));
    }
    let response = request
        .send()
        .with_context(|| format!("cannot reach {}", url))?;
    let status = response.status();
    let text = response
        .text()
        .with_context(|| format!("cannot read the reply from {}", url))?;

    if !status.is_success() {
        if status.as_u16() == 402 {
            bail!(
                "out of credits — run `mn credits` to check balance, top up at {}",
                crate::context::console_url("usage")
            );
        }
        bail!(
            "{} answered {}: {}",
            url,
            status,
            scrub_secrets(
                &provider_error(&text).unwrap_or_else(|| clip(&text, 600)),
                api_key
            )
        );
    }

    let value: Value = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} did not answer with JSON: {}",
            url,
            scrub_secrets(&clip(&text, 400), api_key)
        )
    })?;

    // Expect OpenAI-compatible format: { "data": [{ "id": "model-1" }, ...] }
    let models = value["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("unexpected models response format: missing 'data' array"))?
        .iter()
        .filter_map(|m| m["id"].as_str().map(|s| s.to_string()))
        .collect();

    Ok(models)
}

/// Fetch the model ids a configured provider offers.
///
/// `fetch_memnoth_models` speaks the managed endpoint's dialect; this speaks
/// each wire shape's own, so `/models` can show what the provider actually
/// serves instead of a catalogue baked into the binary. A provider that does
/// not implement `GET /models` is a normal failure here, not a broken config:
/// the caller keeps its offline catalogue.
pub fn fetch_provider_models(
    kind: Kind,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("memnoth/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("cannot build the HTTP client")?;

    // Anthropic serves `/v1/models`, the OpenAI shape serves `/models` on the
    // base URL that already ends in `/v1` for most gateways.
    let url = match kind {
        Kind::Anthropic => format!("{}/v1/models", base_url.trim_end_matches('/')),
        _ => format!("{}/models", base_url.trim_end_matches('/')),
    };

    let mut request = client.get(&url);
    if let Some(key) = api_key.filter(|key| !key.trim().is_empty()) {
        request = match kind {
            Kind::Anthropic => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => request.header("authorization", format!("Bearer {}", key)),
        };
    }

    let response = request
        .send()
        .with_context(|| format!("cannot reach {}", url))?;
    let status = response.status();
    let text = response
        .text()
        .with_context(|| format!("cannot read the reply from {}", url))?;

    if !status.is_success() {
        bail!(
            "{} answered {}: {}",
            url,
            status,
            scrub_secrets(
                &provider_error(&text).unwrap_or_else(|| clip(&text, 300)),
                api_key
            )
        );
    }

    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("{} did not answer with JSON", url))?;
    let models = value["data"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("unexpected models response: no 'data' array"))?
        .iter()
        .filter_map(|model| model["id"].as_str().map(str::to_string))
        .collect();
    Ok(models)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    /// Who said it.
    pub role: Role,
    /// What was said.
    pub text: String,
}

/// A completed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The model's final text.
    pub text: String,
    /// Prompt tokens reported by the provider.
    pub input_tokens: u64,
    /// Completion tokens reported by the provider.
    pub output_tokens: u64,
    /// How many tool rounds it took (0 for an immediate answer).
    pub rounds: usize,
}

impl Answer {
    /// Accounting for the session file.
    pub fn usage(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }
}

/// The tools a turn may call. Implemented once, over the CLI's own dispatcher.
pub trait Tools {
    /// Run one tool and return the text the model should see.
    fn call(&mut self, name: &str, input: &Value) -> Result<String>;

    /// Retrieve any write receipts generated during tool calls.
    /// Called after a turn completes to persist file edits to the session.
    fn take_writes(&mut self) -> Vec<memnoth_session::WriteReceipt> {
        Vec::new()
    }

    /// Retrieve approval decisions made during tool calls.
    ///
    /// A write whose consent is not recorded is a write nobody can audit, so
    /// the decision travels beside the write it authorised.
    fn take_approvals(&mut self) -> Vec<memnoth_session::ApprovalRecord> {
        Vec::new()
    }
}
/// A change the model wants to make, shown to the person before it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// The tool that asked.
    pub tool: &'static str,
    /// One line: what, and where.
    pub title: String,
    /// What will change, in words a person can judge without reading code.
    pub body: String,
}

/// How a tool asks for permission.
///
/// Implemented by the session, which has a screen and a keyboard. A caller
/// without one supplies no approver — and then every write is refused rather
/// than performed silently, because "no one could be asked" is not consent.
pub trait Approver {
    /// `true` to perform the change, `false` to refuse it.
    fn approve(&mut self, request: &ApprovalRequest) -> bool;
}

/// Tools that change something, and therefore always ask first.
pub fn requires_approval(name: &str) -> bool {
    matches!(name, "vault_remember" | "edit_file")
}

/// The vault, the code graph and the workspace, as the model sees them.
pub struct Toolbox<'a> {
    ctx: &'a crate::context::AppContext,
    approver: Option<Box<dyn Approver + 'a>>,
    /// Write receipts collected during this turn.
    writes: Vec<WriteReceipt>,
    /// Approval decisions collected during this turn.
    approvals: Vec<memnoth_session::ApprovalRecord>,
    /// Current turn index for correlating writes with receipts.
    turn_id: Option<usize>,
}
impl<'a> Toolbox<'a> {
    /// Bind the tools to this invocation's workspace and vault.
    pub fn new(ctx: &'a crate::context::AppContext) -> Self {
        Self {
            ctx,
            approver: None,
            writes: Vec::new(),
            approvals: Vec::new(),
            turn_id: None,
        }
    }

    /// Give the toolbox a way to ask the person before a write.
    pub fn with_approver(mut self, approver: Box<dyn Approver + 'a>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Set the current turn index for correlating writes with receipts.
    pub fn with_turn_id(mut self, turn_id: usize) -> Self {
        self.turn_id = Some(turn_id);
        self
    }

    /// Ask, and turn a refusal into an error the model can read and respect.
    ///
    /// Every outcome is recorded, including the refusal: "nothing was written"
    /// and "the agent never asked" are different facts, and only one of them is
    /// visible from the file system afterwards.
    fn confirm(&mut self, request: &ApprovalRequest) -> Result<()> {
        let Some(approver) = self.approver.as_mut() else {
            // No approver is a refusal by policy, not by absence of evidence.
            self.approvals
                .push(self.approval_record(request, false, "no-approver"));
            bail!(
                "this change needs the user's confirmation, and this session has no one to ask, \
                 so nothing was written"
            );
        };
        let approved = approver.approve(request);
        self.approvals
            .push(self.approval_record(request, approved, "user"));
        if approved {
            return Ok(());
        }
        bail!(
            "the user declined this change. Do not retry it; say what you would have changed and \
             ask how they want to proceed."
        )
    }

    /// The row written for one decision.
    fn approval_record(
        &self,
        request: &ApprovalRequest,
        approved: bool,
        rule: &str,
    ) -> memnoth_session::ApprovalRecord {
        let mut record = memnoth_session::ApprovalRecord {
            approved_by: approver_identity(),
            ..memnoth_session::ApprovalRecord::user(
                request.tool,
                &request.title,
                approved,
                &approver_identity(),
            )
        };
        record.rule = rule.to_string();
        if let Some(turn_id) = self.turn_id {
            record = record.at_turn(turn_id);
        }
        record
    }

    fn kg_index(&mut self, input: &Value) -> Result<String> {
        let full = input["full"].as_bool().unwrap_or(false);
        let no_embed = input["no_embed"].as_bool().unwrap_or(false);
        let mut quiet = |_: &str| {};
        let report = crate::kg::index(self.ctx, full, no_embed, &mut quiet)?;
        Ok(crate::kg::render_index(&report))
    }

    fn edit_file(&mut self, input: &Value) -> Result<String> {
        let path = text_arg("edit_file", input, "path")?;
        let old = input["old"].as_str().unwrap_or("");
        let new = input["new"]
            .as_str()
            .with_context(|| "edit_file needs a `new` string".to_string())?;
        let preview = crate::edit::preview_edit(&self.ctx.root, &path, old, new)?;
        let request = ApprovalRequest {
            tool: "edit_file",
            title: format!(
                "{} {}",
                if preview.created { "create" } else { "edit" },
                preview.display
            ),
            body: preview.confirmation(),
        };
        self.confirm(&request)?;
        let result = crate::edit::apply(&preview)?;

        // Record the write receipt
        let receipt = WriteReceipt {
            at_ms: session_now_ms(),
            path: preview.display.clone(),
            before: preview.before.clone(),
            after: preview.after.clone(),
            turn_id: self.turn_id,
        };
        self.writes.push(receipt);

        Ok(result)
    }
}

impl<'a> Toolbox<'a> {
    /// The person's own denial, if they wrote one for this tool.
    ///
    /// Read from the config *file*, not from the snapshot the invocation
    /// loaded: `/permissions` saves mid-session, and a denial that only takes
    /// effect after a restart is a screen that does not work. A file that
    /// cannot be read answers "no denial" — startup already refused a malformed
    /// config, so this is a race at worst, and refusing every tool because a
    /// read failed would be a worse lie than the one it prevents.
    fn denied(&self, name: &str) -> Option<String> {
        let config = crate::context::Config::load(&self.ctx.config_path).ok()?;
        let denied = config.tools.map(|tools| tools.denied).unwrap_or_default();
        denied.contains(&name.to_string()).then(|| {
            // A denial names the rule that would allow the call, so the model
            // can ask for the smallest widening instead of retrying the same
            // request or inventing a workaround. `tools.denied` is the exact
            // config key; removing this one name from it is the exact change.
            format!(
                "{name} is denied by the rule `tools.denied` on this machine. The smallest rule \
                 that would allow it is the same list without `{name}` — `/permissions` can make \
                 that change, and the chat agent cannot make it itself."
            )
        })
    }
}

impl Tools for Toolbox<'_> {
    fn call(&mut self, name: &str, input: &Value) -> Result<String> {
        // The denial comes first, before any branch can read or write: a gate
        // that only covers the verbs would leave `read_file` open, and a gate
        // that runs after an edit has already happened is decoration.
        if let Some(reason) = self.denied(name) {
            bail!(reason);
        }

        // File tools are not verbs: `memnoth` has no `edit` command, because an
        // edit typed by a person is their editor. They are here, under the same
        // confirmation gate as a vault write.
        match name {
            "read_file" => {
                let path = text_arg(name, input, "path")?;
                let start = input["start"].as_u64().map(|value| value as usize);
                let end = input["end"].as_u64().map(|value| value as usize);
                return crate::edit::read(&self.ctx.root, &path, start, end);
            }
            "edit_file" => return self.edit_file(input),
            // `kg_index` is dispatched here rather than through its verb for one
            // reason: `memnoth kg index` narrates its progress to stderr, which
            // is right in a terminal and wrong under the alternate screen. The
            // body is the same `kg::index`, rendered by the same function.
            "kg_index" => return self.kg_index(input),
            _ => {}
        }

        // A graph question in a workspace that has never been indexed used to
        // answer "no code graph", leaving the model with nothing to do but ask
        // the person to run `/kg index`. The graph is derived data, not their
        // work, so the agent builds it once and answers from it.
        //
        // The test is the file, not `kg::status`: status stats every indexed
        // path to count staleness, and paying that on every graph call to learn
        // "there is no graph" would be the wrong trade. A graph that exists but
        // cannot be read fails in the tool itself, and `kg_status` names it.
        let auto_indexed = name.starts_with("kg_")
            && !matches!(name, "kg_status" | "kg_index")
            && !self.ctx.graph_path.is_file();
        if auto_indexed {
            let mut quiet = |_: &str| {};
            crate::kg::index(self.ctx, false, false, &mut quiet)?;
        }

        let verb = verb_for(name, input)?;
        if requires_approval(name) {
            self.confirm(&approval_for(name, input))?;
        }
        let outcome = crate::commands::execute(self.ctx, &verb)?;
        if auto_indexed {
            // Said out loud: a graph that appeared mid-turn is a fact about the
            // answer, and the model should not cite it as if it had been there.
            return Ok(format!(
                "this workspace had no code graph, so it was indexed just now before this answer\n\n{}",
                outcome.text
            ));
        }
        Ok(outcome.text)
    }

    fn take_writes(&mut self) -> Vec<memnoth_session::WriteReceipt> {
        std::mem::take(&mut self.writes)
    }

    fn take_approvals(&mut self) -> Vec<memnoth_session::ApprovalRecord> {
        std::mem::take(&mut self.approvals)
    }
}

/// The person at the keyboard, as recorded on an approval.
///
/// `MEMNOTH_USER` first so a shared workstation or a scripted run can name the
/// accountable person; the OS identity is the fallback because the common case
/// is one person on their own machine. `"unknown"` is stated rather than
/// guessed: an approval attributed to a fabricated name is worse than one that
/// admits it has no author.
fn approver_identity() -> String {
    for key in ["MEMNOTH_USER", "USERNAME", "USER"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    "unknown".to_string()
}

/// The confirmation text for a verb-backed write.
fn approval_for(name: &str, input: &Value) -> ApprovalRequest {
    match name {
        "vault_remember" => ApprovalRequest {
            tool: "vault_remember",
            title: format!(
                "remember {} · {}",
                input["name"].as_str().unwrap_or("?"),
                input["kind"].as_str().unwrap_or("fact")
            ),
            body: format!(
                "Record this in the vault?\n\n{}",
                clip(input["text"].as_str().unwrap_or(""), 600)
            ),
        },
        other => ApprovalRequest {
            tool: "write",
            title: format!("change via {other}"),
            body: clip(&input.to_string(), 600),
        },
    }
}

/// A required string argument, named by the tool that wanted it.
fn text_arg(tool: &str, input: &Value, key: &str) -> Result<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .with_context(|| format!("{tool} needs a `{key}` string"))
}

/// Map a tool call onto the verb of the same name.
///
/// Every branch is a documented CLI command: `kg_search` is `memnoth kg search`,
/// `vault_recall` is `memnoth vault recall`. An agent therefore cannot see a
/// different graph than a person typing the verb, and a bug in one is a bug in
/// both rather than in whichever was tested.
pub fn verb_for(name: &str, input: &Value) -> Result<Verb> {
    let text = |key: &str| text_arg(name, input, key);
    let number = |key: &str, default: usize| -> usize {
        input
            .get(key)
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .filter(|value| *value > 0)
            .unwrap_or(default)
            .min(50)
    };

    match name {
        "vault_recall" => Ok(Verb::Vault(VaultArgs {
            action: VaultCommand::Recall {
                query: words(&text("query")?),
                limit: number("limit", 8),
                at: None,
            },
        })),
        "vault_history" => Ok(Verb::Vault(VaultArgs {
            action: VaultCommand::History {
                name: text("name")?,
            },
        })),
        "vault_asof" => Ok(Verb::Vault(VaultArgs {
            action: VaultCommand::AsOf {
                at: Some(text("at")?),
                name: input
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                limit: number("limit", 60),
            },
        })),
        "vault_contradictions" => Ok(Verb::Vault(VaultArgs {
            action: VaultCommand::Contradictions {
                name: input
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
        })),
        "kg_search" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Search {
                query: words(&text("query")?),
                limit: number("limit", 12),
            },
        })),
        "kg_impact" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Impact {
                symbol: text("symbol")?,
                depth: number("depth", 2) as u32,
            },
        })),
        // `explore` and `callers` are the two calls an agent needs to work in a
        // file it has not opened: explore returns the declaration with its
        // snippet and who mentions it, and callers resolves the JSX/import edges
        // that a name search cannot. The session offered only search/impact/
        // status, so the agent had to re-derive both from a flat reference list.
        "kg_explore" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Explore {
                symbol: text("symbol")?,
            },
        })),
        "kg_callers" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Callers {
                symbol: text("symbol")?,
                limit: number("limit", 20),
            },
        })),
        // A caller list answers "who breaks if I change this signature"; it
        // does not answer "what does this function touch", which is the other
        // half of the same edit. `kg_def` is the declaration the two lists are
        // about, and `kg_refs` is every mention when the callers list is too
        // narrow (a type used in a field, a constant named in a test).
        "kg_callees" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Callees {
                symbol: text("symbol")?,
                limit: number("limit", 50),
            },
        })),
        "kg_def" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Def {
                symbol: text("symbol")?,
            },
        })),
        "kg_refs" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Refs {
                symbol: text("symbol")?,
                limit: number("limit", 100),
            },
        })),
        "kg_status" => Ok(Verb::Kg(KgArgs {
            action: KgCommand::Status,
        })),
        "vault_remember" => Ok(Verb::Vault(VaultArgs {
            action: VaultCommand::Remember {
                name: text("name")?,
                kind: input["kind"].as_str().unwrap_or("fact").to_string(),
                confidence: input["confidence"].as_f64(),
                entity_kind: input["entity_kind"]
                    .as_str()
                    .unwrap_or("concept")
                    .to_string(),
                text: words(&text("text")?),
                author: input["author"].as_str().map(str::to_owned),
            },
        })),
        other => bail!(
            "unknown tool {other:?} — this session offers {}",
            tool_names().join(", ")
        ),
    }
}

fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_string).collect()
}

/// Every tool name, for `doctor`-style messages and for tests.
pub fn tool_names() -> Vec<&'static str> {
    spec().iter().map(|(name, _, _)| *name).collect::<Vec<_>>()
}

/// Every tool with the sentence the model reads about it.
///
/// `/permissions` shows this list rather than a hand-written one: a screen that
/// claims to audit the agent's power has to name the tools the agent actually
/// has, or it is auditing something else.
pub fn tool_descriptions() -> Vec<(&'static str, &'static str)> {
    spec()
        .iter()
        .map(|(name, description, _)| (*name, *description))
        .collect()
}

/// Whether this tool changes something, for the permissions screen.
pub fn is_write_tool(name: &str) -> bool {
    requires_approval(name)
}

fn spec() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "vault_recall",
            "Search the temporal vault — facts, decisions, lessons and corrections with the dates they became true. Use it for anything the team decided or learned, not for anything visible in the code.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to search for." },
                    "limit": { "type": "integer", "description": "Maximum hits (default 8)." }
                },
                "required": ["query"]
            }),
        ),
        (
            "vault_history",
            "Every version of one vault entity, oldest first, with the reason each version closed. Use it when a belief changed and the reason for the change is the question.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Vault entity name." } },
                "required": ["name"]
            }),
        ),
        (
            "vault_asof",
            "What the vault believed at an instant. `at` is RFC 3339, a date (midnight UTC) or epoch milliseconds. Use it for \"what did we think in March\".",
            json!({
                "type": "object",
                "properties": {
                    "at": { "type": "string", "description": "Instant to look at." },
                    "name": { "type": "string", "description": "Restrict to one entity." },
                    "limit": { "type": "integer", "description": "Maximum entities (default 60)." }
                },
                "required": ["at"]
            }),
        ),
        (
            "vault_contradictions",
            "Beliefs that were explicitly contradicted, with both sides kept. Use it before recommending either side of a disagreement.",
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Restrict to one entity." } }
            }),
        ),
        (
            "vault_remember",
            "Record a fact, decision, lesson, gotcha or correction in the temporal vault. The person sees the belief and must approve it before it is written. Use it when they ask you to remember something, not to store your own commentary.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Entity the belief is about." },
                    "text": { "type": "string", "description": "The belief, as one or two sentences." },
                    "kind": {
                        "type": "string",
                        "enum": ["fact", "decision", "lesson", "gotcha", "correction"],
                        "description": "What kind of belief it is (default: fact)."
                    },
                    "confidence": { "type": "number", "description": "0 to 1, when the person gave one." },
                    "entity_kind": { "type": "string", "description": "Entity type (default: concept)." }
                },
                "required": ["name", "text"]
            }),
        ),
        (
            "kg_search",
            "Find symbols declared in the code graph by name. Use it to learn a symbol's exact spelling and where it is defined before asking about impact.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Symbol name or a fragment of it." },
                    "limit": { "type": "integer", "description": "Maximum hits (default 12)." }
                },
                "required": ["query"]
            }),
        ),
        (
            "kg_impact",
            "Definitions, references, tests and the blast radius of one symbol. This is the tool for \"what breaks if I change this\".",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Exact symbol name, optionally qualified (`Module::name`)." },
                    "depth": { "type": "integer", "description": "How far to follow definitions (default 2)." }
                },
                "required": ["symbol"]
            }),
        ),
        (
            "kg_explore",
            "One symbol's definition, the source snippet, and every place it is mentioned. Use it instead of reading a whole file when you want to know what a name is and how it is used.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Exact symbol name, optionally qualified (`Module::name`)." }
                },
                "required": ["symbol"]
            }),
        ),
        (
            "kg_callers",
            "Who uses a symbol: the imports and JSX/function call sites that reach it. Use it before changing a signature to see the call sites a rename must follow.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Exact symbol name." },
                    "limit": { "type": "integer", "description": "Maximum call sites (default 20)." }
                },
                "required": ["symbol"]
            }),
        ),
        (
            "kg_callees",
            "What a function calls: the declarations it mentions, marked call-shaped where the source reads like a call. The mirror of kg_callers, and the pair to read before changing a signature or a body.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Exact symbol name." },
                    "limit": { "type": "integer", "description": "Maximum callees (default 50)." }
                },
                "required": ["symbol"]
            }),
        ),
        (
            "kg_def",
            "Where a symbol is declared, with a short snippet of the declaration. Use it when you need the file, line and kind rather than the graph around it; kg_explore adds references, callers and callees to the same answer.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Exact symbol name, optionally qualified." }
                },
                "required": ["symbol"]
            }),
        ),
        (
            "kg_refs",
            "Every mention of a symbol, call sites and incidental mentions alike. Use it when kg_callers is too narrow: a type in a field, a constant named in a test, a name mentioned in prose.",
            json!({
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Exact symbol name." },
                    "limit": { "type": "integer", "description": "Maximum references (default 100)." }
                },
                "required": ["symbol"]
            }),
        ),
        (
            "kg_index",
            "Build or refresh the code graph for this workspace. Derived data, not a change to the person's work, so you may call it yourself without asking — do it when kg_status says the graph is not indexed or is stale, and never tell the person to run `memnoth kg index` for something you can do.",
            json!({
                "type": "object",
                "properties": {
                    "full": { "type": "boolean", "description": "Rebuild from scratch instead of updating changed files." },
                    "no_embed": { "type": "boolean", "description": "Skip embedding symbols, for a structural-only rebuild." }
                }
            }),
        ),
        (
            "kg_status",
            "What the code graph was built from, when, and how stale it is. Call it when a search unexpectedly returns nothing — and when it says the graph is missing or stale, refresh it with kg_index before trusting another miss.",
            json!({ "type": "object", "properties": {} }),
        ),
        (
            "read_file",
            "Read a UTF-8 file from the workspace, optionally a line range. Read before editing: `edit_file` replaces an exact snippet, so the text has to match the file byte for byte. Raw text, no line numbers.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace." },
                    "start": { "type": "integer", "description": "First line, 1-based." },
                    "end": { "type": "integer", "description": "Last line, inclusive." }
                },
                "required": ["path"]
            }),
        ),
        (
            "edit_file",
            "Replace one exact snippet in a file, or create a file when `old` is empty and the path does not exist. `old` must occur exactly once. The person sees the change and must approve it before anything is written; a refusal is final.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace." },
                    "old": { "type": "string", "description": "Exact text to replace; empty to create a new file." },
                    "new": { "type": "string", "description": "Replacement text." }
                },
                "required": ["path", "new"]
            }),
        ),
    ]
}

fn wire_tools(kind: Kind) -> Value {
    let tools: Vec<Value> = spec()
        .into_iter()
        .map(|(name, description, parameters)| match kind {
            Kind::Anthropic => json!({
                "name": name,
                "description": description,
                "input_schema": parameters,
            }),
            Kind::OpenAi | Kind::Memnoth | Kind::Kilo => json!({
                "type": "function",
                "function": { "name": name, "description": description, "parameters": parameters },
            }),
        })
        .collect();
    Value::Array(tools)
}

/// The instructions the model runs under. Paths are absolute so a citation can
/// be opened by the person reading it.
fn identity_prompt(root: &Path) -> String {
    format!(
        "You are memnoth, the agent inside the user's terminal session, working in the workspace \
         `{}`.\n\n\
         You have tools over everything that belongs to this workspace: the temporal vault \
         (beliefs with the dates they became true, including decisions and corrections), the code \
         graph (symbols and references extracted from the files), and the files themselves. Use \
         them before answering a question about this codebase or this team's history — a guess that \
         reads fluently is worse than a tool call that returns nothing. Cite what you used: \
         `path:line` for code, the entity name for a vault fact. Say plainly when a store has \
         nothing; do not fill the gap with invention.\n\n\
         Three tools change state: `kg_index` builds the code graph, `vault_remember` records a \
         belief and `edit_file` changes a file. Indexing is derived data, so call `kg_index` \
         yourself whenever the graph is missing or `kg_status` says it is stale — never ask the \
         person to run `memnoth kg index` for you. The other two the user sees and must approve it \
         first; if they decline, do not retry it, do not repeat it in different words, and say \
         what you would have changed. Never claim a change a tool did not make, and never claim to \
         have run a command — those tools are the only actions you have.\n\n\
         Read a file before editing it, and copy the snippet for `old` exactly from `read_file`. If \
         the tool says the text does not match or is ambiguous, read again; do not guess a second \
         snippet.\n\n\
         Answer as terminal text: short paragraphs or dashed lists, no markdown headings, no \
         preamble about what you are about to do. If the question is ambiguous in a way that \
         changes the answer, ask one specific question.",
        root.display()
    )
}

/// The prompt slots. Larger than `budget::default_slots` because these are the
/// authored layers, not retrieved context: the identity contract is ~450 tokens
/// and truncating it would drop the approval rule the agent depends on.
const PROMPT_SLOTS: [(&str, usize); 3] = [("identity", 800), ("policy", 800), ("session", 400)];

/// The effective instruction text from the files this workspace actually has.
///
/// One discovery path, shared with `mn doctor`: the assembler decides precedence
/// and reports lint leakage, so the prompt cannot disagree with the audit.
fn project_policy(root: &Path) -> Option<String> {
    let mut files: Vec<(memnoth_context::Scope, PathBuf)> = vec![
        (memnoth_context::Scope::Project, root.join("AGENTS.md")),
        (memnoth_context::Scope::Project, root.join("CLAUDE.md")),
    ];
    if let Some(home) = dirs::home_dir() {
        files.push((
            memnoth_context::Scope::User,
            home.join(".memnoth").join("AGENTS.md"),
        ));
    }
    let set = memnoth_context::load_from(&files).ok()?;
    let rendered = set.render();
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

/// Installed skills, rendered as a list the model can choose from.
///
/// The registry used to be reachable only through `/skill`: the model could not
/// see that a skill existed, so it never asked to run one. The block names the
/// skill and its purpose; it does not inline the instructions, which would cost
/// the whole prefix budget for skills the turn will not use.
fn skills_block_from(dir: &Path) -> Option<String> {
    if !dir.is_dir() {
        return None;
    }
    let registry = memnoth_skills::SkillRegistry::with_dir(dir).ok()?;
    let skills = registry.list();
    if skills.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Installed skills, available through `mn skill run <name>`. Run one only when it fits \
         the request, and say why:",
    );
    for entry in skills {
        text.push_str(&format!(
            "\n- {}: {}",
            entry.frontmatter.metadata.name, entry.frontmatter.metadata.description
        ));
    }
    Some(text)
}

fn skills_block() -> Option<String> {
    let dir = memnoth_skills::default_skills_dir().ok()?;
    skills_block_from(&dir)
}

fn prompt_pieces(root: &Path) -> Vec<Piece> {
    let mut pieces = vec![Piece {
        layer: Layer::Global,
        slot: "identity",
        text: identity_prompt(root),
    }];
    if let Some(policy) = project_policy(root) {
        pieces.push(Piece {
            layer: Layer::Project,
            slot: "policy",
            text: policy,
        });
    }
    if let Some(skills) = skills_block() {
        pieces.push(Piece {
            layer: Layer::Session,
            slot: "session",
            text: skills,
        });
    }
    pieces
}

/// Assemble the system prompt and keep the receipt.
///
/// `system_prompt` is the rendering; this is the auditable object behind it.
/// Both requests and `mn doctor` go through here, so "what the model saw" has
/// one answer.
pub fn assemble_system_prompt(root: &Path) -> Assembly {
    let slots: Vec<Slot> = PROMPT_SLOTS
        .iter()
        .map(|(name, max_tokens)| Slot {
            name,
            max_tokens: *max_tokens,
        })
        .collect();
    let total = slots.iter().map(|slot| slot.max_tokens).sum();
    assemble(prompt_pieces(root), &slots, &Budget { total })
}

/// The system prompt as the provider receives it.
fn system_prompt(root: &Path) -> String {
    let assembly = assemble_system_prompt(root);
    if assembly.tail.is_empty() {
        assembly.prefix
    } else {
        format!("{}\n\n{}", assembly.prefix, assembly.tail)
    }
}

/// Run one turn: the prompt, the tools, and whatever the model asks for in
/// between.
///
/// `on_note` is called for each tool round with a one-line description of what
/// is being looked up, so a caller can show the turn happening rather than a
/// frozen screen. It is deliberately not called for the final text: the caller
/// already renders that.
pub fn run_turn(
    settings: &Settings,
    root: &Path,
    history: &[Exchange],
    prompt: &str,
    tools: &mut dyn Tools,
    on_note: &mut dyn FnMut(&str),
) -> Result<Answer> {
    match settings.api_key.as_deref() {
        Some(key) if !key.trim().is_empty() => {}
        _ => bail!(
            "no model configured: export {} (or MEMNOTH_API_KEY) and run again",
            settings.key_hint()
        ),
    }
    if settings.model.trim().is_empty() {
        bail!("provider.model is empty — set it in ~/.memnoth/config.toml or MEMNOTH_MODEL");
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECONDS))
        .user_agent(concat!("memnoth/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("cannot build the HTTP client")?;

    let mut conversation = Conversation::new(settings, root, history, prompt);
    for round in 0..MAX_ROUNDS {
        let (body, url, headers) = conversation.request();
        let reply = post_json(
            &client,
            &url,
            headers,
            &body,
            settings.kind,
            settings.api_key.as_deref(),
        )?;
        let step = conversation.absorb(&reply)?;
        if step.calls.is_empty() {
            return Ok(Answer {
                text: step.text,
                input_tokens: conversation.input_tokens,
                output_tokens: conversation.output_tokens,
                rounds: round,
            });
        }

        let mut results = Vec::new();
        for call in &step.calls {
            let label = describe_call(&call.name, &call.input);
            on_note(&label);
            let output = match tools.call(&call.name, &call.input) {
                Ok(text) => text,
                // A failing tool is information, not a dead turn: the model can
                // report it or try a narrower query.
                Err(error) => format!("tool error: {error:#}"),
            };
            results.push((call.id.clone(), clip(&output, TOOL_RESULT_CHARS)));
        }
        conversation.record_round(step.raw_content, results);
    }

    bail!(
        "the model called tools for {MAX_ROUNDS} rounds without answering — stopping the turn \
         (ask a narrower question, or start the session again)"
    )
}

/// One tool call the model asked for.
struct Call {
    id: String,
    name: String,
    input: Value,
}

/// What one response contained.
struct Step {
    text: String,
    calls: Vec<Call>,
    /// The assistant message exactly as the provider sent it. It has to go back
    /// unchanged: Anthropic rejects a tool result whose `tool_use` block was
    /// reconstructed instead of echoed.
    raw_content: Value,
}

/// The provider-neutral state one turn keeps between rounds.
struct Conversation<'a> {
    settings: &'a Settings,
    history: &'a [Exchange],
    prompt: &'a str,
    /// Anthropic messages, or OpenAI messages.
    messages: Vec<Value>,
    /// Assembled once per turn: the prefix must not change between rounds.
    system: String,
    input_tokens: u64,
    output_tokens: u64,
}

impl<'a> Conversation<'a> {
    fn new(
        settings: &'a Settings,
        root: &'a Path,
        history: &'a [Exchange],
        prompt: &'a str,
    ) -> Self {
        let mut conversation = Self {
            settings,
            history,
            prompt,
            messages: Vec::new(),
            system: system_prompt(root),
            input_tokens: 0,
            output_tokens: 0,
        };
        conversation.seed();
        conversation
    }

    fn seed(&mut self) {
        match self.settings.kind {
            Kind::Anthropic => {
                for exchange in self.history {
                    let role = match exchange.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        // A system row from an older file has no place in the
                        // middle of a conversation; the system prompt is ours.
                        Role::System | Role::Tool => continue,
                    };
                    self.messages.push(json!({
                        "role": role,
                        "content": [{ "type": "text", "text": exchange.text }]
                    }));
                }
                self.messages.push(json!({
                    "role": "user",
                    "content": [{ "type": "text", "text": self.prompt }]
                }));
            }
            Kind::OpenAi | Kind::Memnoth | Kind::Kilo => {
                self.messages.push(json!({
                    "role": "system",
                    "content": self.system
                }));
                for exchange in self.history {
                    let role = match exchange.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::System | Role::Tool => continue,
                    };
                    self.messages
                        .push(json!({ "role": role, "content": exchange.text }));
                }
                self.messages
                    .push(json!({ "role": "user", "content": self.prompt }));
            }
        }
    }

    /// Sampling knobs, added only when the config asked for them.
    ///
    /// Sending nothing is not the same as sending 0.7: the provider's own
    /// default is the right answer for anyone who never opened the setting.
    /// Anthropic spells reasoning as a thinking budget rather than an effort
    /// level, so an OpenAI-shaped key is never sent there.
    fn sampled(&self, mut body: Value) -> Value {
        if let Some(object) = body.as_object_mut() {
            if let Some(temperature) = self.settings.temperature {
                object.insert("temperature".into(), json!(temperature));
            }
            if let Some(top_p) = self.settings.top_p {
                object.insert("top_p".into(), json!(top_p));
            }
            if !matches!(self.settings.kind, Kind::Anthropic)
                && takes_reasoning_effort(&self.settings.model)
                && let Some(effort) = self.settings.reasoning_effort.as_deref()
            {
                object.insert("reasoning_effort".into(), json!(effort));
            }
        }
        body
    }

    fn request(&self) -> (Value, String, Vec<(&'static str, String)>) {
        match self.settings.kind {
            Kind::Anthropic => {
                let body = self.sampled(json!({
                    "model": self.settings.model,
                    "max_tokens": self.settings.max_tokens,
                    "system": self.system,
                    "messages": self.messages,
                    "tools": wire_tools(Kind::Anthropic),
                }));
                let url = format!("{}/v1/messages", self.settings.base_url);
                let headers = vec![
                    ("anthropic-version", "2023-06-01".to_string()),
                    (
                        "x-api-key",
                        self.settings.api_key.clone().unwrap_or_default(),
                    ),
                ];
                (body, url, headers)
            }
            Kind::OpenAi | Kind::Kilo => {
                let body = self.sampled(json!({
                    "model": self.settings.model,
                    "max_tokens": self.settings.max_tokens,
                    "messages": self.messages,
                    "tools": wire_tools(Kind::OpenAi),
                    "tool_choice": "auto",
                }));
                let url = format!("{}/chat/completions", self.settings.base_url);
                let headers = vec![(
                    "authorization",
                    format!(
                        "Bearer {}",
                        self.settings.api_key.clone().unwrap_or_default()
                    ),
                )];
                (body, url, headers)
            }
            Kind::Memnoth => {
                let body = self.sampled(json!({
                    "model": self.settings.model,
                    "max_tokens": self.settings.max_tokens,
                    "messages": self.messages,
                    "tools": wire_tools(Kind::OpenAi),
                    "tool_choice": "auto",
                }));
                let url = format!("{}/chat/completions", self.settings.base_url);
                let headers = vec![(
                    "authorization",
                    format!(
                        "Bearer {}",
                        self.settings.api_key.clone().unwrap_or_default()
                    ),
                )];
                (body, url, headers)
            }
        }
    }

    /// Read one response: the text, the tool calls, and the accounting.
    fn absorb(&mut self, reply: &Value) -> Result<Step> {
        match self.settings.kind {
            Kind::Anthropic => {
                let usage = &reply["usage"];
                self.input_tokens += usage["input_tokens"].as_u64().unwrap_or(0);
                self.output_tokens += usage["output_tokens"].as_u64().unwrap_or(0);

                let blocks = reply["content"].as_array().with_context(|| {
                    format!(
                        "the provider returned no content: {}",
                        clip(&reply.to_string(), 400)
                    )
                })?;
                let mut text = String::new();
                let mut calls = Vec::new();
                for block in blocks {
                    match block["type"].as_str().unwrap_or_default() {
                        "text" => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(block["text"].as_str().unwrap_or_default());
                        }
                        "tool_use" => calls.push(Call {
                            id: block["id"].as_str().unwrap_or_default().to_string(),
                            name: block["name"].as_str().unwrap_or_default().to_string(),
                            input: block["input"].clone(),
                        }),
                        _ => {}
                    }
                }
                if text.trim().is_empty() && calls.is_empty() {
                    bail!(
                        "the model returned nothing (stop_reason {:?})",
                        reply["stop_reason"].as_str().unwrap_or("unknown")
                    );
                }
                if reply["stop_reason"].as_str() == Some("max_tokens") {
                    text.push_str("\n\n[stopped at max_tokens — raise provider.max_tokens for a longer answer]");
                }
                Ok(Step {
                    text: text.trim().to_string(),
                    calls,
                    raw_content: reply["content"].clone(),
                })
            }
            Kind::OpenAi | Kind::Kilo => {
                let usage = &reply["usage"];
                self.input_tokens += usage["prompt_tokens"].as_u64().unwrap_or(0);
                self.output_tokens += usage["completion_tokens"].as_u64().unwrap_or(0);

                let message = &reply["choices"][0]["message"];
                if message.is_null() {
                    bail!(
                        "the provider returned no message: {}",
                        clip(&reply.to_string(), 400)
                    );
                }
                let text = message["content"].as_str().unwrap_or_default().to_string();
                let mut calls = Vec::new();
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let arguments = &call["function"]["arguments"];
                    // OpenAI sends arguments as a JSON string; a gateway that
                    // sends an object is accepted rather than punished.
                    let input = match arguments {
                        Value::String(raw) => {
                            serde_json::from_str(raw).unwrap_or_else(|_| json!({ "_raw": raw }))
                        }
                        Value::Null => json!({}),
                        other => other.clone(),
                    };
                    calls.push(Call {
                        id: call["id"].as_str().unwrap_or_default().to_string(),
                        name: call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        input,
                    });
                }
                if text.trim().is_empty() && calls.is_empty() {
                    bail!(
                        "the model returned nothing (finish_reason {:?})",
                        reply["choices"][0]["finish_reason"]
                            .as_str()
                            .unwrap_or("unknown")
                    );
                }
                Ok(Step {
                    text: text.trim().to_string(),
                    calls,
                    raw_content: message.clone(),
                })
            }
            Kind::Memnoth => {
                let usage = &reply["usage"];
                self.input_tokens += usage["prompt_tokens"].as_u64().unwrap_or(0);
                self.output_tokens += usage["completion_tokens"].as_u64().unwrap_or(0);

                let message = &reply["choices"][0]["message"];
                if message.is_null() {
                    bail!(
                        "the provider returned no message: {}",
                        clip(&reply.to_string(), 400)
                    );
                }
                let text = message["content"].as_str().unwrap_or_default().to_string();
                let mut calls = Vec::new();
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    let arguments = &call["function"]["arguments"];
                    let input = match arguments {
                        Value::String(raw) => {
                            serde_json::from_str(raw).unwrap_or_else(|_| json!({ "_raw": raw }))
                        }
                        Value::Null => json!({}),
                        other => other.clone(),
                    };
                    calls.push(Call {
                        id: call["id"].as_str().unwrap_or_default().to_string(),
                        name: call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        input,
                    });
                }
                if text.trim().is_empty() && calls.is_empty() {
                    bail!(
                        "the model returned nothing (finish_reason {:?})",
                        reply["choices"][0]["finish_reason"]
                            .as_str()
                            .unwrap_or("unknown")
                    );
                }
                Ok(Step {
                    text: text.trim().to_string(),
                    calls,
                    raw_content: message.clone(),
                })
            }
        }
    }

    /// Append the assistant message and the tool results, in the shape this
    /// provider expects back.
    fn record_round(&mut self, raw_content: Value, results: Vec<(String, String)>) {
        match self.settings.kind {
            Kind::Anthropic => {
                self.messages.push(json!({
                    "role": "assistant",
                    "content": raw_content,
                }));
                let blocks: Vec<Value> = results
                    .into_iter()
                    .map(|(id, content)| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": content,
                        })
                    })
                    .collect();
                self.messages
                    .push(json!({ "role": "user", "content": blocks }));
            }
            Kind::OpenAi | Kind::Kilo => {
                let mut message = raw_content;
                if message["content"].is_null() {
                    message["content"] = Value::Null;
                }
                self.messages
                    .push(json!({ "role": "assistant", "content": message["content"], "tool_calls": message["tool_calls"] }));
                for (id, content) in results {
                    self.messages.push(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": content,
                    }));
                }
            }
            Kind::Memnoth => {
                let mut message = raw_content;
                if message["content"].is_null() {
                    message["content"] = Value::Null;
                }
                self.messages
                    .push(json!({ "role": "assistant", "content": message["content"], "tool_calls": message["tool_calls"] }));
                for (id, content) in results {
                    self.messages.push(json!({
                        "role": "tool",
                        "tool_call_id": id,
                        "content": content,
                    }));
                }
            }
        }
    }
}

/// Remove the credential from text a provider sent back.
///
/// A rejected request often quotes the key it rejected, and the reply body is
/// written straight into the transcript. The configured key is stripped
/// verbatim on top of the generic pattern redaction, so a provider that masks
/// nothing still cannot put a secret on screen.
pub(crate) fn scrub_secrets(text: &str, secret: Option<&str>) -> String {
    let mut out = memnoth_auth::redaction::redact_token(text);
    if let Some(secret) = secret {
        let secret = secret.trim();
        if secret.len() >= 8 {
            out = out.replace(secret, "***");
        }
    }
    out
}

fn post_json(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: Vec<(&'static str, String)>,
    body: &Value,
    kind: Kind,
    secret: Option<&str>,
) -> Result<Value> {
    let mut request = client.post(url).json(body);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .with_context(|| format!("cannot reach {url}"))?;
    let status = response.status();
    let text = response
        .text()
        .with_context(|| format!("cannot read the reply from {url}"))?;
    if !status.is_success() {
        // Handle 402 (out of credits) specially for Memnoth provider
        if status.as_u16() == 402 && matches!(kind, Kind::Memnoth) {
            bail!(
                "out of credits — run `mn credits` to check balance, top up at {}",
                crate::context::console_url("usage")
            );
        }
        bail!(
            "{url} answered {status}: {}",
            scrub_secrets(
                &provider_error(&text).unwrap_or_else(|| clip(&text, 600)),
                secret
            )
        );
    }
    serde_json::from_str(&text).with_context(|| {
        format!(
            "{url} did not answer with JSON: {}",
            scrub_secrets(&clip(&text, 400), secret)
        )
    })
}

/// Pull the provider's own error sentence out of an error body, so the user
/// reads "invalid x-api-key" instead of a wall of JSON.
fn provider_error(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    ["error", "message", "detail"]
        .iter()
        .find_map(|key| value[*key].as_str().map(str::to_string))
        .or_else(|| value["error"]["message"].as_str().map(str::to_string))
}

/// One line describing a tool call, for the transcript.
pub fn describe_call(name: &str, input: &Value) -> String {
    let arg = ["query", "symbol", "name", "at"]
        .iter()
        .find_map(|key| input[*key].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let text = input.to_string();
            if text == "{}" {
                String::new()
            } else {
                clip(&text, 60)
            }
        });
    let label = name.replace('_', " ");
    if arg.is_empty() {
        label
    } else {
        format!("{label} {arg}")
    }
}

/// Truncate on a character boundary, saying that it happened.
///
/// The note is counted inside the budget rather than added to it: a caller that
/// promised the model `TOOL_RESULT_CHARS` gets at most that many characters,
/// marker included.
pub fn clip(text: &str, limit: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    const NOTE: &str = "\n… truncated; narrow the query";
    let note_len = NOTE.chars().count();
    if limit <= note_len {
        return text.chars().take(limit).collect();
    }
    let mut out: String = text.chars().take(limit - note_len).collect();
    out.push_str(NOTE);
    out
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(kind: Kind) -> Settings {
        Settings {
            kind,
            model: "test-model".into(),
            base_url: kind.default_base_url().into(),
            max_tokens: 512,
            api_key: Some("k".into()),
            pricing: None,
            provider_name: None,
            temperature: None,
            top_p: None,
            reasoning_effort: None,
            key_source: Some("test".into()),
        }
    }

    #[test]
    fn an_unknown_provider_kind_is_named_rather_than_defaulted() {
        let error = Kind::parse("gemini").unwrap_err().to_string();
        assert!(error.contains("gemini"), "{error}");
        assert!(error.contains("anthropic"), "{error}");
        assert_eq!(Kind::parse("OpenAI").unwrap(), Kind::OpenAi);
        assert_eq!(Kind::parse("claude").unwrap(), Kind::Anthropic);
        assert_eq!(Kind::parse("kilo").unwrap(), Kind::Kilo);
        assert_eq!(Kind::parse("Kilo-Gateway").unwrap(), Kind::Kilo);
    }

    #[test]
    fn a_gateway_model_is_never_sent_a_reasoning_effort() {
        // Kilo routes Claude through an OpenAI-shaped endpoint, so the model id
        // decides, not the wire shape: the field is a 400 on these ids.
        assert!(takes_reasoning_effort("o3"));
        assert!(takes_reasoning_effort("gpt-5-mini"));
        assert!(takes_reasoning_effort("openai/o3"));
        assert!(!takes_reasoning_effort("anthropic/claude-sonnet-4.5"));
        assert!(!takes_reasoning_effort("meta-llama/llama-3.1-70b"));
        assert!(!takes_reasoning_effort("gpt-4o"));
    }

    #[test]
    fn tool_calls_map_onto_the_documented_verbs() {
        let Verb::Vault(VaultArgs {
            action: VaultCommand::Recall { query, limit, .. },
        }) = verb_for("vault_recall", &json!({"query": "http only", "limit": 3})).unwrap()
        else {
            panic!("expected a recall");
        };
        assert_eq!(query, ["http", "only"].map(str::to_string));
        assert_eq!(limit, 3);

        let Verb::Kg(KgArgs {
            action: KgCommand::Impact { symbol, depth },
        }) = verb_for("kg_impact", &json!({"symbol": "MemoryEngine"})).unwrap()
        else {
            panic!("expected an impact");
        };
        assert_eq!(symbol, "MemoryEngine");
        assert_eq!(depth, 2);
    }

    #[test]
    fn a_tool_call_without_its_argument_fails_by_name() {
        let error = verb_for("kg_search", &json!({})).unwrap_err().to_string();
        assert!(error.contains("kg_search"), "{error}");
        assert!(error.contains("query"), "{error}");
        let error = verb_for("rm_rf", &json!({})).unwrap_err().to_string();
        assert!(error.contains("rm_rf"), "{error}");
    }

    #[test]
    fn limits_are_bounded_so_a_model_cannot_ask_for_everything() {
        let Verb::Kg(KgArgs {
            action: KgCommand::Search { limit, .. },
        }) = verb_for("kg_search", &json!({"query": "x", "limit": 100000})).unwrap()
        else {
            panic!("expected a search");
        };
        assert_eq!(limit, 50);
    }

    #[test]
    fn a_session_offers_the_two_calls_that_locate_a_symbol_in_context() {
        // Without these, an agent could search a name and ask for its blast
        // radius but not read the declaration or find the call sites — the two
        // questions a chat session actually starts with.
        let names = tool_names();
        assert!(names.contains(&"kg_explore"), "{names:?}");
        assert!(names.contains(&"kg_callers"), "{names:?}");
        // The other half of an edit: what the function calls, where it lives,
        // and every mention. Without these a signature change is half-blind.
        assert!(names.contains(&"kg_callees"), "{names:?}");
        assert!(names.contains(&"kg_def"), "{names:?}");
        assert!(names.contains(&"kg_refs"), "{names:?}");
        assert!(matches!(
            verb_for("kg_callees", &json!({ "symbol": "Globe" })),
            Ok(Verb::Kg(KgArgs {
                action: KgCommand::Callees { .. }
            }))
        ));
        assert!(matches!(
            verb_for("kg_def", &json!({ "symbol": "Globe" })),
            Ok(Verb::Kg(KgArgs {
                action: KgCommand::Def { .. }
            }))
        ));
        assert!(matches!(
            verb_for("kg_refs", &json!({ "symbol": "Globe" })),
            Ok(Verb::Kg(KgArgs {
                action: KgCommand::Refs { .. }
            }))
        ));

        let explored = verb_for("kg_explore", &json!({ "symbol": "Globe" }));
        assert!(matches!(
            explored,
            Ok(Verb::Kg(KgArgs {
                action: KgCommand::Explore { .. }
            }))
        ));
        let callers = verb_for("kg_callers", &json!({ "symbol": "Globe" }));
        assert!(matches!(
            callers,
            Ok(Verb::Kg(KgArgs {
                action: KgCommand::Callers { .. }
            }))
        ));
    }

    #[test]
    fn every_offered_tool_has_an_implementation_behind_it() {
        let input = json!({
            "query": "q", "name": "n", "symbol": "s", "at": "2026-01-01",
            "text": "t", "path": "p", "new": "n"
        });
        for name in tool_names() {
            let implemented = matches!(name, "read_file" | "edit_file" | "kg_index")
                || verb_for(name, &input).is_ok();
            assert!(
                implemented,
                "{name} is advertised without an implementation"
            );
        }
    }

    #[test]
    fn only_the_write_tools_ask_the_person_first() {
        assert!(requires_approval("vault_remember"));
        assert!(requires_approval("edit_file"));
        for read_only in [
            "vault_recall",
            "vault_history",
            "kg_impact",
            "kg_explore",
            "kg_callers",
            "read_file",
            "kg_status",
        ] {
            assert!(!requires_approval(read_only), "{read_only}");
        }
    }

    /// The agent may index itself: the graph is derived data, so there is no
    /// approval card and no "run this command for me" hand-off. A workspace
    /// with no graph at all is indexed on the way to answering the question.
    #[test]
    fn the_agent_indexes_itself_instead_of_asking_the_person_to_run_a_command() {
        assert!(!requires_approval("kg_index"));

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn widget() {}\n").unwrap();
        let ctx = test_context(dir.path());
        let mut tools = Toolbox::new(&ctx);

        let answer = tools
            .call("kg_search", &json!({ "query": "widget" }))
            .expect("a graph question must build the graph it needs");
        assert!(ctx.graph_path.is_file(), "the graph is on disk");
        assert!(answer.contains("indexed just now"), "{answer}");

        // An explicit refresh is the same build, without the footnote.
        let refreshed = tools.call("kg_index", &json!({})).expect("kg_index works");
        assert!(!refreshed.contains("indexed just now"), "{refreshed}");
    }

    #[test]
    fn a_vault_write_without_anyone_to_ask_is_refused_rather_than_written() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_context(dir.path());
        let mut tools = Toolbox::new(&ctx);
        let error = tools
            .call(
                "vault_remember",
                &json!({"name": "auth", "text": "uses httpOnly cookies"}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("confirmation"), "{error}");
        assert!(
            !ctx.vault_path.exists(),
            "a refused write must not create the vault"
        );
    }

    #[test]
    fn a_declined_edit_leaves_the_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "before\n").unwrap();
        let ctx = test_context(dir.path());

        struct Decline;
        impl Approver for Decline {
            fn approve(&mut self, request: &ApprovalRequest) -> bool {
                assert_eq!(request.title, "edit a.txt");
                assert!(request.body.contains("edit a.txt"), "{}", request.body);
                false
            }
        }

        let mut tools = Toolbox::new(&ctx).with_approver(Box::new(Decline));
        let error = tools
            .call(
                "edit_file",
                &json!({"path": "a.txt", "old": "before", "new": "after"}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("declined"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before\n");
    }

    #[test]
    fn an_approved_edit_writes_exactly_the_previewed_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let ctx = test_context(dir.path());

        struct Approve;
        impl Approver for Approve {
            fn approve(&mut self, _request: &ApprovalRequest) -> bool {
                true
            }
        }

        let mut tools = Toolbox::new(&ctx).with_approver(Box::new(Approve));
        let result = tools
            .call(
                "edit_file",
                &json!({"path": "a.txt", "old": "two", "new": "three"}),
            )
            .unwrap();
        assert!(result.contains("edited a.txt"), "{result}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nthree\n");
    }

    #[test]
    fn a_read_is_not_gated_and_returns_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
        let ctx = test_context(dir.path());
        let mut tools = Toolbox::new(&ctx);
        let text = tools.call("read_file", &json!({"path": "a.txt"})).unwrap();
        assert_eq!(text, "hello\n");
    }

    #[test]
    fn a_model_whose_id_says_free_is_priced_at_zero_not_left_unknown() {
        // The gateway's own name for the rate is a fact, so "cost unknown" for
        // it would understate what this machine knows.
        assert_eq!(builtin_price("kilo-auto/free"), Some(Pricing::FREE));
        assert_eq!(builtin_price("openrouter/free"), Some(Pricing::FREE));
        assert_eq!(Pricing::FREE.cost_micros(1_000_000, 1_000_000), 0);
        assert!(builtin_price("a-local-model").is_none());
    }

    #[test]
    fn the_models_this_build_offers_have_their_published_rate() {
        // These are the ids in the provider registry's own default lists: the
        // models a person is actually offered. An unknown cost for one of them
        // was a hole in the table, not a fact about the model.
        for model in [
            "claude-3-5-sonnet-20241022",
            "claude-3-opus-20240229",
            "claude-3-5-haiku-20241022",
            "claude-3-haiku-20240307",
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4-turbo",
            "gpt-3.5-turbo",
            "o1-mini",
            "o1-preview",
            "gemini-1.5-pro",
            "gemini-1.5-flash",
            "gemini-2.0-flash-exp",
            "deepseek-chat",
            "deepseek-coder",
            "mistral-large-latest",
            "codestral-latest",
            "command-r-plus",
            "command-r",
            "llama-3.1-sonar-large-128k-online",
            "anthropic/claude-sonnet-4.5",
        ] {
            assert!(builtin_price(model).is_some(), "{model} has no rate");
        }
        assert_eq!(
            builtin_price("gemini-1.5-flash").unwrap().input_per_mtok,
            0.075,
            "a dotted id is the same family as its dashed spelling"
        );
        assert_eq!(
            builtin_price("claude-3-haiku-20240307")
                .unwrap()
                .input_per_mtok,
            0.25,
            "3 Haiku is not 3.5 Haiku's price"
        );
        assert!(
            builtin_price("meta-llama/llama-3.1-405b-instruct").is_none(),
            "a reseller sets that price, so it stays unknown"
        );
    }

    #[test]
    fn cost_is_priced_from_the_rates_in_force_and_never_invented() {
        let priced = Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
        };
        assert_eq!(priced.cost_micros(1_000_000, 0), 3_000_000);
        assert_eq!(priced.cost_micros(0, 1_000_000), 15_000_000);
        assert_eq!(priced.cost_micros(0, 0), 0);

        let sonnet = builtin_price("claude-sonnet-4-5-20250929").expect("known model");
        assert_eq!(sonnet.input_per_mtok, 3.0);
        assert_eq!(
            builtin_price("gpt-4.1-mini").unwrap().input_per_mtok,
            0.4,
            "the longest prefix wins, so a mini model is not billed as its parent"
        );
        assert!(builtin_price("a-local-model").is_none());
        assert_eq!(
            builtin_price("anthropic/claude-sonnet-4.5")
                .unwrap()
                .input_per_mtok,
            3.0,
            "a gateway's vendor prefix does not hide the price"
        );
    }

    fn test_context(root: &Path) -> crate::context::AppContext {
        crate::context::AppContext {
            root: root.to_path_buf(),
            home: root.join(".memnoth"),
            vault_path: root.join(".memnoth").join("vault.db"),
            namespace: "default".into(),
            graph_path: root.join(".memnoth").join("graph.db"),
            sessions_dir: root.join("sessions"),
            config_path: root.join("config.toml"),
            exe_path: std::path::PathBuf::from("memnoth"),
            config: Config::default(),
            json: false,
            quiet: true,
        }
    }

    #[test]
    fn both_wire_shapes_advertise_the_same_tool_names() {
        for kind in [Kind::Anthropic, Kind::OpenAi, Kind::Memnoth, Kind::Kilo] {
            let tools = wire_tools(kind);
            let names: Vec<&str> = tools
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| match kind {
                    Kind::Anthropic => tool["name"].as_str().unwrap(),
                    Kind::OpenAi | Kind::Memnoth | Kind::Kilo => {
                        tool["function"]["name"].as_str().unwrap()
                    }
                })
                .collect();
            assert_eq!(names, tool_names());
        }
    }

    #[test]
    fn a_truncated_tool_result_says_so_instead_of_cutting_mid_word_silently() {
        let long = "α".repeat(TOOL_RESULT_CHARS + 100);
        let clipped = clip(&long, TOOL_RESULT_CHARS);
        assert!(clipped.contains("truncated"));
        assert!(clipped.chars().count() <= TOOL_RESULT_CHARS);
        assert_eq!(clip("short", 10), "short");
    }

    #[test]
    fn an_anthropic_reply_is_read_into_text_calls_and_usage() {
        let settings = settings(Kind::Anthropic);
        let mut conversation = Conversation::new(&settings, Path::new("/ws"), &[], "what breaks?");
        let reply = json!({
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 11, "output_tokens": 4 },
            "content": [
                { "type": "text", "text": "Looking." },
                { "type": "tool_use", "id": "t1", "name": "kg_impact", "input": { "symbol": "A" } }
            ]
        });
        let step = conversation.absorb(&reply).unwrap();
        assert_eq!(step.text, "Looking.");
        assert_eq!(step.calls.len(), 1);
        assert_eq!(step.calls[0].id, "t1");
        assert_eq!(conversation.input_tokens, 11);
        assert_eq!(conversation.output_tokens, 4);

        conversation.record_round(step.raw_content, vec![("t1".into(), "hit".into())]);
        assert_eq!(
            conversation.messages.len(),
            3,
            "user, assistant, tool result"
        );
        assert_eq!(
            conversation.messages[2]["content"][0]["type"],
            "tool_result"
        );
    }

    #[test]
    fn an_openai_reply_is_read_into_text_calls_and_usage() {
        let settings = settings(Kind::OpenAi);
        let mut conversation = Conversation::new(&settings, Path::new("/ws"), &[], "what breaks?");
        let reply = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "c1",
                        "type": "function",
                        "function": { "name": "kg_search", "arguments": "{\"query\":\"Engine\"}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 2 }
        });
        let step = conversation.absorb(&reply).unwrap();
        assert_eq!(step.calls.len(), 1);
        assert_eq!(step.calls[0].input["query"], "Engine");
        assert_eq!(conversation.input_tokens, 7);

        conversation.record_round(step.raw_content, vec![("c1".into(), "found".into())]);
        assert_eq!(conversation.messages.last().unwrap()["role"], "tool");
        assert_eq!(conversation.messages.last().unwrap()["tool_call_id"], "c1");
    }

    #[test]
    fn a_gateway_that_sends_arguments_as_an_object_is_accepted() {
        let settings = settings(Kind::OpenAi);
        let mut conversation = Conversation::new(&settings, Path::new("/ws"), &[], "q");
        let reply = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "c1",
                        "function": { "name": "kg_search", "arguments": { "query": "x" } }
                    }]
                }
            }]
        });
        let step = conversation.absorb(&reply).unwrap();
        assert_eq!(step.calls[0].input["query"], "x");
    }

    #[test]
    fn the_system_prompt_names_the_workspace_and_the_confirmation_rule() {
        let prompt = system_prompt(Path::new("/work/thing"));
        assert!(prompt.contains("/work/thing"));
        assert!(prompt.contains("must approve it first"));
        assert!(prompt.contains("Never claim a change"));
    }

    #[test]
    fn the_assembled_prompt_is_byte_identical_between_calls() {
        let first = assemble_system_prompt(Path::new("/work/thing"));
        let second = assemble_system_prompt(Path::new("/work/thing"));
        assert_eq!(first.prefix, second.prefix);
        assert_eq!(first.receipt, second.receipt);
        assert_eq!(
            first.receipt.total_tokens,
            first
                .receipt
                .pieces
                .iter()
                .map(|piece| piece.tokens)
                .sum::<usize>(),
            "the receipt accounts for every admitted piece exactly once"
        );
        assert!(!first.prefix.is_empty(), "identity is always in the prefix");
    }

    #[test]
    fn a_projects_agents_file_becomes_the_policy_layer() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("AGENTS.md"),
            "- Never force-push to main\n- Cite the file you read\n",
        )
        .unwrap();
        let policy = project_policy(root.path()).expect("rules were discovered");
        assert!(policy.contains("Never force-push to main"));
        assert!(policy.contains("Cite the file you read"));
    }

    #[test]
    fn installed_skills_are_visible_to_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("release");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname = \"release\"\ndescription = \"Cut a release\"\n---\n\nSteps.\n",
        )
        .unwrap();
        let block = skills_block_from(dir.path()).expect("the skill was discovered");
        assert!(block.contains("release: Cut a release"));
        assert!(block.contains("mn skill run"));
    }

    #[test]
    fn a_missing_key_is_refused_before_a_request_is_made() {
        let mut settings = settings(Kind::Anthropic);
        settings.api_key = None;
        let mut tools = FakeTools;
        let error = run_turn(
            &settings,
            Path::new("/ws"),
            &[],
            "hello",
            &mut tools,
            &mut |_| {},
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("MEMNOTH_API_KEY") || error.contains("ANTHROPIC_API_KEY"));
    }

    #[derive(Default)]
    struct FakeTools;
    impl Tools for FakeTools {
        fn call(&mut self, _name: &str, _input: &Value) -> Result<String> {
            Ok("nothing".into())
        }
    }
}
