use crate::app::{App, Focus, MsgKind, SidebarItem};
use crate::util::short;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

// ══════════════════════════════════════════════════════════════
//  attacca.cc - 공식 Core Colors 다크 모드 (2026-07-29)
//  출처: attacca-web/src/index.css
// ══════════════════════════════════════════════════════════════

const BG: Color = Color::Rgb(20, 16, 14);          // --background:  #14100E
const CARD: Color = Color::Rgb(29, 23, 21);         // --card:        #1D1715
const POPOVER: Color = Color::Rgb(37, 30, 27);      // --popover:     #251E1B

const TEXT: Color = Color::Rgb(236, 231, 229);      // --foreground:  #ECE7E5
const DIM: Color = Color::Rgb(158, 142, 137);       // --muted-foreground: #9E8E89

const P: Color = Color::Rgb(204, 109, 92);           // --primary:     #CC6D5C
const P_FG: Color = Color::Rgb(13, 9, 8);            // --primary-foreground: #0D0908
const P_DIM: Color = Color::Rgb(180, 85, 70);

const ACCENT_BG: Color = Color::Rgb(62, 39, 35);     // --accent:      #3E2723

const DESTRUCTIVE: Color = Color::Rgb(232, 88, 84);  // --destructive: #E85854

// border: #FFFDF9 @ 13% over #14100E ≈ #2D2A27
const BORDER: Color = Color::Rgb(45, 42, 39);
// input: #FFFDF9 @ 16% over #14100E ≈ #302D2A
const INPUT_BG: Color = Color::Rgb(32, 26, 23);

const GREEN: Color = Color::Rgb(90, 180, 115);
const YELLOW: Color = Color::Rgb(220, 175, 65);

const SIDEW: u16 = 28;
/// Marks a card still being filled by token deltas.
const CURSOR: &str = "▌";

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
    let link = if app.connected {
        Span::styled("  bridge ◉", Style::new().fg(GREEN))
    } else {
        Span::styled("  reconnecting ○", Style::new().fg(DESTRUCTIVE))
    };

    let who = app
        .me
        .as_ref()
        .map(|m| m.display_name.clone())
        .unwrap_or_else(|| "…".to_string());

    let sid = app.sid.as_deref().map(short).unwrap_or_default();
    let status = if app.busy() { "running" } else { "ready" };
    let status_color = if app.busy() { YELLOW } else { GREEN };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(P).add_modifier(Modifier::BOLD)),
            Span::styled("Attacca", Style::new().fg(TEXT).add_modifier(Modifier::BOLD)),
            Span::styled(" ┃ ", Style::new().fg(BORDER)),
            Span::styled(status, Style::new().fg(status_color).add_modifier(Modifier::BOLD)),
            link,
            Span::styled(format!("  {who}"), Style::new().fg(TEXT)),
            Span::styled(format!("  {sid}"), Style::new().fg(DIM)),
        ])).style(Style::new().bg(CARD)),
        area,
    );

    f.render_widget(
        Paragraph::new("").style(Style::new().bg(BORDER)),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    );
}

// ───── Sidebar ─────

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    f.render_widget(Paragraph::new("").style(Style::new().bg(POPOVER)), area);

    // accent rail
    let rail_color = if focused { P } else { P_DIM };
    f.render_widget(
        Paragraph::new("").style(Style::new().bg(rail_color)),
        Rect::new(area.x, 1, 2, area.height.saturating_sub(2)),
    );

    // header
    let fg = if focused { P } else { DIM };
    let text_fg = if focused { TEXT } else { DIM };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ◆ ", Style::new().fg(fg).add_modifier(Modifier::BOLD).bg(POPOVER)),
            Span::styled("sessions", Style::new().fg(text_fg).add_modifier(Modifier::BOLD).bg(POPOVER)),
        ])).style(Style::new().bg(POPOVER)),
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
                    let s = if hl {
                        Style::new().fg(P).add_modifier(Modifier::BOLD).bg(ACCENT_BG)
                    } else {
                        Style::new().fg(P_DIM).bg(POPOVER)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {} {} {}", icon, short_name(name, 16), session_count), s),
                    ]))
                }
                SidebarItem::Session { title, active, running, .. } => {
                    let dot = if *running { "◉" } else if *active { "●" } else { "○" };
                    let s = if *active {
                        Style::new().fg(P).add_modifier(Modifier::BOLD)
                            .bg(if hl { ACCENT_BG } else { POPOVER })
                    } else if hl {
                        Style::new().fg(TEXT).add_modifier(Modifier::BOLD).bg(ACCENT_BG)
                    } else {
                        Style::new().fg(DIM).bg(POPOVER)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("   {} {}", dot, short_name(title, 17)), s),
                    ]))
                }
                SidebarItem::NewSession => {
                    let label = if hl { " ▸ + new" } else { "   + new" };
                    let s = if hl {
                        Style::new().fg(GREEN).bg(ACCENT_BG)
                    } else {
                        Style::new().fg(DIM).bg(POPOVER)
                    };
                    ListItem::new(Line::from(vec![Span::styled(label, s)]))
                }
            }
        }).collect();

    let la = Rect::new(area.x + 4, area.y + 1, area.width.saturating_sub(5), area.height.saturating_sub(4));
    f.render_widget(List::new(items).style(Style::new().bg(POPOVER)), la);

    let hint_s = if focused { Style::new().fg(P).bg(POPOVER) } else { Style::new().fg(DIM).bg(POPOVER) };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ↑↓·enter  ", hint_s),
            Span::styled("+new", Style::new().fg(GREEN).add_modifier(Modifier::BOLD).bg(POPOVER)),
        ])).style(Style::new().bg(POPOVER)),
        Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1),
    );
}

// ───── Chat ─────

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let w = area.width.saturating_sub(2) as usize;

    for m in &app.chat.msgs {
        match m.kind {
            MsgKind::Sys => {
                lines.push(Line::from(vec![
                    Span::styled(" ── ", Style::new().fg(DIM)),
                    Span::styled(m.text.clone(), Style::new().fg(DIM)),
                ]));
            }
            MsgKind::Agent => card(
                &mut lines, "assistant", w,
                Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                Style::new().fg(DIM),
                Style::new().fg(TEXT),
                &m.text, m.streaming,
            ),
            MsgKind::User => card(
                &mut lines, "you", w,
                Style::new().fg(P).add_modifier(Modifier::BOLD),
                Style::new().fg(P),
                Style::new().fg(TEXT),
                &m.text, false,
            ),
            MsgKind::Reasoning => card(
                &mut lines, "thinking", w,
                Style::new().fg(DIM).add_modifier(Modifier::DIM),
                Style::new().fg(DIM),
                Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
                &m.text, m.streaming,
            ),
            // Tool calls arrive as announced-capability events now: a record of what the agent did,
            // not a prompt to approve it.
            MsgKind::Tool => card(
                &mut lines, "tool", w,
                Style::new().fg(YELLOW).add_modifier(Modifier::BOLD),
                Style::new().fg(YELLOW),
                Style::new().fg(TEXT),
                &m.text, false,
            ),
            MsgKind::Result => {
                if let Some(first) = m.text.lines().next() {
                    let ok = !first.starts_with("err");
                    let (icon, color) = if ok { ("✔", GREEN) } else { ("✘", DESTRUCTIVE) };
                    let label: String = first.chars().take(60).collect();
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {icon} {label}"), Style::new().fg(color)),
                    ]));
                }
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  ◆ ", Style::new().fg(P)),
            Span::styled("type something -  enter:send  tab:autocomplete  /help", Style::new().fg(DIM)),
        ]));
    } else if app.busy() && !app.chat.msgs.last().is_some_and(|m| m.streaming) {
        // Only while nothing is arriving yet: once deltas land, the growing card is the indicator.
        lines.push(Line::from(vec![
            Span::styled(" ◉ thinking…", Style::new().fg(P)),
        ]));
    }

    let max_vis = area.height.saturating_sub(1) as usize;
    let total = lines.len();
    let off = if app.at_end || total <= max_vis {
        total.saturating_sub(max_vis)
    } else {
        total.saturating_sub(max_vis).saturating_sub(app.scroll)
    };

    f.render_widget(
        Paragraph::new(Text::from(lines)).scroll((off as u16, 0)).style(Style::new().bg(BG)),
        area,
    );
}

/// One bordered message card.
///
/// Wrapping happens here rather than via `Paragraph::wrap` so that `lines.len()` stays the true
/// rendered height - the scroll offset above is computed from it, and a widget that re-wrapped
/// underneath would drift the viewport by however many lines it added.
#[allow(clippy::too_many_arguments)]
fn card(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    w: usize,
    head_style: Style,
    rail_style: Style,
    body_style: Style,
    text: &str,
    streaming: bool,
) {
    let head = format!("┌─ {label} ");
    let pad = w.saturating_sub(head.chars().count());
    lines.push(Line::from(vec![Span::styled(
        format!("{head}{}", "─".repeat(pad)),
        head_style,
    )]));

    let body = wrap(text, w.saturating_sub(2));
    let last = body.len().saturating_sub(1);
    for (i, l) in body.into_iter().enumerate() {
        let l = if streaming && i == last {
            format!("{l}{CURSOR}")
        } else {
            l
        };
        lines.push(Line::from(vec![
            Span::styled("│ ", rail_style),
            Span::styled(l, body_style),
        ]));
    }

    lines.push(Line::from(vec![Span::styled(
        format!("└{}", "─".repeat(w.saturating_sub(1))),
        rail_style.add_modifier(Modifier::DIM),
    )]));
}

// ───── Input box ─────

fn draw_box(f: &mut Frame, app: &App, area: Rect) {
    let chat_focused = app.focus == Focus::Chat;

    // separator
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("─".repeat(area.width as usize), Style::new().fg(BORDER)),
        ])).style(Style::new().bg(INPUT_BG)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let prompt = if app.busy() {
        Span::styled(" ◉ ", Style::new().fg(P))
    } else if chat_focused {
        Span::styled(" > ", Style::new().fg(P))
    } else {
        Span::styled("   ", Style::new().fg(DIM))
    };

    let content: Vec<Span> = if !chat_focused {
        vec![Span::styled("press Tab to focus chat", Style::new().fg(DIM))]
    } else if app.busy() && app.input.is_empty() {
        vec![prompt, Span::styled("waiting…", Style::new().fg(DIM))]
    } else if app.input.is_empty() {
        vec![prompt, Span::styled("type a message…", Style::new().fg(DIM))]
    } else {
        vec![prompt, Span::raw(&app.input), Span::styled("█", Style::new().fg(P))]
    };

    let ir = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);
    f.render_widget(
        Paragraph::new(Text::from(Line::from(content))).style(Style::new().bg(BG)),
        ir,
    );

    // autocomplete
    let suggestions = &app.autocomplete_suggestions;
    if !suggestions.is_empty() && chat_focused && app.input.trim().starts_with('/') {
        let items: Vec<ListItem> = suggestions.iter().enumerate().map(|(i, cmd)| {
            let hl = Some(i) == app.autocomplete_idx;
            let desc = match cmd.as_str() {
                "/exit" => "exit",
                "/help" => "help",
                "/sessions" => "sidebar",
                "/new" => "new chat",
                "/cancel" => "stop the turn",
                "/login" => "authorize again",
                "/whoami" => "identity & scopes",
                "/logout" => "forget credential",
                "/tools" => "announced tools",
                _ => "",
            };
            let s = if hl { Style::new().bg(P).fg(P_FG) } else { Style::new().bg(CARD).fg(TEXT) };
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {}  {}", cmd, desc), s.add_modifier(if hl { Modifier::BOLD } else { Modifier::empty() })),
            ]))
        }).collect();

        let h = items.len() as u16 + 1;
        let pr = Rect::new(area.x + 1, area.y.saturating_sub(h), 28, h);
        f.render_widget(Paragraph::new("").style(Style::new().bg(CARD)), pr);
        f.render_widget(List::new(items).style(Style::new().bg(CARD)), Rect::new(pr.x, pr.y, 28, h.saturating_sub(1)));
    }
}

// ───── Helpers ─────

/// Greedy word wrap. A word longer than the line is broken rather than clipped, which token
/// streaming makes routine: a URL or a stack frame arrives as one unbroken run of characters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out: Vec<String> = Vec::new();

    for raw in text.split('\n') {
        let mut cur = String::new();
        let mut len = 0usize;

        for word in raw.split(' ') {
            let wlen = word.chars().count();
            if len > 0 && len + 1 + wlen > width {
                out.push(std::mem::take(&mut cur));
                len = 0;
            }
            if wlen > width {
                if len > 0 {
                    out.push(std::mem::take(&mut cur));
                }
                for ch in word.chars() {
                    if cur.chars().count() == width {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.push(ch);
                }
                len = cur.chars().count();
                continue;
            }
            if len > 0 {
                cur.push(' ');
                len += 1;
            }
            cur.push_str(word);
            len += wlen;
        }
        out.push(cur);
    }

    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn short_name(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_on_spaces_and_keeps_every_word() {
        let lines = wrap("the quick brown fox jumps", 10);
        assert!(lines.iter().all(|l| l.chars().count() <= 10), "{lines:?}");
        assert_eq!(lines.join(" "), "the quick brown fox jumps");
    }

    #[test]
    fn wrap_preserves_explicit_newlines() {
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
    }

    /// A long unbroken run - a URL, a base64 blob - must be split, not clipped off the right edge.
    #[test]
    fn wrap_hard_breaks_a_word_longer_than_the_line() {
        let lines = wrap(&"x".repeat(25), 10);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.chars().count() <= 10), "{lines:?}");
        assert_eq!(lines.concat(), "x".repeat(25));
    }

    #[test]
    fn wrap_is_utf8_safe() {
        let lines = wrap("한국어 텍스트가 줄바꿈됩니다", 8);
        assert!(lines.iter().all(|l| l.chars().count() <= 8), "{lines:?}");
    }
}
