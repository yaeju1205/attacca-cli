use crate::app::{App, SidebarItem};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

// ── opencode-inspired color palette ──

const BG: Color = Color::Rgb(13, 13, 20);
const SURFACE: Color = Color::Rgb(18, 18, 28);
const BORDER: Color = Color::Rgb(28, 28, 42);
const ACCENT: Color = Color::Rgb(0, 212, 170);
const TEXT: Color = Color::Rgb(210, 210, 225);
const DIM: Color = Color::Rgb(100, 100, 130);
const USER: Color = Color::Rgb(255, 180, 100);
const AGENT: Color = Color::Rgb(100, 180, 255);
const TOOL: Color = Color::Rgb(255, 200, 80);
const GREEN: Color = Color::Rgb(80, 200, 120);
const RED: Color = Color::Rgb(255, 80, 80);

const SIDEW: u16 = 30;

pub fn draw(f: &mut Frame, app: &App) {
    let a = f.area();
    if a.width < 50 || a.height < 10 { return; }
    f.render_widget(Paragraph::new("").style(Style::new().bg(BG)), a);
    let chunks = Layout::default().direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)])
        .split(a);
    draw_status(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_input(f, app, chunks[2]);
}

// ── Status ──

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let sid = app.sid.as_ref().map(|s| short(s)).unwrap_or_default();
    let status = if app.busy { " ◉ running" } else { " ● ready" };
    let key = if app.api.key.is_empty() { Span::styled(" ✗ no key", Style::new().fg(RED)) }
               else { Span::styled(" ✓ key", Style::new().fg(GREEN)) };
    let line = Line::from(vec![
        Span::styled(" ❖ ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled("attacca", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
        Span::styled(status, Style::new().fg(if app.busy { TOOL } else { GREEN })),
        Span::styled(format!("  {}  ", sid), Style::new().fg(DIM)),
        key,
    ]);
    f.render_widget(Paragraph::new(line).style(Style::new().bg(BORDER)), area);
}

// ── Main ──

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    if app.show_sidebar {
        let c = Layout::default().direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEW), Constraint::Min(30)]).split(area);
        draw_sidebar(f, app, c[0]);
        draw_chat(f, app, c[1]);
    } else {
        draw_chat(f, app, area);
    }
}

// ── Sidebar (tree: Project → Sessions) ──

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Paragraph::new("").style(Style::new().bg(SURFACE)), area);

    // header
    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled("  projects / chats", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD).bg(SURFACE))]))
            .style(Style::new().bg(SURFACE)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let sel = app.sel;
    let items: Vec<ListItem> = app.sidebar_items.iter().enumerate().map(|(i, item)| {
        let highlight = i == sel;
        match item {
            SidebarItem::ProjectHeader { name, expanded, session_count, .. } => {
                let icon = if *expanded { "▼" } else { "▶" };
                let label = format!(" {icon} {name} ({session_count})");
                let style = if highlight {
                    Style::new().fg(TEXT).add_modifier(Modifier::BOLD).bg(BORDER)
                } else {
                    Style::new().fg(ACCENT).bg(SURFACE)
                };
                ListItem::new(Line::from(vec![Span::styled(label, style)]))
            }
            SidebarItem::Session { title, active, .. } => {
                let dot = if *active { "●" } else { "○" };
                let t: String = title.chars().take(20).collect();
                let label = format!("   {dot} {t}");
                let style = if *active {
                    Style::new().fg(AGENT).add_modifier(Modifier::BOLD).bg(if highlight { BORDER } else { SURFACE })
                } else if highlight {
                    Style::new().fg(TEXT).add_modifier(Modifier::BOLD).bg(BORDER)
                } else {
                    Style::new().fg(DIM).bg(SURFACE)
                };
                ListItem::new(Line::from(vec![Span::styled(label, style)]))
            }
            SidebarItem::NewSession => {
                let label = if highlight { " ▸ + new session" } else { "   + new session" };
                let style = if highlight {
                    Style::new().fg(ACCENT).bg(BORDER)
                } else {
                    Style::new().fg(DIM).bg(SURFACE)
                };
                ListItem::new(Line::from(vec![Span::styled(label, style)]))
            }
        }
    }).collect();

    let list_area = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(4));
    f.render_widget(
        List::new(items).highlight_style(Style::new().bg(BORDER)).style(Style::new().bg(SURFACE)),
        list_area,
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ◄ enter:open  ←:collapse  tab/esc:close", Style::new().fg(DIM).bg(SURFACE))
        ])).style(Style::new().bg(SURFACE)),
        Rect::new(area.x, area.y + area.height.saturating_sub(2), area.width, 1),
    );
}

// ── Chat ──

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for m in &app.msgs {
        match m.role.as_str() {
            "sys" => {
                lines.push(Line::from(vec![Span::styled(format!(" ┄ {} ────", m.text), Style::new().fg(DIM))]));
            }
            "user" => {
                lines.push(Line::from(vec![Span::styled(" ── you ", Style::new().fg(USER).add_modifier(Modifier::BOLD))]));
                for l in m.text.lines() {
                    lines.push(Line::from(vec![Span::styled(" │ ", Style::new().fg(DIM)), Span::raw(l)]));
                }
            }
            "agent" => {
                for (i, l) in m.text.lines().enumerate() {
                    if i == 0 {
                        lines.push(Line::from(vec![Span::styled(" ── assistant ", Style::new().fg(AGENT).add_modifier(Modifier::BOLD))]));
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
                    let prefix = if m.text.starts_with("err") || m.text.starts_with("skipped") { " ✖" } else { " ✔" };
                    let label: String = first.chars().take(60).collect();
                    lines.push(Line::from(vec![Span::styled(format!("   {prefix} {label}"), Style::new().fg(DIM))]));
                }
            }
            _ => {}
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![Span::styled(" enter:send · tab:sessions · y/n:tool · q:quit", Style::new().fg(DIM))]));
    } else if app.busy {
        // show loading indicator in chat
        let last_role = app.msgs.last().map(|m| m.role.as_str()).unwrap_or("");
        if last_role == "user" {
            lines.push(Line::from(vec![
                Span::styled(" ◉ ", Style::new().fg(TOOL).add_modifier(Modifier::BOLD)),
                Span::styled("thinking…", Style::new().fg(DIM)),
            ]));
        }
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

// ── Input ──

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let spans: Vec<Span> = if app.input.is_empty() && !app.busy {
        vec![Span::styled(" type a message…", Style::new().fg(DIM))]
    } else if app.busy && app.input.is_empty() {
        vec![Span::styled(" ◉ ", Style::new().fg(TOOL)), Span::styled("waiting…", Style::new().fg(DIM))]
    } else {
        vec![Span::raw(format!(" {}", app.input))]
    };

    f.render_widget(
        Paragraph::new(Text::from(Line::from(spans)))
            .block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(BORDER)))
            .style(Style::new().bg(SURFACE)),
        area,
    );
}

// ── Util ──

fn short(s: &str) -> String {
    s.chars().take(8).collect()
}
