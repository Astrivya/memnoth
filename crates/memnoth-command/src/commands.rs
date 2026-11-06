//! All 34 P0 command specifications — the single source of truth.
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
//! - Cloud: cloud spine (login, logout, status, sync, profile, usage, billing)
//! - Config: configuration (get, set, list)
//! - Completion: shell completions (install, show)

use crate::registry::RegistryBuilder;
use crate::spec::{
    AuthLevel, CommandSpec, DangerLevel, OverlayType, ParamSpec, ParamType, SlashSpec,
};

/// Build the P0 command registry with all 34 command specs.
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
                    ParamSpec::enum_opt("profile", "Tool profile to register. Defaults to full.", vec!["micro".into(), "memory".into(), "coord".into(), "code".into(), "full".into(), "agent".into()], "full"),
                    ParamSpec::string_opt("url", "Server endpoint. Defaults to the configured URL", ""),
                    ParamSpec::string_opt("token", "Bearer token for the client's header. Defaults to $MEMNOTH_MCP_TOKEN. Never written to memnoth's own config.", ""),
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
                examples: vec!["mcp add --clients claude,cursor,codex --profile code".into()],
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
            id: "cloud.usage".into(),
            cli_path: vec!["cloud".into(), "usage".into()],
            slash: SlashSpec {
                name: "usage".into(),
                palette: Some("usage".into()),
                description: "Show cloud usage and billing summary".into(),
                long_description: None,
                params: vec![ParamSpec::enum_opt(
                    "period",
                    "Usage period: day, week, month",
                    vec!["day".into(), "week".into(), "month".into()],
                    "month",
                )],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::Cloud,
                profile: None,
                categories: vec!["cloud".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["cloud usage --period month".into()],
            },
            handler: "cloud_usage".into(),
            json_output: true,
            streaming: false,
            timeout_secs: None,
            min_version: None,
            tags: vec!["cloud".into(), "billing".into()],
            related: vec!["cloud.billing".into()],
        })
        .unwrap();

    builder = builder
        .command(CommandSpec {
            id: "cloud.billing".into(),
            cli_path: vec!["cloud".into(), "billing".into()],
            slash: SlashSpec {
                name: "billing".into(),
                palette: Some("credits".into()),
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
            related: vec!["cloud.usage".into()],
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
                long_description: Some("Outputs all command specifications as Markdown. Use --output to write to a file, otherwise prints to stdout.".into()),
                params: vec![
                    ParamSpec::string_opt("output", "Output file path (default: stdout)", "").short('o'),
                ],
                overlay: OverlayType::None,
                danger: DangerLevel::Safe,
                auth: AuthLevel::None,
                profile: None,
                categories: vec!["docs".into()],
                aliases: vec![],
                hidden: false,
                deprecated: None,
                examples: vec!["docs --output docs/commands.md".into(), "docs".into()],
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

    builder
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
