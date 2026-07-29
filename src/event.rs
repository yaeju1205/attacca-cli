//! Main event loop — the heart of the TUI.
//!
//! Initialises the terminal, runs the draw→input→bg drain→action drain loop,
//! and cleans up on exit.

use crate::api;
use crate::app::{Action, App, BgEvent};
use crate::bg;
use crate::handler;
use crate::tools::parse_tools;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
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

    let mut stdout = io::stdout();
    if crossterm::execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture)
        .is_err()
    {
        eprintln!("alt screen failed");
        terminal::disable_raw_mode().ok();
        return;
    }

    let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("term init failed");
            terminal::disable_raw_mode().ok();
            return;
        }
    };

    let _ = term.clear();

    // ── Initial data load ──
    load_initial_data(app).await;
    app.add_msg(
        "sys",
        "── attacca ── enter:send  tab:autocomplete  y/n:tool  read/find/grep auto ──",
    );

    // ── Main loop ──
    loop {
        // Draw
        if term.draw(|f| crate::ui::draw(f, app)).is_err() {
            break;
        }
        if app.exit_requested {
            break;
        }

        // Drain background events
        drain_bg_events(app);

        // Drain actions
        drain_actions(app);

        // Consume all pending terminal events (non-blocking spin)
        let had_event = consume_events(app);

        if app.exit_requested {
            break;
        }

        // Brief sleep when idle to avoid 100 % CPU
        if !had_event {
            tokio::time::sleep(Duration::from_millis(8)).await;
        }
    }

    // ── Cleanup ──
    terminal::disable_raw_mode().ok();
    let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture);
}

// ── Initial data loading ───────────────────────────────────────

async fn load_initial_data(app: &mut App) {
    // Load sessions, projects, and user info concurrently
    let (sessions, projects, (user_name, _email, user_plan, user_credits)) = tokio::join!(
        api::fetch_sessions(&app.transport),
        api::fetch_projects(&app.transport),
        api::whoami_detailed(&app.transport),
    );

    app.sessions = sessions
        .into_iter()
        .map(|s| (s.project_id, s.title, s.id))
        .collect();
    app.project_names = projects;
    app.user_name = user_name;
    app.user_plan = user_plan;
    app.user_credits = user_credits;

    // Auto-expand first project + "All"
    if let Some(first) = app.sessions.first() {
        if !first.0.is_empty() {
            app.expanded_projects.insert(first.0.clone());
        }
    }
    app.expanded_projects.insert(String::new());

    handler::rebuild_sidebar(app);
}

// ── Background event drain ─────────────────────────────────────

fn drain_bg_events(app: &mut App) {
    while let Ok(ev) = app.bg_rx.try_recv() {
        match ev {
            BgEvent::NewMsgs { sid, msgs, new_cur } => {
                if app.sid.as_deref() != Some(&sid) {
                    continue;
                }
                app.cursor = new_cur;
                for m in msgs {
                    match m.role.as_str() {
                        "assistant" => {
                            let (clean, tools) = parse_tools(&m.text);
                            if !clean.is_empty() {
                                app.upsert_msg(m.cursor, "agent", &clean);
                            }
                            for j in tools {
                                // Safe tools auto-execute without y/n approval
                                if crate::tools::is_safe_tool(&j) {
                                    let result = crate::tools::exec_tool(&j);
                                    app.add_msg("result", &result);
                                    if app.sid.is_some() {
                                        let transport = app.transport.clone();
                                        let tx = app.bg_tx.clone();
                                        let sid = app.sid.clone().unwrap();
                                        app.inc_busy();
                                        crate::bg::poll_after_tool(transport, tx, sid, result);
                                    }
                                } else {
                                    app.add_tool_msg(&j);
                                }
                            }
                        }
                        "user" => {
                            if !m.text.is_empty() {
                                app.upsert_msg(m.cursor, "user", &m.text);
                            }
                        }
                        _ => {
                            if !m.text.is_empty() {
                                app.upsert_msg(m.cursor, "sys", &m.text);
                            }
                        }
                    }
                }
            }
            BgEvent::Done { sid } => {
                if app.sid.as_deref() == Some(&sid) || app.sid.is_none() {
                    app.dec_busy();
                }
                // Auto-refresh usage after the session finishes
                if !app.busy() && app.sid.is_some() {
                    let transport = app.transport.clone();
                    let tx = app.bg_tx.clone();
                    let sid = app.sid.clone().unwrap();
                    tokio::spawn(async move {
                        let v = api::get_session_usage(&transport, &sid).await;
                        if !v.is_null() {
                            let _ = tx.send(BgEvent::Usage(v));
                        }
                    });
                }
            }
            BgEvent::SessionCreated(sid) => {
                if app.sid.is_none() {
                    app.sid = Some(sid);
                }
            }
            BgEvent::Usage(v) => {
                app.usage_model = api::field_val(&v["model"]);
                app.usage_credits_used = api::field_val(&v["credits_used"]);
                app.usage_context_tokens = api::field_val(&v["context_tokens"]);
                app.usage_input_tokens = api::field_val(&v["input_tokens"]);
                app.usage_output_tokens = api::field_val(&v["output_tokens"]);
                app.usage_total_tokens = api::field_val(&v["total_tokens"]);

                if let Some(pid) = v["project_id"].as_str().filter(|s| !s.is_empty()) {
                    let name = app
                        .project_names
                        .get(pid)
                        .cloned()
                        .unwrap_or_else(|| format!("📁 {}", crate::tools::short(pid)));
                    app.current_project_name = name;
                }
            }
        }
    }
}

// ── Action drain ───────────────────────────────────────────────

fn drain_actions(app: &mut App) {
    while !app.actions.is_empty() {
        let action = app.actions.remove(0);
        let is_send = matches!(&action, Action::Send(_));
        if is_send && app.busy() {
            app.actions.insert(0, action);
            break;
        }
        app.inc_busy();
        match action {
            Action::Send(msg) => {
                let transport = app.transport.clone();
                let tx = app.bg_tx.clone();
                let sid = app.sid.clone();
                let is_first = app.first;
                if is_first {
                    app.first = false;
                }
                bg::spawn_send(transport, tx, msg, sid, is_first);
            }
            Action::Open(sid) => {
                app.sid = Some(sid.clone());
                app.cursor = 0;
                app.first = false;
                app.msgs.clear();
                app.scroll = 0;
                app.at_end = true;
                app.add_msg("sys", &format!("loading {}", crate::tools::short(&sid)));

                // Set project name from sessions
                app.current_project_name = app
                    .sessions
                    .iter()
                    .find(|(_, _, id)| id == &sid)
                    .and_then(|(pid, _, _)| {
                        if pid.is_empty() {
                            None
                        } else {
                            Some(
                                app.project_names
                                    .get(pid)
                                    .cloned()
                                    .unwrap_or_else(|| format!("📁 {}", crate::tools::short(pid))),
                            )
                        }
                    })
                    .unwrap_or_default();

                handler::rebuild_sidebar(app);

                let transport = app.transport.clone();
                let tx = app.bg_tx.clone();
                bg::spawn_open(transport, tx, sid);
            }
            Action::Create => {
                app.sid = None;
                app.first = true;
                app.msgs.clear();
                app.scroll = 0;
                app.at_end = true;
                app.add_msg("sys", "creating session…");

                let transport = app.transport.clone();
                let tx = app.bg_tx.clone();
                bg::spawn_create(transport, tx);
            }
            Action::Login(key) => {
                app.transport.key = key.clone();
                let transport = app.transport.clone();
                let tx = app.bg_tx.clone();
                bg::spawn_login(transport, tx, key);
            }
            Action::ShowInfo => {
                let transport = app.transport.clone();
                let tx = app.bg_tx.clone();
                let sid = app.sid.clone();
                bg::spawn_show_info(transport, tx, sid);
            }
            Action::ToolResult(result) => {
                let transport = app.transport.clone();
                let tx = app.bg_tx.clone();
                let sid = app.sid.clone().unwrap();
                bg::poll_after_tool(transport, tx, sid, result);
            }
        }
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
                            && !handler::handle_key(app, k.code)
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
