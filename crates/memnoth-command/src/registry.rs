//! Command registry with builders for all surfaces.

use crate::spec::{CommandSpec, ParamSpec, ParamType};
use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::boxed::Box;

/// Built artifacts for a single surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltSurface {
    /// Surface name.
    pub name: String,
    /// JSON representation of the built surface.
    pub data: Value,
}
/// Registry metadata for a single command.
///
/// These fields describe how a command is presented, gated, and supported
/// across the CLI, TUI, and headless surfaces. They are the single source of
/// truth for command registry metadata.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CommandMetadata {
    /// Unique command identifier (e.g., "vault.init", "kg.impact").
    pub id: String,
    /// Whether this is the primary entry point for its capability.
    pub primary: bool,
    /// Alternate names the command can be invoked by.
    pub aliases: Vec<String>,
    /// Command group the command belongs to.
    pub group: String,
    /// Short human-readable summary.
    pub summary: String,
    /// Usage line shown in help surfaces.
    pub usage: String,
    /// Example invocations.
    pub examples: Vec<String>,
    /// Risk level of running the command (e.g., "safe", "caution", "destructive").
    pub risk: String,
    /// Surfaces the command is available on (e.g., ["cli", "tui"]).
    pub modes: Vec<String>,
    /// Capabilities or commands required before this one can run.
    pub requires: Vec<String>,
    /// Screen/surface the command renders on.
    pub screen: String,
    /// Availability gate (e.g., "always", "feature:kg").
    pub availability: String,
    /// Parameter names the command accepts.
    pub params: Vec<String>,
    /// Keybindings that trigger the command.
    pub keys: Vec<String>,
    /// Whether the command touches secrets.
    pub secret: bool,
    /// Command is only available on the TUI surface.
    pub tui_only: bool,
    /// Command is available in headless/CLI mode.
    pub headless: bool,
    /// Stability maturity (e.g., "stable", "preview", "experimental").
    pub stability: String,
    /// Version the command was introduced in.
    pub since: String,
    /// Whether the command is hidden from help surfaces.
    pub hidden: bool,
    /// Related command IDs.
    pub related: Vec<String>,
    /// Hint shown when the command fails.
    pub failure_hint: String,
}

/// Command registry — the single source of truth for all commands.
#[derive(Debug, Default, Clone)]
pub struct CommandRegistry {
    commands: IndexMap<String, CommandSpec>,
    categories: std::collections::BTreeMap<String, Vec<String>>,
}

impl CommandRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command spec.
    pub fn register(&mut self, spec: CommandSpec) -> Result<()> {
        if self.commands.contains_key(&spec.id) {
            bail!("Command with id '{}' already registered", spec.id);
        }
        for cat in &spec.slash.categories {
            self.categories
                .entry(cat.clone())
                .or_default()
                .push(spec.id.clone());
        }
        self.commands.insert(spec.id.clone(), spec);
        Ok(())
    }

    /// Get a command by ID.
    pub fn get(&self, id: &str) -> Option<&CommandSpec> {
        self.commands.get(id)
    }

    /// Get all commands.
    pub fn all(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands.values()
    }

    /// Get commands by category.
    pub fn by_category(&self, category: &str) -> Vec<&CommandSpec> {
        self.categories
            .get(category)
            .map(|ids| ids.iter().filter_map(|id| self.commands.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get all categories.
    pub fn categories(&self) -> Vec<&String> {
        self.categories.keys().collect()
    }

    /// Number of registered commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Build a clap Command from the registry.
    pub fn build_clap(&self) -> clap::Command {
        use clap::{Arg, ArgAction, Command as ClapCommand};

        let mut root = ClapCommand::new("memnoth")
            .version(env!("CARGO_PKG_VERSION"))
            // `help` is a registered command, not clap's builtin: leaving the
            // builtin in place duplicates the name and panics in debug builds
            // (and would give the projection a command the binary lacks).
            .disable_help_subcommand(true)
            .about("Terminal agent, bi-temporal vault, code graph and MCP server")
            .long_about("One binary for the whole local product.")
            .after_help("Run `memnoth doctor` when something is not wired.");

        // Global arguments
        root = root
            .arg(
                Arg::new("root")
                    .long("root")
                    .global(true)
                    .value_name("DIR")
                    .help("Workspace to read."),
            )
            .arg(
                Arg::new("vault")
                    .long("vault")
                    .global(true)
                    .value_name("PATH")
                    .help("Vault database."),
            )
            .arg(
                Arg::new("namespace")
                    .long("namespace")
                    .global(true)
                    .value_name("NAME")
                    .default_value("default")
                    .help("Vault namespace."),
            )
            .arg(
                Arg::new("json")
                    .long("json")
                    .global(true)
                    .action(ArgAction::SetTrue)
                    .help("JSON output."),
            )
            .arg(
                Arg::new("quiet")
                    .long("quiet")
                    .global(true)
                    .action(ArgAction::SetTrue)
                    .help("Suppress progress."),
            )
            .arg(
                Arg::new("resume")
                    .long("resume")
                    .global(true)
                    .value_name("NAME")
                    .help("Resume session."),
            );

        // Build subcommands from registry
        let mut built: Vec<(Vec<String>, ClapCommand)> = Vec::new();
        for spec in self.all() {
            if spec.cli_path.is_empty() {
                continue;
            }

            let name: &'static str = &*Box::leak(spec.name().to_string().into_boxed_str());
            let about: &'static str = &*Box::leak(spec.slash.description.clone().into_boxed_str());
            let long: &'static str = &*Box::leak(
                spec.slash
                    .long_description
                    .clone()
                    .unwrap_or_else(|| spec.slash.description.clone())
                    .into_boxed_str(),
            );
            let mut cmd = ClapCommand::new(name).about(about).long_about(long);

            // clap requires positional indexes to be contiguous from 1; the
            // declared `position` is only an ordering hint and is not guaranteed
            // to be dense.
            let mut positional_index = 0usize;
            for param in &spec.slash.params {
                if param.hidden {
                    continue;
                }
                let long_name: &'static str = &*Box::leak(
                    param
                        .long
                        .clone()
                        .unwrap_or_else(|| param.name.replace('_', "-"))
                        .into_boxed_str(),
                );
                let help: &'static str = &*Box::leak(param.description.clone().into_boxed_str());
                let param_name: &str = &*Box::leak(param.name.clone().into_boxed_str());
                // A positional argument may not carry a long or short name; clap
                // rejects that at build time, and it would make the flag and the
                // positional ambiguous if it were accepted.
                let mut arg = if param.positional {
                    Arg::new(param_name).help(help)
                } else {
                    Arg::new(param_name).long(long_name).help(help)
                };

                if let Some(short) = param.short
                    && !param.positional
                {
                    arg = arg.short(short);
                }

                if param.required && !param.positional {
                    arg = arg.required(true);
                }

                if param.positional {
                    positional_index += 1;
                    arg = arg.index(positional_index);
                }

                match &param.param_type {
                    ParamType::Boolean => {
                        arg = arg.action(ArgAction::SetTrue).num_args(0);
                    }
                    ParamType::Enum { choices } => {
                        let vals: Vec<&str> = choices
                            .iter()
                            .map(|s| &*Box::leak(s.clone().into_boxed_str()))
                            .collect();
                        arg = arg.value_parser(clap::builder::PossibleValuesParser::new(vals));
                    }
                    ParamType::Integer { .. } => {
                        arg = arg.value_parser(clap::value_parser!(i64));
                    }
                    ParamType::Float { .. } => {
                        arg = arg.value_parser(clap::value_parser!(f64));
                    }
                    ParamType::String { .. } => {}
                    ParamType::Array { .. } => {
                        arg = arg.num_args(1..);
                    }
                    ParamType::Path { .. } => {
                        arg = arg.value_parser(clap::value_parser!(std::path::PathBuf));
                    }
                    ParamType::Json => {
                        arg = arg.value_parser(clap::value_parser!(serde_json::Value));
                    }
                    ParamType::Duration => {}
                    ParamType::Timestamp => {}
                }

                cmd = cmd.arg(arg);
            }

            for alias in &spec.slash.aliases {
                let alias_static: &'static str = &*Box::leak(alias.clone().into_boxed_str());
                cmd = cmd.alias(alias_static);
            }

            built.push((spec.cli_path.clone(), cmd));
        }

        root = Self::compose(root, built);

        root
    }

    /// Hang the built leaf commands under the verb groups their `cli_path`
    /// names, creating the groups above them.
    ///
    /// This used to be flat: every leaf became a top-level command, so the row
    /// for `["kg", "impact"]` projected to `memnoth impact` — a command the
    /// binary does not accept — and `mn doctor` installs completions from this
    /// projection, so the wrong shape reached real shells. The real parse tree
    /// is `cli::Cli`; this projection only has to agree with it.
    ///
    /// A row whose path is fully consumed by a group (e.g. `["session"]` next to
    /// `["session", "start"]`) becomes the group's own command, keeping its
    /// arguments, and the deeper rows hang beneath it.
    fn compose(mut node: clap::Command, rows: Vec<(Vec<String>, clap::Command)>) -> clap::Command {
        type GroupRow = (Vec<String>, clap::Command);
        type Group = (String, Vec<GroupRow>);
        let mut groups: Vec<Group> = Vec::new();
        for (path, cmd) in rows {
            match path.split_first() {
                None => node = node.subcommand(cmd),
                Some((head, tail)) => {
                    let tail = tail.to_vec();
                    match groups.iter_mut().find(|(name, _)| name == head) {
                        Some((_, bucket)) => bucket.push((tail, cmd)),
                        None => groups.push((head.clone(), vec![(tail, cmd)])),
                    }
                }
            }
        }

        for (head, bucket) in groups {
            let name: &'static str = Box::leak(head.into_boxed_str());
            let mut child = clap::Command::new(name);
            let mut deeper: Vec<GroupRow> = Vec::new();
            for (tail, cmd) in bucket {
                if tail.is_empty() {
                    child = cmd;
                } else {
                    deeper.push((tail, cmd));
                }
            }
            if !deeper.is_empty() {
                child = Self::compose(child, deeper);
            }
            node = node.subcommand(child);
        }

        node
    }

    /// Build palette data (for TUI command palette).
    pub fn build_palette(&self) -> Value {
        let items: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "name": c.name(),
                    "description": c.slash.description,
                    "cli_command": c.cli_command(),
                    "categories": c.slash.categories,
                    "overlay": c.slash.overlay,
                    "danger": c.slash.danger,
                    "auth": c.slash.auth,
                    "aliases": c.slash.aliases,
                    "examples": c.slash.examples,
                    "params": c.slash.params.iter().map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                            "required": p.required,
                            "type": p.param_type,
                            "default": p.default,
                            "short": p.short,
                            "positional": p.positional,
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();

        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commands": items,
        })
    }

    /// Every row, with the metadata a surface needs to render it.
    ///
    /// This is the one projection. The palette, the slash dropdown, `/commands`,
    /// help, and the footer read this rather than re-deriving a description, a
    /// risk chip, or an argument hint from a second table. Hidden rows are
    /// included with the reason they are hidden, so "not in the palette" is a
    /// decision with an explanation instead of a silently missing command.
    pub fn build_command_rows(&self) -> Value {
        let rows: Vec<Value> = self
            .all()
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "name": c.name(),
                    "cli_command": c.cli_command(),
                    "palette": c.palette_line(),
                    "description": c.slash.description,
                    "long_description": c.slash.long_description,
                    "availability": c.availability(),
                    "risk": c.risk(),
                    "screen": c.screen(),
                    "hidden": c.slash.hidden,
                    "hidden_reason": if c.slash.hidden {
                        Some("advanced: run `memnoth help --all` or type the verb")
                    } else {
                        None
                    },
                    "params": c.slash.params.iter().map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "description": p.description,
                            "required": p.required,
                            "type": p.param_type,
                            "default": p.default,
                            "placeholder": match p.default {
                                Some(ref value) => value.to_string(),
                                None => format!("<{}>", p.name),
                            },
                        })
                    }).collect::<Vec<_>>(),
                    "keys": Value::Null,
                    "secret": c.id.contains("token") || c.id.contains("key"),
                    "headless": c.headless(),
                    "failure_hint": c.failure_hint(),
                    "aliases": c.slash.aliases,
                    "related": c.related,
                    "examples": c.slash.examples,
                })
            })
            .collect();

        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "command_count": self.len(),
            "commands": rows,
        })
    }

    /// Build slash command data (for TUI slash commands).
    pub fn build_slash(&self) -> Value {
        let commands: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                serde_json::json!({
                    "name": c.slash.name,
                    "description": c.slash.description,
                    "long_description": c.slash.long_description,
                    "params": c.slash.params,
                    "overlay": c.slash.overlay,
                    "danger": c.slash.danger,
                    "auth": c.slash.auth,
                    "profile": c.slash.profile,
                    "categories": c.slash.categories,
                    "aliases": c.slash.aliases,
                    "examples": c.slash.examples,
                    "deprecated": c.slash.deprecated,
                })
            })
            .collect();

        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "commands": commands,
        })
    }

    /// Build MCP tool definitions.
    pub fn build_mcp(&self, _profile: Option<&str>) -> Value {
        let tools: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                let input_schema = Self::params_to_json_schema(&c.slash.params);
                serde_json::json!({
                    "name": c.id.replace('.', "_"),
                    "description": c.slash.description,
                    "inputSchema": input_schema,
                })
            })
            .collect();

        serde_json::json!({
            "tools": tools,
        })
    }

    /// Convert parameters to JSON Schema.
    fn params_to_json_schema(params: &[ParamSpec]) -> Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for param in params {
            if param.hidden {
                continue;
            }

            let schema = Self::param_type_to_json_schema(&param.param_type);
            let mut prop = serde_json::json!({
                "type": schema["type"],
                "description": param.description,
            });

            if let Some(default) = &param.default {
                prop["default"] = default.clone();
            }

            if let Some(schema_obj) = schema.as_object() {
                for (k, v) in schema_obj {
                    if k != "type" {
                        prop[k] = v.clone();
                    }
                }
            }

            properties.insert(param.name.clone(), prop);

            if param.required {
                required.push(param.name.clone());
            }
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    /// Convert ParamType to JSON Schema.
    fn param_type_to_json_schema(param_type: &ParamType) -> Value {
        match param_type {
            ParamType::String {
                pattern,
                completions,
            } => {
                let mut schema = serde_json::json!({ "type": "string" });
                if let Some(pat) = pattern {
                    schema["pattern"] = Value::String(pat.clone());
                }
                if let Some(comps) = completions {
                    schema["enum"] =
                        Value::Array(comps.iter().map(|c| Value::String(c.clone())).collect());
                }
                schema
            }
            ParamType::Integer { min, max } => {
                let mut schema = serde_json::json!({ "type": "integer" });
                if let Some(min) = min {
                    schema["minimum"] = Value::Number((*min).into());
                }
                if let Some(max) = max {
                    schema["maximum"] = Value::Number((*max).into());
                }
                schema
            }
            ParamType::Float { min, max } => {
                let mut schema = serde_json::json!({ "type": "number" });
                if let Some(min) = min {
                    schema["minimum"] = Value::Number(serde_json::Number::from_f64(*min).unwrap());
                }
                if let Some(max) = max {
                    schema["maximum"] = Value::Number(serde_json::Number::from_f64(*max).unwrap());
                }
                schema
            }
            ParamType::Boolean => serde_json::json!({ "type": "boolean" }),
            ParamType::Enum { choices } => {
                serde_json::json!({
                    "type": "string",
                    "enum": choices,
                })
            }
            ParamType::Array {
                items,
                min_items,
                max_items,
            } => {
                let mut schema = serde_json::json!({
                    "type": "array",
                    "items": Self::param_type_to_json_schema(items),
                });
                if let Some(min) = min_items {
                    schema["minItems"] = Value::Number((*min).into());
                }
                if let Some(max) = max_items {
                    schema["maxItems"] = Value::Number((*max).into());
                }
                schema
            }
            ParamType::Path { .. } => serde_json::json!({ "type": "string", "format": "path" }),
            ParamType::Json => serde_json::json!({ "type": "object" }),
            ParamType::Duration => serde_json::json!({ "type": "string", "format": "duration" }),
            ParamType::Timestamp => serde_json::json!({ "type": "string", "format": "date-time" }),
        }
    }

    /// Build documentation (markdown).
    pub fn build_docs(&self) -> String {
        let mut md = String::new();
        md.push_str("# Memnoth CLI Command Reference\n\n");
        md.push_str(&format!("Version: {}\n\n", env!("CARGO_PKG_VERSION")));
        md.push_str("---\n\n");

        for category in self.categories() {
            let commands = self.by_category(category);
            if commands.is_empty() {
                continue;
            }

            md.push_str(&format!("## {}\n\n", category.to_uppercase()));

            for cmd in commands {
                md.push_str(&format!("### `memnoth {}`\n\n", cmd.cli_command()));
                md.push_str(&format!("{}\n\n", cmd.slash.description));

                if let Some(long) = &cmd.slash.long_description {
                    md.push_str(&format!("{}\n\n", long));
                }

                if !cmd.slash.aliases.is_empty() {
                    md.push_str(&format!(
                        "**Aliases:** `{}`\n\n",
                        cmd.slash.aliases.join("`, `")
                    ));
                }

                if !cmd.slash.examples.is_empty() {
                    md.push_str("**Examples:**\n\n");
                    for ex in &cmd.slash.examples {
                        md.push_str(&format!("```bash\nmemnoth {}\n```\n\n", ex));
                    }
                }

                let required_params: Vec<_> = cmd
                    .slash
                    .params
                    .iter()
                    .filter(|p| p.required && !p.hidden)
                    .collect();

                let optional_params: Vec<_> = cmd
                    .slash
                    .params
                    .iter()
                    .filter(|p| !p.required && !p.hidden)
                    .collect();

                if !required_params.is_empty() {
                    md.push_str("**Required arguments:**\n\n");
                    for param in required_params {
                        md.push_str(&format!(
                            "- `--{}` ({}) — {}\n",
                            param.long.as_ref().unwrap_or(&param.name),
                            Self::param_type_short(&param.param_type),
                            param.description
                        ));
                    }
                    md.push('\n');
                }

                if !optional_params.is_empty() {
                    md.push_str("**Optional arguments:**\n\n");
                    for param in optional_params {
                        let default = param
                            .default
                            .as_ref()
                            .map(|d| format!(" (default: {})", d))
                            .unwrap_or_default();
                        md.push_str(&format!(
                            "- `--{}` ({}) — {}{}\n",
                            param.long.as_ref().unwrap_or(&param.name),
                            Self::param_type_short(&param.param_type),
                            param.description,
                            default
                        ));
                    }
                    md.push('\n');
                }

                md.push_str(&format!(
                    "**Overlay:** `{:?}` | **Danger:** `{:?}` | **Auth:** `{:?}`\n\n",
                    cmd.slash.overlay, cmd.slash.danger, cmd.slash.auth
                ));

                md.push_str("---\n\n");
            }
        }

        md
    }

    fn param_type_short(param_type: &ParamType) -> &'static str {
        match param_type {
            ParamType::String { .. } => "string",
            ParamType::Integer { .. } => "integer",
            ParamType::Float { .. } => "float",
            ParamType::Boolean => "bool",
            ParamType::Enum { .. } => "enum",
            ParamType::Array { .. } => "array",
            ParamType::Path { .. } => "path",
            ParamType::Json => "json",
            ParamType::Duration => "duration",
            ParamType::Timestamp => "timestamp",
        }
    }

    /// Build shell completions.
    pub fn build_completions(&self, shell: clap_complete::Shell) -> String {
        let mut cmd = self.build_clap();
        let mut buf = Vec::new();
        clap_complete::generate(shell, &mut cmd, "memnoth", &mut buf);
        String::from_utf8(buf).unwrap_or_default()
    }

    /// Build JSON schema for all commands.
    pub fn build_json_schema(&self) -> Value {
        let commands: Vec<Value> = self
            .all()
            .filter(|c| !c.slash.hidden)
            .map(|c| {
                serde_json::json!({
                    "id": c.id,
                    "cli_path": c.cli_path,
                    "name": c.name(),
                    "description": c.slash.description,
                    "handler": c.handler,
                    "json_output": c.json_output,
                    "streaming": c.streaming,
                    "timeout_secs": c.timeout_secs,
                    "min_version": c.min_version,
                    "tags": c.tags,
                    "related": c.related,
                    "params": c.slash.params,
                    "overlay": c.slash.overlay,
                    "danger": c.slash.danger,
                    "auth": c.slash.auth,
                    "profile": c.slash.profile,
                    "categories": c.slash.categories,
                    "aliases": c.slash.aliases,
                    "examples": c.slash.examples,
                })
            })
            .collect();

        serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "Memnoth Command Schema",
            "version": env!("CARGO_PKG_VERSION"),
            "type": "object",
            "properties": {
                "commands": {
                    "type": "array",
                    "items": {
                        "type": "object",
                    },
                },
            },
            "commands": commands,
        })
    }
}

/// The twelve verbs D7 makes public. Everything else is hidden from `--help`
/// but still parses, so a script written against an older surface keeps its
/// command and `help --all` still documents it.
pub const PUBLIC_VERBS: &[&str] = &[
    "session",
    "ask",
    "vault",
    "kg",
    "mcp",
    "skills",
    "config",
    "auth",
    "doctor",
    "update",
    "completion",
    "version",
];

/// Builder for CommandRegistry with fluent API.
pub struct RegistryBuilder {
    registry: CommandRegistry,
}

impl RegistryBuilder {
    /// Create a new builder.
    pub fn new() -> Self {
        Self {
            registry: CommandRegistry::new(),
        }
    }

    /// Add a command spec.
    pub fn command(mut self, spec: CommandSpec) -> Result<Self> {
        self.registry.register(spec)?;
        Ok(self)
    }

    /// Add multiple command specs.
    pub fn commands(mut self, specs: Vec<CommandSpec>) -> Result<Self> {
        for spec in specs {
            self.registry.register(spec)?;
        }
        Ok(self)
    }

    /// Apply D7 to the rows that were just registered.
    ///
    /// This is the one place the public surface is decided: the CLI hides the
    /// same set, and `help --all` reads `hidden` from here. `skill` is the old
    /// spelling of `skills`, so it is renamed once here rather than in every
    /// row; the binary still accepts `mn skill` as an alias.
    pub fn apply_public_surface(mut self) -> Self {
        for spec in self.registry.commands.values_mut() {
            if spec.cli_path.first().is_some_and(|verb| verb == "skill") {
                spec.cli_path[0] = "skills".into();
            }
            let public = spec
                .cli_path
                .first()
                .is_some_and(|verb| PUBLIC_VERBS.contains(&verb.as_str()));
            if !public {
                spec.slash.hidden = true;
            }
        }
        self
    }

    /// Build the registry.
    pub fn build(self) -> CommandRegistry {
        self.registry
    }
}

impl Default for RegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of parity check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityReport {
    /// Whether all surfaces match.
    pub ok: bool,
    /// Mismatches found.
    pub mismatches: Vec<ParityMismatch>,
}

/// A single parity mismatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityMismatch {
    /// Command ID.
    pub command_id: String,
    /// Surface that differs.
    pub surface: String,
    /// Expected value.
    pub expected: Value,
    /// Actual value.
    pub actual: Value,
    /// Description of the mismatch.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{AuthLevel, DangerLevel, OverlayType, SlashSpec};

    #[test]
    fn test_registry_basic() {
        let mut registry = CommandRegistry::new();
        assert!(registry.is_empty());

        let spec = CommandSpec {
            id: "test.cmd".into(),
            cli_path: vec!["test".into(), "cmd".into()],
            slash: SlashSpec {
                name: "cmd".into(),
                description: "Test command".into(),
                long_description: None,
                palette: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["test".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![],
            },
            handler: "test_cmd".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec![],
            related: vec![],
        };

        registry.register(spec).unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.get("test.cmd").is_some());
    }

    /// The projection must nest verbs the way `cli::Cli` parses them, because
    /// `mn doctor` installs shell completions from it.
    #[test]
    fn clap_projection_nests_verb_groups() {
        let registry = crate::build_p0_registry().build();
        let mut command = registry.build_clap();
        command.build();

        let mut leaf = command.clone();
        for word in ["kg", "impact"] {
            let next = leaf
                .get_subcommands()
                .find(|sub| sub.get_name() == word)
                .cloned();
            leaf = match next {
                Some(sub) => sub,
                None => panic!("`memnoth {word}` is missing from the clap projection"),
            };
        }
        assert!(
            leaf.get_subcommands().next().is_none(),
            "`memnoth kg impact` should be a leaf"
        );

        // W13-03/W13-13: `/impact` is a primary verb, and `kg impact` stays as
        // the old spelling until 1.0 plus one minor. Both must project, and
        // neither may become a second implementation.
        assert!(
            command
                .get_subcommands()
                .any(|sub| sub.get_name() == "impact"),
            "`impact` is a primary verb missing from the clap projection"
        );
    }
}
