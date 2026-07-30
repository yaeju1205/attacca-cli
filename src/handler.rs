//! Keyboard, mouse, and sidebar selection handling.
//!
//! All input processing goes here — no API calls, no rendering, no background
//! tasks. Side effects are enqueued as [`Action`]s and drained by the event loop.

use crate::app::{Action, App, Focus, SidebarItem};
use crossterm::event::{KeyCode, KeyModifiers, MouseEventKind};

/// Process a keyboard event. Returns `false` on exit request.
pub fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if let KeyCode::Char('/') = code {
        if app.focus == Focus::Sidebar {
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

/// Process a bracketed-paste event. Pasted text is inserted verbatim —
/// including any embedded newlines — without going through the per-key
/// Enter handling, so a newline in clipboard content never submits the
/// message early.
pub fn handle_paste(app: &mut App, text: &str) {
    if app.focus != Focus::Chat {
        return;
    }
    app.input.push_str(text);
    app.input_scroll = usize::MAX;
    update_autocomplete(app);
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
                if let SidebarItem::ProjectHeader { id, .. } = &app.sidebar_items[i] {
                    let id = id.clone();
                    app.expanded_projects.remove(&id);
                    app.rebuild_sidebar();
                    app.sidebar_sel = i;
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
            id, ref expanded, ..
        } => {
            if *expanded {
                app.expanded_projects.remove(&id);
            } else {
                app.expanded_projects.insert(id.clone());
            }
            app.rebuild_sidebar();
        }
        SidebarItem::Session { id, .. } => {
            app.actions.push(Action::Open(id));
        }
        SidebarItem::NewSession => {
            app.actions.push(Action::Create);
        }
    }
}

// ── Chat input ─────────────────────────────────────────────────

fn handle_chat(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    const SCROLL_SPEED: usize = 3;
    match code {
        // Ctrl+↑/↓: scroll the input box by wrapped *visual* rows. The max
        // offset (`input_max_scroll`) is recomputed each frame in the UI, so
        // this stays correct for long soft-wrapped lines, not just `\n` lines.
        KeyCode::Up if modifiers.contains(KeyModifiers::CONTROL) => {
            let cur = if app.input_scroll == usize::MAX {
                app.input_max_scroll
            } else {
                app.input_scroll
            };
            app.input_scroll = cur.saturating_sub(1);
        }
        KeyCode::Down if modifiers.contains(KeyModifiers::CONTROL) => {
            let cur = if app.input_scroll == usize::MAX {
                app.input_max_scroll
            } else {
                app.input_scroll
            };
            app.input_scroll = cur.saturating_add(1).min(app.input_max_scroll);
        }
        // ↑/↓ (no modifier): scroll chat history
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
        // ── Newline insertion (Enter + any modifier or Ctrl+J) ──
        //
        // Kitty keyboard protocol (enabled in event.rs via
        // PushKeyboardEnhancementFlags) makes the terminal report
        // Shift+Enter as \x1B[13;2u → KeyCode::Enter + SHIFT.
        //
        // On terminals without Kitty support we fall back to:
        //   - Alt+Enter → ESC+\r → KeyCode::Enter + ALT
        //   - Ctrl+J    → 0x0A    → KeyCode::Char('j') + CTRL
        KeyCode::Enter
            if modifiers.contains(KeyModifiers::SHIFT)
                || modifiers.contains(KeyModifiers::ALT)
                || modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.input.push('\n');
            app.input_scroll = usize::MAX;
            update_autocomplete(app);
        }
        // Ctrl+J (0x0A = LF) → newline fallback for terminals without Kitty protocol.
        // In raw mode the terminal sends 0x0A as Ctrl+J, which is equivalent to
        // what Ctrl+Enter produces on gnome-terminal, VSCode, iTerm2, etc.
        KeyCode::Char('j') if modifiers == KeyModifiers::CONTROL => {
            app.input.push('\n');
            update_autocomplete(app);
        }
        KeyCode::Enter => dispatch_command(app),
        // Ctrl+W: delete word backward
        KeyCode::Char('w') if modifiers == KeyModifiers::CONTROL => {
            delete_word_backward(app);
            app.input_scroll = usize::MAX;
            update_autocomplete(app);
        }
        // Ctrl+Backspace: delete word backward
        KeyCode::Backspace if modifiers.contains(KeyModifiers::CONTROL) => {
            delete_word_backward(app);
            app.input_scroll = usize::MAX;
            update_autocomplete(app);
        }
        KeyCode::Char(c) => {
            app.input.push(c);
            app.input_scroll = usize::MAX;
            update_autocomplete(app);
        }
        KeyCode::Backspace => {
            app.input.pop();
            app.input_scroll = usize::MAX;
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
                app.input_scroll = usize::MAX;
                app.actions.push(Action::Open(id.clone()));
                return;
            }
        }
    }

    app.input.clear();
    app.input_scroll = usize::MAX;
    app.autocomplete_suggestions.clear();
    app.autocomplete_idx = None;

    match raw.as_str() {
        "/exit" | "/quit" => {
            app.exit_requested = true;
        }
        "/help" | "/h" => show_help(app),
        "/new" | "/n" => app.actions.push(Action::Create),
        "/cancel" => app.actions.push(Action::Cancel),
        "/logout" => app.actions.push(Action::Logout),
        "/sessions" => {
            app.focus = Focus::Sidebar;
            // Focus *and* refresh, so there is a user-driven way to re-read the list that is not a
            // timer. `turn_events` is per-session, so nothing else pushes the rest of the sidebar.
            app.actions.push(Action::RefreshSessions);
        }
        "/login" => {
            app.push_sys("authorizing — see the terminal");
            app.login_requested = true;
        }
        // Every `.env` in the wild still has an API key in it, so say what changed rather than
        // silently ignoring the argument.
        cmd if cmd.starts_with("/login ") => {
            app.push_sys("attacca-cli no longer takes a key here — /login re-authorizes this node");
            app.push_sys("set ZYRIS_NODE_TOKEN for a static token instead");
        }
        "/credits" | "/me" | "/usage" => app.actions.push(Action::ShowInfo),
        "/whoami" => match &app.me {
            Some(me) => {
                let ident = format!("{} <{}>", me.display_name, me.email);
                let scopes = if me.scopes.is_empty() {
                    "none granted".to_string()
                } else {
                    me.scopes.join(", ")
                };
                app.push_sys(&ident);
                app.push_sys(&format!("scopes: {scopes}"));
            }
            None => app.push_sys("not connected yet"),
        },
        // The sidebar can only push what `turn_events` carries, because that is the one stream
        // `attacca_api` v1 declares. If a deployment announces something account-wide, this is where
        // it would show up — the announced tool list is never compared against the crate's
        // declaration, so a newer server may offer more than `zyris-attacca` knows about.
        "/tools" => match app.slot.get() {
            Some(live) => {
                let lines: Vec<String> = live
                    .conn
                    .peer_descriptors()
                    .iter()
                    .flat_map(|cap| {
                        std::iter::once(format!("{} v{}", cap.name, cap.version)).chain(
                            cap.tools
                                .iter()
                                .map(|t| format!("  {} ({:?})", t.name, t.transfer)),
                        )
                    })
                    .collect();
                for line in lines {
                    app.push_sys(&line);
                }
            }
            None => app.push_sys("not connected yet"),
        },
        other if other.starts_with('/') => {
            app.push_sys(&format!("unknown command: {other}"));
        }
        _ => {
            // Regular message: echoed optimistically, then settled by its durable event.
            app.chat.push(crate::app::MsgKind::User, &raw);
            app.actions.push(Action::Send(raw));
        }
    }
}

fn show_help(app: &mut App) {
    for line in [
        "── Commands ──────────────────────────",
        "  /exit       Exit the program",
        "  /help       Show this help",
        "  /new        Create a new session",
        "  /cancel     Stop the running turn",
        "  /login      Authorize this node again",
        "  /logout     Forget this node's credential",
        "  /whoami     Show identity and granted scopes",
        "  /usage      Show account and session usage",
        "  /tools      What the server announces",
        "  /sessions   Focus the sidebar and refresh it",
        "",
        "── Keys ─────────────────────────────",
        "  Enter       Send message",
        "  Shift+Enter Newline (Alt+Enter, Ctrl+J too)",
        "  Tab         Focus sidebar / autocomplete",
        "  ↑↓          Scroll chat history",
        "  Ctrl+↑/↓    Scroll input",
        "  Ctrl+C      Exit",
    ] {
        app.push_sys(line);
    }
}

// ── Autocomplete ───────────────────────────────────────────────

/// What the popup completes. `/quit`, `/h`, `/n`, `/credits` and `/me` resolve in
/// [`dispatch_command`] but stay out of here — the popup is a fixed 28-column box, and five
/// near-duplicate entries would fill it with noise.
const SLASH_COMMANDS: &[&str] = &[
    "/help", "/exit", "/new", "/sessions", "/cancel", "/login", "/logout", "/whoami", "/usage",
    "/tools",
];

fn update_autocomplete(app: &mut App) {
    app.autocomplete_suggestions.clear();
    app.autocomplete_idx = None;
    let trimmed = app.input.trim();
    if trimmed.starts_with('/') && trimmed.len() > 1 {
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
    let next = app.autocomplete_idx.map(|i| (i + 1) % n).unwrap_or(0);
    app.autocomplete_idx = Some(next);
    if let Some(cmd) = app.autocomplete_suggestions.get(next) {
        app.input = cmd.clone();
        app.input.push(' ');
    }
}

/// Delete one word backward (from cursor to previous word boundary).
/// Behaves like bash's Ctrl+W: removes the word and its preceding
/// separator whitespace. Cursor (end of string) follows naturally.
///
/// A newline is a hard boundary, not just another whitespace char: word
/// deletion never crosses into the previous line, and a lone trailing
/// newline is removed on its own (like a plain backspace) rather than
/// being skipped over to reach the word before it.
fn delete_word_backward(app: &mut App) {
    if app.input.is_empty() {
        return;
    }
    let bytes = app.input.as_bytes();
    let mut i = bytes.len();
    if bytes[i - 1] == b'\n' {
        i -= 1;
        app.input.truncate(i);
        return;
    }
    // 1. Skip trailing space/tab
    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
        i -= 1;
    }
    // 2. Skip the word, stopping at whitespace or a line boundary
    while i > 0 && !matches!(bytes[i - 1], b' ' | b'\t' | b'\n') {
        i -= 1;
    }
    // 3. Skip the space/tab separator before the word too (not a newline)
    if i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
        i -= 1;
    }
    app.input.truncate(i);
}
