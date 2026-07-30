//! Main event loop — the heart of the TUI.
//!
//! Initialises the terminal, runs the draw→bg drain→action drain→input loop,
//! and cleans up on exit.

use crate::app::{title_in, Action, App, BgEvent, MsgKind};
use crate::bg;
use crate::handler;
use crate::util::short;
use crate::zyris_client::missing_scopes;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use std::io;
use std::time::Duration;

/// Launch the TUI event loop. This function owns the terminal for its
/// entire lifetime and returns only when the user exits.
pub async fn run(app: &mut App) {
    // ── Terminal setup ──
    if terminal::enable_raw_mode().is_err() {
        eprintln!("raw mode: not a terminal");
        return;
    }

    if enter_screen().is_err() {
        eprintln!("alt screen failed");
        terminal::disable_raw_mode().ok();
        return;
    }

    let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout())) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("term init failed");
            leave_screen();
            terminal::disable_raw_mode().ok();
            return;
        }
    };

    let _ = term.clear();

    app.push_sys("── attacca ── enter:send  shift+enter:newline  tab:autocomplete  /help ──");
    app.rebuild_sidebar();

    // ── Main loop ──
    // Redraws only when something actually changed. `chat_lines` re-wraps the whole transcript
    // from scratch, and drawing unconditionally on every tick meant a long session paid that cost
    // ~125 times a second even sitting perfectly idle — pinning a CPU core for as long as a chat
    // stayed open. `dirty` starts `true` so the initial frame still paints.
    let mut dirty = true;
    loop {
        if dirty {
            if term.draw(|f| crate::ui::draw(f, app)).is_err() {
                break;
            }
            dirty = false;
        }
        if app.exit_requested {
            break;
        }

        // `/login` has to give the terminal back to a person, so it happens here rather than in
        // the action drain — this module is the one that owns the `Terminal`.
        if app.login_requested {
            app.login_requested = false;
            relogin(app, &mut term).await;
            dirty = true;
        }

        // Drain background events
        if drain_bg_events(app) {
            dirty = true;
        }

        // Drain actions
        if drain_actions(app) {
            dirty = true;
        }

        // Consume all pending terminal events (non-blocking spin)
        let had_event = consume_events(app);
        if had_event {
            dirty = true;
        }

        if app.exit_requested {
            break;
        }

        // Brief sleep when idle to avoid 100 % CPU
        if !dirty {
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
    }

    // ── Cleanup ──
    app.stop_stream();
    terminal::disable_raw_mode().ok();
    leave_screen();
}

/// Enter the alternate screen.
///
/// `DISAMBIGUATE_ESCAPE_CODES` is the Kitty keyboard protocol, and it is what makes Shift+Enter
/// report as `Enter + SHIFT` rather than a bare `Enter` — see the newline arm in
/// [`handler`](crate::handler).
fn enter_screen() -> io::Result<()> {
    crossterm::execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )
}

fn leave_screen() {
    let _ = crossterm::execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        PopKeyboardEnhancementFlags,
    );
}

/// Re-enroll this node, interactively.
///
/// The alternate screen is dropped for the duration because the device grant prints its code with
/// `println!` — that is deliberate on zyris's part so the code survives `RUST_LOG=error`, and it
/// means the only way to show it is to give the terminal back. The wait is unbounded by design: it
/// ends when a person approves the code, or gives up.
async fn relogin<B: ratatui::backend::Backend + io::Write>(app: &mut App, term: &mut Terminal<B>) {
    terminal::disable_raw_mode().ok();
    leave_screen();
    println!("\n── attacca: re-authenticating ──");
    println!("   asking for: {}", app.auth.scopes().join(", "));

    let outcome = app.auth.relogin().await;

    terminal::enable_raw_mode().ok();
    let _ = enter_screen();
    term.clear().ok();

    match outcome {
        Ok(prefix) => {
            app.push_sys(&format!("logged in — credential {prefix}…"));
            // The runner only reads credentials when it dials, so the live connection has to go for
            // the new one to be presented. It redials after its backoff on its own.
            if let Some(live) = app.slot.get() {
                live.conn.close("re-authenticating");
                app.push_sys("reconnecting…");
            }
        }
        Err(e) => {
            app.push_sys(&format!("login failed: {e}"));
            app.push_sys("still connected on the previous credential, if there was one");
        }
    }
}

// ── Background event drain ─────────────────────────────────────

fn drain_bg_events(app: &mut App) -> bool {
    let mut any = false;
    while let Ok(ev) = app.bg_rx.try_recv() {
        any = true;
        match ev {
            BgEvent::Connected(me) => {
                app.connected = true;
                app.push_sys(&format!(
                    "connected — {} <{}>",
                    me.display_name, me.email
                ));
                let missing = missing_scopes(&me);
                if !missing.is_empty() {
                    // Worth naming: without `events:read` there is no turn feed at all, and the
                    // only other symptom is a chat that never produces anything.
                    app.push_sys(&format!(
                        "grant is missing {} — run /login to re-authorize",
                        missing.join(", ")
                    ));
                }
                app.me = Some(*me);
            }
            BgEvent::Disconnected(reason) => {
                app.connected = false;
                app.push_sys(&format!("disconnected — {reason}"));
            }
            BgEvent::Projects(projects) => {
                app.project_names.clear();
                app.project_order.clear();
                for p in projects {
                    if p.is_default {
                        app.expanded_projects.insert(p.id.clone());
                    }
                    app.project_order.push(p.id.clone());
                    app.project_names.insert(p.id, p.name);
                }
                app.sync_current_project();
                app.rebuild_sidebar();
            }
            BgEvent::Sessions(sessions) => {
                app.replace_sessions(sessions);
                // The list replaces the rows wholesale, and its `running` is only true as of when
                // the request was made. The turn feed is more current, so it wins.
                app.sync_open_row();
            }
            BgEvent::Agents(agents) => {
                app.agents = agents;
            }
            BgEvent::SessionCreated(session) => {
                app.push_sys(&format!("new session {}", short(&session.id)));
                let id = session.id.clone();
                app.insert_session(*session);
                attach(app, id);
            }
            BgEvent::StreamHead {
                session_id,
                running,
            } => {
                if app.sid.as_deref() == Some(session_id.as_str()) {
                    app.chat.running = running;
                    app.sync_open_row();
                }
            }
            BgEvent::Frame { session_id, frame } => {
                // A stream task is aborted on session switch, but a frame already in the channel
                // can outlive the abort — so the session it belongs to is checked here too.
                if app.sid.as_deref() != Some(session_id.as_str()) {
                    continue;
                }
                // Read out of the frame before the reducer consumes it.
                let title = title_in(&frame);
                let was_running = app.chat.running;

                app.chat.apply_frame(frame, app.debug_events);

                if let Some(title) = title {
                    app.retitle_open_row(&title);
                }
                if app.chat.running != was_running {
                    app.sync_open_row();
                    if was_running && !app.chat.running {
                        // A turn ending is the moment a server-side auto-title lands, and the only
                        // push signal available for "the session list may have moved on" — there is
                        // no account-wide event stream in `attacca_api` v1 to subscribe to instead.
                        app.inc_busy();
                        bg::refresh_sessions(&app.slot, &app.bg_tx);
                        if let Some(sid) = app.sid.clone() {
                            app.inc_busy();
                            bg::session_usage(&app.slot, &app.bg_tx, sid, true);
                        }
                    }
                }
            }
            BgEvent::Usage(usage) => {
                app.apply_usage(&usage);
            }
            BgEvent::NodeStopped {
                message,
                needs_operator,
            } => {
                app.connected = false;
                app.push_sys(&format!("node stopped — {message}"));
                if needs_operator {
                    app.push_sys("this needs a person: try /login, or check ZYRIS_SERVER_URL");
                }
                app.node_stopped = Some(message);
                app.node_needs_operator = needs_operator;
                app.exit_requested = true;
            }
            BgEvent::Notice(text) => {
                app.push_sys(&text);
            }
            BgEvent::Done => {
                app.dec_busy();
            }
        }
    }
    any
}

/// Point the UI at a session and start its turn feed.
fn attach(app: &mut App, sid: String) {
    app.attach_session(sid.clone());
    app.stream = Some(bg::spawn_session_stream(
        sid,
        app.slot.clone(),
        app.bg_tx.clone(),
    ));
}

// ── Action drain ───────────────────────────────────────────────

fn drain_actions(app: &mut App) -> bool {
    let mut any = false;
    // Gated on one-shot requests only, not on `busy()`: with streaming, `busy()` stays true for the
    // whole of a turn, and a follow-up message would sit in the queue until it ended.
    while !app.requests_in_flight() && !app.actions.is_empty() {
        any = true;
        let action = app.actions.remove(0);
        app.inc_busy();
        match action {
            Action::Send(text) => {
                let sid = app.sid.clone();
                // Resolved only when a session actually has to be created, so the project and agent
                // diagnostics are not reprinted on every message of an ongoing conversation.
                let spec = match sid {
                    Some(_) => None,
                    None => app.new_session_spec(),
                };
                bg::send(&app.slot, &app.bg_tx, text, sid, spec);
            }
            Action::Open(sid) => {
                app.reset_for_session(&sid);
                attach(app, sid.clone());
                // Attaching spawns the feed, not a request, so the drain's increment is spent on
                // the usage fetch instead — quietly, since the user asked to open a session, not
                // to be told what this deployment does not support.
                bg::session_usage(&app.slot, &app.bg_tx, sid, true);
            }
            Action::Create => {
                // Clear before the request, and stop the old feed first: a stream still running for
                // the previous session would repopulate the transcript we just emptied.
                app.stop_stream();
                app.sid = None;
                app.chat = crate::app::Transcript::new();
                app.scroll = 0;
                app.at_end = true;
                app.push_sys("creating session…");
                app.sync_current_project();
                app.rebuild_sidebar();

                let spec = app.new_session_spec();
                bg::create_session(&app.slot, &app.bg_tx, spec);
            }
            Action::Cancel => match app.sid.clone() {
                Some(sid) => bg::cancel_turn(&app.slot, &app.bg_tx, sid),
                None => {
                    app.push_sys("no session to cancel");
                    app.dec_busy();
                }
            },
            Action::Logout => {
                let auth = app.auth.clone();
                bg::logout(&app.bg_tx, auth);
            }
            Action::RefreshSessions => bg::refresh_sessions(&app.slot, &app.bg_tx),
            Action::ShowInfo => {
                show_account(app);
                match app.sid.clone() {
                    Some(sid) => bg::session_usage(&app.slot, &app.bg_tx, sid, false),
                    None => {
                        app.push_sys("no session open");
                        app.dec_busy();
                    }
                }
            }
        }
    }
    any
}

/// The account half of `/usage`, which comes from `me` and needs no request.
fn show_account(app: &mut App) {
    app.push_sys("── Account ───────────────────────────");
    match &app.me {
        Some(me) => {
            let lines = [
                format!("  User:     {} <{}>", me.display_name, me.email),
                match &me.plan {
                    Some(plan) => format!("  Plan:     {plan}"),
                    None => String::new(),
                },
                match &me.credits {
                    Some(c) => format!("  Credits:  {c}"),
                    None => String::new(),
                },
            ];
            for line in lines.into_iter().filter(|l| !l.is_empty()) {
                app.chat.push(MsgKind::Sys, &line);
            }
        }
        None => app.push_sys("  not connected yet"),
    }
}

// ── Terminal event consumption ─────────────────────────────────

fn consume_events(app: &mut App) -> bool {
    let mut had_event = false;
    loop {
        match event::poll(Duration::from_secs(0)) {
            Ok(true) => {
                had_event = true;
                match event::read() {
                    Ok(Event::Key(k)) => {
                        if k.code == KeyCode::Char('c')
                            && k.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            app.exit_requested = true;
                            break;
                        }
                        if (k.kind == crossterm::event::KeyEventKind::Press
                            || k.kind == crossterm::event::KeyEventKind::Repeat)
                            && !handler::handle_key(app, k.code, k.modifiers)
                        {
                            app.exit_requested = true;
                            break;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        handler::handle_mouse(app, m.column, m.row, m.kind);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        app.exit_requested = true;
                        break;
                    }
                }
            }
            Ok(false) => break,
            Err(_) => {
                app.exit_requested = true;
                break;
            }
        }
        if app.exit_requested {
            break;
        }
    }
    had_event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BgTx;
    use crate::brief::NodeBrief;
    use crate::zyris_client::ApiSlot;
    use std::sync::Arc;

    fn app() -> App {
        let (tx, rx): (BgTx, _) = tokio::sync::mpsc::unbounded_channel();
        let auth = Arc::new(crate::auth::Authenticator::new(
            Arc::new(crate::auth::SwappableCredentials::new(Arc::new(
                zyris::runtime::StaticToken::new("znt_test"),
            ))),
            "wss://example.test/zyris/v1/ws".into(),
            "default".into(),
            "test-node".into(),
            "test".into(),
            vec![],
        ));
        App::new(
            tx,
            rx,
            ApiSlot::new(),
            auth,
            NodeBrief {
                node_name: "test-node".into(),
                file_root: "/tmp".into(),
                terminal: false,
            },
        )
    }

    /// The action queue gates on one-shot requests, not on `busy()`. Without the split, a turn's
    /// whole duration would hold back the next message.
    #[tokio::test]
    async fn send_defers_until_the_previous_request_settles() {
        let mut app = app();
        app.actions.push(Action::Send("first".into()));
        app.actions.push(Action::Send("second".into()));

        // Not connected, so `bg::rpc` posts a notice and a `Done` without spawning; the increment
        // in the drain is still outstanding until that `Done` is drained.
        drain_actions(&mut app);
        assert_eq!(app.actions.len(), 1, "the second send must wait");
        assert!(app.requests_in_flight());

        drain_bg_events(&mut app);
        assert!(!app.requests_in_flight(), "Done must balance the increment");

        drain_actions(&mut app);
        assert!(app.actions.is_empty());
    }

    /// A streaming turn must not block the queue the way an in-flight request does.
    #[tokio::test]
    async fn a_running_turn_does_not_gate_the_action_queue() {
        let mut app = app();
        app.chat.running = true;
        assert!(app.busy(), "the spinner should show a running turn");
        assert!(
            !app.requests_in_flight(),
            "but the queue must stay open during one"
        );

        app.actions.push(Action::Send("during a turn".into()));
        drain_actions(&mut app);
        assert!(app.actions.is_empty(), "the send must go out");
    }
}
