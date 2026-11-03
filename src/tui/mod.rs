//! The session: one screen over the vault and the code graph.
//!
//! This is what `memnoth` opens with no verb. A plain question runs a model turn
//! when this machine is configured for one: the model gets the vault and the
//! code graph as read-only tools, and the transcript shows each lookup it made.
//! Without a key the same question is answered from the two stores locally, and
//! the screen says why — a fallback that looks like the model declining to
//! answer would be the worst of both.
//!
//! Four properties matter more than the layout:
//!
//! * **The terminal is always restored.** Raw mode and the alternate screen are
//!   entered only after the session file loads, and a guard leaves both on every
//!   exit path, including an error or a panic.
//! * **Slash commands are the CLI.** `/kg status` builds the same [`Verb`] that
//!   `memnoth kg status` parses, and dispatches through
//!   [`crate::commands::execute`] — one implementation, two doors.
//! * **A turn does not freeze the screen.** The model call runs on a thread and
//!   reports each tool call back over a channel, so the transcript fills in
//!   while the turn is in flight and ctrl-c still works. The session is written
//!   on the UI thread when the answer arrives, not by the worker.
//! * **A plain question still has an answer without a model.** The vault is
//!   searched first (facts with dates), then the graph (symbols with locations);
//!   `/search` asks for exactly that even when a model is configured.
//!
//! Nothing redraws on a timer while idle: the screen changes when a key is
//! pressed, a command finishes, or a turn reports. A TUI that re-queries SQLite
//! four times a second would be the most expensive thing in the process.
pub mod app;
pub mod overlays;
pub mod panes;
pub mod theme;
pub mod ui;
use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use memnoth_engine::now_ms;
use memnoth_sandbox::profiles::{SandboxProfile, load_sandbox_config};
use memnoth_sandbox::rules::{RuleDecision, RuleSet, decide};
use memnoth_session::{Role, Session, TurnReceipt, WriteReceipt};

use crate::cli::{KgArgs, KgCommand, TeamsArgs, TeamsCommand, VaultArgs, VaultCommand, Verb};
use crate::context::AppContext;
use crate::output::{ellipsize, human_age, human_count};
use crate::provider::{self, Tools};
use crate::session::SessionStore;
use crate::tui::app::{
    App, EntryRole, HELP, Home, Parsed, PendingShell, combined_search_answer, display_path,
    parse_line,
};
/// What a running turn reports back to the screen.
enum TurnEvent {
    /// One tool call the model is about to make (or made).
    Note(String),
    /// A write wants permission. The worker blocks until `reply` answers.
    Ask {
        /// What the change would do, and to what.
        request: provider::ApprovalRequest,
        /// `true` to do it, `false` to refuse.
        reply: mpsc::Sender<bool>,
    },
    /// The turn finished, successfully or not.
    Done(Box<anyhow::Result<(provider::Answer, provider::Toolbox<'static>)>>),
}

/// A model turn that is currently in flight.
struct Busy {
    rx: Receiver<TurnEvent>,
    /// The line that asked, recorded when the answer lands.
    prompt: String,
    /// The model that answered; the session file records it.
    model: String,
    /// The rates that turn is billed at, for the session's cost estimate.
    pricing: Option<provider::Pricing>,
}

/// A confirmation the person has not answered yet.
struct Pending {
    reply: mpsc::Sender<bool>,
    title: String,
}

/// Ask the person, from the worker thread, and wait for the answer.
struct AskOnScreen {
    tx: mpsc::Sender<TurnEvent>,
}

impl provider::Approver for AskOnScreen {
    fn approve(&mut self, request: &provider::ApprovalRequest) -> bool {
        let (reply, answer) = mpsc::channel();
        if self
            .tx
            .send(TurnEvent::Ask {
                request: request.clone(),
                reply,
            })
            .is_err()
        {
            return false;
        }
        // A dropped channel (the screen went away) reads as a refusal, never as
        // a yes: silence is not consent.
        answer.recv().unwrap_or(false)
    }
}

/// Open the session screen and run it until the user leaves.
///
/// `name` is the session file's name (`session` by default, or what `--resume`
/// named). Everything the screen shows is resolved before the terminal changes
/// state, so a failure here is an ordinary error message rather than a broken
/// terminal.
pub fn run(ctx: &AppContext, name: &str) -> Result<()> {
    let store = SessionStore::new(ctx.sessions_dir.clone());
    let (mut session, resumed) = store.load_or_create(name)?;

    // The startup screen's facts are read before the terminal changes state:
    // a store that fails to open is a message here, not a half-drawn frame
    // after raw mode has already been entered.
    let (graph, indexed) = graph_state(ctx);
    let mut app = App::new(name, resumed);
    app.config = ctx.config.clone();
    app.config_path = ctx.config_path.clone();
    app.set_paths(display_path(&ctx.root), display_path(&ctx.vault_path));
    app.seed(&session);
    app.home = Some(home(ctx, name, resumed, &graph, indexed));
    app.graph = graph;
    app.context = context_pane(ctx, &session);

    enable_raw_mode().context("cannot put the terminal in raw mode")?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(error).context("cannot enter the alternate screen");
    }
    let guard = RestoreTerminal;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("cannot open the terminal")?;
    let _ = terminal.clear();

    let mut busy: Option<Busy> = None;
    let mut approval: Option<Pending> = None;
    let mut finished = false;
    while !app.quit {
        // the answer is applied here, on the UI thread, so the session file has
        // exactly one writer.
        if let Some(current) = busy.take() {
            let mut running = true;
            finished = false;
            loop {
                match current.rx.try_recv() {
                    Ok(TurnEvent::Note(note)) => {
                        app.push(EntryRole::Info, format!("  · {note}"));
                    }
                    Ok(TurnEvent::Ask { request, reply }) => {
                        app.approval = Some(format!(
                            "{}\n\n{}\n\ny approve · n reject · ctrl-c quit",
                            request.title, request.body
                        ));
                        approval = Some(Pending {
                            reply,
                            title: request.title,
                        });
                    }
                    Ok(TurnEvent::Done(result)) => {
                        running = false;
                        finished = true;
                        app.busy = false;
                        app.approval = None;
                        approval = None;
                        settle(&mut app, &store, &mut session, &current, *result);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        running = false;
                        finished = true;
                        app.busy = false;
                        app.approval = None;
                        approval = None;
                        app.push(
                            EntryRole::Error,
                            "the model turn stopped without an answer".to_string(),
                        );
                        break;
                    }
                }
            }
            if running {
                busy = Some(current);
            }
            if finished {
                // Tokens and cost moved, so the pane is rebuilt from the
                // session that just wrote them.
                refresh(ctx, &mut app, name, resumed, &session);
            }
        }

        terminal
            .draw(|frame| {
                ui::draw(frame, &mut app);
            })
            .context("cannot draw the session")?;

        if event::poll(Duration::from_millis(150)).context("cannot poll the keyboard")? {
            match event::read().context("cannot read the keyboard")? {
                // Windows reports press and release; acting on both would type
                // every character twice.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(pending) = app.pending_shell.take() {
                        let decision = match key.code {
                            KeyCode::Char('y' | 'Y') | KeyCode::Enter => true,
                            KeyCode::Char('n' | 'N') | KeyCode::Esc => false,
                            _ => {
                                // Put it back if not a confirmation key
                                app.pending_shell = Some(pending);
                                false
                            }
                        };
                        if decision {
                            execute_shell_command(app, &pending.command);
                            app.push(EntryRole::Info, "approved".to_string());
                        } else {
                            app.push(EntryRole::Info, "declined".to_string());
                        }
                    } else if let Some(pending) = approval.take() {
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        let decision = match key.code {
                            KeyCode::Char('y' | 'Y') | KeyCode::Enter => Some(true),
                            KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
                            KeyCode::Char('c') if ctrl => {
                                app.quit = true;
                                None
                            }
                            _ => None,
                        };
                        match decision {
                            Some(answer) => {
                                let _ = pending.reply.send(answer);
                                app.push(
                                    EntryRole::Info,
                                    format!(
                                        "{} · {}",
                                        if answer { "approved" } else { "declined" },
                                        pending.title
                                    ),
                                );
                                app.approval = None;
                            }
                            None => approval = Some(pending),
                        }
                    } else {
                        // First let overlays handle the key
                        let action = app.overlay_stack.handle_key(key, &mut app);
                        match action {
                            crate::tui::overlays::OverlayAction::Close => {
                                app.overlay_stack.pop();
                            }
                            crate::tui::overlays::OverlayAction::CloseAll => {
                                while app.overlay_stack.pop().is_some() {}
                            }
                            crate::tui::overlays::OverlayAction::Push(kind) => {
                                app.overlay_stack.push(crate::tui::overlays::create_overlay(kind));
                            }
                            crate::tui::overlays::OverlayAction::Replace(kind) => {
                                app.overlay_stack.pop();
                                app.overlay_stack.push(crate::tui::overlays::create_overlay(kind));
                            }
                            crate::tui::overlays::OverlayAction::None => {
                                // If no overlay handled it, pass to main handler
                                handle_key(&mut app, key, &ctx.root);
                            }
                        }
                    }
                }
            }
        }
        if app.pending_submit {
            app.pending_submit = false;
            if busy.is_some() {
                // Queue the command instead of rejecting it
                let line = app.take_input();
                if !line.is_empty() {
                    app.command_queue.push_back(line);
                    app.push(EntryRole::Info, format!("queued: {line}"));
                }
            } else {
                busy = submit(&mut app, ctx, &store, &mut session);
                refresh(ctx, &mut app, name, resumed, &session);
            }
        }

        // Process command queue when turn completes
        if finished && !app.command_queue.is_empty() {
            if let Some(queued_line) = app.command_queue.pop_front() {
                app.input = queued_line;
                app.pending_submit = true;
            }
        }
    }

    drop(terminal);
    drop(guard);
    Ok(())
}

/// Re-read what the frame shows after work has happened: a `/kg index` or a
/// `/vault init` must be visible on the startup screen the next time it is
/// shown, not only in the header.
fn refresh(ctx: &AppContext, app: &mut App, name: &str, resumed: bool, session: &Session) {
    let (graph, indexed) = graph_state(ctx);
    app.home = Some(home(ctx, name, resumed, &graph, indexed));
    app.graph = graph;
    app.context = context_pane(ctx, session);
}

/// Leave raw mode and the alternate screen, on every path.
struct RestoreTerminal;

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
        let _ = out.flush();
    }
}

fn handle_key(app: &mut App, key: KeyEvent, workspace_root: &Path) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::Char('c') if ctrl => app.quit = true,
        KeyCode::Char('d') if ctrl => app.quit = true,
        KeyCode::Char('l') if ctrl => app.clear(),
        KeyCode::Char('a') if ctrl => app.move_home(),
        KeyCode::Char('e') if ctrl => app.move_end(),
        // Ctrl+K or Ctrl+P opens command palette (like VS Code / lazygit)
        KeyCode::Char('k') if ctrl => {
            app.overlay_stack.push(crate::tui::overlays::create_overlay(crate::tui::overlays::OverlayKind::Palette));
        }
        KeyCode::Char('p') if ctrl => {
            app.overlay_stack.push(crate::tui::overlays::create_overlay(crate::tui::overlays::OverlayKind::Palette));
        }
        // '/' key opens command palette when input is empty (not typing a command)
        KeyCode::Char('/') if app.input.trim().is_empty() && !app.busy => {
            app.overlay_stack.push(crate::tui::overlays::create_overlay(crate::tui::overlays::OverlayKind::Palette));
        }
        KeyCode::Tab => {
            // @file fuzzy completion
            if app.input.contains('@') && !app.busy {
                app.complete_path(workspace_root);
            }
        }
        KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            app.insert_char(ch)
        }
        KeyCode::Enter => {
            if !app.input.trim().is_empty() {
                app.pending_submit = true;
            }
        }
        KeyCode::Esc => {
            app.cancel_completion();
            app.input.clear();
            app.cursor = 0;
        }
        KeyCode::Backspace => {
            app.cancel_completion();
            app.backspace();
        }
        KeyCode::Delete => {
            app.cancel_completion();
            app.delete();
        }
        KeyCode::Left => {
            app.cancel_completion();
            app.move_left();
        }
        KeyCode::Right => {
            app.cancel_completion();
            app.move_right();
        }
        KeyCode::Home => {
            app.cancel_completion();
            app.move_home();
        }
        // End is "newest output", which is the key people reach for while a
        // transcript is running; ctrl-e is end-of-line.
        KeyCode::End => app.to_bottom(),
        KeyCode::Up => {
            app.cancel_completion();
            app.history_prev();
        }
        KeyCode::Down => {
            app.cancel_completion();
            app.history_next();
        }
        KeyCode::PageUp => app.page_up(),
        KeyCode::PageDown => app.page_down(),
        _ => {}
    }
}
/// Run one submitted line, returning a turn that is now in flight.
fn submit(
    app: &mut App,
    ctx: &AppContext,
    store: &SessionStore,
    session: &mut Session,
) -> Option<Busy> {
    let line = app.take_input();
    if line.is_empty() {
        return None;
    }
    app.remember_history(&line);

    match parse_line(&line) {
        Parsed::Empty => None,
        Parsed::Quit => {
            app.quit = true;
            None
        }
        Parsed::Clear => {
            app.clear();
            None
        }
        Parsed::Sessions => {
            let text = sessions_text(store);
            app.push(EntryRole::Info, text);
            None
        }
        Parsed::Run(verb) => {
            run_verb(app, ctx, store, session, &line, &verb);
            None
        }
        Parsed::Ask(words) => ask(app, ctx, store, session, &line, &words),
        Parsed::Search(words) => {
            local_search(app, ctx, store, session, &line, &words);
            None
        }
        Parsed::Shell(cmd) => {
            run_shell(app, ctx, &cmd);
            None
        }
        Parsed::Init => {
            run_init(app, ctx);
            None
        }
        Parsed::Memory => {
            run_memory(app, ctx);
            None
        }
        Parsed::FeedbackOverlay => {
            app.overlays.push(crate::tui::overlays::create_overlay(
                crate::tui::overlays::OverlayKind::Feedback,
            ));
            None
        }
        Parsed::ConnectOverlay => {
            app.overlays.push(crate::tui::overlays::create_overlay(
                crate::tui::overlays::OverlayKind::Connect,
            ));
            None
        }
        Parsed::Unknown(message) => {
            app.push(EntryRole::Error, message);
        }
    }
}
fn run_verb(
    app: &mut App,
    ctx: &AppContext,
    store: &SessionStore,
    session: &mut Session,
    prompt: &str,
    verb: &Verb,
) {
    // Handle theme and screen reader directly without command execution
    match verb {
        Verb::SetTheme(theme) => {
            app.theme = *theme;
            app.push(EntryRole::Info, format!("Theme set to {:?}", theme));
            return;
        }
        Verb::SetScreenReader(mode) => {
            app.screen_reader = *mode;
            app.push(EntryRole::Info, format!("Screen reader mode set to {:?}", mode));
            return;
        }
        _ => {}
    }

    app.push(EntryRole::User, prompt);
    match crate::commands::execute(ctx, verb) {
        Ok(outcome) => {
            app.push(EntryRole::Answer, outcome.text.clone());
            persist(app, store, session, prompt, &outcome.text);
        }
        Err(error) => {
            let text = format!("{error:#}");
            app.push(EntryRole::Error, text.clone());
            persist(app, store, session, prompt, &format!("error: {text}"));
        }
    }
}

/// A plain question: a model turn when one is configured, the two stores when
/// one is not.
fn ask(
    app: &mut App,
    ctx: &AppContext,
    store: &SessionStore,
    session: &mut Session,
    prompt: &str,
    words: &[String],
) -> Option<Busy> {
    match provider::Settings::load(&ctx.config) {
        Ok(settings) if settings.ready() => start_turn(app, ctx, session, prompt, settings),
        Ok(settings) => {
            app.push(
                EntryRole::Info,
                format!(
                    "no model key ({}): answering from the vault and the code graph.\n  \
                     export it, then ask again — /model shows what this session would use.",
                    settings.key_hint()
                ),
            );
            local_search(app, ctx, store, session, prompt, words);
            None
        }
        Err(error) => {
            // A broken provider config must be visible, and must not cost the
            // person the answer the local stores can give right now.
            app.push(
                EntryRole::Error,
                format!("model config: {error:#}\n  answering from the stores instead"),
            );
            local_search(app, ctx, store, session, prompt, words);
            None
        }
    }
}

/// Start a model turn on a worker thread.
///
/// The worker owns a clone of the context (paths only), the settings and the
/// transcript excerpt; it never touches `Session`, the session file or the
/// terminal. That is what keeps the UI responsive and the session file
/// single-writer.
fn start_turn(
    app: &mut App,
    ctx: &AppContext,
    session: &Session,
    prompt: &str,
    settings: provider::Settings,
) -> Option<Busy> {
    app.push(EntryRole::User, prompt);
    app.push(
        EntryRole::Info,
        format!("{} · {} · thinking…", settings.kind.name(), settings.model),
    );
    app.busy = true;

    // A window of the conversation, not all of it: eight exchanges are enough
    // for follow-ups, and a resume of a long session must not hammer the model
    // with the whole file.
    let mut history: Vec<provider::Exchange> = session
        .messages
        .iter()
        .filter(|message| matches!(message.role, Role::User | Role::Assistant))
        .map(|message| provider::Exchange {
            role: message.role,
            text: message.content.clone(),
        })
        .collect();
    history.reverse();
    history.truncate(8);
    history.reverse();

    let model = settings.model.clone();
    let pricing = settings.pricing;
    let asked = prompt.to_string();
    let asked_on_worker = asked.clone();
    let root = ctx.root.clone();
    let worker_ctx = ctx.clone();
    let (tx, rx) = mpsc::channel();
    let asker = tx.clone();

    std::thread::spawn(move || {
        // Every write this turn attempts comes back through `asker` and stops
        // here until the screen answers.
        let turn_id = session
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count();
        let mut tools = provider::Toolbox::new(&worker_ctx)
            .with_approver(Box::new(AskOnScreen { tx: asker }))
            .with_turn_id(turn_id);
        let notes = tx.clone();
        let mut on_note = |line: &str| {
            let _ = notes.send(TurnEvent::Note(line.to_string()));
        };
        let result = provider::run_turn(
            &settings,
            &root,
            &history,
            &asked_on_worker,
            &mut tools,
            &mut on_note,
        );
        let _ = tx.send(TurnEvent::Done(Box::new((result, tools))));
    });

    Some(Busy {
        rx,
        prompt: asked,
        model,
        pricing,
    })
}

/// Apply a finished turn: the answer, the accounting, and the session file.
fn settle(
    app: &mut App,
    store: &SessionStore,
    session: &mut Session,
    busy: &Busy,
    result: anyhow::Result<(provider::Answer, provider::Toolbox<'static>)>,
) {
    match result {
        Ok((answer, mut tools)) => {
            app.push(EntryRole::Answer, answer.text.clone());
            // The file records the model that actually answered, so a resumed
            // session does not claim a model that never ran.
            session.model = busy.model.clone();
            let (input, output) = answer.usage();
            session.account(input, output);

            // Add checkpoint for this turn
            let turn_id = session
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count();
            session.add_checkpoint(turn_id, format!("turn {}", turn_id));

            // Record write receipts from the turn
            let writes = tools.take_writes();
            for write in writes {
                session.record_write(write);
            }

            // One receipt per turn is also what makes the pane's cost honest:
            // `Session::spend` sums priced receipts and counts the unpriced ones
            // separately, rather than treating an unknown price as free.
            session.record_receipt(TurnReceipt {
                at_ms: now_ms(),
                total_tokens: input.saturating_add(output) as usize,
                // This build assembles no layered context, so there is no
                // prefix to fingerprint and no pieces to account for.
                prefix_hash: String::new(),
                layer_tokens: [0; 7],
                pieces: 0,
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_micros: busy
                    .pricing
                    .map(|pricing| pricing.cost_micros(input, output)),
                turn_id: Some(turn_id),
            });
            persist(app, store, session, &busy.prompt, &answer.text);
        }
        Err(error) => {
            app.push(EntryRole::Error, format!("{error:#}"));
            // The question is kept and the answer is empty: a failed turn has to
            // be visible on resume, not silently dropped.
            persist(app, store, session, &busy.prompt, "");
        }
    }
}
/// `/search`: the local answer, even when a model is configured.
fn local_search(
    app: &mut App,
    ctx: &AppContext,
    store: &SessionStore,
    session: &mut Session,
    prompt: &str,
    words: &[String],
) {
    app.push(EntryRole::User, prompt);

    let vault = crate::commands::execute(
        ctx,
        &Verb::Vault(VaultArgs {
            action: VaultCommand::Recall {
                query: words.to_vec(),
                limit: 8,
                at: None,
            },
        }),
    )
    .map(|outcome| outcome.data)
    .map_err(|error| format!("{error:#}"));

    let graph = crate::commands::execute(
        ctx,
        &Verb::Kg(KgArgs {
            action: KgCommand::Search {
                query: words.to_vec(),
                limit: 8,
            },
        }),
    )
    .map(|outcome| outcome.data)
    .map_err(|error| format!("{error:#}"));

    let answer = combined_search_answer(&vault, &graph);
    app.push(EntryRole::Answer, answer.clone());
    persist(app, store, session, prompt, &answer);
}

/// Write the exchange, and report a failed write without losing the answer that
/// was already on screen.
fn persist(app: &mut App, store: &SessionStore, session: &mut Session, prompt: &str, answer: &str) {
    if let Err(error) = store.record(session, prompt, answer) {
        app.push(
            EntryRole::Error,
            format!("the answer above was not saved to the session: {error:#}"),
        );
    }
}

/// The startup screen. It names paths, not labels: the first support question
/// is always "which vault did it open" — and it puts the thing this workspace
/// is still missing at the top of `start here`, so the screen is a next step
/// rather than a splash.
fn home(ctx: &AppContext, name: &str, resumed: bool, graph: &str, indexed: bool) -> Home {
    let vault_ready = ctx.vault_path.exists();
    let (model, model_note, model_ready) = model_state(ctx);

    let mut steps: Vec<(String, String)> = Vec::new();
    if !vault_ready {
        steps.push((
            "/vault init".to_string(),
            "create the temporal vault for this machine".to_string(),
        ));
    }
    if !indexed {
        steps.push((
            "/kg index".to_string(),
            "build the code graph from this workspace".to_string(),
        ));
    }
    if !model_ready {
        steps.push((
            "/model".to_string(),
            "wire a model so questions run as turns".to_string(),
        ));
    }
    steps.push((
        "ask anything".to_string(),
        "a turn with the vault and the graph as tools".to_string(),
    ));
    steps.push((
        "/search".to_string(),
        "answer from the two stores, no model needed".to_string(),
    ));

    Home {
        version: env!("CARGO_PKG_VERSION").to_string(),
        session: format!(
            "session \"{name}\" ({})",
            if resumed { "resumed" } else { "new" }
        ),
        workspace: display_path(&ctx.root),
        vault: display_path(&ctx.vault_path),
        vault_note: if vault_ready {
            "ready".to_string()
        } else {
            "not initialised".to_string()
        },
        graph: graph.to_string(),
        model,
        model_note,
        steps,
    }
}

/// The model row, and whether a turn can actually run: the same settings
/// `doctor` prints, reduced to what fits on one line.
fn model_state(ctx: &AppContext) -> (String, String, bool) {
    match provider::Settings::load(&ctx.config) {
        Ok(settings) if settings.ready() => (settings.describe(), "ready".to_string(), true),
        Ok(settings) => (
            settings.model.clone(),
            format!("no key — export {}", settings.key_hint()),
            false,
        ),
        Err(_) => (
            "not configured".to_string(),
            "config unreadable (/model)".to_string(),
            false,
        ),
    }
}

/// `/model`: what a turn would use, and the exact config that changes it.
fn model_text(ctx: &AppContext) -> String {
    match provider::Settings::load(&ctx.config) {
        Ok(settings) => {
            let mut out = String::from("model\n");
            out.push_str(&format!("  provider   {}\n", settings.kind.name()));
            out.push_str(&format!("  model      {}\n", settings.model));
            out.push_str(&format!("  endpoint   {}\n", settings.base_url));
            out.push_str(&format!("  max tokens {}\n", settings.max_tokens));
            match settings.pricing {
                Some(pricing) => out.push_str(&format!(
                    "  rates      ${:.2} in / ${:.2} out per Mtok (estimate)\n",
                    pricing.input_per_mtok, pricing.output_per_mtok
                )),
                None => out.push_str("  rates      unset — cost will be reported as unknown\n"),
            }
            out.push_str(&format!(
                "  key        {}\n",
                if settings.ready() {
                    "set".to_string()
                } else {
                    format!("missing — export {}", settings.key_hint())
                }
            ));
            out.push_str(&format!(
                "\nconfigure in {}\n\n\
                 [provider]\nkind = \"{}\"\nmodel = \"{}\"\n# input_per_mtok = 3.0\n# output_per_mtok = 15.0\n\n\
                 the key is an environment variable and is never written here:\n  \
                 export MEMNOTH_API_KEY=…\n\n\
                 a gateway or a local server is `provider.base_url` (OpenAI-compatible).",
                ctx.config_path.display(),
                settings.kind.name(),
                settings.model,
            ));
            out
        }
        Err(error) => format!("model: misconfigured — {error:#}"),
    }
}

/// One line about the graph, taken from the same `kg status` JSON `--json`
/// prints, plus whether there is a graph at all.
fn graph_state(ctx: &AppContext) -> (String, bool) {
    let verb = Verb::Kg(KgArgs {
        action: KgCommand::Status,
    });
    match crate::commands::execute(ctx, &verb) {
        Ok(outcome) if outcome.data["indexed"].as_bool().unwrap_or(false) => (
            format!(
                "{} files · {} symbols · {} stale",
                outcome.data["files"].as_u64().unwrap_or(0),
                outcome.data["symbols"].as_u64().unwrap_or(0),
                outcome.data["stale_files"].as_u64().unwrap_or(0),
            ),
            true,
        ),
        Ok(_) => ("not indexed".to_string(), false),
        Err(_) => ("unavailable".to_string(), false),
    }
}

/// The context pane: what the vault believes, and who else is in the room.
///
/// Every read is allowed to fail independently; a missing vault must not blank
/// the coordination section, and a stale coordination row must not hide the
/// vault's decisions.
fn context_pane(ctx: &AppContext, session: &Session) -> String {
    let mut out = String::new();

    out.push_str("vault\n");
    match crate::vault::open_existing(ctx) {
        Ok(engine) => {
            match engine.stats() {
                Ok(stats) => out.push_str(&format!(
                    "  {} entities\n  {} facts\n",
                    stats.entities_current, stats.observations_total
                )),
                Err(error) => out.push_str(&format!("  unreadable ({error})\n")),
            }
            match engine.brain_feed() {
                Ok(feed) => {
                    if !feed.decisions.is_empty() {
                        out.push_str("\ndecisions\n");
                        for decision in feed.decisions.iter().take(3) {
                            out.push_str(&format!(
                                "  • {}\n",
                                ellipsize(decision.title.trim(), 22)
                            ));
                        }
                    }
                    if !feed.lessons.is_empty() {
                        out.push_str("\nlessons\n");
                        for lesson in feed.lessons.iter().take(3) {
                            out.push_str(&format!("  • {}\n", ellipsize(lesson.text.trim(), 22)));
                        }
                    }
                    if feed.decisions.is_empty() && feed.lessons.is_empty() {
                        out.push_str("\nno decisions yet\n  /vault remember <name> <fact…>\n");
                    }
                }
                Err(error) => out.push_str(&format!("\nbrain unavailable ({error})\n")),
            }
        }
        Err(_) => out.push_str("  not initialised\n  /vault init\n"),
    }

    // Coordination lives in the same database; opening it is skipped when the
    // vault is absent so a session never creates one as a side effect.
    if ctx.vault_path.exists() {
        out.push_str("\ncoordination\n");
        match memnoth_coord::Coord::open(&ctx.vault_path) {
            Ok(coord) => match coord.status() {
                Ok(rows) => {
                    let active: Vec<_> = rows.iter().filter(|row| !row.stale).collect();
                    let claims: usize = active.iter().map(|row| row.active_claims.len()).sum();
                    if active.is_empty() {
                        out.push_str("  no sessions\n");
                    } else {
                        out.push_str(&format!(
                            "  {} session(s), {claims} claim(s)\n",
                            active.len()
                        ));
                        for row in active.iter().take(3) {
                            out.push_str(&format!("  • {}\n", ellipsize(row.name.trim(), 22)));
                        }
                    }
                }
                Err(error) => out.push_str(&format!("  unavailable ({error})\n")),
            },
            Err(error) => out.push_str(&format!("  unavailable ({error})\n")),
        }
    }

    out.push_str("\nusage\n");
    let model = match provider::Settings::load(&ctx.config) {
        Ok(settings) if settings.ready() => settings.model,
        _ => "local only".to_string(),
    };
    out.push_str(&format!("  {}\n", ellipsize(&model, 24)));
    if session.receipts.is_empty() {
        out.push_str("  no model turns yet\n");
    } else {
        out.push_str(&format!(
            "  {} turn(s) · {} in · {} out\n",
            session.receipts.len(),
            human_count(session.input_tokens.min(usize::MAX as u64) as usize),
            human_count(session.output_tokens.min(usize::MAX as u64) as usize),
        ));
        let (micros, priced) = session.spend();
        if priced > 0 {
            // Labelled an estimate because the rate is a list price or a
            // configured one, not an invoice: cache reads and provider
            // rounding are not modelled here.
            out.push_str(&format!(
                "  ~${:.2} (estimate)\n",
                micros as f64 / 1_000_000.0
            ));
        } else {
            out.push_str("  cost unknown — set provider rates\n");
        }
    }

    out
}

/// `/sessions`: what is on disk and how to come back to it.
fn sessions_text(store: &SessionStore) -> String {
    let rows = store.list();
    if rows.is_empty() {
        return "No sessions yet. This one is created when you ask something.".to_string();
    }
    let mut out = String::from("sessions\n");
    for row in rows.iter().take(10) {
        out.push_str(&format!(
            "  {:<22} {:>4} msgs  {}\n",
            ellipsize(&row.name, 22),
            row.messages,
            human_age(row.created_ms, now_ms()),
        ));
    }
    out.push_str("\nresume with `memnoth --resume <name>`");
    out
}

/// Execute a shell command with sandbox policy checking.
fn run_shell(app: &mut App, ctx: &AppContext, cmd: &str) {
    app.push(EntryRole::User, format!("!{}", cmd));

    // Load sandbox config and check policy
    let config = load_sandbox_config(&ctx.root);
    let profile = config
        .resolve("workspace")
        .unwrap_or_else(|_| SandboxProfile::default());
    let rule_set = RuleSet::with_user_rules(profile.rules);
    let decision = decide(&rule_set, "Bash", cmd);

    match decision {
        RuleDecision::Allow { .. } => {
            execute_shell_command(app, cmd);
        }
        RuleDecision::Deny { reason, rule } => {
            app.push(
                EntryRole::Error,
                format!("denied: {} (rule: {})", reason, rule),
            );
        }
        RuleDecision::Ask {
            reason,
            rule,
            destructive,
            kind,
        } => {
            // Store for confirmation
            app.pending_shell = Some(PendingShell {
                command: cmd.to_string(),
                decision: RuleDecision::Ask {
                    reason: reason.clone(),
                    rule: rule.clone(),
                    destructive,
                    kind,
                },
            });
            app.push(
                EntryRole::Info,
                format!("⚠ confirm: {} (y/n) [rule: {}]", reason, rule),
            );
            if destructive {
                app.push(
                    EntryRole::Info,
                    "⚠ This is a potentially destructive operation".to_string(),
                );
            }
        }
    }
}

/// Actually execute the shell command and capture output.
fn execute_shell_command(app: &mut App, cmd: &str) {
    use std::process::Command;

    let output = if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", cmd]).output()
    } else {
        Command::new("sh").args(["-c", cmd]).output()
    };

    match output {
        Ok(result) => {
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);
            if !stdout.is_empty() {
                app.push(EntryRole::Answer, stdout.to_string());
            }
            if !stderr.is_empty() {
                app.push(EntryRole::Error, stderr.to_string());
            }
            if result.status.success() {
                app.push(
                    EntryRole::Info,
                    format!("exit code: {}", result.status.code().unwrap_or(0)),
                );
            } else {
                app.push(
                    EntryRole::Error,
                    format!("exit code: {}", result.status.code().unwrap_or(-1)),
                );
            }
        }
        Err(e) => {
            app.push(
                EntryRole::Error,
                format!("failed to execute command: {}", e),
            );
        }
    }
}

/// Generate AGENTS.md file.
fn run_init(app: &mut App, ctx: &AppContext) {
    app.push(EntryRole::User, "/init".to_string());

    let agents_path = ctx.root.join("AGENTS.md");
    let content = format!(
        "# Agent Instructions for {}\n\nThis file was generated by `memnoth /init`.\n\n## Workspace\n- Root: {}\n- Vault: {}\n\n## Commands\n- `/vault init` — Create the temporal vault\n- `/kg index` — Build the code graph\n- `/model` — Configure a model for turns\n- `!<cmd>` — Run shell commands (sandbox policy applies)\n\n## Notes\nAdd project-specific instructions here for AI agents.\n",
        ctx.root.file_name().unwrap_or_default().to_string_lossy(),
        display_path(&ctx.root),
        display_path(&ctx.vault_path)
    );

    match std::fs::write(&agents_path, content) {
        Ok(_) => {
            app.push(
                EntryRole::Answer,
                format!("Created {}", display_path(&agents_path)),
            );
        }
        Err(e) => {
            app.push(
                EntryRole::Error,
                format!("Failed to create AGENTS.md: {}", e),
            );
        }
    }
}

/// Edit project memory (vault).
fn run_memory(app: &mut App, ctx: &AppContext) {
    app.push(EntryRole::User, "/memory".to_string());
    app.push(
        EntryRole::Info,
        "Project memory editing not yet implemented. Use `/vault remember` to record facts."
            .to_string(),
    );
}

/// `/sessions`: what is on disk and how to come back to it.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Config;

    fn context(root: &std::path::Path) -> AppContext {
        AppContext {
            root: root.to_path_buf(),
            home: root.join(".memnoth"),
            vault_path: root.join(".memnoth").join("vault.db"),
            namespace: "default".into(),
            graph_path: root.join(".memnoth").join("graph.db"),
            sessions_dir: root.join("sessions"),
            config_path: root.join("config.toml"),
            config: Config::default(),
            json: false,
            quiet: true,
        }
    }

    fn receipt(input: u64, output: u64, cost: Option<u64>) -> TurnReceipt {
        TurnReceipt {
            at_ms: 1,
            total_tokens: input.saturating_add(output) as usize,
            prefix_hash: String::new(),
            layer_tokens: [0; 4],
            pieces: 0,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_micros: cost,
        }
    }

    #[test]
    fn the_context_pane_reports_tokens_and_the_estimated_cost() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let mut session = Session::new("claude-sonnet-4-5");
        assert!(
            context_pane(&ctx, &session).contains("no model turns yet"),
            "a fresh session says so instead of showing zeroes"
        );

        // 1.2M in at $3/Mtok plus 0.4M out at $15/Mtok is $9.60.
        session.account(1_200_000, 400_000);
        session.record_receipt(receipt(1_200_000, 400_000, Some(9_600_000)));

        let pane = context_pane(&ctx, &session);
        assert!(pane.contains("1 turn(s)"), "{pane}");
        assert!(pane.contains("~$9.60"), "{pane}");
        assert!(pane.contains("estimate"), "{pane}");
    }

    #[test]
    fn an_unpriced_turn_is_reported_as_unknown_rather_than_free() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let mut session = Session::new("some-local-model");
        session.account(10, 5);
        session.record_receipt(receipt(10, 5, None));

        let pane = context_pane(&ctx, &session);
        assert!(pane.contains("cost unknown"), "{pane}");
        assert!(!pane.contains("$0.00"), "{pane}");
    }
}
