//! TUI rendering with ratatui.
//!
//! All drawing functions are pure: they read `App` state and render widgets.
//! No mutation of application state happens here.

mod palette;

use crate::app::{App, Focus, SidebarItem};
use palette::*;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

/// Top-level draw entry point, called once per frame.
pub fn draw(f: &mut Frame, app: &App) {
    let a = f.area();
    if a.width < 50 || a.height < 10 {
        return;
    }
    f.render_widget(Paragraph::new("").style(Style::new().bg(BG)), a);

    // Input box: fixed 3 lines (1 separator + 2 content lines).
    // Long text wraps within the box instead of expanding the layout.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),    // status bar
            Constraint::Min(3),       // main content (chat + sidebar)
            Constraint::Length(1),    // info bar
            Constraint::Length(3),    // input area: separator + 2 content
        ])
        .split(a);

    draw_status(f, app, chunks[0]);
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEW), Constraint::Min(30)])
        .split(chunks[1]);
    draw_sidebar(f, app, main[0]);
    draw_chat(f, app, main[1]);
    draw_info_bar(f, app, chunks[2]);
    draw_input_box(f, app, chunks[3]);
}

// ───── Status bar ──────────────────────────────────────────────

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mode = if app.transport.key.is_empty() {
        Span::styled("  offline", Style::new().fg(DESTRUCTIVE))
    } else {
        Span::styled("  ◉ online", Style::new().fg(GREEN))
    };

    let sid = app.sid.as_ref().map(|s| short(s)).unwrap_or_default();
    let status = if app.busy() { "running" } else { "ready" };
    let status_color = if app.busy() { YELLOW } else { GREEN };

    let mut right = Vec::new();
    if !app.user_name.is_empty() {
        right.push(Span::styled(
            format!("  {}", app.user_name),
            Style::new().fg(TEXT),
        ));
    }
    if !sid.is_empty() {
        right.push(Span::styled(format!("  {sid}"), Style::new().fg(DIM)));
    }

    let mut line = vec![
        Span::styled(" ◆ ", Style::new().fg(P).add_modifier(Modifier::BOLD)),
        Span::styled("Attacca", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
        Span::styled(" ", Style::new().fg(BORDER)),
        Span::styled(status, Style::new().fg(status_color).add_modifier(Modifier::BOLD)),
        mode,
    ];
    line.extend(right);

    f.render_widget(
        Paragraph::new(Line::from(line)).style(Style::new().bg(CARD)),
        area,
    );
    f.render_widget(
        Paragraph::new("").style(Style::new().bg(BORDER)),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

// ───── Sidebar ─────────────────────────────────────────────────

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    f.render_widget(Paragraph::new("").style(Style::new().bg(POPOVER)), area);

    let fg = if focused { P } else { DIM };
    let text_fg = if focused { TEXT } else { DIM };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(fg).add_modifier(Modifier::BOLD).bg(POPOVER)),
            Span::styled("sessions", Style::new().fg(text_fg).add_modifier(Modifier::BOLD).bg(POPOVER)),
        ]))
        .style(Style::new().bg(POPOVER)),
        Rect::new(area.x + 2, area.y, area.width, 1),
    );

    let sel = app.sidebar_sel;
    let scroll = app.sidebar_scroll;
    let max_vis = (area.height.saturating_sub(4)) as usize;

    let items: Vec<ListItem> = app
        .sidebar_items
        .iter()
        .enumerate()
        .filter(|&(i, _)| i >= scroll)
        .take(max_vis)
        .map(|(orig_i, item)| {
            let hl = orig_i == sel;
            match item {
                SidebarItem::ProjectHeader {
                    name,
                    expanded,
                    session_count,
                    ..
                } => {
                    let icon = if *expanded { "▾" } else { "▸" };
                    let s = if hl && focused {
                        Style::new().fg(P).add_modifier(Modifier::BOLD).bg(ACCENT_BG)
                    } else if hl {
                        Style::new().fg(P_DIM).bg(BORDER)
                    } else if !focused {
                        Style::new().fg(DIM).bg(POPOVER)
                    } else {
                        Style::new().fg(P_DIM).bg(POPOVER)
                    };
                    ListItem::new(Line::from(vec![Span::styled(
                        format!(" {} {} {}", icon, short_name(name, 16), session_count),
                        s,
                    )]))
                }
                SidebarItem::Session { title, active, .. } => {
                    let dot = if *active { "●" } else { "○" };
                    let s = if *active && focused {
                        Style::new()
                            .fg(P)
                            .add_modifier(Modifier::BOLD)
                            .bg(if hl { ACCENT_BG } else { POPOVER })
                    } else if *active {
                        Style::new()
                            .fg(P_DIM)
                            .add_modifier(Modifier::BOLD)
                            .bg(if hl { BORDER } else { POPOVER })
                    } else if hl && focused {
                        Style::new()
                            .fg(TEXT)
                            .add_modifier(Modifier::BOLD)
                            .bg(ACCENT_BG)
                    } else if hl {
                        Style::new().fg(DIM).bg(BORDER)
                    } else {
                        Style::new().fg(DIM).bg(POPOVER)
                    };
                    ListItem::new(Line::from(vec![Span::styled(
                        format!("   {} {}", dot, short_name(title, 17)),
                        s,
                    )]))
                }
                SidebarItem::NewSession => {
                    let label = if hl { " ▸ + new" } else { "   + new" };
                    let s = if hl && focused {
                        Style::new().fg(GREEN).bg(ACCENT_BG)
                    } else if hl {
                        Style::new().fg(GREEN).bg(BORDER)
                    } else {
                        Style::new().fg(DIM).bg(POPOVER)
                    };
                    ListItem::new(Line::from(vec![Span::styled(label, s)]))
                }
            }
        })
        .collect();

    let la = Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(3),
        area.height.saturating_sub(4),
    );
    f.render_widget(List::new(items).style(Style::new().bg(POPOVER)), la);

    let hint_s = if focused {
        Style::new().fg(P).bg(POPOVER)
    } else {
        Style::new().fg(DIM).bg(POPOVER)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  arrows enter  ", hint_s),
            Span::styled("+new", Style::new().fg(GREEN).add_modifier(Modifier::BOLD).bg(POPOVER)),
        ]))
        .style(Style::new().bg(POPOVER)),
        Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1),
    );
}

// ───── Chat area ───────────────────────────────────────────────

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    // ── 1. Build all display lines ──
    let mut lines: Vec<Line> = Vec::new();
    let w = area.width.saturating_sub(1) as usize;

    for m in &app.msgs {
        match m.role.as_str() {
            "sys" => {
                lines.push(Line::from(vec![
                    Span::styled(" ── ", Style::new().fg(DIM)),
                    Span::styled(&m.text, Style::new().fg(DIM)),
                ]));
            }
            "agent" => {
                lines.push(Line::from(vec![Span::styled(
                    "┌─ assistant ".to_string() + &"─".repeat(w.saturating_sub(13)),
                    Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                )]));
                for l in m.text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled("│ ", Style::new().fg(DIM)),
                        Span::raw(l),
                    ]));
                }
                lines.push(Line::from(vec![Span::styled(
                    "└".to_string() + &"─".repeat(w.saturating_sub(1)),
                    Style::new().fg(DIM).add_modifier(Modifier::DIM),
                )]));
            }
            "user" => {
                lines.push(Line::from(vec![Span::styled(
                    "┌─ you ".to_string() + &"─".repeat(w.saturating_sub(8)),
                    Style::new().fg(P).add_modifier(Modifier::BOLD),
                )]));
                for l in m.text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled("│ ", Style::new().fg(P)),
                        Span::raw(l),
                    ]));
                }
                lines.push(Line::from(vec![Span::styled(
                    "└".to_string() + &"─".repeat(w.saturating_sub(1)),
                    Style::new().fg(P).add_modifier(Modifier::DIM),
                )]));
            }
            "tool" if !m.done => {
                lines.push(Line::from(vec![Span::styled(
                    "┌─ tool ".to_string() + &"─".repeat(w.saturating_sub(8)),
                    Style::new().fg(YELLOW).add_modifier(Modifier::BOLD),
                )]));
                lines.push(Line::from(vec![
                    Span::styled("│ ", Style::new().fg(YELLOW)),
                    Span::styled(&m.text, Style::new().fg(TEXT)),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("│ ", Style::new().fg(YELLOW)),
                    Span::styled("[", Style::new().fg(DIM)),
                    Span::styled("y", Style::new().fg(GREEN).add_modifier(Modifier::BOLD)),
                    Span::styled("] run  [", Style::new().fg(DIM)),
                    Span::styled("n", Style::new().fg(DESTRUCTIVE).add_modifier(Modifier::BOLD)),
                    Span::styled("] skip", Style::new().fg(DIM)),
                ]));
                lines.push(Line::from(vec![Span::styled(
                    "└".to_string() + &"─".repeat(w.saturating_sub(1)),
                    Style::new().fg(YELLOW).add_modifier(Modifier::DIM),
                )]));
            }
            "tool" => {} // approved — don't re-show
            "result" => {
                if let Some(first) = m.text.lines().next() {
                    let ok = !m.text.starts_with("err") && !m.text.starts_with("skipped");
                    let (icon, color) = if ok { ("✔", GREEN) } else { ("✘", DESTRUCTIVE) };
                    let label: String = first.chars().take(58).collect();
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {icon} {label}"), Style::new().fg(color)),
                    ]));
                }
            }
            _ => {}
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  ◆ ", Style::new().fg(P)),
            Span::styled("type something —  enter:send  /help", Style::new().fg(DIM)),
        ]));
    } else if app.busy()
        && app
            .msgs
            .last()
            .map(|m| m.role.as_str() == "user")
            .unwrap_or(false)
    {
        lines.push(Line::from(vec![
            Span::styled(" ◉ thinking…", Style::new().fg(P)),
        ]));
    }

    // ── 2. Scroll to show the last messages ──
    //
    // With .wrap(), one logical line can become multiple visual lines.
    // Using all visual rows for logical lines would cause the last
    // half of the content to get clipped (hidden behind the input box).
    //
    // Fix: show only about half the available rows as logical lines,
    // leaving room for .wrap() to expand them within the area.

    let total = lines.len();
    let visual_rows = area.height.saturating_sub(1) as usize;
    let safe_count = (visual_rows / 2).max(3); // logical lines that fit after wrapping

    let scroll_off = if app.at_end || total <= safe_count {
        total.saturating_sub(safe_count)
    } else {
        total.saturating_sub(safe_count).saturating_sub(app.scroll)
    };

    // ── 3. Render ──
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll_off as u16, 0))
            .wrap(Wrap { trim: false })
            .style(Style::new().bg(BG)),
        area,
    );
}

// ───── Info bar ────────────────────────────────────────────────

fn draw_info_bar(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();

    if !app.current_project_name.is_empty() {
        spans.push(Span::styled(
            format!(" proj:{}", app.current_project_name),
            Style::new().fg(GREEN),
        ));
    }
    if !app.user_credits.is_empty() {
        spans.push(Span::styled(
            format!(" credits:{}", app.user_credits),
            Style::new().fg(P_DIM),
        ));
    }
    if !app.usage_credits_used.is_empty() {
        spans.push(Span::styled(
            format!(" used:{}", app.usage_credits_used),
            Style::new().fg(YELLOW),
        ));
    }
    if !app.usage_context_tokens.is_empty() {
        spans.push(Span::styled(
            format!(" ctx:{}", app.usage_context_tokens),
            Style::new().fg(P),
        ));
    }
    if !app.usage_total_tokens.is_empty() {
        let label = if !app.usage_input_tokens.is_empty() && !app.usage_output_tokens.is_empty()
        {
            format!(" tokens: {}/{}", app.usage_input_tokens, app.usage_output_tokens)
        } else {
            format!(" tokens:{}", app.usage_total_tokens)
        };
        spans.push(Span::styled(label, Style::new().fg(DIM)));
    }
    if !app.usage_model.is_empty() {
        spans.push(Span::styled(
            format!(" model:{}", app.usage_model),
            Style::new().fg(DIM),
        ));
    }

    if spans.is_empty() {
        spans.push(Span::styled("  no session", Style::new().fg(DIM)));
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(CARD)),
        area,
    );
}

// ───── Input box ───────────────────────────────────────────────

fn draw_input_box(f: &mut Frame, app: &App, area: Rect) {
    let chat_focused = app.focus == Focus::Chat;

    // separator line (top row of the input area)
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            "─".repeat(area.width as usize),
            Style::new().fg(BORDER),
        )]))
        .style(Style::new().bg(BG)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let text_area = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(1),
    );

    let prompt = if app.busy() {
        Span::styled(" ◉ ", Style::new().fg(P))
    } else if chat_focused {
        Span::styled(" > ", Style::new().fg(P))
    } else {
        Span::styled("   ", Style::new().fg(DIM))
    };

    if !chat_focused {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("press Tab to focus chat", Style::new().fg(DIM)),
            ])).style(Style::new().bg(BG)),
            text_area,
        );
        return;
    }

    // Show only the last 2 segments of input (separated by \n from Shift+Enter).
    // More lines get a "..." indicator. Long lines wrap within the box.
    let segments: Vec<&str> = app.input.split('\n').collect();
    let total = segments.len();
    let show_from = total.saturating_sub(2);

    let mut display: Vec<Line> = Vec::new();

    if app.input.is_empty() {
        display.push(Line::from(vec![
            prompt,
            Span::styled("type a message…", Style::new().fg(DIM)),
            Span::styled("█", Style::new().fg(P)),
        ]));
    } else {
        if show_from > 0 {
            display.push(Line::from(vec![
                Span::styled("   ", Style::new().fg(DIM)),
                Span::styled("...", Style::new().fg(DIM)),
            ]));
        }
        for i in show_from..total {
            let seg = segments[i];
            let is_last = i == total - 1;
            let cursor_ch = if is_last { "█" } else { "" };
            if i == 0 && show_from == 0 {
                display.push(Line::from(vec![
                    prompt.clone(),
                    Span::raw(seg.to_string()),
                    Span::styled(cursor_ch, Style::new().fg(P)),
                ]));
            } else {
                display.push(Line::from(vec![
                    Span::styled("   ", Style::new().fg(DIM)),
                    Span::raw(seg.to_string()),
                    Span::styled(cursor_ch, Style::new().fg(P)),
                ]));
            }
        }
    }

    f.render_widget(
        Paragraph::new(Text::from(display))
            .wrap(Wrap { trim: false })
            .style(Style::new().bg(BG)),
        text_area,
    );

    // autocomplete popup
    let suggestions = &app.autocomplete_suggestions;
    if !suggestions.is_empty() && chat_focused && app.input.trim().starts_with('/') {
        let items: Vec<ListItem> = suggestions
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let hl = Some(i) == app.autocomplete_idx;
                let desc = match cmd.as_str() {
                    "/exit" => "exit",
                    "/help" => "help",
                    "/sessions" => "sidebar",
                    "/new" => "new chat",
                    "/login" => "set API key",
                    "/credits" => "account info",
                    _ => "",
                };
                let s = if hl {
                    Style::new().bg(P).fg(P_FG)
                } else {
                    Style::new().bg(CARD).fg(TEXT)
                };
                ListItem::new(Line::from(vec![Span::styled(
                    format!(" {}  {}", cmd, desc),
                    s.add_modifier(if hl { Modifier::BOLD } else { Modifier::empty() }),
                )]))
            })
            .collect();

        let h = items.len() as u16 + 1;
        let pr = Rect::new(area.x + 1, area.y.saturating_sub(h), 28, h);
        f.render_widget(Paragraph::new("").style(Style::new().bg(CARD)), pr);
        f.render_widget(
            List::new(items).style(Style::new().bg(CARD)),
            Rect::new(pr.x, pr.y, 28, h.saturating_sub(1)),
        );
    }
}

// ───── Helpers ─────────────────────────────────────────────────

fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

fn short_name(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max {
        format!("{}..", s.chars().take(max.saturating_sub(2)).collect::<String>())
    } else {
        s.to_string()
    }
}
