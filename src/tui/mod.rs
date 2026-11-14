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
pub mod agents;
pub mod app;
pub mod commands;
pub mod home;
pub mod jobs;
pub mod keymap;
pub mod login;
pub mod markdown;
pub mod overlays;
pub mod panes;
pub mod rail;
pub mod slash;
pub mod surface;
pub mod theme;
pub mod ui;
use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
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
use memnoth_sandbox::profiles::{ProfileName, load_sandbox_config};
use memnoth_sandbox::rules::{RuleDecision, RuleSet, decide};
use memnoth_session::{Role, Session, TurnReceipt, WriteReceipt};

use crate::cli::{KgArgs, KgCommand, VaultArgs, VaultCommand, Verb};
use crate::context::AppContext;
use crate::output::{ellipsize, human_age, human_count};
use crate::provider::{self, Tools};
use crate::session::{SessionStore, validate_name};
use crate::tui::app::VerbDone;
use crate::tui::app::{
    App, Attention, EntryRole, Fact, Home, HomeState, Parsed, PendingShell, SessionSwitch, Step,
    TasksCommand, combined_search_answer, display_path, parse_line,
};
/// How far one wheel notch moves the transcript.
///
/// Three lines is a third of a mouse notch and a comfortable touchpad flick:
/// one event never jumps a screen, and a long answer still scrolls quickly.
const WHEEL_LINES: usize = 3;

/// What a finished turn produced: the answer, the writes, and the approvals
/// that authorised them. One alias so the event and `settle` state the same
/// shape rather than repeating a three-part tuple at both ends.
type TurnOutcome = anyhow::Result<(
    provider::Answer,
    Vec<WriteReceipt>,
    Vec<memnoth_session::ApprovalRecord>,
)>;

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
    /// The turn finished, successfully or not, with what it wrote.
    Done(Box<TurnOutcome>),
    /// One lifecycle event from the turn, already in the harness API's shape.
    /// The screen does not need it today; the socket harness and the benchmark
    /// runner are the consumers, and they must see the same stream.
    Api(memnoth_harness_api::ApiEvent),
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
///
/// Every launch lands on the brief whatever the file holds, `--resume NAME`
/// included: starting a tool should not mean being dropped into the middle of a
/// conversation from last Tuesday. The conversation is still loaded — the next
/// word typed continues it — and it is one `esc` away instead of being the first
/// thing on screen.
pub fn run(ctx: &AppContext, name: &str) -> Result<()> {
    let store = SessionStore::new(ctx.sessions_dir.clone());
    let (mut session, resumed) = store.load_or_create(name)?;

    // The startup screen's facts are read before the terminal changes state:
    // a store that fails to open is a message here, not a half-drawn frame
    // after raw mode has already been entered.
    let graph = graph_state(ctx);
    let mut app = App::new(name, resumed);
    app.config = ctx.config.clone();
    app.config_path = ctx.config_path.clone();
    // The four-move card is shown once. Showing it is not dismissing it: the
    // flag is written by the overlay on skip or completion, so a crash before
    // either brings the card back. `/onboarding` replays it without writing.
    let onboarded = app
        .config
        .ui
        .as_ref()
        .and_then(|ui| ui.onboarded)
        .unwrap_or(false);
    let first_run = !onboarded;
    // Overlays that talk to the cloud (`/teams members`, `/keys create`,
    // `/org invite`) need the invocation's paths and credentials, and the
    // overlay trait only ever sees the app.
    app.ctx = Some(ctx.clone());
    // The Deck and the Trace read this tree; child logs live beside the
    // session files, under the same home.
    app.delegator = Some(memnoth_agent::Delegator::new(ctx.home.join("agents")));
    // First run: the capability card sits above the brief. `present` calls
    // `on_open`, which reads this machine's real state for the four rows.
    if first_run {
        crate::tui::overlays::present(
            &mut app,
            Box::new(crate::tui::overlays::OnboardingOverlay::first_run()),
        );
    }
    // The ranking and the run counts are what this machine actually does, so
    // they outlive the terminal: `/` after a restart already leads with the
    // commands the person uses. A missing or unreadable file reads as no
    // history, never as a failure to start.
    app.slash
        .adopt(crate::tui::slash::load_usage(&store.usage_path()));

    // A preference written by `mn set-theme` or `mn set-screen-reader` shapes
    // the next session; `/theme` still overrides it live.
    if let Some(ui) = &ctx.config.ui {
        if let Some(theme) = ui.theme.as_deref() {
            app.theme = crate::tui::app::Theme::parse(theme).unwrap_or_default();
        }
        if let Some(mode) = ui.screen_reader.as_deref() {
            app.screen_reader = crate::tui::app::ScreenReaderMode::parse(mode).unwrap_or_default();
        }
    }
    // `NO_COLOR` and `TERM=dumb` are requests about *this* terminal, so they
    // outrank a stored preference: a theme written on a desktop machine must not
    // follow a person into a pipe or a screen reader.
    if let Some(theme) = crate::tui::theme::theme_from_env() {
        app.theme = theme;
    }
    app.set_paths(display_path(&ctx.root), display_path(&ctx.vault_path));
    app.seed(&session);
    // The brief is what a launch shows, over whatever the file held. Splitting
    // the two — history loaded, brief in front — is what makes `mn` open the
    // same way on a fresh machine and on the fortieth day of a session, while
    // still leaving `esc` with something to show.
    app.home_visible = true;
    app.home = Some(home(ctx, name, resumed, &graph, &session, first_run));
    app.graph_attention = graph.attention.clone();
    app.context = context_pane(ctx, &session);
    // Read before the first frame: the account line is part of what the screen
    // opened with, not something that appears a redraw later.
    app.account = crate::auth::signed_in_account();
    // Where sessions are filed, named on the context rail. Resolved once here: it
    // is a setting, and a network call per frame to spell it out would be absurd.
    app.project = crate::projects::filed_under(ctx);
    // The dashboard's row for this conversation exists from the moment the screen
    // opens, not only after the first turn: a session a person is about to work
    // in is a session their console should show. Fire-and-forget, so a plane that
    // cannot be reached costs a log line and never a launch.
    crate::report::started(ctx, name, &session);

    // A newer release is worth knowing about and never worth waiting for: the
    // check runs on its own thread behind a day-long cache, and its answer
    // arrives through a channel this loop already polls. Nothing here can stop
    // a launch — an unreachable host is one silent `None`.
    let (update_tx, update_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = update_tx.send(crate::update_cmd::passive_notice(
            memnoth_update::Channel::Stable,
        ));
    });

    enable_raw_mode().context("cannot put the terminal in raw mode")?;
    let mut stdout = io::stdout();
    // Bracketed paste turns a paste into one event instead of a stream of
    // synthesized keys, which is the difference between an API key arriving
    // intact and one arriving with modifiers attached.
    // Mouse capture is what turns a wheel or a touchpad swipe into an event: a
    // scrollbar that no gesture moves is a picture of scrollability, not
    // scrollability. The trade is that the terminal stops doing its own
    // selection; most terminals still select with shift held.
    if let Err(error) = execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    ) {
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
    // The first tool row of the turn in flight. Every running tool row from
    // here on belongs to it, so the answer can resolve them without guessing
    // from text.
    let mut tools_from: Option<usize> = None;
    while !app.quit {
        // the answer is applied here, on the UI thread, so the session file has
        // exactly one writer.
        if let Some(current) = busy.take() {
            let mut running = true;
            finished = false;
            loop {
                match current.rx.try_recv() {
                    // A tool call is a row, not a sentence: the glyph is the
                    // outcome, and it changes on the same line when the result
                    // lands. That is the whole difference between a live record
                    // and a scroll of past tense.
                    Ok(TurnEvent::Api(event)) => {
                        // The wire contract, read by the screen: streamed text
                        // appends to the live answer, streamed reasoning to the
                        // thinking block. Everything else is lifecycle for other
                        // consumers and not transcript content.
                        match event {
                            memnoth_harness_api::ApiEvent::TextDelta { text, .. } => {
                                app.collapse_thinking();
                                app.append_stream_delta(&text);
                            }
                            memnoth_harness_api::ApiEvent::ReasoningDelta { delta, .. } => {
                                app.append_reasoning_delta(&delta);
                            }
                            _ => {}
                        }
                    }
                    Ok(TurnEvent::Note(note)) => {
                        tools_from.get_or_insert(app.entries.len());
                        app.push_tool(note);
                    }
                    Ok(TurnEvent::Ask { request, reply }) => {
                        app.approval = Some(format!(
                            "{}\n\n{}\n\ny approve · n or esc reject",
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
                        let tools = tools_from
                            .map(|from| app.entries.len().saturating_sub(from))
                            .unwrap_or(0);
                        if let Some(from) = tools_from.take() {
                            app.resolve_tools(from);
                        }
                        settle(&mut app, &store, &mut session, &current, tools, *result);
                        // Tokens, files and cost just moved, and the plane's row
                        // is cumulative: one report per finished turn is what
                        // makes the console's numbers match this machine's.
                        crate::report::updated(ctx, &app.name, &session);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        running = false;
                        finished = true;
                        app.busy = false;
                        app.approval = None;
                        approval = None;
                        if let Some(from) = tools_from.take() {
                            app.resolve_tools(from);
                        }
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
                refresh(ctx, &mut app, &session);
            }
        }

        // Sign-in reports from its own thread. Drained before the draw, so a
        // code that arrives is on screen in the very next frame, and a failure
        // leaves the account as it was.
        while let Ok(event) = app.login_rx.try_recv() {
            match event {
                login::LoginEvent::Code { user_code, url } => {
                    // Held for the frame as well as written to the transcript:
                    // the wait can outlive a cleared transcript or a scrolled
                    // one, and a code only in history is a code to hunt for.
                    app.login_prompt = Some(login::LoginPrompt {
                        user_code: user_code.clone(),
                        url: url.clone(),
                    });
                    // The link gets its own line: at the transcript's width a
                    // URL trailed after a sentence wraps at whatever column it
                    // runs out at, which reads as a second sentence rather than
                    // an address to open. Its own indented line keeps evident
                    // what it is, and lets the code sit under it.
                    app.push(
                        EntryRole::Info,
                        format!(
                            "sign in: open this link\n  {url}\n  enter code {user_code} — waiting for approval…"
                        ),
                    );
                }
                login::LoginEvent::Done { email, .. } => {
                    app.login_busy = false;
                    app.login_started = None;
                    app.login_prompt = None;
                    app.account = Some(email.clone());
                    // The credential is stored by the time this arrives, so the
                    // identity is stated here and nowhere else: no second line
                    // waits on the organisation to say who signed in.
                    app.push(EntryRole::Info, login::status_line(Some(&email)));
                    refresh(ctx, &mut app, &session);
                }
                login::LoginEvent::Enriched { org_name, .. } => {
                    app.push(EntryRole::Info, format!("organization {org_name}"));
                }
                login::LoginEvent::Cancelled => {
                    app.login_busy = false;
                    app.login_started = None;
                    app.login_prompt = None;
                    app.push(
                        EntryRole::Info,
                        "sign in cancelled — nothing was stored".to_string(),
                    );
                }
                login::LoginEvent::Failed(message) => {
                    app.login_busy = false;
                    app.login_started = None;
                    app.login_prompt = None;
                    app.push(EntryRole::Error, format!("sign in failed: {message}"));
                }
            }
        }

        // Finished children are reaped and mirrored into the rail every tick;
        // the poll is non-blocking, so this is also the only place a job's
        // output becomes visible without a keypress.
        app.sync_jobs();

        // Provider tests finish on their own thread. Applying them here keeps
        // the connect screen honest in the very next frame; nothing used to
        // drain this channel, so every test stayed "Testing…" forever.
        while let Some(result) = app.try_recv_provider_test_result() {
            app.overlay_stack.deliver_provider_test(&result);
        }

        // Model lists do the same, for the same reason: `/model` asks the
        // provider on a thread, and the answer lands here rather than inside a
        // draw that would freeze the screen until the request timed out.
        while let Some(result) = app.try_recv_models_loaded() {
            app.overlay_stack.deliver_models(&result);
        }

        // An overlay collects its own background answer here: a team list, a
        // minted key. Rendering is not where state changes, and a keypress is
        // not guaranteed, so neither can drain a channel of its own.
        //
        // The stack is lifted out for the call: an overlay reports back by
        // writing to the app (a transcript line, the prompt it fills in), and
        // `&mut app.overlay_stack` beside `&mut app` is two borrows of one value.
        {
            let mut stack = std::mem::take(&mut app.overlay_stack);
            stack.tick(&mut app);
            app.overlay_stack = stack;
        }

        // Verbs run on their own thread so the screen stays live: `/stats`
        // makes two cloud calls with a 30-second timeout each, and run inline
        // that froze the frame in a way no one can tell from a hang.
        while let Some(done) = app.try_recv_verb() {
            finish_verb(&mut app, &store, &mut session, done);
            refresh(ctx, &mut app, &session);
        }

        // The release check answers once, on the first frame after it lands.
        // One entry, filed as information: it is not an error, and it must not
        // look like the answer to anything the person asked.
        if let Ok(Some(notice)) = update_rx.try_recv() {
            app.push(EntryRole::Info, notice);
        }

        terminal
            .draw(|frame| {
                let area = frame.area();
                ui::draw(frame, &mut app);
                crate::tui::rail::Rail::default().render(frame, area);
                if app.theme == crate::tui::app::Theme::NoColor {
                    crate::tui::theme::strip_colors(frame.buffer_mut());
                }
            })
            .context("cannot draw the session")?;
        if event::poll(Duration::from_millis(150)).context("cannot poll the keyboard")? {
            match event::read().context("cannot read the keyboard")? {
                // Focus and resize carry no command; the panes pick up the new
                // size on the next draw. Paste is text: the open overlay
                // gets it first (an API key is always pasted into `/connect`),
                // and with no overlay it goes in the prompt, because losing a
                // pasted stack trace is the worst thing a terminal agent can do
                // with it.
                Event::FocusGained | Event::FocusLost => {}
                Event::Paste(text) => {
                    if !app.overlay_stack.handle_paste(&text) && !app.overlay_stack.has_overlay() {
                        for ch in text.chars() {
                            app.insert_char(ch);
                        }
                    }
                }
                Event::Resize(_, _) => {}
                // The wheel is the one mouse event that carries a command: a
                // touchpad swipe is a scroll, and a visible scrollbar no gesture
                // moves is a lie. Overlays and the brief own their own keys, so
                // the wheel belongs to the transcript.
                Event::Mouse(mouse) if !app.overlay_stack.has_overlay() && !app.home_showing() => {
                    match mouse.kind {
                        MouseEventKind::ScrollUp => app.wheel(WHEEL_LINES, true),
                        MouseEventKind::ScrollDown => app.wheel(WHEEL_LINES, false),
                        // Pointer motion is tracked for one thing only: the
                        // thinking header's expand hint appears while the
                        // pointer rests on it, and nowhere else.
                        MouseEventKind::Moved => app.mouse_row = Some(mouse.row),
                        // Clicks, drags and horizontal scroll: nothing on this
                        // screen is clickable, and a selectable row that a click
                        // did not select would be worse than no pointer at all.
                        _ => {}
                    }
                }
                // An open overlay owns the mouse: the wheel scrolls its list
                // and clicks land on its rows, because a gesture that moves
                // the transcript behind a modal is a gesture that lies.
                Event::Mouse(mouse) => {
                    let mut stack = std::mem::take(&mut app.overlay_stack);
                    let action = stack.handle_mouse(mouse, &mut app);
                    app.overlay_stack = stack;
                    match action {
                        Some(crate::tui::overlays::OverlayAction::Close) => {
                            app.overlay_stack.pop();
                        }
                        Some(crate::tui::overlays::OverlayAction::CloseAll) => {
                            while app.overlay_stack.pop().is_some() {}
                        }
                        _ => {}
                    }
                }
                // Windows reports press and release; acting on both would type
                // every character twice. Every other `Event::Key` arm below
                // this one is unreachable, so this must stay the only one.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(pending) = app.pending_shell.take() {
                        // Three outcomes, not two: a key that is not an answer
                        // must leave the question standing. Only y/n/enter/esc
                        // decide, and the command that was parked is the command
                        // that runs — it is not looked up a second time.
                        match key.code {
                            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                                // The box keeps its promise: `background` decides
                                // whether this lands in the transcript or on the
                                // rail, and the command is the parked one.
                                if pending.background {
                                    start_job(&mut app, ctx, &pending.command);
                                } else {
                                    execute_shell_command(&mut app, ctx, &pending.command);
                                }
                                app.push(
                                    EntryRole::Info,
                                    format!("approved · {}", pending.command),
                                );
                            }
                            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                                app.push(
                                    EntryRole::Info,
                                    format!("declined · {} was not run", pending.command),
                                );
                            }
                            _ => app.pending_shell = Some(pending),
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
                        // First let overlays handle the key. The stack is moved
                        // out for the call so the overlay can borrow the app it
                        // is acting on; modals are single-threaded by design.
                        let mut stack = std::mem::take(&mut app.overlay_stack);
                        let action = stack.handle_key(key, &mut app);
                        app.overlay_stack = stack;
                        // No overlay was open: the session gets the key. When
                        // one is open it owns every key, so nothing it ignores
                        // can leak into the prompt behind it.
                        match action {
                            None => handle_key(&mut app, key, &ctx.root),
                            Some(crate::tui::overlays::OverlayAction::Close) => {
                                app.overlay_stack.pop();
                            }
                            Some(crate::tui::overlays::OverlayAction::CloseAll) => {
                                while app.overlay_stack.pop().is_some() {}
                            }
                            Some(crate::tui::overlays::OverlayAction::Push(kind)) => {
                                open_overlay(&mut app, kind);
                            }
                            Some(crate::tui::overlays::OverlayAction::Replace(kind)) => {
                                app.overlay_stack.pop();
                                open_overlay(&mut app, kind);
                            }
                            Some(crate::tui::overlays::OverlayAction::None) => {}
                        }
                    }
                }
                // Key releases, repeats and any future key event kind: no
                // command. This arm must come after the `Press` arm, never
                // before it, or nothing above is ever reached.
                Event::Key(_) => {}
            }
        }
        // A cancel requested from the keyboard is honoured here, where the
        // in-flight receiver lives. Dropping it discards the answer instead of
        // letting a turn the person abandoned land in the transcript later.
        if app.cancel_turn {
            app.cancel_turn = false;
            finished = false;
            if let Some(current) = busy.take() {
                drop(current);
                app.busy = false;
                app.approval = None;
                approval = None;
                app.push(
                    EntryRole::Info,
                    "turn cancelled — its answer will be discarded".to_string(),
                );
                refresh(ctx, &mut app, &session);
            }
        }

        if app.pending_submit {
            app.pending_submit = false;
            // Remembered before the line is taken: `/` next time offers what was
            // actually run, at the top, without a filter. The history is written
            // now rather than on exit, so a session that is killed still keeps
            // what it ran; a failed write costs a ranking, not a turn.
            app.slash.record(&app.input);
            let _ = crate::tui::slash::save_usage(&store.usage_path(), &app.slash.usage());
            if busy.is_some() {
                // Queue the command instead of rejecting it
                let line = app.take_input();
                if !line.is_empty() {
                    app.push(EntryRole::Info, format!("queued: {line}"));
                    app.command_queue.push_back(line);
                }
            } else {
                busy = submit(&mut app, ctx, &store, &mut session);
                refresh(ctx, &mut app, &session);
            }
            // The line is gone, so the dropdown that described it goes too.
            app.sync_slash();
        }

        // A screen may ask for a different conversation — `/new`, or a row in
        // the sessions picker — but it may not open one: the file is recorded
        // here, by this loop, and a second writer is how a transcript forks.
        // Taken before the draw so the next frame is already the new session,
        // and never while a turn is in flight: the answer would land in a
        // conversation nobody is looking at.
        if let Some(switch) = app.pending_session.take() {
            if busy.is_some() {
                app.push(
                    EntryRole::Info,
                    "this turn is still recording into the session \u{2014} cancel it, then switch"
                        .to_string(),
                );
                // The prompt waited for a switch that did not happen; firing it
                // now would file it into the conversation the person left.
                app.pending_prompt = None;
            } else {
                match switch_session(ctx, &mut app, &store, &mut session, switch) {
                    Ok(()) => {
                        // A question typed on the brief waited for the fresh
                        // conversation this switch just created.
                        if let Some(prompt) = app.pending_prompt.take() {
                            app.input = prompt;
                            busy = submit(&mut app, ctx, &store, &mut session);
                        }
                    }
                    Err(error) => {
                        app.pending_prompt = None;
                        app.push(EntryRole::Error, format!("{error:#}"));
                    }
                }
            }
        }

        // Process command queue when turn completes
        if finished
            && !app.command_queue.is_empty()
            && let Some(queued_line) = app.command_queue.pop_front()
        {
            app.input = queued_line;
            app.pending_submit = true;
        }
    }

    // The row's `ended_at` is what rolls the session into the usage charts, so
    // the close is reported before the screen is torn down. `settle` has already
    // written the session file; this only tells the plane about it.
    crate::report::finished(ctx, &app.name, &session);
    // The close is queued, and the process is about to end: the worker is a
    // detached thread, so without waiting here the one report that makes the
    // plane roll this session into usage is killed with the process and the row
    // stays `running`. Bounded, so an unreachable plane delays exit by at most
    // five seconds and never more.
    crate::report::flush(std::time::Duration::from_secs(5));

    drop(terminal);
    drop(guard);
    Ok(())
}

/// Re-read what the frame shows after work has happened: a `/kg index` or a
/// `/vault init` must be visible on the startup screen the next time it is
/// shown, not only in the header.
fn refresh(ctx: &AppContext, app: &mut App, session: &Session) {
    let graph = graph_state(ctx);
    // Read from the app, not from parameters captured when the screen opened:
    // `/new` and the sessions picker change which conversation this is, and a
    // brief that kept naming the old one would be describing a file nobody is
    // recording any more.
    let name = app.name.clone();
    let resumed = app.resumed;
    app.home = Some(home(ctx, &name, resumed, &graph, session, false));
    app.graph_attention = graph.attention.clone();
    app.home_cursor = 0;
    app.context = context_pane(ctx, session);
    // The label beside the prompt: the model the next turn will actually use,
    // including the case where it cannot run because no key is configured.
    app.model = match provider::Settings::load(&ctx.config) {
        Ok(settings) if settings.ready() => settings.model,
        Ok(settings) => format!("{} · no key", settings.model),
        Err(_) => String::new(),
    };
}

/// Leave raw mode and the alternate screen, on every path.
struct RestoreTerminal;

impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(
            out,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            Show
        );
        let _ = out.flush();
    }
}

fn handle_key(app: &mut App, key: KeyEvent, workspace_root: &Path) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // The brief owns four keys while the prompt is empty: its `next` rows are
    // selectable, and a list that looks selectable but is not was the defect
    // this replaced. With text in the prompt the arrows are history again,
    // exactly as they were.
    if app.home_showing() && app.input.is_empty() && !app.slash.is_open() {
        match key.code {
            KeyCode::Up => {
                app.home_move(-1);
                return;
            }
            KeyCode::Down => {
                app.home_move(1);
                return;
            }
            KeyCode::Char(ch @ '1'..='3') if !ctrl => {
                app.home_jump(ch.to_digit(10).unwrap_or(0) as usize);
                return;
            }
            // `enter` runs the selected row: the command goes into the prompt and
            // is submitted by the same path a typed line takes, so the transcript
            // shows exactly what was run.
            KeyCode::Enter => {
                if let Some(command) = app.home_command() {
                    app.input = command;
                    app.cursor = app.input.chars().count();
                    app.pending_submit = true;
                }
                return;
            }
            // `tab` puts the row in the prompt and stops there, leaving a space for
            // the argument. It never runs: no key means two things in one surface.
            KeyCode::Tab => {
                if let Some(draft) = app.home_draft() {
                    app.input = draft;
                    app.cursor = app.input.chars().count();
                }
                return;
            }
            _ => {}
        }
    }

    // A focused task rail owns four keys while the prompt is empty: the
    // arrows walk jobs, Enter opens the selected job's output, and ctrl-x
    // stops it. With text in the prompt the keys belong to the line, so a
    // command can still be typed without leaving the rail.
    if app.active_pane == 2 && app.input.is_empty() {
        match key.code {
            KeyCode::Up => {
                app.tasks_pane.select_relative(-1);
                return;
            }
            KeyCode::Down => {
                app.tasks_pane.select_relative(1);
                return;
            }
            KeyCode::Enter => {
                if let Some(id) = app.tasks_pane.selected_task_id() {
                    push_job_logs(app, &id);
                }
                return;
            }
            KeyCode::Char('x') if ctrl => {
                if let Some(id) = app.tasks_pane.selected_task_id() {
                    cancel_job(app, &id);
                }
                return;
            }
            _ => {}
        }
    }
    // ctrl-g runs the fix the header is asking for. The command goes into the
    // prompt and is submitted by the same path a typed line takes, so the
    // transcript records exactly what ran. Empty line only: a half-written
    // message is never thrown away to satisfy a status chip.
    if ctrl && matches!(key.code, KeyCode::Char('g')) {
        if app.input.is_empty()
            && !app.slash.is_open()
            && let Some(attention) = &app.graph_attention
        {
            app.input = attention.fix.clone();
            app.cursor = app.input.chars().count();
            app.pending_submit = true;
        }
        // Consumed even when there is nothing to fix, so the key cannot mean
        // "insert a g" on one screen and "run the header's fix" on the next.
        return;
    }
    // Ctrl+1..8 switch surfaces: chat, review, agents, memory, context,
    // tasks, sessions, brief. Global keys: work whether a prompt is being
    // typed or not.
    if ctrl && matches!(key.code, KeyCode::Char('1'..='8')) {
        let index = match key.code {
            KeyCode::Char(d) => d.to_digit(10).unwrap_or(0) as u8,
            _ => 0,
        };
        app.surface.switch(index);
        app.surface_focus = match app.surface {
            crate::tui::surface::Surface::Chat => crate::tui::surface::FocusOwner::Chat,
            crate::tui::surface::Surface::Review => crate::tui::surface::FocusOwner::Review,
            crate::tui::surface::Surface::Agents => crate::tui::surface::FocusOwner::Agents,
            crate::tui::surface::Surface::Memory => crate::tui::surface::FocusOwner::Memory,
            crate::tui::surface::Surface::Context => crate::tui::surface::FocusOwner::Context,
            crate::tui::surface::Surface::Tasks => crate::tui::surface::FocusOwner::Tasks,
            crate::tui::surface::Surface::Sessions => crate::tui::surface::FocusOwner::Sessions,
            crate::tui::surface::Surface::Brief => crate::tui::surface::FocusOwner::Chat,
        };
        app.focus_owner = app.surface_focus.label().to_string();
        return;
    }

    // Alt+A opens the live Agent Deck and Alt+T the trace tree. Global keys,
    // consumed even when there is nothing running: a key that sometimes opens
    // a roster and sometimes inserts a letter is two meanings for one press.
    if key.modifiers.contains(KeyModifiers::ALT) {
        match key.code {
            KeyCode::Char('a' | 'A') => {
                open_overlay(app, crate::tui::overlays::OverlayKind::Deck);
                return;
            }
            KeyCode::Char('t' | 'T') => {
                open_overlay(app, crate::tui::overlays::OverlayKind::Trace);
                return;
            }
            _ => {}
        }
    }

    let is_ctrl_c = ctrl && matches!(key.code, KeyCode::Char('c'));
    // The second ctrl-c only quits when it follows the first without another
    // key between them; a stale arm must never turn a later press into a quit.
    if !is_ctrl_c {
        app.ctrl_c_armed = false;
    }

    // While the `/` dropdown is open it owns the navigation keys: ↑/↓ walk the
    // commands on screen, not the history behind it. Esc closes the list and
    // leaves the line alone, so a wrong guess costs one key.
    if app.slash.is_open() {
        match key.code {
            KeyCode::Up => {
                app.slash.select_previous();
                return;
            }
            KeyCode::Down => {
                app.slash.select_next();
                return;
            }
            KeyCode::PageUp => {
                app.slash.page_up();
                return;
            }
            KeyCode::PageDown => {
                app.slash.page_down();
                return;
            }
            KeyCode::Home => {
                app.slash.first();
                return;
            }
            KeyCode::End => {
                app.slash.last();
                return;
            }
            // Tab completes the line and stops there: the command is in the
            // prompt to be read or extended, never run by a completion key.
            KeyCode::Tab => {
                if accept_slash(app).is_some() {
                    app.pending_submit = false;
                }
                return;
            }
            // Enter completes a command, because a command is what the list is
            // for. With arguments on offer Enter runs the line instead: the
            // value is still being typed and tab is the key that fills it in,
            // so no key means two things in one surface.
            KeyCode::Enter => {
                if app.slash.choosing_command()
                    && let Some(submit) = accept_slash(app)
                {
                    app.pending_submit = submit;
                    return;
                }
                // Nothing matched: the line typed is the command, and the
                // ordinary Enter below runs it.
            }
            KeyCode::Esc => {
                app.slash.close();
                return;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('c') if ctrl => {
            if app.busy {
                if app.ctrl_c_armed {
                    app.quit = true;
                } else {
                    app.ctrl_c_armed = true;
                    app.cancel_turn = true;
                    app.push(
                        EntryRole::Info,
                        "cancelling the turn — ctrl-c again to quit".to_string(),
                    );
                }
            } else {
                app.quit = true;
            }
        }
        KeyCode::Char('d') if ctrl => app.quit = true,
        // The link a pending sign-in printed, opened without leaving the
        // session: the same helper the command line uses, and only when there
        // is a link to open. Nothing opens on its own — a browser that appears
        // unbidden mid-edit is the surprise this key exists to avoid.
        KeyCode::Char('o') if ctrl => {
            if let Some(signin) = app.login_prompt.clone() {
                match crate::auth::open_browser(&signin.url) {
                    Ok(()) => app.push(EntryRole::Info, format!("opened {}", signin.url)),
                    Err(error) => app.push(
                        EntryRole::Error,
                        format!("could not open a browser ({error}); select the link above"),
                    ),
                }
            }
        }
        KeyCode::Char('l') if ctrl => app.clear(),
        KeyCode::Char('a') if ctrl => app.move_home(),
        KeyCode::Char('e') if ctrl => app.move_end(),
        // The newest thinking block, opened or tucked away.
        KeyCode::Char('r') if ctrl => app.toggle_thinking(),
        KeyCode::F(1) => {
            app.overlay_stack.push(crate::tui::overlays::create_overlay(
                crate::tui::overlays::OverlayKind::Help,
            ));
        }
        // Ctrl+M is Enter on terminals that fold the control bit into CR; where
        // it is reported separately, it opens the same model browser as
        // `/models`.
        KeyCode::Char('m') if ctrl => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Models);
        }
        KeyCode::Char('t') if ctrl => app.toggle_tasks(),
        KeyCode::Char(ch) if ctrl && ('1'..='4').contains(&ch) => {
            app.focus_pane(ch as u8 - b'1');
        }
        // Ctrl+K or Ctrl+P opens command palette (like VS Code / lazygit)
        KeyCode::Char('k') if ctrl => {
            app.overlay_stack.push(crate::tui::overlays::create_overlay(
                crate::tui::overlays::OverlayKind::Palette,
            ));
        }
        // ctrl-p used to open the same palette as ctrl-k. One key for one
        // surface; ctrl-p is left to the terminal again.

        // '/' starts a command, and the dropdown under the prompt lists them:
        // a command is discovered instead of remembered. ctrl-k still opens the
        // full palette for someone who wants the modal.
        KeyCode::Char('/') => {
            app.insert_char('/');
            app.sync_slash();
        }
        // '?' is help only when there is no line to ask about; otherwise it is
        // a character in the question.
        KeyCode::Char('?') if app.input.is_empty() => {
            app.overlay_stack.push(crate::tui::overlays::create_overlay(
                crate::tui::overlays::OverlayKind::Help,
            ));
        }
        KeyCode::Tab => {
            // @file fuzzy completion wins while it is active; otherwise Tab is
            // pane navigation, never a silent no-op.
            if app.input.contains('@') && !app.busy {
                app.complete_path(workspace_root);
            } else {
                app.cycle_pane(true);
            }
        }
        KeyCode::BackTab => app.cycle_pane(false),
        // ctrl-o attaches the path the line starts with. The composer holds
        // prose, so the line doubles as the selection: `/tmp/shot.png what is
        // wrong here?` attaches the file and leaves the question on the line.
        KeyCode::Char('o') if ctrl => attach_from_input(app),
        KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            app.insert_char(ch);
            app.sync_slash();
        }
        KeyCode::Enter => {
            if !app.input.trim().is_empty() {
                app.pending_submit = true;
            }
        }
        KeyCode::Esc => {
            // A wait for approval is the thing in front of you, so esc ends it —
            // but only where the prompt has nothing else for the key. The
            // states below all claim esc first, and each of them keeps `/login`
            // as the way out.
            if app.esc_cancels_signin() {
                stop_signin(app);
                return;
            }
            app.cancel_completion();
            if app.busy {
                // One Esc gets out of a running turn; the input that asked for
                // it is left alone, because the person may want to edit it.
                app.cancel_turn = true;
                app.push(
                    EntryRole::Info,
                    "cancelling the turn — esc again is harmless".to_string(),
                );
            } else if app.input.is_empty() && app.home_showing() {
                // esc cancels what is in front of you; on the brief that is the
                // brief itself. It stays put when there is no transcript to
                // return to.
                app.close_home();
            } else {
                app.input.clear();
                app.cursor = 0;
                app.sync_slash();
            }
        }
        KeyCode::Backspace => {
            app.cancel_completion();
            app.backspace();
            app.sync_slash();
        }
        KeyCode::Delete => {
            app.cancel_completion();
            app.delete();
            app.sync_slash();
        }
        KeyCode::Left => {
            app.cancel_completion();
            app.move_left();
            app.sync_slash();
        }
        KeyCode::Right => {
            app.cancel_completion();
            app.move_right();
            app.sync_slash();
        }
        KeyCode::Home => {
            app.cancel_completion();
            app.move_home();
            app.sync_slash();
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
/// `ctrl-o`: read the leading token of the line as an image or PDF.
///
/// The read happens here, while the person is looking at the composer, so a
/// missing file or a mislabelled extension is answered immediately instead of
/// failing the turn after the question is already in the transcript.
fn attach_from_input(app: &mut App) {
    let line = app.input.trim().to_string();
    if line.is_empty() {
        app.push(
            EntryRole::Info,
            "ctrl-o takes a path: type the image or PDF path on the line first".to_string(),
        );
        return;
    }
    let candidate = line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    match crate::provider::Attachment::read(&candidate) {
        Ok(file) => {
            let name = file.path.clone();
            app.attachments.push(file);
            let rest = line[candidate.len()..].trim_start().to_string();
            app.input = rest;
            app.cursor = app.input.chars().count();
            app.push(EntryRole::Info, format!("attached {name}"));
            app.sync_slash();
        }
        Err(error) => app.push(EntryRole::Error, format!("{error:#}")),
    }
}

/// Whether a submitted prompt must open a fresh conversation first.
///
/// The brief is drawn over a resumed file — that is what keeps its history one
/// `esc` away — so a question typed at the brief belongs to a new conversation,
/// never to the file standing behind it. An empty conversation is already
/// fresh; switching there would create a second empty file for no reason.
fn prompt_wants_fresh_chat(app: &App) -> bool {
    app.home_showing() && !app.entries.is_empty()
}

/// `/merge <run>`: name the child and say what a merge actually needs.
///
/// A merge needs a diff, and a diff needs a child that ran. The tree is
/// consulted first so the answer is about the run the person named, not a
/// generic refusal.
fn merge_child(app: &mut App, branch: &str) {
    let Some(delegator) = app.delegator.as_ref() else {
        app.push(
            EntryRole::Info,
            "no delegation tree in this session — /delegate <agent> <task> spawns a run first"
                .to_string(),
        );
        return;
    };
    let nodes = delegator.nodes();
    let target = nodes
        .iter()
        .find(|node| node.agent_id == branch || node.agent == branch);
    match target {
        None => app.push(
            EntryRole::Error,
            format!(
                "/merge {branch}: no child run with that id or agent — {} run(s) in the tree",
                nodes.len()
            ),
        ),
        Some(node) => {
            let status = delegator
                .registry()
                .get(&node.agent_id)
                .map(|entry| entry.status.as_str())
                .unwrap_or("unknown");
            let where_it_lives = node
                .worktree
                .as_ref()
                .map(|path| format!("worktree: {path}"))
                .unwrap_or_else(|| "no worktree — the child ran read-only".to_string());
            app.push(
                EntryRole::Info,
                format!(
                    "/merge {} — run is {status}\n  {where_it_lives}\n  git -C <worktree> diff HEAD --stat, then git merge, then /vault conflicts",
                    node.agent_id
                ),
            );
        }
    }
}

/// Files changed in a child worktree, relative to the worktree root. This is
/// evidence gathered from git, not a claim copied from the child result.
fn worktree_changed_files(path: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for args in [
        vec!["diff", "--name-only", "HEAD"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let Ok(output) = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(&args)
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        for file in String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|file| !file.is_empty())
        {
            if !files.iter().any(|existing| existing == file) {
                files.push(file.to_string());
            }
        }
    }
    files
}

/// Contradictions in the vault that name a belief proposed by this child.
fn belief_conflicts_for_proposals(
    engine: &memnoth_engine::MemoryEngine,
    proposals: &[memnoth_engine::ProposedBelief],
) -> Vec<String> {
    let Ok(audit) = memnoth_engine::vault_contradictions_all(engine) else {
        return Vec::new();
    };
    let Some(rows) = audit
        .get("contradictions")
        .and_then(|value| value.as_array())
    else {
        return Vec::new();
    };
    let mut conflicts = Vec::new();
    for row in rows {
        let source = row.get("source").and_then(|value| value.as_str());
        let target = row.get("target").and_then(|value| value.as_str());
        let mut names = source.into_iter().chain(target);
        if !names.any(|name| proposals.iter().any(|proposal| proposal.entity == name)) {
            continue;
        }
        let kind = row
            .get("kind")
            .and_then(|value| value.as_str())
            .unwrap_or("relation");
        let reason = row
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("contradicted");
        conflicts.push(format!(
            "{} {} {}: {}",
            source.unwrap_or("?"),
            kind,
            target.unwrap_or("?"),
            reason
        ));
    }
    conflicts
}

/// Create a child run under this session's root and put it on the Deck.
///
/// The run is real: it is in the registry, it has its own JSONL, and it is a
/// child of this session's root at depth 1. The card it pushes is the same node
/// the Deck draws, so the timeline and the deck cannot describe two fleets.
///
/// Spawning also drives the child: it gets its own worktree (W8-06), the
/// executor runs its read-only plan against a scoped vault slice, and the
/// parent accepts that slice into vault proposals with one episode per run
/// (W8-09). The delegator is taken out and put back because the executor
/// borrows it mutably for the whole run.
fn spawn_delegate(app: &mut App, ctx: &AppContext, session: &Session, name: &str, task: &str) {
    if name.trim().is_empty() || task.trim().is_empty() {
        app.push(
            EntryRole::Error,
            "usage: /delegate <agent> <task>".to_string(),
        );
        return;
    }
    if app.delegator.is_none() {
        app.delegator = Some(memnoth_agent::Delegator::new(ctx.home.join("agents")));
    }

    let root = match app.delegation_root.clone() {
        Some(id) => id,
        None => {
            let created = app
                .delegator
                .as_mut()
                .expect("delegator was just ensured")
                .root(Some(&session.id), "memnoth", "session");
            match created {
                Ok(root) => {
                    app.delegation_root = Some(root.agent_id.clone());
                    root.agent_id
                }
                Err(error) => {
                    app.push(EntryRole::Error, format!("{error}"));
                    return;
                }
            }
        }
    };

    let mut delegator = app.delegator.take().expect("delegator was just ensured");
    let spawned = delegator.spawn(Some(&root), Some(&session.id), name, task);
    let child = match spawned {
        Ok(child) => child,
        Err(error) => {
            app.delegator = Some(delegator);
            app.push(EntryRole::Error, format!("{error}"));
            return;
        }
    };

    // A child that may touch files gets its own checkout, so a bad edit cannot
    // land in the workspace it shares with the parent (W8-06). Best effort:
    // a workspace that is not a git repo runs read-only in place, and the run
    // says so on its card instead of refusing to spawn.
    let worktree = memnoth_worktree::create_worktree(&ctx.root, &child.agent_id, None, None).ok();
    if let Some(info) = &worktree {
        let _ = delegator.set_worktree(&child.agent_id, info.path.to_string_lossy().to_string());
    }

    // Read-only by default (D10): the child's plan reads, never writes. The
    // slice stages one scoped belief per step — the instruction it inspected —
    // and the closure does nothing else, because there is no model behind this
    // build yet and a plan that pretended otherwise would lie on the card.
    let steps = vec![
        memnoth_agent::Step::with_detail("read_file", task),
        memnoth_agent::Step::with_detail("grep", task),
    ];
    let agent_name = name.to_string();
    let task_owned = task.to_string();
    let started_at = memnoth_engine::now_ms();
    let mut executor = memnoth_agent::Executor::without_hooks(&mut delegator);
    let run_result = executor.run(&child.agent_id, &steps, |step, slice| {
        slice.propose(
            &agent_name,
            &format!("delegated task: {task_owned}"),
            "fact",
            0.5,
            &step.detail,
            1,
        );
        Ok(())
    });
    let proposals: Vec<memnoth_engine::ProposedBelief> = executor
        .slice(&child.agent_id)
        .map(|slice| slice.proposals().to_vec())
        .unwrap_or_default();
    drop(executor);
    app.delegator = Some(delegator);

    // The parent accepts the scoped slice through the ordinary vault path:
    // these become proposal rows (`vault proposals`), not beliefs, so nothing
    // the child staged is a fact until a person approves it (W8-09). One
    // episode covers the whole run, opened and closed here.
    let mut accepted = 0;
    let mut belief_conflicts = Vec::new();
    if let Ok(engine) = ctx.open_vault() {
        belief_conflicts = belief_conflicts_for_proposals(&engine, &proposals);
        let now = memnoth_engine::now_ms();
        for proposal in &proposals {
            let provenance = proposal.provenance.to_json();
            if engine
                .propose_belief(
                    &proposal.entity,
                    memnoth_engine::EntityKind::Concept,
                    &proposal.content,
                    &proposal.kind,
                    proposal.confidence,
                    &provenance,
                    now,
                )
                .is_ok()
            {
                accepted += 1;
            }
        }
        let summary = run_result
            .as_ref()
            .map(|result| result.summary.clone())
            .unwrap_or_else(|error| format!("failed: {error}"));
        if let Ok(episode) =
            engine.append_episode("delegate", Some(&agent_name), Some(&summary), started_at)
        {
            let _ = engine.close_episode(episode, memnoth_engine::now_ms());
        }
    }

    // The card is the timeline row: the same node the Deck draws, put
    // where the work was asked for (W13-18). It is projected from the child
    // result and the worktree/vault evidence gathered above, so the parent
    // sees what was done, where it lives, and what tests/conflicts it left.
    let worktree_files = worktree
        .as_ref()
        .map(|info| worktree_changed_files(&info.path))
        .unwrap_or_default();
    let info = crate::tui::agents::roster(app)
        .into_iter()
        .find(|candidate| candidate.agent_id == child.agent_id);
    if let Some(mut info) = info {
        let card = match &run_result {
            Ok(result) => {
                let card = crate::tui::agents::cards::AgentCard::from_result(
                    info.clone(),
                    result,
                    &worktree_files,
                    belief_conflicts,
                );
                app.push(
                    EntryRole::Info,
                    format!(
                        "delegated to {} — run {} (depth {}) — {} accepted proposal(s) into the vault",
                        child.agent, child.agent_id, child.depth, accepted
                    ),
                );
                card
            }
            Err(error) => {
                info.status = crate::tui::agents::AgentStatus::Failed;
                let mut card = crate::tui::agents::cards::AgentCard::new(info);
                card.activity = format!("failed: {error}");
                app.push(
                    EntryRole::Error,
                    format!("child {} failed: {error}", child.agent_id),
                );
                card
            }
        };
        app.push_card(&card);
    }
    app.push(EntryRole::Info, format!("log: {}", child.log.display()));
    if let Some(info) = &worktree {
        app.push(
            EntryRole::Info,
            format!(
                "worktree: {} on branch {}",
                info.path.display(),
                info.branch
            ),
        );
    } else {
        app.push(
            EntryRole::Info,
            "no worktree (not a git repo) — the child ran read-only in place".to_string(),
        );
    }
}

/// Set (or clear) the goal and open the overlay on the stored row.
fn open_goal(app: &mut App, ctx: &AppContext, condition: Option<&str>) {
    match crate::tui::commands::goal::run(ctx, condition) {
        Ok(outcome) => {
            let current = outcome.data["condition"].as_str().map(str::to_string);
            crate::tui::overlays::present(
                app,
                Box::new(crate::tui::agents::goal::GoalOverlay::new().with_condition(current)),
            );
            app.push(EntryRole::Info, outcome.text);
        }
        Err(error) => app.push(EntryRole::Error, error.message),
    }
}

/// Enter plan mode and open the overlay on the stored plan.
fn open_plan(app: &mut App, ctx: &AppContext, name: Option<&str>, step: Option<&str>) {
    match crate::tui::commands::plan::run(ctx, name, step) {
        Ok(outcome) => {
            let task = outcome.data["task"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let steps: Vec<String> = outcome.data["steps"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            crate::tui::overlays::present(
                app,
                Box::new(crate::tui::agents::plan::PlanOverlay::new().with_task(task, steps)),
            );
            app.push(EntryRole::Info, outcome.text);
        }
        Err(error) => app.push(EntryRole::Error, error.message),
    }
}

/// Rename the conversation and keep writing to the new file.
fn rename_session(app: &mut App, ctx: &AppContext, session: &mut Session, name: &str) {
    match crate::tui::commands::rename::run(ctx, name) {
        Ok(outcome) => {
            let renamed = outcome.data["to"].as_str().unwrap_or_default().to_string();
            session.id = renamed.clone();
            app.name = renamed;
            app.push(EntryRole::Info, outcome.text);
        }
        Err(error) => app.push(EntryRole::Error, error.message),
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

    // `/ext:<extension>:<command>` expands to the prompt the extension ships
    // and then takes the ordinary Ask path. An extension command is a prompt,
    // not a second execution model inside the TUI.
    let line = if line.trim_start().starts_with("/ext:") {
        match crate::extension_commands::expand(&ctx.home, &line) {
            Ok(Some(body)) => body,
            Ok(None) => line,
            Err(error) => {
                app.push(EntryRole::Error, format!("{error}"));
                return None;
            }
        }
    } else {
        line
    };

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
        Parsed::Home => {
            app.open_home();
            None
        }
        Parsed::NewChat(name) => {
            // Parked rather than performed: the `Session` and its writer belong
            // to the run loop, and this function only has the screen.
            app.pending_session = Some(SessionSwitch::New(name.unwrap_or_default()));
            None
        }
        Parsed::Sessions => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Sessions);
            None
        }
        // `/delegate` and `/rename` change state the handler cannot see: the
        // live session's spawn tree, and the name the session is written under.
        Parsed::Run(Verb::Delegate(args)) => {
            spawn_delegate(app, ctx, session, &args.name, &args.task);
            None
        }
        Parsed::Run(Verb::Rename(args)) => {
            rename_session(app, ctx, session, &args.name);
            None
        }
        Parsed::CheckpointsOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Checkpoints);
            None
        }
        Parsed::Run(Verb::Onboarding) => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Onboarding);
            None
        }
        Parsed::Run(Verb::Merge(args)) => {
            merge_child(app, &args.branch);
            None
        }
        // `/goal` and `/plan` write the session's goal file and then open the
        // overlay on the row they just wrote, so the screen and the store
        // cannot describe two different states.
        Parsed::Run(Verb::Goal(args)) => {
            open_goal(app, ctx, args.condition.as_deref());
            None
        }
        Parsed::Run(Verb::Plan(args)) => {
            open_plan(app, ctx, args.name.as_deref(), args.step.as_deref());
            None
        }
        Parsed::Run(verb) => {
            run_verb(app, ctx, &line, verb);
            None
        }
        Parsed::TeamsOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Teams);
            None
        }
        Parsed::KeyCreate {
            name,
            scope,
            project,
            expires_in_days,
        } => {
            // The flags that were typed are the form's starting values: a typed
            // flag is never silently dropped on the way to another screen.
            crate::tui::overlays::present(
                app,
                Box::new(crate::tui::overlays::KeyCreateOverlay::seeded(
                    name,
                    scope,
                    project,
                    expires_in_days,
                )),
            );
            None
        }
        Parsed::Invite { email, role } => {
            crate::tui::overlays::present(
                app,
                Box::new(crate::tui::overlays::InviteOverlay::seeded(email, role)),
            );
            None
        }
        Parsed::Ask(words) => {
            // A prompt typed on the brief opens a fresh conversation, always.
            // The brief is drawn over the resumed file — that is what keeps its
            // history one `esc` away — so answering here filed the new question
            // into a conversation the person had only glanced at. The loop owns
            // the session file, so the line is parked with the switch and
            // replayed once the fresh session exists.
            if prompt_wants_fresh_chat(app) {
                app.pending_session = Some(SessionSwitch::New(String::new()));
                app.pending_prompt = Some(line);
                return None;
            }
            ask(app, ctx, store, session, &line, &words)
        }
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
        Parsed::Help => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Help);
            None
        }
        Parsed::ModelsOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Models);
            None
        }
        Parsed::Tasks(command) => {
            run_tasks(app, ctx, command);
            None
        }
        Parsed::SearchOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Search);
            None
        }
        Parsed::ContextOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Context);
            None
        }
        Parsed::DiffOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Diff);
            None
        }
        Parsed::RewindOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Rewind);
            None
        }
        Parsed::PermissionsOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Permissions);
            None
        }
        Parsed::FeedbackOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Feedback);
            None
        }
        Parsed::ConnectOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Connect);
            None
        }
        Parsed::Login => {
            start_login(app, ctx);
            None
        }
        Parsed::Logout => {
            sign_out(app, ctx);
            None
        }
        Parsed::PaletteOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Palette);
            None
        }
        Parsed::Onboarding => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Onboarding);
            None
        }
        Parsed::CommandsOverlay => {
            open_overlay(app, crate::tui::overlays::OverlayKind::Commands);
            None
        }
        Parsed::Info(message) => {
            app.push(EntryRole::Info, message);
            None
        }
        Parsed::Usage(message) => {
            // A known command with the wrong arguments: the usage line is the
            // answer, and the session stays open so it can be retyped.
            app.push(EntryRole::Info, message);
            None
        }
        Parsed::Unknown(message) => {
            app.push(EntryRole::Error, message);
            None
        }
    }
}
/// `/tasks` and the keys the focused rail uses.
///
/// Every branch leaves a transcript line behind: a job that started silently
/// is one the person has to go looking for, and a cancel that did nothing must
/// say which of those two things happened.
fn run_tasks(app: &mut App, ctx: &AppContext, command: TasksCommand) {
    match command {
        TasksCommand::Show => app.toggle_tasks(),
        // A background job is the same command as `!cmd` with a different
        // destination, so it goes through the same rule set. The rail showed the
        // argv before; it did not ask anyone about it, and "in the background"
        // is not a permission.
        TasksCommand::Run(command) => match decide_shell(ctx, &command) {
            Ok(RuleDecision::Allow { .. }) => start_job(app, ctx, &command),
            Ok(RuleDecision::Deny { reason, rule }) => app.push(
                EntryRole::Error,
                format!(
                    "denied: {} (rule: {})\n  the job was not started",
                    reason, rule
                ),
            ),
            Ok(RuleDecision::Ask {
                reason,
                rule,
                destructive,
                kind,
            }) => {
                app.pending_shell = Some(PendingShell {
                    command,
                    decision: RuleDecision::Ask {
                        reason,
                        rule,
                        destructive,
                        kind,
                    },
                    background: true,
                });
            }
            Err(error) => app.push(
                EntryRole::Error,
                format!("sandbox: {error:#}\n  the job was not started"),
            ),
        },
        TasksCommand::Status => {
            app.sync_jobs();
            app.push(EntryRole::Info, app.jobs.status_text());
        }
        TasksCommand::Logs(id) => {
            app.sync_jobs();
            match id.or_else(|| app.tasks_pane.selected_task_id()) {
                Some(id) => push_job_logs(app, &id),
                None => app.push(
                    EntryRole::Info,
                    "no background jobs — /tasks run <command> starts one".to_string(),
                ),
            }
        }
        TasksCommand::Cancel(id) => cancel_job(app, &id),
    }
}

/// Start a command as a background job, under the same policy and boundary as
/// `!cmd`.
fn start_job(app: &mut App, ctx: &AppContext, command: &str) {
    let launch = launch_shell(ctx, command);
    say_if_unconfined(app, &launch.confinement);
    let started = app
        .jobs
        .start_with(&launch.program, &launch.args, command, &ctx.root);
    match started {
        Ok(id) => {
            app.sync_jobs();
            app.focus_pane(2);
            app.push(
                EntryRole::Info,
                format!("started {id}: {command}\n  /tasks logs {id} · /tasks cancel {id}"),
            );
        }
        Err(error) => app.push(EntryRole::Error, format!("{error:#}")),
    }
}

/// Print the tail of one job into the transcript.
fn push_job_logs(app: &mut App, id: &str) {
    let Some(job) = app.jobs.get(id) else {
        app.push(EntryRole::Error, format!("no job {id}"));
        return;
    };
    let tail = job.tail(40);
    let text = if tail.is_empty() {
        format!("{} · {} · no output yet", id, job.status.label())
    } else {
        format!("{} · {}\n{}", id, job.status.label(), tail.join("\n"))
    };
    app.push(EntryRole::Answer, text);
}

/// Stop one job, reporting which of "stopped" and "already finished" happened.
fn cancel_job(app: &mut App, id: &str) {
    match app.jobs.cancel(id) {
        Ok(true) => {
            app.sync_jobs();
            app.push(EntryRole::Info, format!("cancelled {id}"));
        }
        Ok(false) => app.push(EntryRole::Info, format!("{id} has already finished")),
        Err(error) => app.push(EntryRole::Error, format!("{error:#}")),
    }
}

/// Put the dropdown's highlighted command into the prompt.
///
/// A command that takes an argument stops with a trailing space rather than
/// being submitted and rejected for the one it is missing. Returns whether the
/// line should be run now, or `None` when nothing matched.
fn accept_slash(app: &mut App) -> Option<bool> {
    let (line, submit) = app.slash.accept()?;
    // The pick is the whole line, space included: a command that needs an
    // argument already carries the room for it, and a directory keeps its own
    // slash instead.
    app.input = line;
    app.cursor = app.input.chars().count();
    Some(submit)
}

/// Begin the device flow on a worker thread.
///
/// The poll blocks for up to a minute while someone finishes in the browser, so
/// it cannot run on the frame: a frozen screen reads as a hang. The code and
/// the URL arrive over the channel and land in the transcript.
/// End a pending sign-in. The waiting thread answers within a tick.
///
/// Two keys reach this: esc where the prompt has nothing else for it, and
/// `/login` a second time, which cancels in every state — a half-typed line and
/// a running turn both own esc and would otherwise leave no way out.
fn stop_signin(app: &mut App) {
    app.login_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    app.push(EntryRole::Info, "cancelling sign in…".to_string());
}

fn start_login(app: &mut App, ctx: &AppContext) {
    if app.login_busy {
        stop_signin(app);
        return;
    }
    if let Some(account) = &app.account {
        app.push(
            EntryRole::Info,
            format!(
                "{} · /logout first to use another account",
                login::status_line(Some(account))
            ),
        );
        return;
    }
    app.login_busy = true;
    app.login_started = Some(std::time::Instant::now());
    // A fresh flag per attempt: the previous one may still be set.
    app.login_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.push(
        EntryRole::Info,
        "sign in: asking memnoth cloud for a one-time code…".to_string(),
    );
    login::start(
        ctx.clone(),
        "memnoth-cli".to_string(),
        app.login_tx.clone(),
        app.login_cancel.clone(),
    );
}

/// Forget this machine's credentials, revoking the token best-effort.
///
/// The verb is the same one `mn auth logout` runs, so the two doors cannot
/// disagree about what signing out clears.
fn sign_out(app: &mut App, ctx: &AppContext) {
    let verb = Verb::Auth(crate::cli::AuthArgs {
        action: crate::cli::AuthCommand::Logout(crate::cli::AuthLogoutArgs {}),
    });
    match crate::commands::execute(ctx, &verb) {
        Ok(_) => {
            app.account = None;
            app.push(EntryRole::Info, "signed out".to_string());
        }
        Err(error) => app.push(EntryRole::Error, format!("sign out: {error:#}")),
    }
}

/// Put one overlay on the stack.
///
/// Every slash command that opens a modal comes through here, so "opening an
/// overlay" has exactly one definition.
fn open_overlay(app: &mut App, kind: crate::tui::overlays::OverlayKind) {
    // Opening is also reading: the model browser loads the catalogue, the
    // connect screen reads the keys, the team picker asks the org. None of them
    // can do that from `show`, which has no `App`.
    crate::tui::overlays::present(app, crate::tui::overlays::create_overlay(kind));
}

/// Run a verb without stopping the screen.
///
/// Every verb that reaches here talks to a store, a graph or a cloud, and any of
/// those can take long enough that a frozen frame is indistinguishable from a
/// hang. The work therefore runs on a thread and reports over a channel the run
/// loop drains: `/stats` used to block for up to a minute on two cloud calls.
fn run_verb(app: &mut App, ctx: &AppContext, prompt: &str, verb: Verb) {
    // Theme and screen reader are app state, not work: there is nothing to wait
    // for, and a thread would only make them land a frame late.
    match &verb {
        Verb::SetTheme(theme) => {
            app.theme = *theme;
            // The preference outlives the session: a theme chosen in the TUI is
            // the same `[ui] theme` a plain `mn` start reads. A read-only config
            // leaves the choice in force for this session and says so.
            app.config.ui.get_or_insert_default().theme = Some(theme.as_str().to_string());
            let saved = app.config.save(&app.config_path).is_ok();
            app.push(
                EntryRole::Info,
                format!(
                    "Theme set to {:?}{}",
                    theme,
                    if saved {
                        " · saved"
                    } else {
                        " · this session only"
                    }
                ),
            );
            return;
        }
        Verb::SetScreenReader(mode) => {
            app.screen_reader = *mode;
            app.config.ui.get_or_insert_default().screen_reader = Some(mode.as_str().to_string());
            let saved = app.config.save(&app.config_path).is_ok();
            app.push(
                EntryRole::Info,
                format!(
                    "Screen reader mode set to {:?}{}",
                    mode,
                    if saved {
                        " · saved"
                    } else {
                        " · this session only"
                    }
                ),
            );
            return;
        }
        _ => {}
    }

    app.push(EntryRole::User, prompt);
    // One at a time: two verbs in flight would interleave their answers in a
    // transcript where each entry is filed against the line that asked.
    if let Some(running) = app.running_verb.clone() {
        app.push(
            EntryRole::Info,
            format!("still running: {running} — wait for it, or quit and start again"),
        );
        return;
    }
    app.running_verb = Some(prompt.to_string());

    let ctx = ctx.clone();
    let prompt = prompt.to_string();
    let tx = app.verb_tx.clone();
    std::thread::spawn(move || {
        let result = crate::commands::execute(&ctx, &verb).map_err(|error| format!("{error:#}"));
        let _ = tx.send(crate::tui::app::VerbDone { prompt, result });
    });
}

/// Show a verb's answer, and file it against the line that asked for it.
fn finish_verb(app: &mut App, store: &SessionStore, session: &mut Session, done: VerbDone) {
    app.running_verb = None;
    match done.result {
        Ok(outcome) => {
            let text = answer_with_next(&outcome.text, &outcome.data);
            refresh_filing(app, &outcome);

            // A list-shaped answer opens a window rather than flattening a table
            // into prose. It renders the same `data` the pipe gets, so the two
            // doors cannot disagree about what the verb found; the full text is
            // still what the session file keeps, so a resume reads as before.
            match crate::tui::overlays::popover::PopoverOverlay::for_answer(
                outcome.command,
                &outcome.data,
            ) {
                Some(spec) => {
                    let summary = spec.transcript_line();
                    crate::tui::overlays::present(
                        app,
                        Box::new(crate::tui::overlays::PopoverOverlay::new(spec)),
                    );
                    app.push(
                        EntryRole::Answer,
                        format!(
                            "{summary}\n  opened in a window — up/down to browse, enter to act, esc to close"
                        ),
                    );
                    persist(app, store, session, &done.prompt, &text);
                }
                None => {
                    app.push(EntryRole::Answer, text.clone());
                    persist(app, store, session, &done.prompt, &text);
                }
            }
        }
        Err(text) => {
            app.push(EntryRole::Error, text.clone());
            persist(app, store, session, &done.prompt, &format!("error: {text}"));
        }
    }
}

/// Point the rail at what a projects verb just stored.
///
/// The rail is where a person sees which project their sessions are filed under,
/// so `/projects use acme` has to change it in the same breath rather than at the
/// next launch. The answer already carries the project it wrote, which is what
/// keeps this off the network: [`crate::projects::filed_under`] resolves a name
/// from the org, and a round trip here would put the cloud in front of the next
/// frame.
fn refresh_filing(app: &mut App, outcome: &crate::output::Outcome) {
    let name = outcome
        .data
        .get("project")
        .and_then(|project| project.get("name"))
        .and_then(|name| name.as_str());
    app.project = match outcome.command {
        "projects.use" => outcome
            .data
            .get("project_id")
            .and_then(|id| id.as_str())
            .map(|id| match name {
                Some(name) => format!("{name} ({id})"),
                None => id.to_string(),
            }),
        // A delete that cleared this machine's choice leaves nothing to name; a
        // delete of any other project left the choice alone.
        "projects.delete"
            if outcome.data.get("unset").and_then(|unset| unset.as_bool()) == Some(true) =>
        {
            None
        }
        _ => return,
    };
}

/// The answer plus the commands it suggests, rewritten to run in this session.
///
/// Commands emit `memnoth vault …`; here the same line is `/vault …`, so the
/// suggestion can be typed without the reader translating it first.
fn answer_with_next(text: &str, data: &serde_json::Value) -> String {
    let next: Vec<String> = crate::output::next_commands(data)
        .into_iter()
        .map(|command| match command.strip_prefix("memnoth ") {
            Some(rest) => format!("/{rest}"),
            None => command.to_string(),
        })
        .collect();
    if next.is_empty() {
        return text.to_string();
    }
    let mut out = text.trim_end().to_string();
    out.push_str("\n\nnext\n");
    for command in next {
        out.push_str("  ");
        out.push_str(&command);
        out.push('\n');
    }
    out
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
    // `app.config`, not `ctx.config`: the screens write the app's copy and save
    // it, so a model picked in `/models` is the model the next turn uses. Reading
    // the startup snapshot here is what used to make a switch need a restart.
    match provider::Settings::load(&app.config) {
        Ok(settings) if settings.ready() => start_turn(app, ctx, session, prompt, settings),
        Ok(settings) => {
            app.push(
                EntryRole::Info,
                format!(
                    "no model key ({}): answering from the vault and the code graph.\n  \
                     export it, then ask again — /connect (or /models) shows what this session would use.",
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
    // Taken before the worker starts: the turn owns these bytes, and a second
    // turn must not silently resend what the first already carried.
    let attachments = std::mem::take(&mut app.attachments);
    app.turn_started_ms = crate::session::now_ms();
    // A failed turn may have left its stream indices pointing at entries
    // that are never going to finish; a new turn starts its own segments.
    app.stream_answer = None;
    app.stream_answer_first = None;
    app.stream_think = None;
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

    // Counted before the worker starts: the transcript is read on this thread.
    // The id travels with the events so a socket consumer can file them under
    // the same session the screen is writing.
    let session_id = session.id.clone();
    let turn_id = session
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .count();

    std::thread::spawn(move || {
        // Every write this turn attempts comes back through `asker` and stops
        // here until the screen answers.
        let mut tools = provider::Toolbox::new(&worker_ctx)
            .with_approver(Box::new(AskOnScreen { tx: asker }))
            .with_turn_id(turn_id);
        let notes = tx.clone();
        let events = tx.clone();
        let mut emit = move |event: memnoth_harness_api::ApiEvent| {
            let _ = events.send(TurnEvent::Api(event));
        };
        let mut on_note = |line: &str| {
            let _ = notes.send(TurnEvent::Note(line.to_string()));
        };
        let result = provider::run_turn_attached(
            &settings,
            &root,
            &history,
            &asked_on_worker,
            &attachments,
            &mut tools,
            &mut on_note,
            &session_id,
            &mut emit,
        );
        // The screen needs the answer and what was written, not the toolbox:
        // the toolbox borrows this worker's context and cannot outlive it.
        let writes = tools.take_writes();
        let approvals = tools.take_approvals();
        let _ = tx.send(TurnEvent::Done(Box::new(
            result.map(|answer| (answer, writes, approvals)),
        )));
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
    tools: usize,
    result: TurnOutcome,
) {
    match result {
        Ok((answer, writes, approvals)) => {
            let (input, output) = answer.usage();
            let elapsed = crate::session::now_ms() - app.turn_started_ms;
            let cost = busy
                .pricing
                .map(|pricing| pricing.cost_micros(input, output));
            let stats = crate::tui::app::turn_stats(elapsed, input, output, cost, tools);
            // What was read is what was streamed: the finished turn replaces
            // the first segment's text with the authoritative answer, so the
            // screen and the session file hold the same words. Later segments
            // — the text that arrived after the tool rows — fold back into
            // that first block: one answer, read as one answer, with the pace
            // line under it, instead of the same words in two places.
            if let Some(first) = app.stream_answer_first
                && let Some(entry) = app.entries.get_mut(first)
            {
                entry.text = answer.text.clone();
                entry.stats = stats;
                let turn_start = app.turn_started_ms;
                let extra: Vec<usize> = app
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(index, entry)| {
                        *index > first
                            && entry.role == crate::tui::app::EntryRole::Answer
                            && entry.at_ms >= turn_start
                    })
                    .map(|(index, _)| index)
                    .collect();
                for index in extra.into_iter().rev() {
                    app.entries.remove(index);
                }
            } else {
                app.push(EntryRole::Answer, answer.text.clone());
                if let Some(entry) = app.entries.last_mut() {
                    entry.stats = stats;
                }
            }
            app.finish_stream();
            // The file records the model that actually answered, so a resumed
            // session does not claim a model that never ran.
            session.model = busy.model.clone();
            session.account(input, output);

            // Add checkpoint for this turn
            let turn_id = session
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count();
            session.add_checkpoint(turn_id, format!("turn {}", turn_id));

            // Record write receipts from the turn
            for write in writes {
                session.record_write(write);
            }

            // And the decisions that authorised them. A write in the file with
            // no matching approval row is now a visible anomaly rather than the
            // normal state of every session.
            for approval in approvals {
                session.record_approval(approval);
            }

            // One receipt per turn is also what makes the pane's cost honest:
            // `Session::spend` sums priced receipts and counts the unpriced ones
            // separately, rather than treating an unknown price as free.
            session.record_receipt(TurnReceipt {
                at_ms: now_ms(),
                total_tokens: answer.context_tokens,
                // The prefix + tail uniquely identifies the prompt bytes the
                // model saw, so replay can reproduce them identically.
                prefix_hash: format!("{:016x}", answer.prefix_hash),
                layer_tokens: answer.layer_tokens,
                pieces: answer.pieces,
                input_tokens: input,
                output_tokens: output,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_micros: busy
                    .pricing
                    .map(|pricing| pricing.cost_micros(input, output)),
                prompt: answer.prompt.clone(),
                turn_id: Some(turn_id),
            });
            // What it cost and how it was reached, under the answer: the line
            // that separates an answer from a claim. Both numbers are known
            // here, so neither is guessed. Hidden for now: the row is a
            // receipt, not a status bar, and the receipt is already in the
            // session file. One switch brings it back.
            const SHOW_EVIDENCE: bool = false;
            if SHOW_EVIDENCE {
                let total = input.saturating_add(output);
                app.push(
                    EntryRole::Info,
                    format!(
                        "evidence · {} {} · {} tokens",
                        tools,
                        if tools == 1 { "tool" } else { "tools" },
                        human_count(total.min(usize::MAX as u64) as usize)
                    ),
                );
            }
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
/// is always "which vault did it open" — and it puts what this workspace is
/// still missing at the top of `next`, so the screen is a next step rather than
/// a splash.
///
/// The first row that needs no setup is always in the list, even on a machine
/// where everything is missing: a briefing that only lists prerequisites tells
/// a new person the product does not work yet.
///
/// `session` is the conversation this process is recording. Its size is shown
/// rather than assumed absent: a launch always starts here, so the screen has
/// to say that `esc` opens something — and how much of it there is.
fn home(
    ctx: &AppContext,
    name: &str,
    resumed: bool,
    graph: &GraphStatus,
    session: &Session,
    first_run: bool,
) -> Home {
    let turns = session.turns().len();
    let (model, model_ready) = model_state(ctx);
    let identity = crate::auth::signed_in_identity();

    let mut facts = vec![Fact {
        label: "workspace",
        value: display_path(&ctx.root),
        state: HomeState::Ready,
    }];
    facts.push(vault_fact(ctx));
    facts.push(Fact {
        label: "graph",
        value: graph.value.clone(),
        state: if !graph.indexed {
            HomeState::Missing
        } else if graph.stale > 0 {
            HomeState::Stale
        } else {
            HomeState::Ready
        },
    });
    facts.push(Fact {
        label: "model",
        value: model.clone(),
        state: if model_ready {
            HomeState::Ready
        } else {
            HomeState::Missing
        },
    });

    // What this machine has done, in the two numbers a person can act on: how
    // many conversations exist to come back to, and what this one has cost.
    // Both are reads of what is already on disk — the brief never counts
    // something it did not measure.
    let conversations = session_file_count(ctx);
    facts.push(Fact {
        label: "sessions",
        value: if turns == 0 {
            format!("{conversations} on disk · this one is new")
        } else {
            format!("{conversations} on disk · this one {turns} turn(s)")
        },
        state: HomeState::Ready,
    });
    facts.push(Fact {
        label: "spend",
        value: spend_line(ctx, session),
        state: HomeState::Ready,
    });
    match memnoth_sandbox::profiles::load_sandbox_config(&ctx.root).and_then(|config| {
        memnoth_sandbox::backend::BackendPlan::build(
            memnoth_sandbox::backend::OsBackend::host(),
            &memnoth_sandbox::profiles::ProfileName::Workspace,
            &ctx.root,
            &config,
        )
    }) {
        Ok(plan) => {
            let status = memnoth_sandbox::status::status(&plan);
            facts.push(Fact {
                label: "sandbox",
                value: status.statusline(),
                state: if status.report.mode == "enforced" {
                    HomeState::Ready
                } else {
                    HomeState::Missing
                },
            });
        }
        Err(_) => {
            facts.push(Fact {
                label: "sandbox",
                value: "not enforced — policy refused".to_string(),
                state: HomeState::Missing,
            });
        }
    }

    // The team is not a mode: it is the organisation the cached account is
    // already in. Until shared sync ships, the row says where the work is.
    match &identity {
        Some(id) if id.in_team() => facts.push(Fact {
            label: "team",
            value: teams_line(ctx, id),
            state: HomeState::Ready,
        }),
        _ => facts.push(Fact {
            label: "cloud",
            value: if identity.is_some() {
                "connected · local only".to_string()
            } else {
                "not connected · /login".to_string()
            },
            state: HomeState::Ready,
        }),
    }

    let mut steps: Vec<Step> = Vec::new();
    let mut setup = |command: &str, description: &str| {
        steps.push(Step {
            command: command.to_string(),
            description: description.to_string(),
            runnable: true,
        });
    };
    if !ctx.vault_path.exists() {
        setup("/vault init", "beliefs with dates, kept on this machine");
    }
    if !graph.indexed {
        setup(
            "/kg index",
            "index the workspace so code questions have answers",
        );
    }
    if !model_ready {
        setup("/model", "wire a model so questions run as turns");
    }
    // Setup takes at most two rows; the third always works, on a fresh machine
    // as much as a warm one.
    steps.truncate(2);
    steps.push(Step {
        command: "/search".to_string(),
        description: "answer from both stores, no model needed".to_string(),
        runnable: true,
    });
    // Only worth a row once there is something to come back to: a picker that
    // opens on one empty conversation is a door into an empty room.
    if conversations > 1 {
        steps.push(Step {
            command: "/sessions".to_string(),
            description: "pick up a conversation you left".to_string(),
            runnable: true,
        });
    }
    if first_run {
        // Prepend rather than replace: a vault or graph that is already set up
        // keeps its row, and the panel takes the first two slots so the whole
        // thing is three lines.
        steps.insert(
            0,
            Step {
                command: "/login".to_string(),
                description: "connect the cloud (optional)".to_string(),
                runnable: true,
            },
        );
        steps.insert(
            0,
            Step {
                command: "/doctor".to_string(),
                description: "check this machine".to_string(),
                runnable: true,
            },
        );
        steps.truncate(3);
    }

    Home {
        identity: identity_line(identity.as_ref(), name, resumed),
        facts,
        steps,
        // Turns, not messages: a briefing that says "6 earlier turns" for three
        // questions and three answers is off by the half people count.
        hidden_turns: turns,
        invitation: if first_run {
            Some("start working — type a question".to_string())
        } else {
            model_ready.then_some("a turn with the vault and the graph as tools".to_string())
        },
        recent: recent_activity(ctx, name, session),
    }
}

/// `solo · not signed in · session "payments" (new)`, or the three facts a
/// team account adds. The version rides on the line above.
fn identity_line(identity: Option<&crate::auth::Identity>, name: &str, resumed: bool) -> String {
    let mut who = match identity {
        Some(id) if id.in_team() => format!("{} (team) · {}", id.org_name.trim(), id.email),
        Some(id) => format!("solo · {}", id.email),
        None => "solo · not signed in".to_string(),
    };
    if let Some(id) = identity
        && id.in_team()
        && !id.role.trim().is_empty()
    {
        who.push_str(" · ");
        who.push_str(id.role.trim());
    }
    who.push_str(&format!(
        " · session \"{name}\" ({})",
        if resumed { "resumed" } else { "new" }
    ));
    who
}

/// The vault row: counters when it exists, the word `missing` when it does not.
///
/// `18 closed` is `entities_total - entities_current`: the beliefs this vault
/// stopped standing behind. It is the number that makes the vault a vault
/// rather than a notes file, and it costs one subtraction on counters the
/// engine already returns.
fn vault_fact(ctx: &AppContext) -> Fact {
    if !ctx.vault_path.exists() {
        return Fact {
            label: "vault",
            value: "missing".to_string(),
            state: HomeState::Missing,
        };
    }
    let value = match crate::vault::open_existing(ctx) {
        Ok(engine) => match engine.stats() {
            Ok(stats) => {
                let closed = (stats.entities_total - stats.entities_current).max(0);
                format!(
                    "{} beliefs · {} closed · local only",
                    thousands(stats.entities_current.max(0) as u64),
                    thousands(closed as u64)
                )
            }
            Err(_) => "unreadable · /doctor".to_string(),
        },
        Err(_) => "unreadable · /doctor".to_string(),
    };
    let state = if value.starts_with("unreadable") {
        HomeState::Missing
    } else {
        HomeState::Ready
    };
    Fact {
        label: "vault",
        value,
        state,
    }
}

/// How many conversations exist on disk.
///
/// Counted from file names, not from parsed transcripts: the brief is rebuilt
/// after every command, and `SessionStore::list` reads every session to derive
/// its title — the wrong price to pay for one number.
fn session_file_count(ctx: &AppContext) -> usize {
    let Ok(entries) = std::fs::read_dir(&ctx.sessions_dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .count()
}

/// `148k tokens · ~$1.24`, or the truth that this conversation has not run yet.
///
/// The same numbers the context pane reports, from the same receipts, so the
/// brief and the rail cannot quote two different bills.
fn spend_line(ctx: &AppContext, session: &Session) -> String {
    let tokens = session.input_tokens.saturating_add(session.output_tokens);
    if tokens == 0 {
        return "no turns yet".to_string();
    }
    let tokens = thousands(tokens);
    match session_cost_micros(ctx, session) {
        Some(0) => format!("{tokens} tokens · free"),
        Some(micros) => format!("{tokens} tokens · ~${:.2}", micros as f64 / 1_000_000.0),
        // No rate for this model: the token count is still a fact, the bill is
        // not, and inventing one would put a number nobody measured on screen.
        None => format!("{tokens} tokens"),
    }
}

/// The rate that applies to this conversation: the configured one when it
/// describes the same model, otherwise the built-in list price for the model.
///
/// A configured pair is what this deployment actually pays, so it outranks a
/// published list price — but only for the model it was written for. An older
/// conversation on another model is priced by that model's own family.
fn session_rate(ctx: &AppContext, session: &Session) -> Option<provider::Pricing> {
    if let Ok(settings) = provider::Settings::load(&ctx.config)
        && settings.model == session.model
        && let Some(pricing) = settings.pricing
    {
        return Some(pricing);
    }
    provider::builtin_price(&session.model)
}

/// What this session has cost, in micro-dollars, when it can be known.
///
/// Receipts written before a model had a price carry no `cost_micros`, so the
/// rate in force is applied to their token counts here instead of leaving an
/// older conversation permanently "cost unknown". `None` means the model has no
/// price at all — the one answer that is not a number.
///
/// The brief and the context rail both call this, so the two cannot quote
/// different bills for the same conversation.
fn session_cost_micros(ctx: &AppContext, session: &Session) -> Option<u64> {
    if session.receipts.is_empty() {
        return None;
    }
    let rate = session_rate(ctx, session);
    let mut total = 0u64;
    for receipt in &session.receipts {
        total += match receipt.cost_micros {
            Some(micros) => micros,
            None => rate?.cost_micros(receipt.input_tokens, receipt.output_tokens),
        };
    }
    Some(total)
}

/// The team row. Presence and claims are local (one `coord.db`), so the row
/// says `on this machine` rather than implying two laptops agree.
fn teams_line(ctx: &AppContext, id: &crate::auth::Identity) -> String {
    let mut line = id.org_name.trim().to_string();
    if let Ok(coord) = memnoth_coord::Coord::open(&ctx.coord_path())
        && let Ok(rows) = coord.status()
    {
        let active: Vec<_> = rows.iter().filter(|row| !row.stale).collect();
        let claims: usize = active.iter().map(|row| row.active_claims.len()).sum();
        if active.is_empty() {
            line.push_str(" · on this machine");
        } else {
            line.push_str(&format!(
                " · {} session(s) · {claims} claimed · on this machine",
                active.len()
            ));
        }
    } else {
        line.push_str(" · on this machine");
    }
    line
}

/// The last three things this vault took in, as `(when, row)`.
///
/// The timestamp is returned rather than formatted because the activity strip
/// merges this with conversations and sorts the result: an age printed before
/// the sort would be the age of a row that is no longer in its place.
fn vault_recent(ctx: &AppContext) -> Vec<(i64, String)> {
    let Ok(engine) = crate::vault::open_existing(ctx) else {
        return Vec::new();
    };
    let Ok(mut stmt) = engine.connection().prepare(
        "SELECT e.name, o.content, o.valid_from
         FROM observations o JOIN entities e ON e.id = o.entity_id
         WHERE o.valid_to IS NULL
         ORDER BY o.valid_from DESC, o.id DESC
         LIMIT 3",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };
    rows.filter_map(Result::ok)
        .map(|(entity, content, at)| {
            (
                at,
                format!(
                    "{entity}   {}",
                    content.split('\n').next().unwrap_or_default().trim()
                ),
            )
        })
        .collect()
}

/// The last three things that happened, newest first.
///
/// A home screen with no activity on it is a header: the facts say what exists
/// and `next` says what to do, but neither says the tool has been used. Three
/// sources answer that, all already on disk — the vault's own timeline, the
/// conversations this machine has open, and when this one last ran. Nothing is
/// counted here that was not measured.
fn recent_activity(ctx: &AppContext, name: &str, session: &Session) -> Vec<String> {
    let now = now_ms();
    let mut rows: Vec<(i64, String)> = Vec::new();
    if ctx.vault_path.exists() {
        rows.extend(vault_recent(ctx));
    }
    // Two conversations, not three: this session's own row is usually the
    // newest, and three files plus it would push the vault off the strip.
    for summary in SessionStore::new(ctx.sessions_dir.clone()).recent(2) {
        if summary.name == name || summary.title.is_empty() {
            continue;
        }
        rows.push((
            summary.modified_ms,
            format!("{}   {}", summary.name, summary.title),
        ));
    }
    if let Some(last) = session
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, Role::User))
    {
        let title = crate::session::title_of(session);
        rows.push((
            last.at_ms,
            if title.is_empty() {
                "this session".to_string()
            } else {
                format!("this session   {title}")
            },
        ));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    rows.into_iter()
        .take(3)
        // The age is a column, so it is padded: three rows of ragged ages read
        // as three unrelated lines rather than as one timeline.
        .map(|(at, row)| format!("{:<9} {row}", human_age(at, now)))
        .collect()
}

/// `1204` as `1,204`: a file count is read, not abbreviated.
fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The model row, and whether a turn can actually run: the same settings
/// `doctor` prints, reduced to what fits on one line.
///
/// The value is the whole sentence — model, then `ready` or the key that is
/// missing — because a fact row may not carry its own fix (§2.1).
pub(crate) fn model_state(ctx: &AppContext) -> (String, bool) {
    match provider::Settings::load(&ctx.config) {
        Ok(settings) if settings.ready() => (format!("{} · ready", settings.label()), true),
        Ok(settings) => (
            format!(
                "{} · no key — export {}",
                settings.model,
                settings.key_hint()
            ),
            false,
        ),
        Err(_) => ("not configured · /model".to_string(), false),
    }
}

/// What the graph row says, and whether it is there at all.
#[derive(Debug, Clone, Default)]
pub(crate) struct GraphStatus {
    /// `1,204 files · 9,830 symbols · 12 stale · 2m old`, or `missing`.
    value: String,
    /// Whether there is a graph to query.
    indexed: bool,
    /// Files changed since the last index.
    stale: u64,
    /// What the header says when this needs a person, with the verb that fixes
    /// it. `None` when there is nothing to act on — decided here, beside the
    /// facts, so the header never has to parse `value` back apart.
    attention: Option<Attention>,
}

/// The graph row, taken from the same `kg status` JSON `--json` prints, plus
/// how long ago the graph was last written.
///
/// The age is the field that makes `/kg index` worth remembering: `12 stale`
/// with no date is a number, `12 stale · 2m old` is a decision.
pub(crate) fn graph_state(ctx: &AppContext) -> GraphStatus {
    let verb = Verb::Kg(KgArgs {
        action: KgCommand::Status,
    });
    match crate::commands::execute(ctx, &verb) {
        Ok(outcome) if outcome.data["indexed"].as_bool().unwrap_or(false) => {
            let stale = outcome.data["stale_files"].as_u64().unwrap_or(0);
            let mut value = format!(
                "{} files · {} symbols · {} stale",
                thousands(outcome.data["files"].as_u64().unwrap_or(0)),
                thousands(outcome.data["symbols"].as_u64().unwrap_or(0)),
                thousands(stale),
            );
            if let Some(age) = file_age(&ctx.graph_path) {
                value.push_str(&format!(" · {age}"));
            }
            GraphStatus {
                value,
                indexed: true,
                stale,
                attention: (stale > 0).then(|| Attention {
                    label: format!("graph {stale} stale"),
                    fix: "/kg index".to_string(),
                }),
            }
        }
        Ok(_) => GraphStatus {
            value: "missing".to_string(),
            indexed: false,
            stale: 0,
            attention: Some(Attention {
                label: "graph not indexed".to_string(),
                fix: "/kg index".to_string(),
            }),
        },
        Err(_) => GraphStatus {
            value: "unavailable · /doctor".to_string(),
            indexed: false,
            stale: 0,
            attention: Some(Attention {
                label: "graph unavailable".to_string(),
                fix: "/doctor".to_string(),
            }),
        },
    }
}

/// How long ago a file was written, in the words the fact rows use.
fn file_age(path: &std::path::Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let ms = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(human_age(ms, now_ms()))
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
        Err(_) => out.push_str("  missing\n  /vault init\n"),
    }

    // Coordination is its own database in the state directory — the same one
    // `mn team` reads. Opening is skipped until it exists so a status screen
    // never creates a board as a side effect.
    if ctx.coord_path().exists() {
        out.push_str("\ncoordination\n");
        match memnoth_coord::Coord::open(&ctx.coord_path()) {
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
        match session_cost_micros(ctx, session) {
            // A free model has a price: zero. Saying so beats a prompt to
            // configure rates the provider does not charge.
            Some(0) => out.push_str("  free\n"),
            // Labelled an estimate because the rate is a list price or a
            // configured one, not an invoice: cache reads and provider
            // rounding are not modelled here.
            Some(micros) => out.push_str(&format!(
                "  ~${:.2} (estimate)\n",
                micros as f64 / 1_000_000.0
            )),
            None => out.push_str("  cost unknown — set provider rates\n"),
        }
    }

    out
}

/// Make `switch` the conversation this screen is recording.
///
/// Everything that identifies the session moves together — the name in the
/// header, the resumed flag, the transcript and the panes built from it — so
/// the frame after this cannot describe two conversations at once. The store is
/// the only writer: `/new` writes its file immediately (a session that exists
/// only in memory is a name the picker cannot show), and opening one reads the
/// file rather than trusting the row it was chosen from.
fn switch_session(
    ctx: &AppContext,
    app: &mut App,
    store: &SessionStore,
    session: &mut Session,
    switch: SessionSwitch,
) -> Result<()> {
    match switch {
        SessionSwitch::New(requested) => {
            let name = if requested.trim().is_empty() {
                store.next_name()
            } else {
                validate_name(&requested)?
            };
            let fresh = store.create(&name)?;
            store.save(&fresh)?;
            *session = fresh;
            app.name = name;
            app.resumed = false;
            app.entries.clear();
            // A fresh conversation has nothing to show yet, so it shows the
            // brief — the same screen a launch gives, naming the new session.
            app.home_visible = true;
            // A conversation the person asked for is a conversation on their
            // dashboard, whether or not a turn has run in it yet.
            crate::report::started(ctx, &app.name, session);
        }
        SessionSwitch::Open(name) => {
            *session = store.load(&name)?;
            app.name = session.id.clone();
            app.resumed = true;
            app.entries.clear();
            app.seed(session);
            // The conversation is what was picked, so it is what is shown.
            app.home_visible = false;
            // Resuming a conversation re-posts its own id, so the plane updates
            // the row it already has rather than opening a second one.
            crate::report::started(ctx, &app.name, session);
        }
        SessionSwitch::Rename { from, to } => {
            let renamed = store.rename(&from, &to)?;
            // The file moved, so the writer moves with it: `Session::id` is the
            // file name, and leaving it behind would recreate the old path on
            // the next save.
            if app.name == from {
                session.id = renamed.clone();
                app.name = renamed.clone();
            }
            app.push(
                EntryRole::Info,
                format!("renamed \"{from}\" to \"{renamed}\""),
            );
        }
    }
    app.home_cursor = 0;
    app.scroll = 0;
    app.follow = true;
    refresh(ctx, app, session);
    Ok(())
}

/// The plan a shell command runs under, or why there is none.
///
/// One function for `!cmd` and `/tasks run`, so the two cannot disagree about
/// which profile is in force. A profile that cannot be resolved is a refusal:
/// an unenforced shell is worse than none.
fn shell_plan(ctx: &AppContext) -> anyhow::Result<memnoth_sandbox::backend::BackendPlan> {
    // An unparseable `sandbox.toml` is a refusal, not an empty policy: the
    // deny lists it carries are protection, and a typo must not drop them.
    let config = load_sandbox_config(&ctx.root)?;
    memnoth_sandbox::backend::BackendPlan::build(
        memnoth_sandbox::backend::OsBackend::host(),
        &ProfileName::Workspace,
        &ctx.root,
        &config,
    )
}

/// What the rule set says about one command line.
///
/// `~/.memnoth/rules.txt` is parsed here, and a rule file that does not parse is
/// an error rather than an empty rule set: a typo must not quietly downgrade
/// the policy to the built-in defaults.
fn decide_shell(ctx: &AppContext, cmd: &str) -> anyhow::Result<RuleDecision> {
    let rules_path = ctx.home.join("rules.txt");
    let user_rules = match std::fs::read_to_string(&rules_path) {
        Ok(text) => memnoth_sandbox::rules::parse_rules(&text)
            .map_err(|error| anyhow::anyhow!("{}: {error:#}", rules_path.display()))?,
        Err(_) => Vec::new(),
    };
    let rule_set = RuleSet::with_user_rules(user_rules);
    Ok(decide(&rule_set, "Bash", cmd))
}

/// One command, ready to spawn, with the boundary that will (or will not) hold it.
struct Launch {
    program: String,
    args: Vec<String>,
    confinement: memnoth_sandbox::Confinement,
}

/// Decide the argv for a command line and confine it where this machine can.
///
/// The plan is applied here rather than in the child, because there is no child
/// yet: `bwrap` *is* the child, and its argv is the boundary.
fn launch_shell(ctx: &AppContext, cmd: &str) -> Launch {
    let (program, args) = jobs::shell_argv(cmd);
    let mut argv = vec![program.clone()];
    argv.extend(args.iter().cloned());
    let confinement = match shell_plan(ctx) {
        Ok(plan) => memnoth_sandbox::confine(&plan, &ctx.root, &argv),
        Err(error) => memnoth_sandbox::Confinement::Unavailable {
            reason: format!("{error:#}"),
        },
    };
    match confinement.confined_argv() {
        Some((wrapped, wrapped_args)) => Launch {
            program: wrapped,
            args: wrapped_args,
            confinement,
        },
        None => Launch {
            program,
            args,
            confinement,
        },
    }
}

/// Print the boundary once per session, before the first command that has none.
///
/// Once, and not under every command: a warning repeated on each line is one the
/// reader learns to skip, and the answer is the same every time.
fn say_if_unconfined(app: &mut App, confinement: &memnoth_sandbox::Confinement) {
    if confinement.is_enforced() || app.sandbox_notice_shown {
        return;
    }
    app.sandbox_notice_shown = true;
    app.push(EntryRole::Info, confinement.describe());
}

/// Execute a shell command with sandbox policy checking.
fn run_shell(app: &mut App, ctx: &AppContext, cmd: &str) {
    app.push(EntryRole::User, format!("!{}", cmd));

    let decision = match decide_shell(ctx, cmd) {
        Ok(decision) => decision,
        Err(error) => {
            app.push(
                EntryRole::Error,
                format!("sandbox: {error:#}\n  the command was not run"),
            );
            return;
        }
    };

    match decision {
        RuleDecision::Allow { .. } => execute_shell_command(app, ctx, cmd),
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
            // Parked for confirmation. The question itself is the modal in
            // `ui::draw`, not a transcript line: a line can scroll away, and a
            // second copy of the same sentence under the box read as noise.
            app.pending_shell = Some(PendingShell {
                command: cmd.to_string(),
                decision: RuleDecision::Ask {
                    reason: reason.clone(),
                    rule: rule.clone(),
                    destructive,
                    kind,
                },
                background: false,
            });
        }
    }
}

/// Actually execute the shell command and capture output.
fn execute_shell_command(app: &mut App, ctx: &AppContext, cmd: &str) {
    use std::process::Command;
    let launch = launch_shell(ctx, cmd);
    say_if_unconfined(app, &launch.confinement);
    let output = Command::new(&launch.program).args(&launch.args).output();
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

/// Project memory (the vault) as a hub: its real state, then the three ways to
/// change it.
///
/// It answered "not yet implemented" while the palette advertised it — the
/// broken promise W13-31 names. The numbers come from `vault::status`, so this
/// screen and `mn vault status` cannot disagree.
fn run_memory(app: &mut App, ctx: &AppContext) {
    app.push(EntryRole::User, "/memory".to_string());
    match crate::vault::status(ctx) {
        Ok(outcome) => {
            for line in outcome.text.lines() {
                app.push(EntryRole::Info, line.to_string());
            }
            app.push(
                EntryRole::Info,
                "next\n  /vault remember <fact>   /vault status   /vault forget <id>".to_string(),
            );
        }
        Err(err) => {
            app.push(EntryRole::Error, err.message.clone());
            if let Some(fix) = err.fix.clone() {
                app.push(EntryRole::Info, format!("fix: {fix}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Config;

    #[test]
    fn next_commands_are_rewritten_for_the_session() {
        let data = serde_json::json!({
            "next_commands": ["memnoth vault status", "memnoth kg index"]
        });
        let text = answer_with_next("wrote 2 facts", &data);
        assert!(text.starts_with("wrote 2 facts"), "{text}");
        assert!(text.contains("\nnext\n  /vault status"), "{text}");
        assert!(text.contains("  /kg index"), "{text}");
    }

    #[test]
    fn an_answer_with_no_follow_ups_is_left_alone() {
        let data = serde_json::json!({});
        assert_eq!(answer_with_next("done", &data), "done");
    }

    /// A question typed on the brief is a new conversation, not a message in
    /// the file the brief happens to be standing in front of.
    #[test]
    fn a_prompt_typed_on_the_brief_is_a_new_conversation_not_the_old_one() {
        let mut app = App::new("payments", true);
        app.home = Some(Home::default());
        // Order matters: an entry clears `home_visible`, which is the rule that
        // keeps the brief from covering an answer. The state under test is the
        // one `/home` and a launch produce: history, with the brief in front.
        app.push(EntryRole::User, "the earlier question");
        app.home_visible = true;
        assert!(
            prompt_wants_fresh_chat(&app),
            "the brief over a resumed file must not absorb the new question"
        );

        app.entries.clear();
        assert!(
            !prompt_wants_fresh_chat(&app),
            "an empty conversation is already fresh: switching would create a second empty file"
        );
    }

    /// A verb answers from its own thread, and the screen is usable until it does.
    ///
    /// `/stats` used to run inline with two cloud calls at a 30-second timeout
    /// each, so the frame stopped until the network did — which is reported as
    /// "hangs", not as "slow".
    #[test]
    fn a_verb_answers_without_freezing_the_screen() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let store = SessionStore::new(ctx.sessions_dir.clone());
        let (mut session, _) = store.load_or_create("s").unwrap();
        let mut app = App::new("s", false);

        run_verb(
            &mut app,
            &ctx,
            "/kg status",
            Verb::Kg(crate::cli::KgArgs {
                action: crate::cli::KgCommand::Status,
            }),
        );
        assert_eq!(app.running_verb.as_deref(), Some("/kg status"));

        // A second verb is refused by name rather than queued: two answers would
        // be filed against one prompt, or against none.
        run_verb(
            &mut app,
            &ctx,
            "/vault status",
            Verb::Vault(crate::cli::VaultArgs {
                action: crate::cli::VaultCommand::Status,
            }),
        );
        assert_eq!(app.running_verb.as_deref(), Some("/kg status"));
        assert!(
            app.entries
                .iter()
                .any(|entry| entry.text.contains("still running")),
            "the refusal has to be on screen, not silent"
        );

        let done = wait_for_verb(&mut app);
        assert_eq!(done.prompt, "/kg status", "the answer knows its question");
        finish_verb(&mut app, &store, &mut session, done);
        assert_eq!(
            app.running_verb, None,
            "the slot frees when the answer lands"
        );
        assert!(
            app.entries.iter().any(|entry| {
                matches!(entry.role, EntryRole::Answer | EntryRole::Error)
                    && !entry.text.trim().is_empty()
            }),
            "the answer is in the transcript"
        );
    }

    fn wait_for_verb(app: &mut App) -> VerbDone {
        for _ in 0..500 {
            if let Some(done) = app.try_recv_verb() {
                return done;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the verb never answered on its thread");
    }

    fn context(root: &std::path::Path) -> AppContext {
        AppContext {
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
    fn the_brief_reports_a_fresh_workspace_and_always_leaves_a_runnable_row() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let graph = graph_state(&ctx);
        let session = Session::new(crate::session::SESSION_MODEL);
        let home = home(&ctx, "payments", false, &graph, &session, false);

        // Not the `solo` prefix: whether this machine is signed in is the
        // machine's business, and a test that fails on the developer's account
        // tells nobody anything. What the brief must always say is which
        // session it opened.
        assert!(
            home.identity.contains("session \"payments\" (new)"),
            "{}",
            home.identity
        );

        let vault = home.facts.iter().find(|fact| fact.label == "vault");
        let vault = vault.expect("the vault row is always there");
        assert_eq!(vault.value, "missing");
        assert_eq!(vault.state, HomeState::Missing);

        // A briefing that only lists prerequisites tells a new person the
        // product does not work yet.
        assert!(
            home.steps.iter().any(|step| step.runnable),
            "{:?}",
            home.steps
        );
        assert!(
            home.steps.iter().any(|step| step.command == "/vault init"),
            "{:?}",
            home.steps
        );
        assert!(home.recent.is_empty(), "a fresh vault has no history");
    }

    #[test]
    fn the_onboarding_card_has_four_state_derived_rows() {
        // A developer's exported API keys make `Settings::load` report a ready
        // model, and the card would name an example instead of the unlock. A
        // fresh machine has no keys in the environment either: strip them.
        let stripped: Vec<(&str, Option<String>)> = [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "KILO_API_KEY",
            "MEMNOTH_PROVIDER",
            "MEMNOTH_MODEL",
        ]
        .iter()
        .map(|name| (*name, std::env::var(name).ok()))
        .collect();
        unsafe {
            for (name, _) in &stripped {
                std::env::remove_var(name);
            }
        }
        let rows = {
            let dir = tempfile::tempdir().unwrap();
            let ctx = context(dir.path());
            crate::tui::overlays::onboarding::rows_for(&ctx)
        };
        unsafe {
            for (name, value) in &stripped {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert_eq!(rows[0].label, "Ask");
        // A fresh machine has no model, so the row names the unlock rather
        // than an example that cannot run.
        assert_eq!(rows[0].example, "/model");
        assert_eq!(rows[1].label, "Remember");
        assert_eq!(rows[2].label, "Prove");
        assert_eq!(rows[3].label, "Delegate");
        assert_eq!(
            rows[3].action,
            crate::tui::overlays::onboarding::RowAction::Explain
        );
    }

    #[test]
    fn the_first_run_panel_is_three_lines() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let graph = graph_state(&ctx);
        let session = Session::new(crate::session::SESSION_MODEL);

        let fresh = home(&ctx, "first", false, &graph, &session, true);
        assert_eq!(fresh.steps.len(), 3, "{:?}", fresh.steps);
        assert_eq!(fresh.steps[0].command, "/doctor");
        assert_eq!(fresh.steps[1].command, "/login");
        assert_eq!(
            fresh.invitation.as_deref(),
            Some("start working — type a question")
        );

        // The same machine after `ui.onboarded = true`: the panel is gone and
        // the brief is the ordinary one.
        let later = home(&ctx, "second", false, &graph, &session, false);
        assert!(
            later.steps.iter().all(|step| step.command != "/doctor"),
            "{:?}",
            later.steps
        );
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
            prompt: String::new(),
            cost_micros: cost,
            turn_id: None,
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

    #[test]
    fn an_older_turn_is_priced_from_the_rate_in_force() {
        // Receipts written before a model had a price carry no `cost_micros`.
        // The rate is still known, so the bill is shown rather than leaving the
        // conversation permanently "cost unknown".
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let mut session = Session::new("claude-sonnet-4-5");
        session.account(1_000_000, 0);
        session.record_receipt(receipt(1_000_000, 0, None));

        let pane = context_pane(&ctx, &session);
        assert!(pane.contains("~$3.00 (estimate)"), "{pane}");
        assert!(
            spend_line(&ctx, &session).contains("~$3.00"),
            "{}",
            spend_line(&ctx, &session)
        );
    }

    #[test]
    fn a_configured_rate_prices_an_older_turn_on_that_model() {
        // `[provider]` is what this deployment actually pays, so it outranks the
        // published list price for the model it was written for.
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context(dir.path());
        ctx.config.provider = Some(crate::provider::ProviderConfig {
            model: Some("claude-sonnet-4-5".to_string()),
            input_per_mtok: Some(1.0),
            output_per_mtok: Some(2.0),
            ..Default::default()
        });
        let mut session = Session::new("claude-sonnet-4-5");
        session.account(1_000_000, 0);
        session.record_receipt(receipt(1_000_000, 0, None));

        let pane = context_pane(&ctx, &session);
        assert!(pane.contains("~$1.00 (estimate)"), "{pane}");
        assert!(
            spend_line(&ctx, &session).contains("~$1.00"),
            "{}",
            spend_line(&ctx, &session)
        );
    }

    #[test]
    fn a_free_model_says_free_rather_than_asking_for_rates() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let mut session = Session::new("kilo-auto/free");
        session.account(6_000, 228);
        session.record_receipt(receipt(6_000, 228, None));

        let pane = context_pane(&ctx, &session);
        assert!(pane.contains("free"), "{pane}");
        assert!(!pane.contains("cost unknown"), "{pane}");
        assert!(
            spend_line(&ctx, &session).ends_with("free"),
            "{}",
            spend_line(&ctx, &session)
        );
    }

    #[test]
    fn a_background_job_asks_the_same_question_as_a_foreground_one() {
        // The rail used to start whatever it was given. A job is a process with
        // the person's permissions, so it asks first — and the box has to know
        // the answer puts it on the rail, not in the transcript.
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let mut app = App::new("s", false);
        run_tasks(
            &mut app,
            &ctx,
            TasksCommand::Run("rm -rf build".to_string()),
        );
        let pending = app.pending_shell.expect("a destructive job asks first");
        assert!(pending.background, "yes starts a job, not a foreground run");
        assert!(
            pending.prompt().contains("background job"),
            "{}",
            pending.prompt()
        );
        assert!(
            app.jobs.get("job-1").is_none(),
            "nothing started unapproved"
        );
    }

    #[test]
    fn a_denied_command_never_becomes_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let mut app = App::new("s", false);
        run_tasks(
            &mut app,
            &ctx,
            TasksCommand::Run("sudo rm -rf /tmp/x".to_string()),
        );
        let last = app.entries.last().expect("a line saying why");
        assert!(format!("{last:?}").contains("denied"), "{last:?}");
        assert!(app.pending_shell.is_none(), "a deny is not a question");
        assert!(app.jobs.get("job-1").is_none());
    }

    #[test]
    fn the_activity_strip_names_this_session_and_the_conversations_beside_it() {
        // A home screen with no activity on it is a header: the strip is the
        // only row that says the tool has been used.
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let store = SessionStore::new(&ctx.sessions_dir);
        let mut other = store.create("payments").unwrap();
        store
            .record(
                &mut other,
                "index the payments service",
                "indexed 412 files",
            )
            .unwrap();

        let mut session = Session::new("claude-sonnet-4-5");
        session.id = "session".to_string();
        session.messages.push(memnoth_session::Message::new(
            Role::User,
            "why does the retry cap reset after a restart",
        ));

        let rows = recent_activity(&ctx, "session", &session);
        assert!(rows.len() <= 3, "{rows:?}");
        assert!(rows.iter().any(|row| row.contains("payments")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("this session")),
            "{rows:?}"
        );
    }

    /// One keystroke through the real key handler, which is where the dropdown
    /// and the prompt agree or disagree.
    fn press(app: &mut App, code: KeyCode) {
        handle_key(
            app,
            KeyEvent::new(code, KeyModifiers::NONE),
            std::path::Path::new("."),
        );
    }

    /// One keystroke with Alt held: the agent key layer.
    fn press_alt(app: &mut App, code: KeyCode) {
        handle_key(
            app,
            KeyEvent::new(code, KeyModifiers::ALT),
            std::path::Path::new("."),
        );
    }

    #[test]
    fn alt_a_opens_the_deck_and_alt_t_the_trace_without_typing() {
        let mut app = App::new("s", false);
        let empty = format!("{:?}", app.overlay_stack);

        press_alt(&mut app, KeyCode::Char('a'));
        let after_deck = format!("{:?}", app.overlay_stack);
        assert_ne!(empty, after_deck, "alt-a opens the agent deck");

        press_alt(&mut app, KeyCode::Char('t'));
        let after_trace = format!("{:?}", app.overlay_stack);
        assert_ne!(after_deck, after_trace, "alt-t stacks the trace");

        // Neither key is a letter: the prompt is unchanged, so a chord cannot
        // half-open a screen and half-type a message.
        assert!(app.input.is_empty(), "{:?}", app.input);
    }

    #[test]
    fn ctrl_digits_switch_surfaces_and_name_the_focus_owner() {
        let mut app = App::new("s", false);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('3'), KeyModifiers::CONTROL),
            std::path::Path::new("."),
        );
        assert_eq!(app.surface, crate::tui::surface::Surface::Agents);
        // The status line names the owner the surface calls itself: "agent",
        // not the surface's plural title.
        assert_eq!(app.focus_owner, "agent");
        assert!(app.input.is_empty(), "a surface key is not a character");
    }

    #[test]
    fn a_slash_opens_the_dropdown_and_the_arrows_walk_it() {
        let mut app = App::new("s", false);
        press(&mut app, KeyCode::Char('/'));
        assert!(app.slash.is_open(), "a slash lists the commands");

        let first = app.slash.selected_index();
        press(&mut app, KeyCode::Down);
        assert_ne!(
            app.slash.selected_index(),
            first,
            "down moves the highlight, not the history"
        );
        press(&mut app, KeyCode::Up);
        assert_eq!(app.slash.selected_index(), first, "up comes back");

        // Navigating the list must never run the line it describes.
        assert_eq!(app.input, "/");
        assert!(!app.pending_submit);
    }

    #[test]
    fn tab_fills_the_prompt_and_enter_runs_it() {
        let mut app = App::new("s", false);
        for ch in "/help".chars() {
            press(&mut app, KeyCode::Char(ch));
        }

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/help", "the pick lands in the prompt");
        assert!(!app.pending_submit, "a completion key never runs the line");
        assert!(!app.slash.is_open(), "the pick closes the list");

        press(&mut app, KeyCode::Enter);
        assert!(app.pending_submit, "enter on a finished line runs it");
    }

    /// ctrl-g, which the shared `press` helper cannot express (it sends no
    /// modifiers).
    fn press_ctrl(app: &mut App, ch: char) {
        handle_key(
            app,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL),
            std::path::Path::new("."),
        );
    }

    #[test]
    fn ctrl_g_runs_the_fix_the_header_names() {
        let mut app = App::new("s", false);
        app.graph_attention = Some(Attention {
            label: "graph not indexed".to_string(),
            fix: "/kg index".to_string(),
        });
        press_ctrl(&mut app, 'g');
        assert_eq!(app.input, "/kg index", "the fix lands in the prompt");
        assert!(app.pending_submit, "and runs the way a typed line does");
    }

    #[test]
    fn ctrl_g_leaves_a_half_written_line_alone() {
        let mut app = App::new("s", false);
        app.graph_attention = Some(Attention {
            label: "graph not indexed".to_string(),
            fix: "/kg index".to_string(),
        });
        app.input = "/vau".to_string();
        press_ctrl(&mut app, 'g');
        assert_eq!(app.input, "/vau", "typing is never overwritten");
        assert!(!app.pending_submit, "and nothing runs unasked");
    }

    #[test]
    fn esc_ends_a_pending_sign_in_only_when_the_prompt_has_nothing_for_it() {
        // Nothing typed, no turn, no brief: the wait is the thing in front of
        // you, and esc is the key that cancels what is in front of you.
        let mut app = App::new("s", false);
        app.login_busy = true;
        press(&mut app, KeyCode::Esc);
        assert!(
            app.login_cancel.load(std::sync::atomic::Ordering::Relaxed),
            "the wait was asked to stop"
        );

        // A half-typed line owns esc: it clears the line and leaves the sign-in
        // alone, which `/login` still cancels.
        let mut app = App::new("s", false);
        app.login_busy = true;
        app.input = "fix the parser".to_string();
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input, "", "esc still clears the line");
        assert!(
            !app.login_cancel.load(std::sync::atomic::Ordering::Relaxed),
            "a line in the prompt means the wait keeps going"
        );
    }

    #[test]
    fn esc_closes_the_dropdown_and_leaves_the_line_to_fix() {
        let mut app = App::new("s", false);
        for ch in "/vau".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert!(app.slash.is_open());

        press(&mut app, KeyCode::Esc);
        assert!(!app.slash.is_open());
        assert_eq!(
            app.input, "/vau",
            "closing the list is not clearing the line"
        );
    }
}
