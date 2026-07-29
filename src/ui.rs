use crate::app::{App, SidebarItem};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

// ── Attacca brand color palette ──

const BG: Color = Color::Rgb(10, 10, 18);
const SURFACE: Color = Color::Rgb(16, 16, 28);
const BORDER: Color = Color::Rgb(30, 30, 48);
const ACCENT: Color = Color::Rgb(139, 92, 246);
const ACCENT_DIM: Color = Color::Rgb(100, 60, 200);
const TEXT: Color = Color::Rgb(220, 220, 240);
const DIM: Color = Color::Rgb(100, 100, 130);
const USER: Color = Color::Rgb(251, 191, 36);
const AGENT: Color = Color::Rgb(96, 165, 250);
const TOOL: Color = Color::Rgb(250, 204, 21);
const GREEN: Color = Color::Rgb(52, 211, 153);
const RED: Color = Color::Rgb(239, 68, 68);
const STATUS_BAR: Color = Color::Rgb(20, 20, 35);

const SIDEW: u16 = 28;

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
    draw_input(f, app, chunks[2]);
}

// ── Status Bar ──

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let status = if app.busy { " ◉ running" } else { " ● ready" };
    let status_color = if app.busy { TOOL } else { GREEN };
    let sid = app.sid.as_ref().map(|s| short(s)).unwrap_or_default();
    let key = if app.api.key.is_empty() {
        Span::styled("  ✗ no key", Style::new().fg(RED))
    } else {
        Span::styled("  ✓ connected", Style::new().fg(GREEN))
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled("attacca", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
            Span::styled(status, Style::new().fg(status_color)),
            Span::styled(if sid.is_empty() { String::new() } else { format!("  {}", sid) }, Style::new().fg(DIM)),
            key,
        ])).style(Style::new().bg(STATUS_BAR)),
        area,
    );
}

// ── Sidebar ──

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Paragraph::new("").style(Style::new().bg(SURFACE)), area);

    // purple accent line
    f.render_widget(
        Paragraph::new("").style(Style::new().bg(ACCENT)),
        Rect::new(area.x, area.y + 1, 2, area.height.saturating_sub(3)),
    );

    // header
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD).bg(SURFACE)),
            Span::styled("sessions", Style::new().fg(TEXT).add_modifier(Modifier::BOLD).bg(SURFACE)),
        ])).style(Style::new().bg(SURFACE)),
        Rect::new(area.x + 3, area.y, area.width, 1),
    );

    let sel = app.sel;
    let scroll = app.sidebar_scroll;
    let max_vis = (area.height.saturating_sub(4)) as usize;
    let items: Vec<ListItem> = app.sidebar_items.iter().enumerate()
        .filter(|&(i, _)| i >= scroll)
        .take(max_vis)
        .map(|(orig_i, item)| {
            let highlight = orig_i == sel;
            match item {
                SidebarItem::ProjectHeader { name, expanded, session_count, .. } => {
                    let icon = if *expanded { "▾" } else { "▸" };
                    let style = if highlight {
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD).bg(BORDER)
                    } else {
                        Style::new().fg(ACCENT_DIM).bg(SURFACE)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {} ", icon), style),
                        Span::styled(short_name(name, 18), style),
                        Span::styled(format!(" {}", session_count), Style::new().fg(DIM).bg(if highlight { BORDER } else { SURFACE })),
                    ]))
                }
                SidebarItem::Session { title, active, .. } => {
                    let dot = if *active { "●" } else { "○" };
                    let style = if *active {
                        Style::new().fg(AGENT).add_modifier(Modifier::BOLD).bg(if highlight { BORDER } else { SURFACE })
                    } else if highlight {
                        Style::new().fg(TEXT).add_modifier(Modifier::BOLD).bg(BORDER)
                    } else {
                        Style::new().fg(DIM).bg(SURFACE)
                    };
                    let indent = "  ";
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{}{} ", indent, dot), style),
                        Span::styled(short_name(title, 18), style),
                    ]))
                }
                SidebarItem::NewSession => {
                    let label = if highlight { " ▸ + new" } else { "   + new" };
                    let style = if highlight {
                        Style::new().fg(GREEN).bg(BORDER)
                    } else {
                        Style::new().fg(DIM).bg(SURFACE)
                    };
                    ListItem::new(Line::from(vec![Span::styled(label, style)]))
                }
            }
        }).collect();

    let list_area = Rect::new(area.x + 3, area.y + 1, area.width.saturating_sub(3), area.height.saturating_sub(4));
    f.render_widget(
        List::new(items).style(Style::new().bg(SURFACE)),
        list_area,
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ↑↓ scroll · enter open", Style::new().fg(DIM).bg(SURFACE)),
        ])).style(Style::new().bg(SURFACE)),
        Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1),
    );
}

// ── Chat ──

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for m in &app.msgs {
        match m.role.as_str() {
            "sys" => {
                lines.push(Line::from(vec![
                    Span::styled(" ── ", Style::new().fg(DIM)),
                    Span::styled(&m.text, Style::new().fg(DIM)),
                ]));
            }
            "user" => {
                lines.push(Line::from(vec![
                    Span::styled(" ── ", Style::new().fg(USER).add_modifier(Modifier::BOLD)),
                    Span::styled("you", Style::new().fg(USER).add_modifier(Modifier::BOLD)),
                ]));
                for l in m.text.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(" │ ", Style::new().fg(DIM)),
                        Span::raw(l),
                    ]));
                }
            }
            "agent" => {
                for (i, l) in m.text.lines().enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![
                            Span::styled(" ", Style::new().fg(AGENT)),
                            Span::styled("assistant", Style::new().fg(AGENT).add_modifier(Modifier::BOLD)),
                        ]));
                    }
                    lines.push(Line::from(Span::raw(format!(" {}", l))));
                }
            }
            "tool" if !m.done => {
                lines.push(Line::from(vec![
                    Span::styled(" ◆ ", Style::new().fg(TOOL).add_modifier(Modifier::BOLD)),
                    Span::styled(&m.text, Style::new().fg(TOOL)),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("    [", Style::new().fg(DIM)),
                    Span::styled("y", Style::new().fg(GREEN).add_modifier(Modifier::BOLD)),
                    Span::styled("] run  [", Style::new().fg(DIM)),
                    Span::styled("n", Style::new().fg(RED).add_modifier(Modifier::BOLD)),
                    Span::styled("] skip", Style::new().fg(DIM)),
                ]));
            }
            "tool" => {}
            "result" => {
                if let Some(first) = m.text.lines().next() {
                    let ok = !m.text.starts_with("err") && !m.text.starts_with("skipped");
                    let prefix = if ok { " ✔" } else { " ✖" };
                    let label: String = first.chars().take(60).collect();
                    lines.push(Line::from(vec![Span::styled(format!("  {prefix} {label}"), Style::new().fg(if ok { GREEN } else { RED }))]));
                }
            }
            _ => {}
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  ◆  enter:send · y/n:tool · /exit\n", Style::new().fg(DIM)),
        ]));
    } else if app.busy
        && app.msgs.last().map(|m| m.role.as_str() == "user").unwrap_or(false)
    {
        lines.push(Line::from(vec![
            Span::styled(" ◉ ", Style::new().fg(ACCENT)),
            Span::styled("thinking…", Style::new().fg(DIM)),
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

// ── Input Bar (opencode-style: clean, no border box) ──

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    // thin separator line
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("─".repeat(area.width as usize), Style::new().fg(BORDER)),
        ])).style(Style::new().bg(SURFACE)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let prompt = if app.busy {
        Span::styled(" ◉ ", Style::new().fg(ACCENT))
    } else {
        Span::styled(" > ", Style::new().fg(ACCENT_DIM))
    };

    let content: Vec<Span> = if app.busy && app.input.is_empty() {
        vec![prompt, Span::styled("waiting…", Style::new().fg(DIM))]
    } else if app.input.is_empty() {
        vec![prompt, Span::styled("type a message…", Style::new().fg(DIM))]
    } else {
        vec![
            prompt,
            Span::raw(&app.input),
            Span::styled("█", Style::new().fg(ACCENT_DIM)),
        ]
    };

    let input_area = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);
    f.render_widget(
        Paragraph::new(Text::from(Line::from(content))).style(Style::new().bg(BG)),
        input_area,
    );
}

// ── Helpers ──

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
