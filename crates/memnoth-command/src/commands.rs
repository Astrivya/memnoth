//! The command specifications — the single source of truth.
//!
//! Each [`CommandSpec`] defines one command row. Handlers are not implemented
//! here; this crate is the spec layer only. Implementation happens in the
//! crates that consume this registry.
//!
//! Categories:
//! - Session: session lifecycle (start, resume, list, status, stop)
//! - Vault: temporal vault (init, status, remember, recall, asof, history, contradictions)
//! - Kg: code graph (index, status, impact, search)
//! - Mcp: MCP server and clients (serve, doctor, add, call, selftest)
//! - Diagnostics: doctor
//! - Cloud: cloud spine (login, logout, status, sync, profile, billing)
//! - Config: configuration (get, set, list)
//! - Completion: shell completions (install, show)

use crate::registry::RegistryBuilder;
use crate::spec::{
    AuthLevel, CommandSpec, DangerLevel, OverlayType, ParamSpec, ParamType, SlashSpec,
};

/// Build the P0 command registry.
pub fn build_p0_registry() -> RegistryBuilder {
    let mut builder = RegistryBuilder::new();

    // ── Session commands ────────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "session.start".into(),
            cli_path: vec!["session".into(), "start".into()],
            slash: SlashSpec {
                name: "start".into(),
                palette: None,
                description: "Open a new session".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt("name", "Session name", "session").short('n')],
                overlay: OverlayType::Fullscreen,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["session".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["session start --name payments".into()],
            },
            handler: "session_start".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["session".into()],
            related: vec!["session.resume".into(), "session.stop".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "session.resume".into(),
            cli_path: vec!["session".into(), "resume".into()],
            slash: SlashSpec {
                name: "resume".into(),
                palette: None,
                description: "Resume a named session".into(),
                long_description: None,
                params: vec![ParamSpec::string("name", "Session name to resume").short('n')],
                overlay: OverlayType::Fullscreen,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["session".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["session resume --name payments".into()],
            },
            handler: "session_resume".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["session".into()],
            related: vec!["session.start".into(), "session.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "session.list".into(),
            cli_path: vec!["session".into(), "list".into()],
            slash: SlashSpec {
                name: "list".into(),
                palette: None,
                description: "List all sessions".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::Picker,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["session".into()],
                aliases: vec!["ls".into()],
                hidden: false,
                deprecated: None,
                examples: vec!["session list".into()],
            },
            handler: "session_list".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["session".into()],
            related: vec!["session.start".into(), "session.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "session.status".into(),
            cli_path: vec!["session".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                palette: None,
                description: "Show current session status".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["session".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["session status".into()],
            },
            handler: "session_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["session".into()],
            related: vec!["session.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "session.stop".into(),
            cli_path: vec!["session".into(), "stop".into()],
            slash: SlashSpec {
                name: "stop".into(),
                palette: None,
                description: "Stop the current session".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string_opt("name", "Session name to stop", "").short('n'),
                    ParamSpec::bool("force", "Force stop without confirmation").short('f'),
                ],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["session".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["session stop --force".into()],
            },
            handler: "session_stop".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["session".into()],
            related: vec!["session.start".into()],
        })
        .unwrap();

    // ── Vault commands ──────────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "vault.init".into(),
            cli_path: vec!["vault".into(), "init".into()],
            slash: SlashSpec {
                name: "init".into(),
                palette: Some("vault init".into()),
                description: "Create the vault (idempotent; safe to run once per machine)".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["vault".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["vault init".into()],
            },
            handler: "vault_init".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["vault".into()],
            related: vec!["vault.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "vault.status".into(),
            cli_path: vec!["vault".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                palette: Some("vault status".into()),
                description: "What the vault holds now: namespaces, counts, path, size".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["vault".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["vault status".into()],
            },
            handler: "vault_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["vault".into()],
            related: vec!["vault.init".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "vault.remember".into(),
            cli_path: vec!["vault".into(), "remember".into()],
            slash: SlashSpec {
                name: "remember".into(),
                palette: Some("vault remember <name> [--summary ...] [--kind ...]".into()),
                description: "Record a fact, decision, lesson, gotcha, or correction".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("name", "Entity the fact is about").positional(0),
                    ParamSpec::enum_opt(
                        "kind",
                        "Observation kind",
                        vec!["fact".into(), "decision".into(), "lesson".into(), "gotcha".into(), "correction".into()],
                        "fact",
                    ).short('k'),
                    ParamSpec::string_opt("confidence", "Confidence, 0.0–1.0", "0.9").short('c'),
                    ParamSpec::string_opt("entity_kind", "Entity kind when created", "concept"),
                    ParamSpec {
                        name: "text".into(),
                        description: "The fact itself".into(),
                        param_type: ParamType::Array {
                            items: Box::new(ParamType::String { pattern: None, completions: None }),
                            min_items: Some(1),
                            max_items: None,
                        },
                        required: true,
                        default: None,
                        short: None,
                        long: None,
                        env: None,
                        positional: true,
                        position: Some(1),
                        hidden: false,
                        deprecated: None,
                    },
                ],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["vault".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["vault remember PaymentRetrier --kind decision --confidence 0.9 -- retries capped at 3".into()],
            },
            handler: "vault_remember".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["vault".into()],
            related: vec!["vault.recall".into(), "vault.asof".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "vault.recall".into(),
            cli_path: vec!["vault".into(), "recall".into()],
            slash: SlashSpec {
                name: "recall".into(),
                palette: Some("vault recall <query>".into()),
                description: "Search the vault (FTS + graph, temporal windows understood)".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("query", "What to look for").positional(0),
                    ParamSpec::integer_opt("limit", "How many entities to return", 10).short('l'),
                    ParamSpec::string_opt("at", "Answer as of this instant instead of now", ""),
                ],
                overlay: OverlayType::Picker,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["vault".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["vault recall retries --limit 20".into()],
            },
            handler: "vault_recall".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["vault".into()],
            related: vec!["vault.remember".into(), "vault.asof".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "vault.asof".into(),
            cli_path: vec!["vault".into(), "asof".into()],
            slash: SlashSpec {
                name: "asof".into(),
                palette: Some("vault asof [--at WHEN]".into()),
                description: "What the vault believed at an instant".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string_opt(
                        "at",
                        "The instant to reconstruct (RFC 3339, date, or epoch ms)",
                        "",
                    ),
                    ParamSpec::string_opt("name", "Restrict to one entity", ""),
                    ParamSpec::integer_opt("limit", "Maximum entities when no --name", 200)
                        .short('l'),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["vault".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["vault asof --at 2026-09-01".into()],
            },
            handler: "vault_asof".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["vault".into()],
            related: vec!["vault.history".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "vault.history".into(),
            cli_path: vec!["vault".into(), "history".into()],
            slash: SlashSpec {
                name: "history".into(),
                palette: Some("vault history <name>".into()),
                description: "Every version of one entity, with the reasons they changed".into(),
                long_description: None,
                params: vec![ParamSpec::string("name", "Entity name").positional(0)],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["vault".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["vault history auth".into()],
            },
            handler: "vault_history".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["vault".into()],
            related: vec!["vault.asof".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "vault.contradictions".into(),
            cli_path: vec!["vault".into(), "contradictions".into()],
            slash: SlashSpec {
                name: "contradictions".into(),
                palette: Some("vault contradictions [--name NAME]".into()),
                description: "Beliefs explicitly contradicted, both sources kept".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt("name", "Restrict to one entity", "")],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["vault".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "vault contradictions".into(),
                    "vault contradictions --name auth".into(),
                ],
            },
            handler: "vault_contradictions".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["vault".into()],
            related: vec![],
        })
        .unwrap();

    // ── Kg commands ─────────────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "kg.index".into(),
            cli_path: vec!["kg".into(), "index".into()],
            slash: SlashSpec {
                name: "index".into(),
                palette: Some("kg index".into()),
                description: "Build or update the code graph from the workspace".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool(
                        "full",
                        "Discard the previous graph and rebuild from scratch",
                    )
                    .short('f'),
                    ParamSpec::bool("no_embed", "Structure only: skip embeddings"),
                ],
                overlay: OverlayType::Wizard,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["kg".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["kg index --full".into(), "kg index --no-embed".into()],
            },
            handler: "kg_index".into(),
            json_output: true,
            streaming: false,
            timeout_secs: Some(3600),
            min_version: None,
            tags: vec!["kg".into(), "index".into()],
            related: vec!["kg.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "kg.status".into(),
            cli_path: vec!["kg".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                palette: Some("kg status".into()),
                description: "What was built, when, and how stale it is".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["kg".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["kg status".into()],
            },
            handler: "kg_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["kg".into()],
            related: vec!["kg.index".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "kg.impact".into(),
            cli_path: vec!["kg".into(), "impact".into()],
            slash: SlashSpec {
                name: "impact".into(),
                palette: Some("kg impact <symbol>".into()),
                description:
                    "What depends on a symbol: definitions, references, tests, blast radius".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("symbol", "Symbol name (e.g., PaymentRetrier, open_vault)")
                        .positional(0),
                    ParamSpec::integer_opt("depth", "How many hops of references to follow", 2)
                        .short('d'),
                ],
                overlay: OverlayType::Picker,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["kg".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["kg impact PaymentRetrier --depth 3".into()],
            },
            handler: "kg_impact".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["kg".into()],
            related: vec!["kg.search".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "kg.search".into(),
            cli_path: vec!["kg".into(), "search".into()],
            slash: SlashSpec {
                name: "search".into(),
                palette: Some("kg search <query>".into()),
                description: "Find symbols by name".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("query", "Name fragment").positional(0),
                    ParamSpec::integer_opt("limit", "Maximum symbols", 20).short('l'),
                ],
                overlay: OverlayType::Picker,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["kg".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["kg search open_vault --limit 50".into()],
            },
            handler: "kg_search".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["kg".into()],
            related: vec!["kg.impact".into()],
        })
        .unwrap();

    // ── Mcp commands ────────────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "mcp.serve".into(),
            cli_path: vec!["mcp".into(), "serve".into()],
            slash: SlashSpec {
                name: "serve".into(),
                palette: Some("mcp serve".into()),
                description: "Serve the graph over MCP (stdio by default, HTTP with --http)".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool("http", "Serve over loopback HTTP instead of stdio"),
                    ParamSpec::string_opt(
                        "db",
                        "Vault to serve. Defaults to this CLI's resolved vault",
                        "",
                    ),
                    ParamSpec::enum_opt(
                        "profile",
                        "Tool profile: micro, memory, coord, code, full, agent",
                        vec![
                            "micro".into(),
                            "memory".into(),
                            "coord".into(),
                            "code".into(),
                            "full".into(),
                            "agent".into(),
                        ],
                        "full",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["mcp".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["mcp serve --http --profile code".into()],
            },
            handler: "mcp_serve".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["mcp".into()],
            related: vec!["mcp.doctor".into(), "mcp.selftest".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "mcp.doctor".into(),
            cli_path: vec!["mcp".into(), "doctor".into()],
            slash: SlashSpec {
                name: "doctor".into(),
                palette: Some("mcp doctor".into()),
                description: "What is registered, and where — profile, tool counts, client configs"
                    .into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["mcp".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["mcp doctor".into()],
            },
            handler: "mcp_doctor".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["mcp".into()],
            related: vec!["mcp.serve".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "mcp.add".into(),
            cli_path: vec!["mcp".into(), "add".into()],
            slash: SlashSpec {
                name: "add".into(),
                palette: Some("mcp add".into()),
                description: "Write a client's MCP config for this server".into(),
                long_description: None,
                params: vec![
                    ParamSpec::array(
                        "clients",
                        "Clients to write: claude, cursor, codex, zed. Comma-separated.",
                        ParamType::Enum { choices: vec!["claude".into(), "cursor".into(), "codex".into(), "zed".into()] },
                    ).short('c'),
                    ParamSpec::enum_opt("profile", "Tool profile to register. Defaults to memory for --local, full otherwise.", vec!["micro".into(), "memory".into(), "coord".into(), "code".into(), "full".into(), "agent".into()], ""),
                    ParamSpec::string_opt("url", "Server endpoint. Defaults to the configured URL", ""),
                    ParamSpec::string_opt("token", "Bearer token for the client's header. Defaults to $MEMNOTH_MCP_TOKEN. Never written to memnoth's own config.", ""),
                    ParamSpec::bool("local", "Register a local stdio server against this vault instead of the cloud endpoint"),
                    ParamSpec::bool("allow_writes", "Allow a local server to write beliefs; read-only otherwise"),
                    ParamSpec::bool("dry_run", "Show what would be written without touching a file"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["mcp".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "mcp add --local --clients claude,cursor,codex".into(),
                    "mcp add --clients claude,cursor,codex --profile code".into(),
                ],
            },
            handler: "mcp_add".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["mcp".into()],
            related: vec!["mcp.doctor".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "mcp.call".into(),
            cli_path: vec!["mcp".into(), "call".into()],
            slash: SlashSpec {
                name: "call".into(),
                palette: None,
                description: "Call one MCP tool without a transport in front of it".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("tool", "Tool name (e.g., kg_status, vault_asof)")
                        .positional(0),
                    ParamSpec::json_opt("args", "Arguments as a JSON object. Defaults to {{}}"),
                    ParamSpec::string_opt("profile", "Profile that gates the call", ""),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Session,
                profile: None,
                categories: vec!["mcp".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["mcp call kg_status --args '{}'".into()],
            },
            handler: "mcp_call".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["mcp".into()],
            related: vec!["mcp.serve".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "mcp.selftest".into(),
            cli_path: vec!["mcp".into(), "selftest".into()],
            slash: SlashSpec {
                name: "selftest".into(),
                palette: None,
                description: "Run the server's own initialize/tools/list/resources/list probe"
                    .into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["mcp".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["mcp selftest".into()],
            },
            handler: "mcp_selftest".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["mcp".into()],
            related: vec!["mcp.doctor".into()],
        })
        .unwrap();

    // ── Diagnostics commands ────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "doctor".into(),
            cli_path: vec!["doctor".into()],
            slash: SlashSpec {
                name: "doctor".into(),
                palette: Some("doctor".into()),
                description: "Report what is wired: paths, versions, profiles, clients".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["diagnostics".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["doctor".into()],
            },
            handler: "doctor".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["diagnostics".into()],
            related: vec![],
        })
        .unwrap();
    builder = builder
        .command(CommandSpec {
            id: "drift".into(),
            cli_path: vec!["drift".into()],
            slash: SlashSpec {
                name: "drift".into(),
                palette: Some("drift [--since WHEN]".into()),
                description: "Beliefs contradicted by current code/config/tests".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt(
                    "since",
                    "Only show contradictions since WHEN",
                    "",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["diagnostics".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec!["drift".into(), "drift --since 2026-01-01".into()],
            },
            handler: "drift".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["drift".into(), "diagnostics".into()],
            related: vec!["vault.contradictions".into(), "kg.search".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "simulate".into(),
            cli_path: vec!["simulate".into()],
            slash: SlashSpec {
                name: "simulate".into(),
                palette: Some("simulate <change>".into()),
                description: "Dry-run plan with zero writes".into(),
                long_description: None,
                params: vec![ParamSpec::string(
                    "change",
                    "Change to simulate: file path, symbol, or description",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["diagnostics".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec![
                    "simulate rename foo to bar".into(),
                    "simulate modify src/main.rs".into(),
                ],
            },
            handler: "simulate".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["simulate".into(), "diagnostics".into()],
            related: vec!["kg.impact".into(), "sandbox.check".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "lineage".into(),
            cli_path: vec!["lineage".into()],
            slash: SlashSpec {
                name: "lineage".into(),
                palette: Some("lineage <entity> [--depth N]".into()),
                description: "Trace source to belief to decision to code, every edge dated".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("entity", "Entity whose provenance to trace"),
                    ParamSpec::string_opt("depth", "How many relation hops to follow", "3"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["memory".into()],
                aliases: vec!["vault.explain".into()],
                hidden: true,
                deprecated: None,
                examples: vec!["lineage rate_limit".into()],
            },
            handler: "lineage".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["lineage".into(), "memory".into()],
            related: vec!["vault.history".into(), "report.receipt".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "why".into(),
            cli_path: vec!["why".into()],
            slash: SlashSpec {
                name: "why".into(),
                palette: Some("why <symbol> [--at WHEN]".into()),
                description: "Why this code is like this: belief, code, and supersession chain"
                    .into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("symbol", "Symbol to explain"),
                    ParamSpec::string_opt("at", "Read the vault as it stood at WHEN", ""),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["memory".into(), "code".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec![
                    "why rate_limit".into(),
                    "why rate_limit --at 2026-09-01".into(),
                ],
            },
            handler: "why".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["why".into(), "memory".into(), "code".into()],
            related: vec![
                "lineage".into(),
                "vault.asof".into(),
                "vault.history".into(),
            ],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "impact".into(),
            cli_path: vec!["impact".into()],
            slash: SlashSpec {
                name: "impact".into(),
                palette: Some("impact <symbol> [--depth N] [--diff]".into()),
                description: "What breaks if this changes: exact and likely callers, tests".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("symbol", "Symbol or file whose blast radius to measure"),
                    ParamSpec::string_opt("depth", "How many reference hops to follow", "2"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["code".into()],
                aliases: vec!["kg.impact".into()],
                hidden: true,
                deprecated: None,
                examples: vec!["impact rate_limit".into(), "impact --diff".into()],
            },
            handler: "impact".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["impact".into(), "code".into()],
            related: vec!["kg.impact".into(), "kg.tests-for".into()],
        })
        .unwrap();

    // ── Cloud commands ──────────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "cloud.login".into(),
            cli_path: vec!["cloud".into(), "login".into()],
            slash: SlashSpec {
                name: "login".into(),
                palette: Some("login".into()),
                description: "Authenticate with memnoth cloud".into(),
                long_description: None,
                params: vec![ParamSpec::enum_opt(
                    "provider",
                    "Auth provider: github, zitadel, oidc",
                    vec!["github".into(), "zitadel".into(), "oidc".into()],
                    "github",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["cloud".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["cloud login --provider github".into()],
            },
            handler: "cloud_login".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["cloud".into(), "auth".into()],
            related: vec!["cloud.logout".into(), "cloud.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "cloud.logout".into(),
            cli_path: vec!["cloud".into(), "logout".into()],
            slash: SlashSpec {
                name: "logout".into(),
                palette: Some("logout".into()),
                description: "Revoke cloud credentials and sign out".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["cloud".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["cloud logout".into()],
            },
            handler: "cloud_logout".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["cloud".into(), "auth".into()],
            related: vec!["cloud.login".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "cloud.status".into(),
            cli_path: vec!["cloud".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                palette: None,
                description: "Show cloud authentication and sync status".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["cloud".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["cloud status".into()],
            },
            handler: "cloud_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["cloud".into()],
            related: vec!["cloud.sync".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "cloud.sync".into(),
            cli_path: vec!["cloud".into(), "sync".into()],
            slash: SlashSpec {
                name: "sync".into(),
                palette: Some("sync".into()),
                description: "Sync local vault and graph to cloud".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool("full", "Full sync including history"),
                    ParamSpec::enum_opt(
                        "direction",
                        "Sync direction: up, down, both",
                        vec!["up".into(), "down".into(), "both".into()],
                        "both",
                    ),
                ],
                overlay: OverlayType::Wizard,
                danger: DangerLevel::CloudWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["cloud".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["cloud sync --direction up".into()],
            },
            handler: "cloud_sync".into(),
            json_output: true,
            streaming: false,
            timeout_secs: Some(300),
            min_version: None,
            tags: vec!["cloud".into()],
            related: vec!["cloud.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "cloud.profile".into(),
            cli_path: vec!["cloud".into(), "profile".into()],
            slash: SlashSpec {
                name: "profile".into(),
                palette: None,
                description: "Manage cloud profile settings".into(),
                long_description: None,
                params: vec![ParamSpec::string("name", "Profile name").positional(0)],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["cloud".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["cloud profile default".into()],
            },
            handler: "cloud_profile".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["cloud".into()],
            related: vec!["cloud.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "cloud.billing".into(),
            cli_path: vec!["cloud".into(), "billing".into()],
            slash: SlashSpec {
                name: "billing".into(),
                // The card lives in the console, not in the session: `/credits`
                // was a CLI-only alias for a cloud balance and is retired with
                // the rest of the local credit surface (W2-11). `mn cloud
                // billing` still answers.
                palette: None,
                description: "Manage cloud billing and payment methods".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::CloudWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["cloud".into(), "billing".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["cloud billing".into()],
            },
            handler: "cloud_billing".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["cloud".into(), "billing".into()],
            related: vec![],
        })
        .unwrap();

    // ── Encrypted vault sync (beta) ─────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "sync.status".into(),
            cli_path: vec!["sync".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                palette: None,
                description: "Show the encrypted-sync cursor".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["sync".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec!["sync status".into()],
            },
            handler: "sync_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["sync".into()],
            related: vec!["sync.now".into(), "sync.pause".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "sync.now".into(),
            cli_path: vec!["sync".into(), "now".into()],
            slash: SlashSpec {
                name: "now".into(),
                palette: None,
                description: "Pull the newest manifest, then push this vault".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt(
                    "phrase",
                    "Recovery phrase; falls back to MEMNOTH_SYNC_PHRASE",
                    "",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::CloudWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["sync".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec!["sync now --phrase '…'".into()],
            },
            handler: "sync_now".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["sync".into()],
            related: vec!["sync.status".into(), "sync.restore".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "sync.pause".into(),
            cli_path: vec!["sync".into(), "pause".into()],
            slash: SlashSpec {
                name: "pause".into(),
                palette: None,
                description: "Stop automatic sync; explicit now still works".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["sync".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec!["sync pause".into()],
            },
            handler: "sync_pause".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["sync".into()],
            related: vec!["sync.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "sync.restore".into(),
            cli_path: vec!["sync".into(), "restore".into()],
            slash: SlashSpec {
                name: "restore".into(),
                palette: None,
                description: "Restore this vault from the newest manifest".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string_opt(
                        "phrase",
                        "Recovery phrase; falls back to MEMNOTH_SYNC_PHRASE",
                        "",
                    ),
                    ParamSpec::bool("force", "Replace a vault that exists locally"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::CloudWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["sync".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec!["sync restore --phrase '…' --force".into()],
            },
            handler: "sync_restore".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["sync".into()],
            related: vec!["sync.status".into(), "sync.now".into()],
        })
        .unwrap();

    // ── Config commands ─────────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "config.get".into(),
            cli_path: vec!["config".into(), "get".into()],
            slash: SlashSpec {
                name: "get".into(),
                palette: None,
                description: "Get a configuration value".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("key", "Configuration key (dot-notation)").positional(0),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["config get provider.model".into()],
            },
            handler: "config_get".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["config".into()],
            related: vec!["config.set".into(), "config.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "config.set".into(),
            cli_path: vec!["config".into(), "set".into()],
            slash: SlashSpec {
                name: "set".into(),
                palette: None,
                description: "Set a configuration value".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("key", "Configuration key (dot-notation)").positional(0),
                    ParamSpec::string("value", "Configuration value").positional(1),
                ],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["config set provider.model claude-sonnet-4-5".into()],
            },
            handler: "config_set".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["config".into()],
            related: vec!["config.get".into(), "config.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "config.list".into(),
            cli_path: vec!["config".into(), "list".into()],
            slash: SlashSpec {
                name: "list".into(),
                palette: None,
                description: "List all configuration values".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["config list".into()],
            },
            handler: "config_list".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["config".into()],
            related: vec!["config.get".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "config.explain".into(),
            cli_path: vec!["config".into(), "explain".into()],
            slash: SlashSpec {
                name: "explain".into(),
                palette: None,
                description: "Explain where a configuration value comes from".into(),
                long_description: Some(
                    "Names the layer that supplies a key's effective value (config file, environment, or default), whether another layer overrides the file, and the command that would change it.".into(),
                ),
                params: vec![
                    ParamSpec::string("key", "Configuration key (dot-notation)").positional(0),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["config explain provider.model".into()],
            },
            handler: "config_explain".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["config".into()],
            related: vec![
                "config.get".into(),
                "config.set".into(),
                "config.list".into(),
            ],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "ask".into(),
            cli_path: vec!["ask".into()],
            slash: SlashSpec {
                name: "ask".into(),
                palette: None,
                description: "Ask one question and print the answer".into(),
                long_description: Some(
                    "One provider turn without the screen: the prompt comes from the argument, `-p`, or stdin; output is text, one JSON envelope, or one event per line.".into(),
                ),
                params: vec![
                    ParamSpec::string_opt("prompt", "Prompt; stdin when omitted", "").short('p'),
                    ParamSpec::enum_opt(
                        "output-format",
                        "Output shape: text, json, jsonl",
                        vec!["text".into(), "json".into(), "jsonl".into()],
                        "text",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["session".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "ask \"what changed in this repo?\"".into(),
                    "printf 'hello\\n' | ask --output-format json".into(),
                ],
            },
            handler: "ask".into(),
            json_output: true,
            streaming: true,
            timeout_secs: None,
            min_version: None,
            tags: vec!["session".into()],
            related: vec!["session.start".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "version".into(),
            cli_path: vec!["version".into()],
            slash: SlashSpec {
                name: "version".into(),
                palette: None,
                description: "Print the version, build target and commit".into(),
                long_description: Some(
                    "The same version `--version` prints, with the build target and commit in `--json` so a bug report can name the exact artifact.".into(),
                ),
                params: vec![ParamSpec::bool("json", "Output as JSON")],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["version".into(), "version --json".into()],
            },
            handler: "version".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["config".into()],
            related: vec!["doctor".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "bench".into(),
            cli_path: vec!["bench".into()],
            slash: SlashSpec {
                name: "bench".into(),
                palette: None,
                description: "Run proof benchmarks (MTS-60, TRS, KG)".into(),
                long_description: Some(
                    "Run a pinned model against a committed task suite N times and report a 95% bootstrap confidence interval. Outputs a scoreboard JSON and an ablation matrix.".into(),
                ),
                params: vec![
                    ParamSpec::string_opt("suite", "Benchmark suite: mts, trs, kg", "mts"),
                    ParamSpec::bool("json", "Output as JSON"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["bench run --suite mts --json".into()],
            },
            handler: "bench".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["proof".into()],
            related: vec!["version".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "help".into(),
            cli_path: vec!["help".into()],
            slash: SlashSpec {
                name: "help".into(),
                palette: None,
                description: "Print help, including hidden verbs with --all".into(),
                long_description: Some(
                    "`help` renders the visible surface, `help <verb>` renders one verb, and `help --all` lists every verb the build accepts, marking the hidden ones.".into(),
                ),
                params: vec![
                    ParamSpec::string_opt("verb", "Verb to describe", ""),
                    ParamSpec::bool("all", "Include hidden verbs"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "help".into(),
                    "help vault".into(),
                    "help --all".into(),
                ],
            },
            handler: "help".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["config".into()],
            related: vec!["version".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "schema".into(),
            cli_path: vec!["schema".into()],
            slash: SlashSpec {
                name: "schema".into(),
                palette: None,
                description: "Print the command registry as JSON".into(),
                long_description: Some(
                    "The machine-readable form of every command specification, for generated clients and drift checks. `--check --output <file>` fails when the committed file disagrees with the registry.".into(),
                ),
                params: vec![
                    ParamSpec::string_opt("output", "Write to this file instead of stdout", ""),
                    ParamSpec::bool("check", "Compare --output against the registry"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["config".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec![
                    "schema".into(),
                    "schema --check --output docs/cli/schema.json".into(),
                ],
            },
            handler: "schema".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["config".into()],
            related: vec!["docs".into(), "help".into()],
        })
        .unwrap();

    // ── Completion commands ─────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "completion.install".into(),
            cli_path: vec!["completion".into(), "install".into()],
            slash: SlashSpec {
                name: "install".into(),
                palette: None,
                description: "Install shell completions for memnoth".into(),
                long_description: None,
                params: vec![
                    ParamSpec::enum_opt(
                        "shell",
                        "Shell: bash, zsh, fish, powershell",
                        vec![
                            "bash".into(),
                            "zsh".into(),
                            "fish".into(),
                            "powershell".into(),
                        ],
                        "bash",
                    )
                    .short('s'),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["completion".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["completion install --shell zsh".into()],
            },
            handler: "completion_install".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["completion".into()],
            related: vec!["completion.show".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "completion.show".into(),
            cli_path: vec!["completion".into(), "show".into()],
            slash: SlashSpec {
                name: "show".into(),
                palette: None,
                description: "Print shell completions for memnoth".into(),
                long_description: None,
                params: vec![
                    ParamSpec::enum_opt(
                        "shell",
                        "Shell: bash, zsh, fish, powershell",
                        vec![
                            "bash".into(),
                            "zsh".into(),
                            "fish".into(),
                            "powershell".into(),
                        ],
                        "bash",
                    )
                    .short('s'),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["completion".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["completion show --shell bash".into()],
            },
            handler: "completion_show".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["completion".into()],
            related: vec!["completion.install".into()],
        })
        .unwrap();
    builder = builder
        .command(CommandSpec {
            id: "docs".into(),
            cli_path: vec!["docs".into()],
            slash: SlashSpec {
                name: "docs".into(),
                palette: Some("docs".into()),
                description: "Generate Markdown documentation from command registry".into(),
                long_description: Some("Outputs all command specifications as Markdown. Use --output to write to a file, otherwise prints to stdout. `--check` compares the file against the registry and exits non-zero when they disagree.".into()),
                params: vec![
                    ParamSpec::string_opt("output", "Output file path (default: stdout)", "").short('o'),
                    ParamSpec::bool(
                        "check",
                        "Compare --output against the registry; exit non-zero when stale",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["docs".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec![
                    "docs --output docs/cli/command-surface.md".into(),
                    "docs --check --output docs/cli/command-surface.md".into(),
                    "docs".into(),
                ],
            },
            handler: "docs".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["docs".into()],
            related: vec![],
        })
        .unwrap();

    // ── Product verbs with no subcommand ───────────────────────────
    //
    // Transcribed from `src/cli.rs`: one spec per leaf verb, with the flags
    // clap actually accepts. A verb missing here is a verb `mn docs` and the
    // completions cannot describe, which is why the gaps were visible as
    // silence rather than as errors.

    // `usage` and `stats` are retired (W2-11): the credit ledger is a cloud
    // surface, not a CLI verb. `mn cloud usage` and `mn cloud billing` still
    // answer, and `POST /api/v1/credits/purchase` is untouched; what is gone is
    // the local verb that made a second, credit-shaped command line out of it.

    builder = builder
        .command(CommandSpec {
            id: "update".into(),
            cli_path: vec!["update".into()],
            slash: SlashSpec {
                name: "update".into(),
                palette: None,
                description: "Check for and apply updates".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool("check", "Only check for updates, don't apply"),
                    ParamSpec::string_opt("channel", "Release channel to check/use", ""),
                    ParamSpec::string_opt(
                        "version",
                        "Specific version to install (e.g. 0.2.0)",
                        "",
                    ),
                    ParamSpec::bool(
                        "no-verify",
                        "Install without verifying the release signature",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["update --check".into()],
            },
            handler: "update".into(),
            json_output: false,
            streaming: false,
            timeout_secs: Some(600),
            min_version: None,
            tags: vec!["product".into()],
            related: vec![],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "install".into(),
            cli_path: vec!["install".into()],
            slash: SlashSpec {
                name: "install".into(),
                palette: None,
                description: "Install memnoth (shows installer instructions)".into(),
                long_description: None,
                params: vec![ParamSpec::bool(
                    "force",
                    "Force reinstall even if already installed",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["install".into()],
            },
            handler: "install".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into()],
            related: vec!["uninstall".into(), "update".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "uninstall".into(),
            cli_path: vec!["uninstall".into()],
            slash: SlashSpec {
                name: "uninstall".into(),
                palette: None,
                description: "Uninstall memnoth".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool("keep_state", "Keep configuration and state files"),
                    ParamSpec::bool("yes", "Answer yes without asking"),
                    ParamSpec::bool(
                        "dry_run",
                        "Show what would be uninstalled without uninstalling",
                    ),
                ],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::Destructive,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["uninstall --keep-state".into()],
            },
            handler: "uninstall".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into()],
            related: vec!["install".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "setup".into(),
            cli_path: vec!["setup".into()],
            slash: SlashSpec {
                name: "setup".into(),
                palette: None,
                description: "First-run setup: vault, code graph, auth, MCP, provider, config"
                    .into(),
                long_description: None,
                params: vec![
                    ParamSpec::string_opt(
                        "skip",
                        "Skip steps (comma-separated: vault,graph,auth,mcp,provider,config)",
                        "",
                    ),
                    ParamSpec::bool("yes", "Accept defaults, don't prompt"),
                ],
                overlay: OverlayType::Wizard,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["setup --yes".into()],
            },
            handler: "setup".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into()],
            related: vec!["doctor".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "open".into(),
            cli_path: vec!["open".into()],
            slash: SlashSpec {
                name: "open".into(),
                palette: None,
                description: "Open a deep-link to the memnoth cloud console".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string_opt(
                        "page",
                        "Console page: dashboard, projects, sessions, settings, usage, org, teams, api-keys, workspaces",
                        "dashboard",
                    )
                    .positional(0),
                    ParamSpec::string_opt("project-id", "Project ID (for projects page)", ""),
                    ParamSpec::string_opt("session-id", "Session ID (for sessions page)", ""),
                    ParamSpec::bool("no-browser", "Don't open the browser automatically"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["open usage".into()],
            },
            handler: "open".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into()],
            related: vec![],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "serve".into(),
            cli_path: vec!["serve".into()],
            slash: SlashSpec {
                name: "serve".into(),
                palette: None,
                description: "Serve the graph and vault over a local HTTP API".into(),
                long_description: None,
                params: vec![
                    ParamSpec::integer_opt("port", "Port to listen on", 3030).short('p'),
                    ParamSpec::string_opt("host", "Host to bind to", "127.0.0.1"),
                    ParamSpec::bool("cors", "Enable CORS for all origins"),
                    ParamSpec::string_opt("token", "Bearer token every request must present", ""),
                    ParamSpec::string_opt("graph-path", "Path to serve the kg index from", ""),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["serve --port 3030".into()],
            },
            handler: "serve".into(),
            json_output: true,
            streaming: true,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into()],
            related: vec!["watch".into(), "mcp.serve".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "batch".into(),
            cli_path: vec!["batch".into()],
            slash: SlashSpec {
                name: "batch".into(),
                palette: None,
                description: "Fan a task out across isolated worktrees".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("task", "Task description").positional(0),
                    ParamSpec::integer_opt("workers", "Number of workers to spawn", 4).short('w'),
                    ParamSpec::string_opt(
                        "command",
                        "Command to run in each worker (e.g. cargo test)",
                        "",
                    ),
                    ParamSpec::string_opt("claim-glob", "Path glob for claims", ""),
                    ParamSpec::integer_opt("claim-ttl", "Claim TTL in seconds", 1800),
                    ParamSpec::string_opt(
                        "workdir",
                        "Working directory relative to worktree root",
                        "",
                    ),
                    ParamSpec::enum_opt(
                        "format",
                        "Output format",
                        vec!["text".into(), "json".into()],
                        "text",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["batch \"split the migration\" --workers 4".into()],
            },
            handler: "batch".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into(), "worktree".into()],
            related: vec!["parallel".into(), "worktree".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "parallel".into(),
            cli_path: vec!["parallel".into()],
            slash: SlashSpec {
                name: "parallel".into(),
                palette: None,
                description: "Run one command in parallel across worktrees".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("command", "Command to execute").positional(0),
                    ParamSpec::string_opt(
                        "targets",
                        "Target worktree names or branches (empty = all non-main worktrees)",
                        "",
                    ),
                    ParamSpec::string_opt("args", "Arguments to pass to the command", ""),
                    ParamSpec::integer_opt("max-concurrent", "Maximum concurrent executions", 4)
                        .short('j'),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["parallel cargo test".into()],
            },
            handler: "parallel".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into(), "worktree".into()],
            related: vec!["batch".into(), "worktree".into()],
        })
        .unwrap();
    // ── Headless agents ─────────────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "agents.list".into(),
            cli_path: vec!["agents".into(), "list".into()],
            slash: SlashSpec {
                name: "agents list".into(),
                palette: None,
                description: "List headless agents in the coordination room".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["agents".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["agents list".into()],
            },
            handler: "agents_list".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["agents".into()],
            related: vec!["agents.run".into(), "agents.result".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "agents.run".into(),
            cli_path: vec!["agents".into(), "run".into()],
            slash: SlashSpec {
                name: "agents run".into(),
                palette: None,
                description: "Run a headless agent task".into(),
                long_description: None,
                params: vec![ParamSpec::string("task", "Task description").positional(0)],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["agents".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["agents run index the workspace".into()],
            },
            handler: "agents_run".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["agents".into()],
            related: vec!["agents.result".into(), "agents.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "agents.result".into(),
            cli_path: vec!["agents".into(), "result".into()],
            slash: SlashSpec {
                name: "agents result".into(),
                palette: None,
                description: "Show the result of an agent run".into(),
                long_description: None,
                params: vec![ParamSpec::string("id", "Worker id").positional(0)],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["agents".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["agents result w-abc123".into()],
            },
            handler: "agents_result".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["agents".into()],
            related: vec!["agents.run".into(), "agents.unpack".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "agents.unpack".into(),
            cli_path: vec!["agents".into(), "unpack".into()],
            slash: SlashSpec {
                name: "agents unpack".into(),
                palette: None,
                description: "Unpack an agent's result artifacts".into(),
                long_description: None,
                params: vec![ParamSpec::string("id", "Worker id").positional(0)],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["agents".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["agents unpack w-abc123".into()],
            },
            handler: "agents_unpack".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["agents".into()],
            related: vec!["agents.result".into(), "agents.list".into()],
        })
        .unwrap();

    // ── Auth, privacy, audit, receipt, UI prefs ───────────────────

    builder = builder
        .command(CommandSpec {
            id: "auth.login".into(),
            cli_path: vec!["auth".into(), "login".into()],
            slash: SlashSpec {
                name: "login".into(),
                palette: None,
                description: "Start device authorization flow (RFC 8628)".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool(
                        "no-browser",
                        "Never offer to open a browser: print the link and wait for approval",
                    ),
                    ParamSpec::string_opt(
                        "with-token",
                        "Use a pre-existing API token (CI/paste path), skipping device flow",
                        "",
                    ),
                    ParamSpec::string_opt(
                        "client-name",
                        "Client name to register (default: hostname-based)",
                        "",
                    ),
                    ParamSpec::string_opt("tier", "Requested tier (optional)", ""),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::CloudWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["auth".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["auth login".into(), "auth login --no-browser".into()],
            },
            handler: "auth_login".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["auth".into()],
            related: vec!["auth.logout".into(), "auth.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "auth.logout".into(),
            cli_path: vec!["auth".into(), "logout".into()],
            slash: SlashSpec {
                name: "logout".into(),
                palette: None,
                description: "Revoke current token and clear local credentials".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::CloudWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["auth".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["auth logout".into()],
            },
            handler: "auth_logout".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["auth".into()],
            related: vec!["auth.login".into(), "auth.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "auth.status".into(),
            cli_path: vec!["auth".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                // No palette entry: the TUI cannot run `/auth status`, and the
                // palette test refuses an entry the session cannot resolve.
                palette: None,
                description: "Show authentication status: who, org, scopes, expiry".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["auth".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["auth status".into()],
            },
            handler: "auth_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["auth".into()],
            related: vec!["auth.whoami".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "auth.whoami".into(),
            cli_path: vec!["auth".into(), "whoami".into()],
            slash: SlashSpec {
                name: "whoami".into(),
                palette: None,
                description: "Print identity one-liner".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["auth".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["auth whoami".into()],
            },
            handler: "auth_whoami".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["auth".into()],
            related: vec!["auth.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "telemetry.status".into(),
            cli_path: vec!["telemetry".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                palette: None,
                description: "Show telemetry status (what's collected, current state)".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["telemetry status".into()],
            },
            handler: "telemetry_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["telemetry.on".into(), "telemetry.off".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "telemetry.on".into(),
            cli_path: vec!["telemetry".into(), "on".into()],
            slash: SlashSpec {
                name: "on".into(),
                palette: None,
                description: "Enable telemetry collection".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["telemetry on".into()],
            },
            handler: "telemetry_on".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["telemetry.off".into(), "telemetry.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "telemetry.off".into(),
            cli_path: vec!["telemetry".into(), "off".into()],
            slash: SlashSpec {
                name: "off".into(),
                palette: None,
                description: "Disable telemetry collection".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["telemetry off".into()],
            },
            handler: "telemetry_off".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["telemetry.on".into(), "telemetry.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "privacy.status".into(),
            cli_path: vec!["privacy".into(), "status".into()],
            slash: SlashSpec {
                name: "status".into(),
                palette: None,
                description: "Show privacy status and what data leaves the machine".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["privacy status".into()],
            },
            handler: "privacy_status".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["privacy.mode".into(), "privacy.show-data".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "privacy.on".into(),
            cli_path: vec!["privacy".into(), "on".into()],
            slash: SlashSpec {
                name: "on".into(),
                palette: None,
                description: "Enable telemetry collection (default: off)".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["privacy on".into()],
            },
            handler: "privacy_on".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["privacy.off".into(), "telemetry.on".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "privacy.off".into(),
            cli_path: vec!["privacy".into(), "off".into()],
            slash: SlashSpec {
                name: "off".into(),
                palette: None,
                description: "Disable telemetry collection (no telemetry data leaves machine)"
                    .into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["privacy off".into()],
            },
            handler: "privacy_off".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["privacy.on".into(), "telemetry.off".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "privacy.mode".into(),
            cli_path: vec!["privacy".into(), "mode".into()],
            slash: SlashSpec {
                name: "mode".into(),
                palette: None,
                description: "Set privacy mode: metadata-only, balanced, open".into(),
                long_description: None,
                params: vec![
                    ParamSpec::enum_param(
                        "mode",
                        "Privacy mode: metadata-only, balanced, open",
                        vec!["metadata-only".into(), "balanced".into(), "open".into()],
                    )
                    .positional(0),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["privacy mode metadata-only".into()],
            },
            handler: "privacy_mode".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["privacy.status".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "privacy.show-data".into(),
            cli_path: vec!["privacy".into(), "show-data".into()],
            slash: SlashSpec {
                name: "show-data".into(),
                palette: None,
                description: "Show what data would be sent in a sync".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["privacy".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["privacy show-data".into()],
            },
            handler: "privacy_show_data".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["privacy".into()],
            related: vec!["privacy.status".into(), "cloud.sync".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "audit.show".into(),
            cli_path: vec!["audit".into(), "show".into()],
            slash: SlashSpec {
                name: "show".into(),
                palette: None,
                description: "List the most recent events".into(),
                long_description: None,
                params: vec![ParamSpec::integer_opt(
                    "limit",
                    "Maximum events to print; 0 prints all",
                    50,
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["audit".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["audit show --limit 20".into()],
            },
            handler: "audit_show".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["audit".into()],
            related: vec!["audit.verify".into(), "audit.export".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "audit.verify".into(),
            cli_path: vec!["audit".into(), "verify".into()],
            slash: SlashSpec {
                name: "verify".into(),
                palette: None,
                description: "Verify the hash chain and exit non-zero on the first break".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["audit".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["audit verify".into()],
            },
            handler: "audit_verify".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["audit".into()],
            related: vec!["audit.show".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "audit.export".into(),
            cli_path: vec!["audit".into(), "export".into()],
            slash: SlashSpec {
                name: "export".into(),
                palette: None,
                description: "Write the trail unchanged as JSONL".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt(
                    "out",
                    "Destination file; without it the JSONL goes to stdout",
                    "",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["audit".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["audit export --out audit.jsonl".into()],
            },
            handler: "audit_export".into(),
            json_output: true,
            streaming: true,
            timeout_secs: None,
            min_version: None,
            tags: vec!["audit".into()],
            related: vec!["audit.show".into()],
        })
        .unwrap();

    for (id, description, example) in [
        (
            "receipt.write",
            "Measure `HEAD` (or --commit) and write its receipt",
            "receipt write",
        ),
        (
            "receipt.show",
            "Print the receipt for a commit",
            "receipt show --commit HEAD~1",
        ),
        (
            "receipt.verify",
            "Verify a receipt against the commit it describes",
            "receipt verify",
        ),
    ] {
        builder = builder
            .command(CommandSpec {
                id: id.into(),
                cli_path: vec!["receipt".into(), id.rsplit('.').next().unwrap().into()],
                slash: SlashSpec {
                    name: id.rsplit('.').next().unwrap().into(),
                    palette: None,
                    description: description.into(),
                    long_description: None,
                    params: vec![ParamSpec::string_opt(
                        "commit",
                        "Revision to measure, show or verify; defaults to HEAD",
                        "",
                    )],
                    overlay: OverlayType::None,
                    danger: if id.ends_with("write") {
                        DangerLevel::LocalWrite
                    } else {
                        DangerLevel::Safe
                    },
                    auth: AuthLevel::None,
                    profile: None,
                    categories: vec!["receipt".into()],
                    aliases: vec![],
                    hidden: false,
                    deprecated: None,
                    examples: vec![example.into()],
                },
                handler: id.replace('.', "_"),
                json_output: true,
                streaming: false,
                timeout_secs: None,
                min_version: None,
                tags: vec!["receipt".into()],
                related: vec!["receipt.write".into(), "receipt.verify".into()],
            })
            .unwrap();
    }

    builder = builder
        .command(CommandSpec {
            id: "set-theme".into(),
            cli_path: vec!["set-theme".into()],
            slash: SlashSpec {
                name: "set-theme".into(),
                palette: None,
                description: "Set UI theme".into(),
                long_description: None,
                params: vec![
                    ParamSpec::enum_param(
                        "theme",
                        "Theme: default, high-contrast or no-color",
                        vec!["default".into(), "high-contrast".into(), "no-color".into()],
                    )
                    .positional(0),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["ui".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["set-theme high-contrast".into()],
            },
            handler: "set_theme".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["ui".into()],
            related: vec!["set-screen-reader".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "set-screen-reader".into(),
            cli_path: vec!["set-screen-reader".into()],
            slash: SlashSpec {
                name: "set-screen-reader".into(),
                palette: None,
                description: "Set screen reader mode".into(),
                long_description: None,
                params: vec![
                    ParamSpec::enum_param(
                        "mode",
                        "Mode: off or plain",
                        vec!["off".into(), "plain".into()],
                    )
                    .positional(0),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["ui".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["set-screen-reader plain".into()],
            },
            handler: "set_screen_reader".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["ui".into()],
            related: vec!["set-theme".into()],
        })
        .unwrap();

    // ── Worktrees and skills ─────────────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "worktree.create".into(),
            cli_path: vec!["worktree".into(), "create".into()],
            slash: SlashSpec {
                name: "create".into(),
                palette: None,
                description: "Create a new worktree".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string(
                        "name",
                        "Worktree name (directory name under .memnoth/worktrees/)",
                    )
                    .positional(0),
                    ParamSpec::string_opt(
                        "branch",
                        "Branch to create/check out in the worktree",
                        "",
                    )
                    .short('b'),
                    ParamSpec::string_opt("base", "Base branch/commit to create from", "HEAD"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["worktree".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["worktree create spike --branch spike".into()],
            },
            handler: "worktree_create".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["worktree".into()],
            related: vec!["worktree.list".into(), "worktree.clean".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "worktree.list".into(),
            cli_path: vec!["worktree".into(), "list".into()],
            slash: SlashSpec {
                name: "list".into(),
                palette: None,
                description: "List all worktrees".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["worktree".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["worktree list".into()],
            },
            handler: "worktree_list".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["worktree".into()],
            related: vec!["worktree.create".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "worktree.clean".into(),
            cli_path: vec!["worktree".into(), "clean".into()],
            slash: SlashSpec {
                name: "clean".into(),
                palette: None,
                description: "Remove a worktree".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("name", "Worktree name to remove").positional(0),
                    ParamSpec::bool("force", "Force removal even with uncommitted changes")
                        .short('f'),
                    ParamSpec::bool("yes", "Answer yes without asking"),
                    ParamSpec::bool(
                        "dry_run",
                        "Show what would be removed without removing anything",
                    ),
                ],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::Destructive,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["worktree".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["worktree clean spike".into()],
            },
            handler: "worktree_clean".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["worktree".into()],
            related: vec!["worktree.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "worktree.finish".into(),
            cli_path: vec!["worktree".into(), "finish".into()],
            slash: SlashSpec {
                name: "finish".into(),
                palette: None,
                description: "End a run: clean its worktree on success, keep it on failure".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("run_id", "Run id (the worktree directory name)")
                        .positional(0),
                    ParamSpec::string_opt("outcome", "success or failed", "success"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["worktree".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["worktree finish run-42 --outcome failed".into()],
            },
            handler: "worktree_finish".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["worktree".into()],
            related: vec!["worktree.prune".into(), "worktree.clean".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "worktree.diff".into(),
            cli_path: vec!["worktree".into(), "diff".into()],
            slash: SlashSpec {
                name: "diff".into(),
                palette: None,
                description: "Diff between worktrees".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("from", "Source worktree/branch name").positional(0),
                    ParamSpec::string_opt("to", "Target worktree/branch name", "").positional(1),
                    ParamSpec::string_opt("paths", "Paths to limit the diff to", "").positional(2),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["worktree".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["worktree diff main spike".into()],
            },
            handler: "worktree_diff".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["worktree".into()],
            related: vec!["worktree.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "skill.add".into(),
            cli_path: vec!["skill".into(), "add".into()],
            slash: SlashSpec {
                name: "add".into(),
                palette: None,
                description: "Add a new skill from a SKILL.md file".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("path", "Path to the SKILL.md file to add").positional(0),
                    ParamSpec::string_opt(
                        "name",
                        "Override the skill name (uses frontmatter name by default)",
                        "",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["skill add ./SKILL.md".into()],
            },
            handler: "skill_add".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "skill.list".into(),
            cli_path: vec!["skill".into(), "list".into()],
            slash: SlashSpec {
                name: "list".into(),
                palette: None,
                description: "List all installed skills".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool("detail", "Show detailed information for each skill")
                        .short('d'),
                    ParamSpec::string_opt("tag", "Filter by tag", ""),
                ],
                overlay: OverlayType::Picker,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["skill list".into()],
            },
            handler: "skill_list".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.add".into(), "skill.run".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "skill.remove".into(),
            cli_path: vec!["skill".into(), "remove".into()],
            slash: SlashSpec {
                name: "remove".into(),
                palette: None,
                description: "Remove a skill by name".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("name", "Name of the skill to remove").positional(0),
                    ParamSpec::bool("force", "Force removal without confirmation").short('f'),
                    ParamSpec::bool("yes", "Answer yes without asking"),
                    ParamSpec::bool(
                        "dry_run",
                        "Show what would be removed without removing anything",
                    ),
                ],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::Destructive,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["skill remove deploy-check".into()],
            },
            handler: "skill_remove".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "skill.run".into(),
            cli_path: vec!["skill".into(), "run".into()],
            slash: SlashSpec {
                name: "run".into(),
                palette: None,
                description: "Run a skill with arguments".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("name", "Name of the skill to run").positional(0),
                    ParamSpec::string_opt("args", "KEY=VALUE arguments for the skill", "")
                        .positional(1),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["skill run deploy-check ENV=staging".into()],
            },
            handler: "skill_run".into(),
            json_output: true,
            streaming: true,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "skill.edit".into(),
            cli_path: vec!["skill".into(), "edit".into()],
            slash: SlashSpec {
                name: "edit".into(),
                palette: None,
                description: "Edit a skill's SKILL.md file".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("name", "Name of the skill to edit").positional(0),
                    ParamSpec::string_opt(
                        "editor",
                        "Editor to use (defaults to $EDITOR or vi)",
                        "",
                    )
                    .short('e'),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["skill".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["skill edit deploy-check".into()],
            },
            handler: "skill_edit".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["skill".into()],
            related: vec!["skill.add".into()],
        })
        .unwrap();

    // ── Billing, provider config, feedback ────────────────────────

    for (id, description, example) in [
        (
            "billing.invoices",
            "Show billing history / invoices link",
            "billing invoices",
        ),
        (
            "billing.plan",
            "Show current plan and quotas",
            "billing plan",
        ),
        (
            "billing.limits",
            "Show rate limits and quotas",
            "billing limits",
        ),
    ] {
        builder = builder
            .command(CommandSpec {
                id: id.into(),
                cli_path: vec!["billing".into(), id.rsplit('.').next().unwrap().into()],
                slash: SlashSpec {
                    name: id.rsplit('.').next().unwrap().into(),
                    palette: None,
                    description: description.into(),
                    long_description: None,
                    params: vec![],
                    overlay: OverlayType::None,
                    danger: DangerLevel::Safe,
                    auth: AuthLevel::Cloud,
                    profile: None,
                    categories: vec!["billing".into()],
                    aliases: vec![],
                    hidden: false,
                    deprecated: None,
                    examples: vec![example.into()],
                },
                handler: id.replace('.', "_"),
                json_output: true,
                streaming: false,
                timeout_secs: None,
                min_version: None,
                tags: vec!["billing".into()],
                related: vec!["billing.plan".into(), "billing.limits".into()],
            })
            .unwrap();
    }

    builder = builder
        .command(CommandSpec {
            id: "connect.add".into(),
            cli_path: vec!["connect".into(), "add".into()],
            slash: SlashSpec {
                name: "add".into(),
                palette: None,
                description: "Add or update a provider configuration".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string_opt(
                        "provider",
                        "Provider name (e.g. anthropic, openai, openrouter)",
                        "",
                    ),
                    ParamSpec::string_opt("api-key", "API key for the provider", ""),
                    ParamSpec::string_opt(
                        "base-url",
                        "Base URL for the provider (custom endpoints/gateways)",
                        "",
                    ),
                    ParamSpec::string_opt("model", "Model ID to use with this provider", ""),
                    ParamSpec::bool(
                        "non-interactive",
                        "Run in non-interactive mode (requires --provider and --api-key)",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["provider".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["connect add --provider anthropic --model claude-sonnet-4-5".into()],
            },
            handler: "connect_add".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["provider".into()],
            related: vec!["connect.list".into(), "connect.test".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "connect.list".into(),
            cli_path: vec!["connect".into(), "list".into()],
            slash: SlashSpec {
                name: "list".into(),
                palette: None,
                description: "List configured providers".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["provider".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["connect list".into()],
            },
            handler: "connect_list".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["provider".into()],
            related: vec!["connect.add".into(), "connect.remove".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "connect.remove".into(),
            cli_path: vec!["connect".into(), "remove".into()],
            slash: SlashSpec {
                name: "remove".into(),
                palette: None,
                description: "Remove a provider configuration".into(),
                long_description: None,
                params: vec![ParamSpec::string("name", "Provider name to remove").positional(0)],
                overlay: OverlayType::Confirm,
                danger: DangerLevel::Destructive,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["provider".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["connect remove openrouter".into()],
            },
            handler: "connect_remove".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["provider".into()],
            related: vec!["connect.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "connect.test".into(),
            cli_path: vec!["connect".into(), "test".into()],
            slash: SlashSpec {
                name: "test".into(),
                palette: None,
                description: "Test connection to a provider".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt(
                    "provider",
                    "Provider name to test (defaults to the configured provider)",
                    "",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["provider".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["connect test".into()],
            },
            handler: "connect_test".into(),
            json_output: true,
            streaming: false,
            timeout_secs: Some(120),
            min_version: None,
            tags: vec!["provider".into()],
            related: vec!["connect.list".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "feedback.send".into(),
            cli_path: vec!["feedback".into(), "send".into()],
            slash: SlashSpec {
                name: "send".into(),
                palette: None,
                description: "Send feedback to the memnoth team".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("message", "Feedback message").positional(0),
                    ParamSpec::string_opt("category", "Feedback category", "general"),
                    ParamSpec::bool(
                        "include-context",
                        "Include session/vault context with feedback",
                    ),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::CloudWrite,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["feedback send \"the palette is hard to reach\"".into()],
            },
            handler: "feedback_send".into(),
            json_output: true,
            streaming: false,
            timeout_secs: Some(60),
            min_version: None,
            tags: vec!["product".into()],
            related: vec![],
        })
        .unwrap();

    // ── Automation (three levels deep) ────────────────────────────

    builder = builder
        .command(CommandSpec {
            id: "automation.hooks.install".into(),
            cli_path: vec!["automation".into(), "hooks".into(), "install".into()],
            slash: SlashSpec {
                name: "install".into(),
                palette: None,
                description: "Install pre-commit hook for kg index".into(),
                long_description: None,
                params: vec![ParamSpec::bool("force", "Overwrite existing hook")],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["automation".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["automation hooks install".into()],
            },
            handler: "automation_hooks_install".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["automation".into()],
            related: vec!["automation.action.init".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "automation.action.init".into(),
            cli_path: vec!["automation".into(), "action".into(), "init".into()],
            slash: SlashSpec {
                name: "init".into(),
                palette: None,
                description: "Generate GitHub Actions workflow for CI".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string_opt("name", "Project name (defaults to directory name)", ""),
                    ParamSpec::bool("kg-index", "Include kg index step in workflow"),
                    ParamSpec::bool("sync", "Include cloud sync step in workflow"),
                    ParamSpec::bool("force", "Overwrite existing workflow"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["automation".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["automation action init --kg-index".into()],
            },
            handler: "automation_action_init".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["automation".into()],
            related: vec!["automation.ci.index".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "automation.ci.detect".into(),
            cli_path: vec!["automation".into(), "ci".into(), "detect".into()],
            slash: SlashSpec {
                name: "detect".into(),
                palette: None,
                description: "Detect CI environment".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["automation".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["automation ci detect".into()],
            },
            handler: "automation_ci_detect".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["automation".into()],
            related: vec!["automation.ci.index".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "automation.ci.index".into(),
            cli_path: vec!["automation".into(), "ci".into(), "index".into()],
            slash: SlashSpec {
                name: "index".into(),
                palette: None,
                description: "Index code graph in CI".into(),
                long_description: None,
                params: vec![
                    ParamSpec::bool("full", "Full re-index (ignore incremental)"),
                    ParamSpec::bool("no-embed", "Skip embedding generation"),
                    ParamSpec::bool("sync", "Also sync to cloud after indexing"),
                    ParamSpec::bool("force", "Force run even outside CI"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["automation".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["automation ci index --full".into()],
            },
            handler: "automation_ci_index".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["automation".into()],
            related: vec!["automation.ci.detect".into(), "kg.index".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "watch".into(),
            cli_path: vec!["watch".into()],
            slash: SlashSpec {
                name: "watch".into(),
                palette: None,
                description: "Watch files and reindex incrementally".into(),
                long_description: None,
                params: vec![
                    // `--port` is an `Option<u16>`: absent means "no event
                    // server", which is not the same as 0. A default would
                    // have to invent a port, so this spec carries none.
                    ParamSpec {
                        name: "port".into(),
                        description:
                            "Port to serve file watch events (optional, for editor integration)"
                                .into(),
                        param_type: ParamType::Integer {
                            min: Some(1),
                            max: Some(65535),
                        },
                        required: false,
                        default: None,
                        short: None,
                        long: None,
                        env: None,
                        positional: false,
                        position: None,
                        hidden: false,
                        deprecated: None,
                    },
                    ParamSpec::integer_opt("debounce", "Debounce interval in milliseconds", 500),
                    ParamSpec::bool("vault-changes", "Also watch vault files for changes"),
                    ParamSpec::bool("once", "Run once and exit (no continuous watch)"),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["watch --once".into()],
            },
            handler: "watch".into(),
            json_output: true,
            streaming: true,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into()],
            related: vec!["serve".into(), "kg.index".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "demo".into(),
            cli_path: vec!["demo".into()],
            slash: SlashSpec {
                name: "demo".into(),
                palette: None,
                description:
                    "Offline vault demo: decision, believed-at, superseded-by, source/commit, both contradiction sides"
                        .into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::None,
                danger: DangerLevel::LocalWrite,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["product".into()],
                aliases: vec![],
                hidden: true,
                deprecated: None,
                examples: vec!["demo".into()],
            },
            handler: "demo".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["product".into()],
            related: vec!["vault.remember".into(), "vault.correct".into()],
        })
        .unwrap();
    // ── Cloud admin surface: models, keys, projects, org, teams ───
    //
    // These are one row per subcommand and share every field except the four
    // that differ, so they go through `cloud_spec`. Names, flags and defaults
    // come from the verbs' own `-h` output; a spec here that accepted a flag
    // the binary does not would be worse than no spec.
    let cloud = vec![
        cloud_spec(
            "keys.list",
            &["keys", "list"],
            "List keys in this org, including revoked ones",
            vec![],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "keys list",
            &["keys.create", "keys.revoke"],
        ),
        cloud_spec(
            "keys.create",
            &["keys", "create"],
            "Mint a key. The secret is printed once and never stored",
            vec![
                ParamSpec::string_opt("name", "A label so the key can be recognised later", ""),
                ParamSpec::enum_opt(
                    "scope",
                    "The key's authority; the server decides the default",
                    vec!["read".into(), "write".into(), "admin".into()],
                    "read",
                ),
                ParamSpec::string_opt(
                    "project",
                    "Bind the key to one project id instead of the whole org",
                    "",
                ),
                integer_flag("expires-in-days", "Stop working after this many days"),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "keys create --name ci --scope write",
            &["keys.list", "keys.revoke"],
        ),
        cloud_spec(
            "keys.revoke",
            &["keys", "revoke"],
            "Revoke one key by id",
            vec![
                ParamSpec::string("id", "Key id, as `mn keys list` reports it").positional(0),
                ParamSpec::bool("yes", "Answer yes without asking"),
                ParamSpec::bool(
                    "dry_run",
                    "Show what would be revoked without revoking anything",
                ),
            ],
            DangerLevel::Destructive,
            AuthLevel::Cloud,
            "keys revoke key_123",
            &["keys.list"],
        ),
        cloud_spec(
            "keys.revoke-all",
            &["keys", "revoke-all"],
            "Revoke every live key in the org (owner or admin)",
            vec![
                ParamSpec::bool("yes", "Revoke every key without asking for confirmation"),
                ParamSpec::bool("dry_run", "Count the live keys; revoke nothing"),
            ],
            DangerLevel::Destructive,
            AuthLevel::Cloud,
            "keys revoke-all --yes",
            &["keys.list"],
        ),
        cloud_spec(
            "projects.list",
            &["projects", "list"],
            "List the projects this credential can see",
            vec![],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "projects list",
            &["projects.create"],
        ),
        cloud_spec(
            "projects.create",
            &["projects", "create"],
            "Create a project",
            vec![
                ParamSpec::string("name", "Project name").positional(0),
                ParamSpec::enum_opt(
                    "visibility",
                    "Who can see it without a grant: org or restricted",
                    vec!["org".into(), "restricted".into()],
                    "org",
                ),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "projects create payments",
            &["projects.list", "projects.rename"],
        ),
        cloud_spec(
            "projects.rename",
            &["projects", "rename"],
            "Rename a project",
            vec![
                ParamSpec::string("id", "Project id").positional(0),
                ParamSpec::string("name", "The new name").positional(1),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "projects rename prj_123 payments-api",
            &["projects.list"],
        ),
        cloud_spec(
            "projects.delete",
            &["projects", "delete"],
            "Delete a project. Its sessions are kept, unassigned",
            vec![
                ParamSpec::string("id", "Project id").positional(0),
                ParamSpec::bool("yes", "Delete without asking for confirmation"),
            ],
            DangerLevel::Destructive,
            AuthLevel::Cloud,
            "projects delete prj_123 --yes",
            &["projects.list"],
        ),
        cloud_spec(
            "projects.use",
            &["projects", "use"],
            "File every session from this machine under a project",
            vec![
                ParamSpec::string_opt("id", "Project id to file sessions under", "").positional(0),
                ParamSpec::bool(
                    "clear",
                    "Stop filing sessions under a project; they report as unassigned again",
                ),
            ],
            DangerLevel::LocalWrite,
            AuthLevel::Cloud,
            "projects use prj_123",
            &["projects.unassigned"],
        ),
        cloud_spec(
            "projects.unassigned",
            &["projects", "unassigned"],
            "List the sessions no project has claimed yet",
            vec![],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "projects unassigned",
            &["projects.file"],
        ),
        cloud_spec(
            "projects.file",
            &["projects", "file"],
            "File one already-recorded session under the current project",
            vec![
                ParamSpec::string(
                    "session",
                    "Session id, as `mn projects unassigned` prints it",
                )
                .positional(0),
                ParamSpec::string_opt(
                    "project",
                    "File into this project instead of the one `mn projects use` stored",
                    "",
                ),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "projects file sess_123",
            &["projects.unassigned"],
        ),
        cloud_spec(
            "org.show",
            &["org", "show"],
            "Show the org: name, plan, your role, and its counts",
            vec![],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "org show",
            &["org.members"],
        ),
        cloud_spec(
            "org.rename",
            &["org", "rename"],
            "Rename the org (owner or admin)",
            vec![ParamSpec::string("name", "The new display name").positional(0)],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "org rename Astrivya",
            &["org.show"],
        ),
        cloud_spec(
            "org.members",
            &["org", "members"],
            "List the org's members",
            vec![],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "org members",
            &["org.add-member"],
        ),
        cloud_spec(
            "org.add-member",
            &["org", "add-member"],
            "Add an existing account to the org by email",
            vec![
                ParamSpec::string("email", "The address of an account that already exists")
                    .positional(0),
                ParamSpec::enum_opt(
                    "role",
                    "Role to grant: owner, admin or member",
                    vec!["owner".into(), "admin".into(), "member".into()],
                    "member",
                ),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "org add-member dev@example.com --role admin",
            &["org.members"],
        ),
        cloud_spec(
            "org.set-role",
            &["org", "set-role"],
            "Change a member's role: owner, admin or member",
            vec![
                ParamSpec::string(
                    "user-id",
                    "The member's user id, as `mn org members` reports it",
                )
                .positional(0),
                ParamSpec::enum_param(
                    "role",
                    "The new role: owner, admin or member",
                    vec!["owner".into(), "admin".into(), "member".into()],
                )
                .positional(1),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "org set-role usr_1 admin",
            &["org.members"],
        ),
        cloud_spec(
            "org.remove-member",
            &["org", "remove-member"],
            "Remove a member, their project grants and their keys",
            vec![ParamSpec::string("user-id", "The member's user id").positional(0)],
            DangerLevel::Destructive,
            AuthLevel::Cloud,
            "org remove-member usr_1",
            &["org.members"],
        ),
        cloud_spec(
            "org.invites",
            &["org", "invites"],
            "List pending invites",
            vec![],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "org invites",
            &["org.invite"],
        ),
        cloud_spec(
            "org.invite",
            &["org", "invite"],
            "Invite someone by email; the link and code are shown once",
            vec![
                ParamSpec::string("email", "The invitee's email address").positional(0),
                ParamSpec::enum_opt(
                    "role",
                    "Role the invite carries: admin or member",
                    vec!["admin".into(), "member".into()],
                    "member",
                ),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "org invite new@example.com",
            &["org.invites"],
        ),
        cloud_spec(
            "org.withdraw",
            &["org", "withdraw"],
            "Withdraw a pending invite",
            vec![
                ParamSpec::string("id", "Invite id, as `mn org invites` reports it").positional(0),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "org withdraw inv_1",
            &["org.invites"],
        ),
        cloud_spec(
            "teams.join",
            &["teams", "join"],
            "Accept an invite code and join an organization",
            vec![
                ParamSpec::string(
                    "code",
                    "The invite code (XXXX-XXXX format, hyphen optional)",
                )
                .positional(0),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "teams join ABCD-EFGH",
            &["teams.list"],
        ),
        cloud_spec(
            "teams.list",
            &["teams", "list"],
            "List the org's teams",
            vec![],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "teams list",
            &["teams.create"],
        ),
        cloud_spec(
            "teams.create",
            &["teams", "create"],
            "Create a team inside the org",
            vec![ParamSpec::string("name", "Team name").positional(0)],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "teams create platform",
            &["teams.list"],
        ),
        cloud_spec(
            "teams.members",
            &["teams", "members"],
            "Show one team: its people and the projects it reaches",
            vec![ParamSpec::string("id", "Team id, as `mn teams list` reports it").positional(0)],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "teams members team_1",
            &["teams.list"],
        ),
        cloud_spec(
            "teams.claim",
            &["teams", "claim"],
            "Claim a task from the shared board",
            vec![ParamSpec::string("task-id", "The task ID to claim").positional(0)],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "teams claim task_1",
            &["teams.release", "teams.tasks"],
        ),
        cloud_spec(
            "teams.release",
            &["teams", "release"],
            "Release a claimed task",
            vec![ParamSpec::string("task-id", "The task ID to release").positional(0)],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "teams release task_1",
            &["teams.claim"],
        ),
        cloud_spec(
            "teams.tasks",
            &["teams", "tasks"],
            "List tasks on the shared board",
            vec![
                ParamSpec::bool("pending", "Show only pending tasks"),
                ParamSpec::bool("in-progress", "Show only in-progress tasks"),
                ParamSpec::bool("done", "Show only done tasks"),
            ],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "teams tasks --pending",
            &["teams.claim"],
        ),
        cloud_spec(
            "teams.inbox",
            &["teams", "inbox"],
            "Show messages in your inbox",
            vec![
                ParamSpec::bool("broadcast", "Include broadcast messages"),
                ParamSpec::bool("ack", "Acknowledge all messages after reading"),
            ],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "teams inbox",
            &["teams.handoff"],
        ),
        cloud_spec(
            "teams.presence",
            &["teams", "presence"],
            "Show team presence (active sessions)",
            vec![
                ParamSpec::bool("stale", "Show stale sessions only"),
                ParamSpec::bool("sync", "Sync presence with cloud"),
            ],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "teams presence",
            &["teams.members"],
        ),
        cloud_spec(
            "teams.lock",
            &["teams", "lock"],
            "Lock a file path for exclusive editing",
            vec![
                ParamSpec::string("path", "The file path glob to lock").positional(0),
                ParamSpec::integer_opt("ttl", "TTL in seconds (default: 1800 = 30 min)", 1800),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "teams lock src/pay/**",
            &["teams.presence"],
        ),
        cloud_spec(
            "teams.handoff",
            &["teams", "handoff"],
            "Hand off a task to another team member",
            vec![
                ParamSpec::string("task-id", "The task ID to hand off").positional(0),
                ParamSpec::string("target", "The target session/user to hand off to").positional(1),
                ParamSpec::string_opt("message", "Message to include with handoff", ""),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "teams handoff task_1 sam",
            &["teams.inbox"],
        ),
        cloud_spec(
            "teams.review",
            &["teams", "review"],
            "Request a review for a task",
            vec![
                ParamSpec::string("task-id", "The task ID to request review for").positional(0),
                ParamSpec::string("reviewer", "The reviewer session/user").positional(1),
                ParamSpec::string_opt("message", "Review message", ""),
            ],
            DangerLevel::CloudWrite,
            AuthLevel::Cloud,
            "teams review task_1 sam",
            &["teams.tasks"],
        ),
        cloud_spec(
            "teams.audit",
            &["teams", "audit"],
            "Run an audit of team activity",
            vec![
                ParamSpec::string_opt("range", "Time range for audit (e.g. 24h, 7d, 30d)", "24h"),
                ParamSpec::string_opt("session", "Filter by session ID", ""),
            ],
            DangerLevel::Safe,
            AuthLevel::Cloud,
            "teams audit --range 7d",
            &["audit.show"],
        ),
    ];
    // ── Signature TUI commands ──────────────────────────────────────
    // W13: one registry row per signature verb, projected into palette,
    // slash dropdown, help, and footer.

    builder = builder
        .command(CommandSpec {
            id: "checkpoint".into(),
            cli_path: vec!["checkpoint".into()],
            slash: SlashSpec {
                name: "checkpoint".into(),
                palette: None,
                description: "Snapshot code, conversation, and vault metadata".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt(
                    "name",
                    "Checkpoint name",
                    "ckpt-auto",
                )],
                overlay: OverlayType::Fullscreen,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["signature".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["checkpoint --name pre-change".into()],
            },
            handler: "checkpoint".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["signature".into(), "checkpoint".into()],
            related: vec!["undo".into(), "redo".into(), "receipt".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "delegate".into(),
            cli_path: vec!["delegate".into()],
            slash: SlashSpec {
                name: "delegate".into(),
                palette: None,
                description: "Delegate a task to a scoped subagent".into(),
                long_description: None,
                params: vec![
                    ParamSpec::string("name", "Agent name").short('n'),
                    ParamSpec::string("task", "Task description"),
                ],
                overlay: OverlayType::Fullscreen,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["signature".into(), "delegation".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["delegate scout map auth entry points".into()],
            },
            handler: "delegate".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["signature".into(), "delegate".into()],
            related: vec!["merge".into(), "handoff".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "receipt".into(),
            cli_path: vec!["receipt".into()],
            slash: SlashSpec {
                name: "receipt".into(),
                palette: None,
                description: "Show and verify a turn receipt".into(),
                long_description: None,
                params: vec![ParamSpec::string_opt("turn", "Turn number", "last")],
                overlay: OverlayType::Fullscreen,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["signature".into(), "receipts".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["receipt --verify".into()],
            },
            handler: "receipt".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["signature".into(), "receipt".into()],
            related: vec!["lineage".into(), "audit".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "brief".into(),
            cli_path: vec!["brief".into()],
            slash: SlashSpec {
                name: "brief".into(),
                palette: None,
                description: "Session brief: primary action, pulse facts, quickstart".into(),
                long_description: None,
                params: vec![],
                overlay: OverlayType::Fullscreen,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["signature".into(), "home".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["brief".into()],
            },
            handler: "brief".into(),
            json_output: false,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["signature".into(), "brief".into()],
            related: vec!["status".into()],
        })
        .unwrap();

    builder = builder.commands(cloud).unwrap();
    // The hidden single-word session verbs: every Verb variant clap parses
    // needs a row, even the ones that stay out of the public surface.
    let hidden_verbs = vec![
        cloud_spec(
            "undo",
            &["undo"],
            "Undo the last turn (conversation only)",
            vec![],
            DangerLevel::Safe,
            AuthLevel::None,
            "undo",
            &["redo"],
        ),
        cloud_spec(
            "redo",
            &["redo"],
            "Redo the last undone turn",
            vec![],
            DangerLevel::Safe,
            AuthLevel::None,
            "redo",
            &["undo"],
        ),
        cloud_spec(
            "merge",
            &["merge"],
            "Merge a child branch, surfacing belief conflicts",
            vec![ParamSpec::string("branch", "The branch to merge in")],
            DangerLevel::Safe,
            AuthLevel::None,
            "merge <branch>",
            &["handoff"],
        ),
        cloud_spec(
            "handoff",
            &["handoff"],
            "Bundle a session for transfer to another machine",
            vec![ParamSpec {
                required: false,
                ..ParamSpec::string("path", "Where to write the bundle")
            }],
            DangerLevel::Safe,
            AuthLevel::None,
            "handoff",
            &["merge"],
        ),
        cloud_spec(
            "compact",
            &["compact"],
            "Compact context, keeping exact facts",
            vec![ParamSpec {
                required: false,
                ..ParamSpec::string("focus", "What to focus the compaction on")
            }],
            DangerLevel::Safe,
            AuthLevel::None,
            "compact",
            &["recap"],
        ),
        cloud_spec(
            "btw",
            &["btw"],
            "Side question; not persisted in the transcript",
            vec![ParamSpec::string("question", "The side question")],
            DangerLevel::Safe,
            AuthLevel::None,
            "btw <question>",
            &["ask"],
        ),
        cloud_spec(
            "recap",
            &["recap"],
            "One-line session summary",
            vec![],
            DangerLevel::Safe,
            AuthLevel::None,
            "recap",
            &["compact"],
        ),
        cloud_spec(
            "export",
            &["export"],
            "Export session transcript with tool calls and receipts",
            vec![ParamSpec {
                required: false,
                ..ParamSpec::string("path", "Where to write the export")
            }],
            DangerLevel::Safe,
            AuthLevel::None,
            "export",
            &["handoff"],
        ),
        cloud_spec(
            "rename",
            &["rename"],
            "Rename the current session, keeping its id and path",
            vec![ParamSpec::string("name", "The new session name")],
            DangerLevel::Safe,
            AuthLevel::None,
            "rename <name>",
            &["session"],
        ),
        cloud_spec(
            "keymap",
            &["keymap"],
            "View and remap key bindings",
            vec![],
            DangerLevel::Safe,
            AuthLevel::None,
            "keymap",
            &["brief"],
        ),
        cloud_spec(
            "onboarding",
            &["onboarding"],
            "Replayable first-run onboarding card",
            vec![],
            DangerLevel::Safe,
            AuthLevel::None,
            "onboarding",
            &["brief"],
        ),
        cloud_spec(
            "acp",
            &["acp"],
            "Agent Communication Protocol server mode",
            vec![],
            DangerLevel::Safe,
            AuthLevel::None,
            "acp",
            &["mcp"],
        ),
        cloud_spec(
            "extension",
            &["extension"],
            "Extension management: new, list, uninstall, doctor",
            vec![ParamSpec {
                required: false,
                ..ParamSpec::string("action", "Subcommand: new, list, uninstall, doctor")
            }],
            DangerLevel::Safe,
            AuthLevel::None,
            "extension list",
            &["skills"],
        ),
        cloud_spec(
            "plan",
            &["plan"],
            "Set or show the session plan",
            vec![
                ParamSpec {
                    required: false,
                    ..ParamSpec::string("name", "Session name")
                },
                ParamSpec::bool("enable", "Enable plan mode"),
                ParamSpec::bool("disable", "Disable plan mode"),
                ParamSpec::bool("json", "Output as JSON"),
            ],
            DangerLevel::Safe,
            AuthLevel::None,
            "plan",
            &["goal"],
        ),
        cloud_spec(
            "goal",
            &["goal"],
            "Set or show the session goal condition",
            vec![ParamSpec {
                required: false,
                ..ParamSpec::string("condition", "The completion condition to set")
            }],
            DangerLevel::Safe,
            AuthLevel::None,
            "goal",
            &["plan"],
        ),
        cloud_spec(
            "sandbox",
            &["sandbox"],
            "Run a command in the sandbox, or set the sandbox up",
            vec![ParamSpec::string(
                "command",
                "Subcommand: test <cmd> or setup",
            )],
            DangerLevel::Safe,
            AuthLevel::None,
            "sandbox setup",
            &["trust"],
        ),
        cloud_spec(
            "budget",
            &["budget"],
            "Show token budget usage",
            vec![],
            DangerLevel::Safe,
            AuthLevel::None,
            "budget",
            &["usage"],
        ),
        cloud_spec(
            "trust",
            &["trust"],
            "Trust: list trusted folders, or forget one",
            vec![ParamSpec {
                required: false,
                ..ParamSpec::string("action", "Subcommand: list or forget <path>")
            }],
            DangerLevel::Safe,
            AuthLevel::None,
            "trust list",
            &["sandbox"],
        ),
    ];
    builder = builder.commands(hidden_verbs).unwrap();
    builder.apply_public_surface()
}

/// One cloud-admin row: every field the surface shares, in one place.
///
/// Eight parameters is the shape of the data — id, path, description, params,
/// danger, auth, one example and its siblings — and a struct here would only
/// move the same names to a call site. The table below reads better as rows.
#[allow(clippy::too_many_arguments)]
fn cloud_spec(
    id: &str,
    path: &[&str],
    description: &str,
    params: Vec<ParamSpec>,
    danger: DangerLevel,
    auth: AuthLevel,
    example: &str,
    related: &[&str],
) -> CommandSpec {
    let category = path.first().copied().unwrap_or("cloud").to_string();
    CommandSpec {
        id: id.into(),
        cli_path: path.iter().map(|word| (*word).to_string()).collect(),
        slash: SlashSpec {
            name: path.last().copied().unwrap_or(id).to_string(),
            palette: None,
            description: description.into(),
            long_description: None,
            params,
            overlay: if danger == DangerLevel::Destructive {
                OverlayType::Confirm
            } else {
                OverlayType::None
            },
            danger,
            auth,
            profile: None,
            categories: vec![category.clone()],
            aliases: vec![],
            hidden: false,
            deprecated: None,
            examples: vec![example.into()],
        },
        handler: id.replace('.', "_"),
        json_output: true,
        streaming: false,
        timeout_secs: None,
        min_version: None,
        tags: vec![category],
        related: related.iter().map(|word| (*word).to_string()).collect(),
    }
}

/// A flag that takes a whole number and has no default: absent is not zero.
fn integer_flag(name: &str, description: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        description: description.into(),
        param_type: ParamType::Integer {
            min: Some(1),
            max: None,
        },
        required: false,
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

/// Helper: optional integer parameter.
pub trait ParamSpecIntegerExt {
    /// An optional integer argument that defaults to `default`.
    fn integer_opt(
        name: impl Into<String>,
        description: impl Into<String>,
        default: usize,
    ) -> ParamSpec;
}

impl ParamSpecIntegerExt for ParamSpec {
    fn integer_opt(
        name: impl Into<String>,
        description: impl Into<String>,
        default: usize,
    ) -> ParamSpec {
        ParamSpec {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Integer {
                min: None,
                max: None,
            },
            required: false,
            default: Some(serde_json::Value::Number(default.into())),
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }
}

/// Helper: optional JSON parameter.
pub trait ParamSpecJsonOptExt {
    /// An optional argument holding a JSON object.
    fn json_opt(name: impl Into<String>, description: impl Into<String>) -> ParamSpec;
}

impl ParamSpecJsonOptExt for ParamSpec {
    fn json_opt(name: impl Into<String>, description: impl Into<String>) -> ParamSpec {
        ParamSpec {
            name: name.into(),
            description: description.into(),
            param_type: ParamType::Json,
            required: false,
            default: Some(serde_json::json!({})),
            short: None,
            long: None,
            env: None,
            positional: false,
            position: None,
            hidden: false,
            deprecated: None,
        }
    }
}
