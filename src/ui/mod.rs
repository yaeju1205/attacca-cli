use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::app::{App, Focus, SidebarItem};
use palette::*;

mod palette;

/// Top-level draw entry point, called once per frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    let a = f.area();
    if a.width < 50 || a.height < 10 {
        return;
    }
    f.render_widget(Paragraph::new("").style(Style::new().bg(BG)), a);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Min(3),    // main content (chat + sidebar)
            Constraint::Length(4), // input area: info/separator + 3 content lines
        ])
        .split(a);

    draw_status(f, app, chunks[0]);
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEW), Constraint::Min(30)])
        .split(chunks[1]);
    draw_sidebar(f, app, main[0]);
    draw_chat(f, app, main[1]);
    draw_input_box(f, app, chunks[2]);
}

// Status bar
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
        right.push(Span::styled(format!("  {}", app.user_name), Style::new().fg(TEXT)));
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

    f.render_widget(Paragraph::new(Line::from(line)).style(Style::new().bg(CARD)), area);
    f.render_widget(
        Paragraph::new("").style(Style::new().bg(BORDER)),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

// Sidebar
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
                SidebarItem::ProjectHeader { name, expanded, session_count, .. } => {
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
                        format!(" {} {} {}", icon, short_name(name, 16), session_count), s,
                    )]))
                }
                SidebarItem::Session { title, active, .. } => {
                    let dot = if *active { "●" } else { "○" };
                    let s = if *active && focused {
                        Style::new().fg(P).add_modifier(Modifier::BOLD)
                            .bg(if hl { ACCENT_BG } else { POPOVER })
                    } else if *active {
                        Style::new().fg(P_DIM).add_modifier(Modifier::BOLD)
                            .bg(if hl { BORDER } else { POPOVER })
                    } else if hl && focused {
                        Style::new().fg(TEXT).add_modifier(Modifier::BOLD).bg(ACCENT_BG)
                    } else if hl {
                        Style::new().fg(DIM).bg(BORDER)
                    } else {
                        Style::new().fg(DIM).bg(POPOVER)
                    };
                    ListItem::new(Line::from(vec![Span::styled(
                        format!("   {} {}", dot, short_name(title, 17)), s,
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

    let la = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(3), area.height.saturating_sub(4));
    f.render_widget(List::new(items).style(Style::new().bg(POPOVER)), la);

    let hint_s = if focused { Style::new().fg(P).bg(POPOVER) } else { Style::new().fg(DIM).bg(POPOVER) };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ↑↓·enter  ", hint_s),
            Span::styled("+new", Style::new().fg(GREEN).add_modifier(Modifier::BOLD).bg(POPOVER)),
        ]))
        .style(Style::new().bg(POPOVER)),
        Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1),
    );
}

// Chat area
fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
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
                    "└".to_string() + &"─".repeat(w.saturating_sub(2)),
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
                    "└".to_string() + &"─".repeat(w.saturating_sub(2)),
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
                    "└".to_string() + &"─".repeat(w.saturating_sub(2)),
                    Style::new().fg(YELLOW).add_modifier(Modifier::DIM),
                )]));
            }
            "tool" => {}
            "result" => {
                if let Some(first) = m.text.lines().next() {
                    let ok = !m.text.starts_with("err") && !m.text.starts_with("skipped");
                    let (icon, color) = if ok { ("ok", GREEN) } else { ("✘", DESTRUCTIVE) };
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
            Span::styled("  >> ", Style::new().fg(P)),
            Span::styled("type something — enter:send /help", Style::new().fg(DIM)),
        ]));
    } else if app.busy()
        && app.msgs.last().map(|m| m.role.as_str() == "user").unwrap_or(false)
    {
        lines.push(Line::from(vec![
            Span::styled(" ◉ thinking…", Style::new().fg(P)),
        ]));
    }

    let total = lines.len();
    let max_rows = area.height.saturating_sub(1) as usize;

    let scroll_off = if app.at_end || total <= max_rows {
        total.saturating_sub(max_rows)
    } else {
        total.saturating_sub(max_rows).saturating_sub(app.scroll)
    };

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((scroll_off as u16, 0))
            .style(Style::new().bg(BG)),
        area,
    );
}

// Input box (includes info bar at the top separator)
fn draw_input_box(f: &mut Frame, app: &mut App, area: Rect) {
    let chat_focused = app.focus == Focus::Chat;

    // Row 0: separator line with session info
    let info_text = {
        let mut s = String::new();
        if !app.current_project_name.is_empty() {
            s.push_str(&format!(" proj:{}", app.current_project_name));
        }
        if !app.usage_context_tokens.is_empty() {
            s.push_str(&format!(" ctx:{}", app.usage_context_tokens));
        }
        if !app.usage_total_tokens.is_empty() {
            if !app.usage_input_tokens.is_empty() && !app.usage_output_tokens.is_empty() {
                s.push_str(&format!(" tokens:{}/{}", app.usage_input_tokens, app.usage_output_tokens));
            } else {
                s.push_str(&format!(" tokens:{}", app.usage_total_tokens));
            }
        }
        if s.is_empty() { s = "  no session".to_string(); }
        s
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(&info_text, Style::new().fg(DIM)),
        ])).style(Style::new().bg(CARD)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // Row 1-2: input text
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

    // Reserve the rightmost column for a scrollbar; wrap text into the rest.
    let scrollbar_area = Rect::new(
        text_area.x + text_area.width.saturating_sub(1),
        text_area.y,
        1,
        text_area.height,
    );
    let text_render_area = Rect::new(
        text_area.x,
        text_area.y,
        text_area.width.saturating_sub(1),
        text_area.height,
    );
    // 3-char left gutter (prompt / continuation indent) shrinks the wrap width.
    let content_width = (text_render_area.width as usize).saturating_sub(3).max(1);
    let view = text_render_area.height as usize;

    // Build display rows, hard-wrapping each logical line into visual rows so
    // scrolling and the cursor stay correct even without explicit newlines.
    let segments: Vec<&str> = app.input.split('\n').collect();
    let seg_count = segments.len();
    let mut rows: Vec<Line> = Vec::new();

    if app.input.is_empty() {
        rows.push(Line::from(vec![
            prompt,
            Span::styled("type a message…", Style::new().fg(DIM)),
            Span::styled("█", Style::new().fg(P)),
        ]));
    } else {
        let mut first_overall = true;
        for (si, seg) in segments.iter().enumerate() {
            let chunks = wrap_chars(seg, content_width);
            let chunk_count = chunks.len();
            for (ci, chunk) in chunks.into_iter().enumerate() {
                let is_last_visual = si == seg_count - 1 && ci == chunk_count - 1;
                let gutter = if first_overall {
                    prompt.clone()
                } else {
                    Span::styled("   ", Style::new().fg(DIM))
                };
                first_overall = false;
                rows.push(Line::from(vec![
                    gutter,
                    Span::raw(chunk),
                    Span::styled(if is_last_visual { "█" } else { "" }, Style::new().fg(P)),
                ]));
            }
        }
    }

    // Scroll: Ctrl+↑/↓ for manual, auto-follow the last row when typing.
    let total_rows = rows.len();
    let max_scroll = total_rows.saturating_sub(view);
    app.input_max_scroll = max_scroll;
    let scroll_off = if app.input_scroll == usize::MAX {
        max_scroll
    } else {
        app.input_scroll.min(max_scroll)
    };

    let end = (scroll_off + view).min(total_rows);
    let visible: Vec<Line> = rows[scroll_off..end].to_vec();

    f.render_widget(
        Paragraph::new(Text::from(visible)).style(Style::new().bg(BG)),
        text_render_area,
    );

    // Scrollbar indicator (only when the content overflows the 3-row viewport).
    if total_rows > view {
        let mut sb_state = ScrollbarState::new(total_rows)
            .viewport_content_length(view)
            .position(scroll_off);
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol("█")
            .track_symbol(Some("│"))
            .thumb_style(Style::new().fg(P))
            .track_style(Style::new().fg(BORDER));
        f.render_stateful_widget(sb, scrollbar_area, &mut sb_state);
    }

    // autocomplete popup
    let suggestions = &app.autocomplete_suggestions;
    if !suggestions.is_empty() && chat_focused && app.input.trim().starts_with('/') {
        let items: Vec<ListItem> = suggestions.iter().enumerate().map(|(i, cmd)| {
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
        }).collect();

        let h = items.len() as u16 + 1;
        let pr = Rect::new(area.x + 1, area.y.saturating_sub(h), 28, h);
        f.render_widget(Paragraph::new("").style(Style::new().bg(CARD)), pr);
        f.render_widget(
            List::new(items).style(Style::new().bg(CARD)),
            Rect::new(pr.x, pr.y, 28, h.saturating_sub(1)),
        );
    }
}

// Helpers

/// Hard-wrap a single logical line into visual rows of at most `width` chars.
/// An empty line yields one empty row so blank input lines still take a row.
fn wrap_chars(seg: &str, width: usize) -> Vec<String> {
    if seg.is_empty() {
        return vec![String::new()];
    }
    let chars: Vec<char> = seg.chars().collect();
    let mut rows = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let end = (i + width).min(chars.len());
        rows.push(chars[i..end].iter().collect());
        i = end;
    }
    rows
}

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
