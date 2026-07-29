//! Keyboard, mouse, and sidebar selection handling.
//!
//! All input processing goes here — no API calls, no rendering, no background
//! tasks. Side effects are enqueued as [`Action`]s and drained by the event loop.

use crate::app::{Action, App, Focus, SidebarItem};
use crate::tools::{exec_tool, short};
use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};
use std::collections::BTreeMap;

/// Process a keyboard event. Returns `false` on exit request.
pub fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    // y/n tool approval — intercept before focus dispatch
    if let KeyCode::Char(c) = code {
        if (c == 'y' || c == 'Y') && app.has_pending_tool() {
            approve_tool(app, true);
            return true;
        }
        if (c == 'n' || c == 'N') && app.has_pending_tool() {
            approve_tool(app, false);
            return true;
        }
        if c == '/' && app.focus == Focus::Sidebar {
            app.focus = Focus::Chat;
        }
    }

    // Tab: autocomplete cycle or focus toggle
    if code == KeyCode::Tab {
        if app.focus == Focus::Chat && !app.autocomplete_suggestions.is_empty() {
            cycle_autocomplete(app);
        } else {
            app.focus = match app.focus {
                Focus::Chat => Focus::Sidebar,
                Focus::Sidebar => Focus::Chat,
            };
        }
        return true;
    }

    match app.focus {
        Focus::Sidebar => handle_sidebar(app, code),
        Focus::Chat => handle_chat(app, code, modifiers),
    }
}

/// Process a mouse event.
pub fn handle_mouse(app: &mut App, column: u16, row: u16, kind: MouseEventKind) {
    match kind {
        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
            if column < 30 {
                app.focus = Focus::Sidebar;
                let list_row = row.saturating_sub(2) as usize;
                let idx = list_row + app.sidebar_scroll;
                if idx < app.sidebar_items.len() {
                    app.sidebar_sel = idx;
                    activate_sidebar_selection(app);
                }
            } else {
                app.focus = Focus::Chat;
            }
        }
        MouseEventKind::ScrollDown => {
            const S: usize = 3;
            if column < 30 {
                app.sidebar_scroll = app
                    .sidebar_scroll
                    .saturating_add(S)
                    .min(app.sidebar_items.len().saturating_sub(1));
                let mv = 12usize.min(app.sidebar_items.len());
                if app.sidebar_sel < app.sidebar_scroll {
                    app.sidebar_sel = app.sidebar_scroll;
                }
                if app.sidebar_sel >= app.sidebar_scroll + mv {
                    app.sidebar_sel = app.sidebar_scroll + mv - 1;
                }
            } else if !app.at_end {
                if app.scroll > S {
                    app.scroll -= S;
                } else {
                    app.at_end = true;
                    app.scroll = 0;
                }
            }
        }
        MouseEventKind::ScrollUp => {
            const S: usize = 3;
            if column < 30 {
                app.sidebar_scroll = app.sidebar_scroll.saturating_sub(S);
                if app.sidebar_sel >= app.sidebar_scroll + 12 {
                    app.sidebar_sel = app.sidebar_scroll + 11;
                }
            } else if app.at_end {
                app.at_end = false;
                app.scroll = S;
            } else {
                app.scroll = app.scroll.saturating_add(S);
            }
        }
        _ => {}
    }
}

// ── Sidebar ────────────────────────────────────────────────────

fn handle_sidebar(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Up => {
            if app.sidebar_sel > 0 {
                app.sidebar_sel -= 1;
                clamp_sidebar_scroll(app);
            }
        }
        KeyCode::Down => {
            let max = app.sidebar_items.len().saturating_sub(1);
            if app.sidebar_sel < max {
                app.sidebar_sel += 1;
                clamp_sidebar_scroll(app);
            }
        }
        KeyCode::Enter | KeyCode::Right => {
            if app.sidebar_sel < app.sidebar_items.len() {
                activate_sidebar_selection(app);
            }
        }
        KeyCode::Left => {
            for i in (0..app.sidebar_sel).rev() {
                if matches!(&app.sidebar_items[i], SidebarItem::ProjectHeader { .. }) {
                    if let SidebarItem::ProjectHeader { id, .. } = &app.sidebar_items[i].clone()
                    {
                        app.expanded_projects.remove(id);
                        rebuild_sidebar(app);
                        app.sidebar_sel = i;
                    }
                    break;
                }
            }
        }
        _ => {}
    }
    true
}

fn clamp_sidebar_scroll(app: &mut App) {
    let vis = 12usize;
    if app.sidebar_sel < app.sidebar_scroll {
        app.sidebar_scroll = app.sidebar_sel;
    } else if app.sidebar_sel >= app.sidebar_scroll + vis {
        app.sidebar_scroll = app.sidebar_sel.saturating_sub(vis) + 1;
    }
}

fn activate_sidebar_selection(app: &mut App) {
    if app.sidebar_sel >= app.sidebar_items.len() {
        return;
    }
    match app.sidebar_items[app.sidebar_sel].clone() {
        SidebarItem::ProjectHeader {
            id,
            ref expanded, ..
        } => {
            if *expanded {
                app.expanded_projects.remove(&id);
            } else {
                app.expanded_projects.insert(id.clone());
            }
            rebuild_sidebar(app);
        }
        SidebarItem::Session { id, .. } => {
            app.actions.push(Action::Open(id));
        }
        SidebarItem::NewSession => {
            app.actions.push(Action::Create);
        }
    }
}

/// Rebuild the sidebar from the current sessions and project data.
pub fn rebuild_sidebar(app: &mut App) {
    let mut project_map: BTreeMap<String, (String, Vec<(String, String)>)> = BTreeMap::new();
    for (pid, title, id) in &app.sessions {
        let entry = project_map.entry(pid.clone()).or_insert_with(|| {
            let name = if pid.is_empty() {
                "📁 All".into()
            } else {
                let known = app.project_names.get(pid).cloned().unwrap_or_default();
                if known.is_empty() {
                    format!("📁 {}", short(pid))
                } else {
                    format!("📁 {} ({})", known, short(pid))
                }
            };
            (name, vec![])
        });
        entry.1.push((title.clone(), id.clone()));
    }
    let active_id = app.sid.clone().unwrap_or_default();
    app.sidebar_items.clear();
    for (pid, (pname, sess_list)) in &project_map {
        let expanded = app.expanded_projects.contains(pid.as_str());
        app.sidebar_items.push(SidebarItem::ProjectHeader {
            id: pid.clone(),
            name: pname.clone(),
            expanded,
            session_count: sess_list.len(),
        });
        if expanded {
            for (title, id) in sess_list {
                app.sidebar_items.push(SidebarItem::Session {
                    title: title.clone(),
                    id: id.clone(),
                    active: *id == active_id,
                });
            }
        }
    }
    app.sidebar_items.push(SidebarItem::NewSession);
    let max = app.sidebar_items.len().saturating_sub(1);
    app.sidebar_sel = app.sidebar_sel.min(max);
    app.sidebar_scroll = app.sidebar_scroll.min(max.saturating_sub(1));
}

// ── Chat input ─────────────────────────────────────────────────

fn handle_chat(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    const SCROLL_SPEED: usize = 3;
    match code {
        KeyCode::Up => {
            if app.at_end {
                app.at_end = false;
                app.scroll = SCROLL_SPEED;
            } else {
                app.scroll = app.scroll.saturating_add(SCROLL_SPEED);
            }
        }
        KeyCode::Down => {
            if !app.at_end {
                if app.scroll > SCROLL_SPEED {
                    app.scroll = app.scroll.saturating_sub(SCROLL_SPEED);
                } else {
                    app.at_end = true;
                    app.scroll = 0;
                }
            }
        }
        KeyCode::PageUp => {
            if app.at_end {
                app.at_end = false;
                app.scroll = 10;
            } else {
                app.scroll = app.scroll.saturating_add(10);
            }
        }
        KeyCode::PageDown => {
            if !app.at_end {
                if app.scroll > 10 {
                    app.scroll = app.scroll.saturating_sub(10);
                } else {
                    app.at_end = true;
                    app.scroll = 0;
                }
            }
        }
        KeyCode::Home => {
            app.at_end = false;
            app.scroll = 9999;
        }
        KeyCode::End => {
            app.at_end = true;
            app.scroll = 0;
        }
        KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
            app.input.push('\n');
            update_autocomplete(app);
        }
        KeyCode::Enter => dispatch_command(app),
        KeyCode::Char(c) => {
            app.input.push(c);
            update_autocomplete(app);
        }
        KeyCode::Backspace => {
            app.input.pop();
            update_autocomplete(app);
        }
        _ => {}
    }
    true
}

fn dispatch_command(app: &mut App) {
    let raw = app.input.trim().to_string();
    if raw.is_empty() {
        return;
    }

    for item in &app.sidebar_items {
        if let SidebarItem::Session { title, id, .. } = item {
            if title == &raw {
                app.input.clear();
                app.actions.push(Action::Open(id.clone()));
                return;
            }
        }
    }

    app.input.clear();

    match raw.as_str() {
        "/exit" | "/quit" => {
            app.exit_requested = true;
        }
        "/help" | "/h" => {
            show_help(app);
        }
        cmd if cmd.starts_with("/login ") => {
            let key = cmd.trim_start_matches("/login ").trim().to_string();
            if !key.is_empty() {
                app.actions.push(Action::Login(key));
            } else {
                app.add_msg("sys", "usage: /login <your_api_key>");
            }
        }
        "/credits" | "/me" | "/usage" => {
            app.actions.push(Action::ShowInfo);
        }
        "/sessions" => {
            app.focus = Focus::Sidebar;
        }
        "/new" | "/n" => {
            app.actions.push(Action::Create);
        }
        _ => {
            // Regular message
            app.add_msg("user", &raw);
            if app.transport.key.is_empty() {
                app.add_msg("sys", "no API key — set ATTACCA_API_KEY");
            } else {
                app.actions.push(Action::Send(raw));
            }
        }
    }
}

fn show_help(app: &mut App) {
    app.add_msg("sys", "── Commands ──────────────────────────");
    app.add_msg("sys", "  /exit       Exit the program");
    app.add_msg("sys", "  /help       Show this help");
    app.add_msg("sys", "  /new        Create a new session");
    app.add_msg("sys", "  /login KEY  Set your API key");
    app.add_msg("sys", "  /credits    Show account/session info");
    app.add_msg("sys", "  /me         Same as /credits");
    app.add_msg("sys", "  /usage      Show session usage details");
    app.add_msg("sys", "  /sessions   Toggle sidebar focus");
    app.add_msg("sys", "");
    app.add_msg("sys", "── Keys ─────────────────────────────");
    app.add_msg("sys", "  Enter      Send message");
    app.add_msg("sys", "  Shift+Enter Newline in input");
    app.add_msg("sys", "  Tab        Focus sidebar / autocomplete");
    app.add_msg("sys", "  ↑↓         Scroll chat history");
    app.add_msg("sys", "  y/n        Approve/skip tool calls");
    app.add_msg("sys", "  Ctrl+C     Exit");
}

// ── Autocomplete ───────────────────────────────────────────────

const SLASH_COMMANDS: &[&str] = &[
    "/exit", "/help", "/sessions", "/new", "/login", "/credits", "/me", "/usage",
];

fn update_autocomplete(app: &mut App) {
    app.autocomplete_suggestions.clear();
    app.autocomplete_idx = None;
    let trimmed = app.input.trim();
    if trimmed.starts_with('/') && !trimmed.is_empty() && trimmed.len() > 1 {
        for cmd in SLASH_COMMANDS {
            if cmd.starts_with(trimmed) {
                app.autocomplete_suggestions.push(cmd.to_string());
            }
        }
    }
}

fn cycle_autocomplete(app: &mut App) {
    let n = app.autocomplete_suggestions.len();
    if n == 0 {
        return;
    }
    let next = app
        .autocomplete_idx
        .map(|i| (i + 1) % n)
        .unwrap_or(0);
    app.autocomplete_idx = Some(next);
    if let Some(cmd) = app.autocomplete_suggestions.get(next) {
        app.input = cmd.clone();
        app.input.push(' ');
    }
}

// ── Tool approval ──────────────────────────────────────────────

/// Execute a tool (or skip it) and queue follow-up actions.
///
/// Called synchronously from the keyboard handler. The tool is run inline,
/// its result is shown immediately, and a background poll is queued via
/// [`Action::ToolResult`].
fn approve_tool(app: &mut App, yes: bool) {
    let idx = app
        .msgs
        .iter()
        .rposition(|m| m.raw.is_some() && !m.done);
    let Some(i) = idx else {
        return;
    };
    let json = app.msgs[i].raw.take().unwrap_or_default();
    app.msgs[i].done = true;

    let result = if yes {
        exec_tool(&json)
    } else {
        "skipped".into()
    };
    app.add_msg("result", &result);

    if yes && app.sid.is_some() {
        app.actions.push(Action::ToolResult(result));
    }
}
