use crate::app::{App, Focus, SidebarItem};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

// ══════════════════════════════════════════════════════════════
//  attacca.cc 공식 다크 팔레트 (v2 — 더 선명하게)
// ══════════════════════════════════════════════════════════════

const BG: Color = Color::Rgb(36, 36, 35);         // 배경
const SIDEBAR_BG: Color = Color::Rgb(26, 26, 25);  // 사이드바 (#1a1a19)
const SURFACE: Color = Color::Rgb(48, 48, 46);     // 입력창/상태바
const BORDER: Color = Color::Rgb(60, 60, 58);      // 테두리

const TEXT: Color = Color::Rgb(236, 235, 235);     // 본문
const DIM: Color = Color::Rgb(155, 154, 152);      // 흐린 텍스트

// 브랜드 — 테라코타
const P: Color = Color::Rgb(218, 125, 105);        // primary #da7d69
const P_DIM: Color = Color::Rgb(170, 92, 75);
const P_DARK: Color = Color::Rgb(60, 42, 38);      // hover 배경

const GREEN: Color = Color::Rgb(90, 180, 115);
const RED: Color = Color::Rgb(220, 80, 68);
const YELLOW: Color = Color::Rgb(220, 180, 70);

const SIDEW: u16 = 28;

// ══════════════════════════════════════════════════════════════

pub fn draw(f: &mut Frame, app: &App) {
    let a = f.area();
    if a.width < 50 || a.height < 10 { return; }
    f.render_widget(Paragraph::new("").style(Style::new().bg(BG)), a);

    let chunks = Layout::default().direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(2)])
        .split(a);

    draw_status(f, app, chunks[0]);
    let main = Layout::default().direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEW), Constraint::Min(30)])
        .split(chunks[1]);
    draw_sidebar(f, app, main[0]);
    draw_chat(f, app, main[1]);
    draw_box(f, app, chunks[2]);
}

// ───── Status ─────

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mode = if app.api.key.is_empty() {
        Span::styled("  offline", Style::new().fg(RED))
    } else {
        Span::styled("  bridge ◉", Style::new().fg(GREEN))
    };

    let sid = app.sid.as_ref().map(|s| short(s)).unwrap_or_default();
    let status = if app.busy { "running" } else { "ready" };
    let status_color = if app.busy { YELLOW } else { GREEN };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            // logo
            Span::styled(" ◆ ", Style::new().fg(P).add_modifier(Modifier::BOLD)),
            Span::styled("Attacca", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
            // divider
            Span::styled(" ┃ ", Style::new().fg(BORDER)),
            Span::styled(status, Style::new().fg(status_color).add_modifier(Modifier::BOLD)),
            mode,
            // right side
            Span::styled(format!("  {}  ", sid), Style::new().fg(DIM)),
        ])).style(Style::new().bg(SURFACE)),
        area,
    );

    // subtle bottom line
    f.render_widget(
        Paragraph::new("").style(Style::new().bg(BORDER)),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

// ───── Sidebar ─────

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;

    // bg
    f.render_widget(Paragraph::new("").style(Style::new().bg(SIDEBAR_BG)), area);

    // accent rail — 2px line
    let rail_color = if focused { P } else { P_DIM };
    f.render_widget(
        Paragraph::new("").style(Style::new().bg(rail_color)),
        Rect::new(area.x, 1, 2, area.height.saturating_sub(2)),
    );

    // header
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(if focused { P } else { DIM }).add_modifier(Modifier::BOLD).bg(SIDEBAR_BG)),
            Span::styled("sessions", Style::new().fg(if focused { TEXT } else { DIM }).add_modifier(Modifier::BOLD).bg(SIDEBAR_BG)),
        ])).style(Style::new().bg(SIDEBAR_BG)),
        Rect::new(area.x + 4, area.y, area.width, 1),
    );

    let sel = app.sel;
    let scroll = app.sidebar_scroll;
    let max_vis = (area.height.saturating_sub(4)) as usize;

    let items: Vec<ListItem> = app.sidebar_items.iter().enumerate()
        .filter(|&(i, _)| i >= scroll)
        .take(max_vis)
        .map(|(orig_i, item)| {
            let hl = orig_i == sel;
            match item {
                SidebarItem::ProjectHeader { name, expanded, session_count, .. } => {
                    let icon = if *expanded { "▾" } else { "▸" };
                    let s = if hl { Style::new().fg(P).add_modifier(Modifier::BOLD).bg(P_DARK) }
                             else { Style::new().fg(P_DIM).bg(SIDEBAR_BG) };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {} ", icon), s),
                        Span::styled(short_name(name, 18), s),
                        Span::styled(format!(" {}", session_count), Style::new().fg(DIM).bg(if hl { P_DARK } else { SIDEBAR_BG })),
                    ]))
                }
                SidebarItem::Session { title, active, .. } => {
                    let dot = if *active { "●" } else { "○" };
                    let s = if *active {
                        Style::new().fg(P).add_modifier(Modifier::BOLD).bg(if hl { P_DARK } else { SIDEBAR_BG })
                    } else if hl {
                        Style::new().fg(TEXT).add_modifier(Modifier::BOLD).bg(BORDER)
                    } else {
                        Style::new().fg(DIM).bg(SIDEBAR_BG)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("   {} {}", dot, short_name(title, 18)), s),
                    ]))
                }
                SidebarItem::NewSession => {
                    let label = if hl { " ▸ + new" } else { "   + new" };
                    let s = if hl { Style::new().fg(GREEN).bg(BORDER) }
                             else { Style::new().fg(DIM).bg(SIDEBAR_BG) };
                    ListItem::new(Line::from(vec![Span::styled(label, s)]))
                }
            }
        }).collect();

    let la = Rect::new(area.x + 4, area.y + 1, area.width.saturating_sub(5), area.height.saturating_sub(4));
    f.render_widget(List::new(items).style(Style::new().bg(SIDEBAR_BG)), la);

    // hint
    let hint_s = if focused { Style::new().fg(P).bg(SIDEBAR_BG) } else { Style::new().fg(DIM).bg(SIDEBAR_BG) };
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("  ↑↓ · enter · tab:chat", hint_s)]))
            .style(Style::new().bg(SIDEBAR_BG)),
        Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1),
    );
}

// ───── Chat ─────

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let w = area.width.saturating_sub(2) as usize;

    for m in &app.msgs {
        match m.role.as_str() {
            "sys" => {
                lines.push(Line::from(vec![
                    Span::styled(" ── ", Style::new().fg(DIM)),
                    Span::styled(&m.text, Style::new().fg(DIM)),
                ]));
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
                    Span::styled("n", Style::new().fg(RED).add_modifier(Modifier::BOLD)),
                    Span::styled("] skip", Style::new().fg(DIM)),
                ]));
                lines.push(Line::from(vec![Span::styled(
                    "└".to_string() + &"─".repeat(w.saturating_sub(1)),
                    Style::new().fg(YELLOW).add_modifier(Modifier::DIM),
                )]));
            }
            "tool" => {}
            "result" => {
                if let Some(first) = m.text.lines().next() {
                    let ok = !m.text.starts_with("err") && !m.text.starts_with("skipped");
                    let (icon, color) = if ok { ("✔", GREEN) } else { ("✘", RED) };
                    let label: String = first.chars().take(60).collect();
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
            Span::styled("type something —  enter:send  y/n:tool  /exit", Style::new().fg(DIM)),
        ]));
    } else if app.busy
        && app.msgs.last().map(|m| m.role.as_str() == "user").unwrap_or(false)
    {
        lines.push(Line::from(vec![
            Span::styled(" ◉ thinking…", Style::new().fg(P)),
        ]));
    }

    let max_vis = area.height.saturating_sub(1) as usize;
    let total = lines.len();
    let off = if app.scroll == usize::MAX || total <= max_vis {
        total.saturating_sub(max_vis)
    } else {
        app.scroll.min(total.saturating_sub(max_vis))
    };

    f.render_widget(
        Paragraph::new(Text::from(lines)).scroll((off as u16, 0)).style(Style::new().bg(BG)),
        area,
    );
}

// ───── Autocomplete ─────

fn draw_box(f: &mut Frame, app: &App, area: Rect) {
    let chat_focused = app.focus == Focus::Chat;

    // separator line
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("─".repeat(area.width as usize), Style::new().fg(BORDER)),
        ])).style(Style::new().bg(SURFACE)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    // input area
    let prompt = if app.busy {
        Span::styled(" ◉ ", Style::new().fg(P))
    } else if chat_focused {
        Span::styled(" > ", Style::new().fg(P))
    } else {
        Span::styled("   ", Style::new().fg(DIM))
    };

    let content: Vec<Span> = if !chat_focused {
        vec![Span::styled("press Tab to focus chat", Style::new().fg(DIM))]
    } else if app.busy && app.input.is_empty() {
        vec![prompt, Span::styled("waiting for response…", Style::new().fg(DIM))]
    } else if app.input.is_empty() {
        vec![prompt, Span::styled("type a message…", Style::new().fg(DIM))]
    } else {
        vec![prompt, Span::raw(&app.input), Span::styled("█", Style::new().fg(P))]
    };

    let input_rect = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);
    f.render_widget(
        Paragraph::new(Text::from(Line::from(content))).style(Style::new().bg(BG)),
        input_rect,
    );

    // ── autocomplete popup (Discord-style) ──
    let suggestions = &app.autocomplete_suggestions;
    if !suggestions.is_empty() && chat_focused && app.input.trim().starts_with('/') {
        let items: Vec<ListItem> = suggestions.iter().enumerate().map(|(i, cmd)| {
            let hl = Some(i) == app.autocomplete_idx;
            let desc = match cmd.as_str() {
                "/exit" => "exit", "/help" => "help", "/sessions" => "sidebar", "/new" => "new chat", _ => "",
            };
            let s = if hl { Style::new().bg(P).fg(BG) } else { Style::new().bg(BORDER).fg(TEXT) };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {}  {}", cmd, desc), s.add_modifier(if hl { Modifier::BOLD } else { Modifier::empty() })),
            ]))
        }).collect();

        let h = items.len() as u16 + 1;
        let px = area.x + 1;
        let py = area.y.saturating_sub(h);
        let pr = Rect::new(px, py, 28, h);
        f.render_widget(Paragraph::new("").style(Style::new().bg(BORDER)), pr);
        f.render_widget(
            List::new(items).style(Style::new().bg(BORDER)),
            Rect::new(px, py, 28, h.saturating_sub(1)),
        );
    }
}

// ───── Helpers ─────

fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

fn short_name(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    } else {
        s.to_string()
    }
}
