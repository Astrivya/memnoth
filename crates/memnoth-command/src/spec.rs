//! Core command specification types.

use serde::{Deserialize, Serialize};

/// Authentication level required to execute a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthLevel {
    /// No authentication required.
    None,
    /// Requires valid session (local vault access).
    Session,
    /// Requires cloud authentication (API key, OAuth).
    Cloud,
    /// Requires operator/admin privileges.
    Operator,
}

/// Danger level of a command — used for confirmation UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DangerLevel {
    /// Safe read-only operation.
    Safe,
    /// Modifies local state (vault, graph, config).
    LocalWrite,
    /// Modifies remote/cloud state.
    CloudWrite,
    /// Destructive operation (deletes data, revokes access).
    Destructive,
}

/// Overlay type for TUI/slash command rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayType {
    /// No overlay — runs inline.
    None,
    /// Confirmation dialog before execution.
    Confirm,
    /// Multi-step wizard/form.
    Wizard,
    /// Interactive picker (fuzzy search, multi-select).
    Picker,
    /// Full-screen editor/view.
    Fullscreen,
}

/// Parameter type for command arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamType {
    /// String parameter.
    String {
        /// Optional regex pattern for validation.
        pattern: Option<String>,
        /// Suggested completions.
        completions: Option<Vec<String>>,
    },
    /// Integer parameter.
    Integer {
        /// Smallest accepted value, or unbounded.
        min: Option<i64>,
        /// Largest accepted value, or unbounded.
        max: Option<i64>,
    },
    /// Float parameter.
    Float {
        /// Smallest accepted value, or unbounded.
        min: Option<f64>,
        /// Largest accepted value, or unbounded.
        max: Option<f64>,
    },
    /// Boolean flag.
    Boolean,
    /// Enum with fixed choices.
    Enum {
        /// The values the argument may take.
        choices: Vec<String>,
    },
    /// Array of values.
    Array {
        /// Type of each element.
        items: Box<ParamType>,
        /// Fewest elements accepted, or unbounded.
        min_items: Option<usize>,
        /// Most elements accepted, or unbounded.
        max_items: Option<usize>,
    },
    /// File path parameter.
    Path {
        /// Refuse a path that is not already on disk.
        must_exist: bool,
        /// A directory is acceptable.
        dir_ok: bool,
        /// A regular file is acceptable.
        file_ok: bool,
    },
    /// JSON object parameter.
    Json,
    /// Duration (e.g., "30s", "5m", "1h").
    Duration,
    /// Timestamp (RFC3339, date, or epoch ms).
    Timestamp,
}

/// Parameter specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    /// Parameter name (also used as CLI flag name).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Parameter type.
    pub param_type: ParamType,
    /// Whether this parameter is required.
    pub required: bool,
    /// Default value (as JSON).
    pub default: Option<serde_json::Value>,
    /// Short flag (single character).
    pub short: Option<char>,
    /// Long flag override (defaults to `--{name}`).
    pub long: Option<String>,
    /// Environment variable name.
    pub env: Option<String>,
    /// Whether this is a positional argument.
    pub positional: bool,
    /// Position index for positional args (0-based).
    pub position: Option<usize>,
    /// Whether to hide from help/usage.
    pub hidden: bool,
    /// Deprecation notice.
    pub deprecated: Option<String>,
}

impl ParamSpec {
    /// Create a required string parameter.
    pub fn string(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::String {
                pattern: None,
                completions: None,
            },
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create an optional string parameter with default.
    pub fn string_opt(
        name: impl Into<String>,
        description: impl Into<String>,
        default: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::String {
                pattern: None,
                completions: None,
            },
            required: false,
            default: Some(serde_json::Value::String(default.into())),
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create a boolean flag.
    pub fn bool(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Boolean,
            required: false,
            default: Some(serde_json::Value::Bool(false)),
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create an integer parameter.
    pub fn integer(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Integer {
                min: None,
                max: None,
            },
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create an enum parameter.
    pub fn enum_param(
        name: impl Into<String>,
        description: impl Into<String>,
        choices: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Enum { choices },
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create an optional enum parameter with default.
    pub fn enum_opt(
        name: impl Into<String>,
        description: impl Into<String>,
        choices: Vec<String>,
        default: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Enum { choices },
            required: false,
            default: Some(serde_json::Value::String(default.into())),
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create an array parameter.
    pub fn array(
        name: impl Into<String>,
        description: impl Into<String>,
        items: ParamType,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Array {
                items: Box::new(items),
                min_items: None,
                max_items: None,
            },
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create a path parameter.
    pub fn path(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Path {
                must_exist: false,
                dir_ok: true,
                file_ok: true,
            },
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create a JSON parameter.
    pub fn json(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Json,
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create a timestamp parameter.
    pub fn timestamp(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Timestamp,
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Create a duration parameter.
    pub fn duration(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Duration,
            required: true,
            default: None,
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }

    /// Set short flag.
    pub fn short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    /// Set long flag override.
    pub fn long(mut self, long: impl Into<String>) -> Self {
        self.long = Some(long.into());
        self
    }

    /// Set environment variable.
    pub fn env(mut self, env: impl Into<String>) -> Self {
        self.env = Some(env.into());
        self
    }

    /// Mark as positional argument.
    pub fn positional(mut self, position: usize) -> Self {
        self.positional = true;
        self.position = Some(position);
        self
    }

    /// Set completions.
    pub fn completions(mut self, completions: Vec<String>) -> Self {
        if let ParamType::String { completions: c, .. } = &mut self.param_type {
            *c = Some(completions);
        }
        self
    }

    /// Set pattern for string validation.
    pub fn pattern(mut self, pattern: impl Into<String>) -> Self {
        if let ParamType::String { pattern: p, .. } = &mut self.param_type {
            *p = Some(pattern.into());
        }
        self
    }

    /// Mark as hidden.
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Mark as deprecated.
    pub fn deprecated(mut self, msg: impl Into<String>) -> Self {
        self.deprecated = Some(msg.into());
        self
    }
}

/// Slash command specification for TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashSpec {
    /// Slash command name (without leading `/`).
    pub name: String,
    /// Short description for palette.
    pub description: String,
    /// Long description for help.
    pub long_description: Option<String>,
    /// Parameter specs in order.
    pub params: Vec<ParamSpec>,
    /// The full slash line shown in the session palette, without the leading
    /// `/` (for example `"vault asof [--at WHEN]"`). `None` means the command
    /// exists but has no session palette row.
    pub palette: Option<String>,
    /// Overlay type for rendering.
    pub overlay: OverlayType,
    /// Danger level.
    pub danger: DangerLevel,
    /// Auth level required.
    pub auth: AuthLevel,
    /// Tool profile this command belongs to (for MCP gating).
    pub profile: Option<String>,
    /// Categories for grouping in palette.
    pub categories: Vec<String>,
    /// Aliases for the slash command.
    pub aliases: Vec<String>,
    /// Whether this command is hidden from palette.
    pub hidden: bool,
    /// Deprecation notice.
    pub deprecated: Option<String>,
    /// Example usages.
    pub examples: Vec<String>,
}

/// Full command specification — the single source of truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    /// Unique command identifier (e.g., "vault.init", "kg.impact").
    pub id: String,
    /// CLI verb path (e.g., ["vault", "init"], ["kg", "impact"]).
    pub cli_path: Vec<String>,
    /// Slash command specification.
    pub slash: SlashSpec,
    /// Handler function name (for derive macro registration).
    pub handler: String,
    /// Whether this command produces JSON output.
    pub json_output: bool,
    /// Whether this command supports streaming output.
    pub streaming: bool,
    /// Timeout in seconds (None = no timeout).
    pub timeout_secs: Option<u64>,
    /// Minimum memnoth version required.
    pub min_version: Option<String>,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Related command IDs.
    pub related: Vec<String>,
}

impl CommandSpec {
    /// Get the command name (last segment of cli_path).
    pub fn name(&self) -> &str {
        self.cli_path.last().map(|s| s.as_str()).unwrap_or(&self.id)
    }

    /// Get the full CLI command as a string.
    pub fn cli_command(&self) -> String {
        self.cli_path.join(" ")
    }

    /// Get all parameter names.
    pub fn param_names(&self) -> Vec<&str> {
        self.slash.params.iter().map(|p| p.name.as_str()).collect()
    }

    /// Get required parameter names.
    pub fn required_param_names(&self) -> Vec<&str> {
        self.slash
            .params
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.as_str())
            .collect()
    }
}

/// Where a registry row can actually run.
///
/// A palette row that cannot say this is a promise the session cannot keep:
/// `mcp add` refuses without a terminal, `login` without an account, and
/// `/serve` blocks the screen. The chip is shown before Enter, never after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Runs in this session.
    Here,
    /// Opens a surface (overlay) instead of producing text.
    Surface,
    /// Needs its own terminal: it owns the screen or runs until interrupted.
    Terminal,
    /// Needs authentication.
    Auth,
    /// Registered, but not runnable in this build.
    Unavailable,
}

/// What a row risks if it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Reads only; no local, remote, or vault write.
    Read,
    /// Writes local state (vault, graph, config, session).
    Write,
    /// Talks to the network.
    Network,
    /// Deletes or revokes something.
    Destructive,
}

/// Rows that cannot run inside the session because they own the screen or
/// never return. Named by id so a new verb has to opt in to the label rather
/// than inherit it from a category.
const TERMINAL_ONLY: &[&str] = &[
    "serve",
    "watch",
    "kg.serve",
    "kg.watch",
    "mcp.serve",
    "mcp.call",
    "mcp.selftest",
    "mcp.token",
];

impl CommandSpec {
    /// The risk chip, derived from the declared danger.
    pub fn risk(&self) -> Risk {
        match &self.slash.danger {
            DangerLevel::Safe => Risk::Read,
            DangerLevel::LocalWrite => Risk::Write,
            DangerLevel::CloudWrite => Risk::Network,
            DangerLevel::Destructive => Risk::Destructive,
        }
    }

    /// Where this row runs, decided before selection.
    pub fn availability(&self) -> Availability {
        if TERMINAL_ONLY.contains(&self.id.as_str()) {
            return Availability::Terminal;
        }
        if matches!(&self.slash.auth, AuthLevel::Cloud | AuthLevel::Operator) {
            return Availability::Auth;
        }
        if !matches!(&self.slash.overlay, OverlayType::None) {
            return Availability::Surface;
        }
        Availability::Here
    }

    /// The surface this row belongs to.
    ///
    /// `terminal` means the row is run elsewhere; `chat` is the default because
    /// a read-only verb reports into the transcript.
    pub fn screen(&self) -> &'static str {
        let has = |name: &str| self.slash.categories.iter().any(|c| c == name);
        if has("memory") {
            "memory"
        } else if has("code") {
            "review"
        } else if has("session") {
            "sessions"
        } else if self.availability() == Availability::Terminal {
            "terminal"
        } else {
            "chat"
        }
    }

    /// The CLI line that produces the same answer without the session.
    pub fn headless(&self) -> String {
        format!("memnoth {}", self.cli_command())
    }

    /// The next command to run when this one cannot.
    ///
    /// A hint names a command, never an error code: "run `mn auth login`" is
    /// actionable, "401" is not.
    pub fn failure_hint(&self) -> String {
        match self.availability() {
            Availability::Terminal => "run it in another terminal".to_string(),
            Availability::Auth => "run `memnoth auth login` first".to_string(),
            Availability::Unavailable => "run `memnoth help --all` to see what ships".to_string(),
            Availability::Surface | Availability::Here => {
                format!("run `memnoth {} --help`", self.cli_command())
            }
        }
    }

    /// The session row for this command, when it has one.
    pub fn palette_line(&self) -> Option<String> {
        self.slash
            .palette
            .as_ref()
            .map(|line| line.trim().trim_start_matches('/').to_string())
            .filter(|line| !line.is_empty())
    }
}
