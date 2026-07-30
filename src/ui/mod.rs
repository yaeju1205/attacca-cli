use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Focus, Msg, MsgKind, SidebarItem};
use crate::util::short;
use palette::*;

mod palette;

/// Trailing block drawn on a card that is still receiving token deltas.
const CURSOR: &str = "▌";

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
    let mode = if app.connected {
        Span::styled("  ◉ online", Style::new().fg(GREEN))
    } else {
        Span::styled("  offline", Style::new().fg(DESTRUCTIVE))
    };

    let sid = app.sid.as_ref().map(|s| short(s)).unwrap_or_default();
    let status = if app.busy() { "running" } else { "ready" };
    let status_color = if app.busy() { YELLOW } else { GREEN };

    let mut right = Vec::new();
    if let Some(me) = &app.me {
        right.push(Span::styled(
            format!("  {}", me.display_name),
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
                SidebarItem::Session { title, active, running, .. } => {
                    let dot = if *running { "◉" } else if *active { "●" } else { "○" };
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

/// One bordered message card: a header rule, the hard-wrapped body behind a `│ ` rail,
/// and a footer rule.
///
/// This is the only place message bodies are wrapped. [`wrap_chars`] is width-aware, so
/// double-width characters cannot overflow the panel — and the streaming cursor is
/// appended as its own span rather than a character, so it never enters the wrap and
/// never changes the row count. Both properties are load-bearing for the scroll offset
/// in [`draw_chat`], which is computed from `lines.len()`.
#[allow(clippy::too_many_arguments)]
fn card(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    w: usize,
    content_width: usize,
    head_style: Style,
    rail_style: Style,
    body_style: Style,
    text: &str,
    streaming: bool,
) {
    let head = format!("┌─ {label} ");
    lines.push(Line::from(vec![Span::styled(
        head.clone() + &"─".repeat(w.saturating_sub(head.chars().count())),
        head_style,
    )]));

    let rows: Vec<String> = text
        .lines()
        .flat_map(|l| wrap_chars(l, content_width))
        .collect();
    let last = rows.len().saturating_sub(1);
    for (i, chunk) in rows.into_iter().enumerate() {
        let mut spans = vec![
            Span::styled("│ ", rail_style),
            Span::styled(chunk, body_style),
        ];
        if streaming && i == last {
            spans.push(Span::styled(CURSOR, Style::new().fg(P)));
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::from(vec![Span::styled(
        "└".to_string() + &"─".repeat(w.saturating_sub(2)),
        rail_style.add_modifier(Modifier::DIM),
    )]));
}

/// Turn the transcript into visual rows. Split out from [`draw_chat`] so it can be
/// tested without a `Frame`.
fn chat_lines(msgs: &[Msg], w: usize, content_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for m in msgs {
        match m.kind {
            MsgKind::Sys => {
                lines.push(Line::from(vec![
                    Span::styled(" ── ", Style::new().fg(DIM)),
                    Span::styled(m.text.clone(), Style::new().fg(DIM)),
                ]));
            }
            MsgKind::Agent => card(
                &mut lines,
                "assistant",
                w,
                content_width,
                Style::new().fg(TEXT).add_modifier(Modifier::BOLD),
                Style::new().fg(DIM),
                Style::new(),
                &m.text,
                m.streaming,
            ),
            MsgKind::Reasoning => card(
                &mut lines,
                "thinking",
                w,
                content_width,
                Style::new().fg(DIM).add_modifier(Modifier::DIM),
                Style::new().fg(DIM),
                Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
                &m.text,
                m.streaming,
            ),
            MsgKind::User => card(
                &mut lines,
                "you",
                w,
                content_width,
                Style::new().fg(P).add_modifier(Modifier::BOLD),
                Style::new().fg(P),
                Style::new(),
                &m.text,
                false,
            ),
            MsgKind::Tool => card(
                &mut lines,
                "tool",
                w,
                content_width,
                Style::new().fg(YELLOW).add_modifier(Modifier::BOLD),
                Style::new().fg(YELLOW),
                Style::new().fg(TEXT),
                &m.text,
                false,
            ),
            MsgKind::Result => {
                if let Some(first) = m.text.lines().next() {
                    let ok = !m.text.starts_with("err");
                    let (icon, color) = if ok { ("ok", GREEN) } else { ("✘", DESTRUCTIVE) };
                    let label: String = first.chars().take(58).collect();
                    lines.push(Line::from(vec![Span::styled(
                        format!("  {icon} {label}"),
                        Style::new().fg(color),
                    )]));
                }
            }
        }
    }

    lines
}

/// Wrap the last `k` messages (post `hide_reasoning` filtering) into visual rows, and report
/// whether `k` reached the start of the transcript.
///
/// Bounding the slice to `k` — rather than the whole transcript — is what keeps this cheap: the
/// clone for `hide_reasoning` is `O(k)`, not `O(history)`, and the default path still borrows.
fn wrap_tail(
    msgs: &[Msg],
    k: usize,
    w: usize,
    content_width: usize,
    hide_reasoning: bool,
) -> (Vec<Line<'static>>, bool) {
    let start = msgs.len().saturating_sub(k);
    let slice = &msgs[start..];
    let filtered: Vec<Msg>;
    let visible: &[Msg] = if hide_reasoning {
        filtered = slice
            .iter()
            .filter(|m| m.kind != MsgKind::Reasoning)
            .cloned()
            .collect();
        &filtered
    } else {
        slice
    };
    (chat_lines(visible, w, content_width), start == 0)
}

/// The rows actually shown in the chat viewport: the placeholder/"thinking" line applied, and
/// scrolled to the current position.
///
/// Only the rows that actually fit on screen (plus however far back a manual scroll has gone) are
/// ever shown, so wrapping the whole transcript from scratch on every call is wasted work that
/// grows without bound as a chat gets longer — and this runs on every frame, ~125/s while a turn is
/// streaming. Instead, wrap just enough of the tail to cover the visible window, doubling the slice
/// until it does (or the transcript runs out). Wrapping is per-message, so this costs
/// `O(viewport)`, not `O(history)`.
#[allow(clippy::too_many_arguments)]
fn visible_chat_rows(
    msgs: &[Msg],
    w: usize,
    content_width: usize,
    max_rows: usize,
    at_end: bool,
    scroll: usize,
    hide_reasoning: bool,
    busy: bool,
) -> Vec<Line<'static>> {
    let needed = max_rows + if at_end { 0 } else { scroll };
    let mut k = needed.max(8);
    let (mut lines, mut exhausted) = wrap_tail(msgs, k, w, content_width, hide_reasoning);
    while lines.len() < needed && !exhausted {
        k = k.saturating_mul(2).max(k + 1);
        (lines, exhausted) = wrap_tail(msgs, k, w, content_width, hide_reasoning);
    }

    if lines.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  ◆ ", Style::new().fg(P)),
            Span::styled("type something —  enter:send  /help", Style::new().fg(DIM)),
        ]));
    } else if busy && !msgs.last().is_some_and(|m| m.streaming) {
        lines.push(Line::from(vec![
            Span::styled(" ◉ thinking…", Style::new().fg(P)),
        ]));
    }

    // Show lines starting from scroll offset. Message bodies are hard-wrapped
    // above, so each entry here is already a visual row (1:1, no bottom clip).
    let total = lines.len();
    let scroll_off = if at_end || total <= max_rows {
        total.saturating_sub(max_rows)
    } else {
        total.saturating_sub(max_rows).saturating_sub(scroll)
    };
    let end = (scroll_off + max_rows).min(total);
    lines.drain(scroll_off..end).collect()
}

// Chat area
fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let w = area.width.saturating_sub(1) as usize;
    // Content width inside the "│ " gutter, used to hard-wrap message bodies
    // so long lines flow to the next row instead of overflowing the panel.
    let content_width = w.saturating_sub(2).max(1);
    let max_rows = area.height.saturating_sub(1) as usize;

    let window = visible_chat_rows(
        &app.chat.msgs,
        w,
        content_width,
        max_rows,
        app.at_end,
        app.scroll,
        app.hide_reasoning,
        app.busy(),
    );

    f.render_widget(
        Paragraph::new(Text::from(window)).style(Style::new().bg(BG)),
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
        if !app.usage_credits_used.is_empty() {
            s.push_str(&format!(" credits:{}", app.usage_credits_used));
        }
        if !app.usage_model.is_empty() {
            s.push_str(&format!(" {}", app.usage_model));
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
    let mut rows: Vec<Line> = Vec::new();

    if app.input.is_empty() {
        rows.push(Line::from(vec![
            prompt,
            Span::styled("type a message…", Style::new().fg(DIM)),
        ]));
    } else {
        // Locate the cursor's segment and its byte offset within that segment, so the
        // block glyph lands inline at the right spot instead of always at the end.
        let cursor = app.input_cursor.min(app.input.len());
        let mut first_overall = true;
        let mut seg_start = 0usize;
        for seg in segments.iter() {
            let seg_len = seg.len();
            let cursor_offset =
                (cursor >= seg_start && cursor <= seg_start + seg_len).then(|| cursor - seg_start);

            let chunks = wrap_chars(seg, content_width);
            let chunk_count = chunks.len();
            let mut chunk_start = 0usize;
            for (ci, chunk) in chunks.into_iter().enumerate() {
                let chunk_end = chunk_start + chunk.len();
                let gutter = if first_overall {
                    prompt.clone()
                } else {
                    Span::styled("   ", Style::new().fg(DIM))
                };
                first_overall = false;

                let cursor_here = cursor_offset.filter(|&off| {
                    (off >= chunk_start && off < chunk_end) || (off == chunk_end && ci == chunk_count - 1)
                });

                match cursor_here {
                    Some(off) => {
                        let (left, right) = chunk.split_at(off - chunk_start);
                        rows.push(Line::from(vec![
                            gutter,
                            Span::raw(left.to_string()),
                            Span::raw(right.to_string()),
                        ]));
                        // Stash (global_row, col) for hardware cursor placement.
                        app.cursor_screen = Some((
                            (rows.len() - 1) as u16,
                            (3 + UnicodeWidthStr::width(&*left)) as u16,
                        ));
                    }
                    None => rows.push(Line::from(vec![gutter, Span::raw(chunk)])),
                }
                chunk_start = chunk_end;
            }
            seg_start += seg_len + 1; // +1 skips the '\n' separator
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

    // Resolve stashed cursor position into screen coordinates (after scroll_off is known).
    // For empty input the cursor sits right after the prompt.
    let cursor_screen = if app.input.is_empty() && chat_focused {
        Some((text_render_area.x + 3, text_render_area.y))
    } else {
        app.cursor_screen.and_then(|(global_row, col)| {
            let gr = global_row as usize;
            if gr >= scroll_off && gr < scroll_off + view {
                let vr = (gr - scroll_off) as u16;
                Some((
                    (text_render_area.x + col).min(text_render_area.x + text_render_area.width.saturating_sub(1)),
                    text_render_area.y + vr,
                ))
            } else {
                None
            }
        })
    };
    app.cursor_screen = cursor_screen;

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
                "/sessions" => "sidebar + refresh",
                "/new" => "new chat",
                "/cancel" => "stop the turn",
                "/login" => "authorize node",
                "/logout" => "forget credential",
                "/whoami" => "identity, scopes",
                "/usage" => "account + usage",
                "/tools" => "server capabilities",
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

/// Hard-wrap a single logical line into visual rows of at most `width`
/// terminal columns, accounting for double-width characters (e.g. Hangul,
/// CJK) so wrapped rows never overflow the panel. An empty line yields one
/// empty row so blank input lines still take a row.
fn wrap_chars(seg: &str, width: usize) -> Vec<String> {
    if seg.is_empty() {
        return vec![String::new()];
    }
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in seg.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cur_w + cw > width && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += cw;
    }
    rows.push(cur);
    rows
}

fn short_name(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max {
        format!("{}..", s.chars().take(max.saturating_sub(2)).collect::<String>())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(s: &str) -> usize {
        s.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(0)).sum()
    }

    /// One sample per class `wrap_chars` has to get right: single-width, double-width,
    /// the mix that broke the panel in the first place, emoji, and zero-width marks.
    const SAMPLES: &[&str] = &[
        "the quick brown fox jumps over the lazy dog",
        "안녕하세요 반갑습니다 오늘도 좋은 하루 되세요",
        "mixed 한글 and ASCII 텍스트 in one line 12345",
        "🎉 emoji 🚀 mixed 🔥 with text",
        "e\u{0301}gal combining\u{0301} marks\u{0301}",
        "짧",
    ];

    /// Widths start at 2: a budget of 1 cannot hold a double-width character, and
    /// `wrap_chars` emits it anyway rather than dropping it or looping forever.
    const WIDTHS: std::ops::RangeInclusive<usize> = 2..=20;

    #[test]
    fn wrap_chars_never_exceeds_the_column_budget() {
        for seg in SAMPLES {
            for width in WIDTHS {
                for row in wrap_chars(seg, width) {
                    assert!(
                        cols(&row) <= width,
                        "{:?} at width {width} produced a {}-column row {:?}",
                        seg,
                        cols(&row),
                        row
                    );
                }
            }
        }
    }

    #[test]
    fn wrap_chars_preserves_every_character() {
        for seg in SAMPLES {
            for width in WIDTHS {
                assert_eq!(
                    wrap_chars(seg, width).concat(),
                    **seg,
                    "{seg:?} lost or gained characters at width {width}"
                );
            }
        }
    }

    #[test]
    fn wrap_chars_yields_one_row_for_an_empty_segment() {
        assert_eq!(wrap_chars("", 10), vec![String::new()]);
    }

    #[test]
    fn a_segment_that_fits_is_left_as_one_row() {
        assert_eq!(wrap_chars("짧은 글", 10), vec!["짧은 글".to_string()]);
    }

    #[test]
    fn a_streaming_card_adds_exactly_one_row_beyond_its_wrapped_body() {
        // `draw_chat` derives its scroll offset from `lines.len()`, so a card must occupy
        // the same number of rows streaming as settled — otherwise the viewport jumps at
        // the moment the cursor disappears.
        let text = "안녕하세요 반갑습니다 오늘도 좋은 하루 되세요 and then some ASCII to push it over";
        for w in [20usize, 40, 80] {
            let cw = w.saturating_sub(2).max(1);
            let plain = Style::new();
            let mut streaming = Vec::new();
            card(&mut streaming, "assistant", w, cw, plain, plain, plain, text, true);
            let mut settled = Vec::new();
            card(&mut settled, "assistant", w, cw, plain, plain, plain, text, false);
            assert_eq!(
                streaming.len(),
                settled.len(),
                "row count changed with the cursor at width {w}"
            );
        }
    }

    // ── Windowed vs. full-history rendering ────────────────────

    /// What [`visible_chat_rows`] replaced: wrap the *entire* transcript, then slice out the
    /// visible window. Kept here only as an oracle to check the windowed version against.
    #[allow(clippy::too_many_arguments)]
    fn full_visible_rows(
        msgs: &[Msg],
        w: usize,
        content_width: usize,
        max_rows: usize,
        at_end: bool,
        scroll: usize,
        hide_reasoning: bool,
        busy: bool,
    ) -> Vec<Line<'static>> {
        let filtered: Vec<Msg>;
        let visible: &[Msg] = if hide_reasoning {
            filtered = msgs
                .iter()
                .filter(|m| m.kind != MsgKind::Reasoning)
                .cloned()
                .collect();
            &filtered
        } else {
            msgs
        };
        let mut lines = chat_lines(visible, w, content_width);

        if lines.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  ◆ ", Style::new().fg(P)),
                Span::styled("type something —  enter:send  /help", Style::new().fg(DIM)),
            ]));
        } else if busy && !msgs.last().is_some_and(|m| m.streaming) {
            lines.push(Line::from(vec![
                Span::styled(" ◉ thinking…", Style::new().fg(P)),
            ]));
        }

        let total = lines.len();
        let scroll_off = if at_end || total <= max_rows {
            total.saturating_sub(max_rows)
        } else {
            total.saturating_sub(max_rows).saturating_sub(scroll)
        };
        let end = (scroll_off + max_rows).min(total);
        lines.drain(scroll_off..end).collect()
    }

    fn synthetic_msgs(n: usize) -> Vec<Msg> {
        (0..n)
            .map(|i| {
                let kind = match i % 6 {
                    0 => MsgKind::Sys,
                    1 => MsgKind::User,
                    2 => MsgKind::Agent,
                    3 => MsgKind::Reasoning,
                    4 => MsgKind::Tool,
                    _ => MsgKind::Result,
                };
                let text = format!(
                    "message {i} with 한글 텍스트 and some padding {}",
                    "x".repeat(i % 7)
                );
                Msg {
                    kind,
                    text,
                    streaming: false,
                }
            })
            .collect()
    }

    /// The windowed tail computation must show exactly what wrapping the whole transcript and
    /// then slicing would have shown — across transcript lengths (short, and long enough to force
    /// the doubling loop to grow past its first guess), viewport sizes, scroll depths, `at_end`,
    /// `hide_reasoning`, and a busy/streaming tail.
    #[test]
    fn windowed_rows_match_full_history_computation() {
        for n in [0usize, 1, 5, 50] {
            for streaming_last in [false, true] {
                let mut msgs = synthetic_msgs(n);
                if let Some(last) = msgs.last_mut() {
                    last.streaming = streaming_last;
                }
                for w in [40usize, 80] {
                    let content_width = w.saturating_sub(2).max(1);
                    for max_rows in [3usize, 10] {
                        for at_end in [true, false] {
                            for scroll in [0usize, 7] {
                                for hide_reasoning in [false, true] {
                                    for busy in [false, true] {
                                        let got = visible_chat_rows(
                                            &msgs,
                                            w,
                                            content_width,
                                            max_rows,
                                            at_end,
                                            scroll,
                                            hide_reasoning,
                                            busy,
                                        );
                                        let want = full_visible_rows(
                                            &msgs,
                                            w,
                                            content_width,
                                            max_rows,
                                            at_end,
                                            scroll,
                                            hide_reasoning,
                                            busy,
                                        );
                                        assert_eq!(
                                            got, want,
                                            "n={n} streaming_last={streaming_last} w={w} \
                                             max_rows={max_rows} at_end={at_end} scroll={scroll} \
                                             hide_reasoning={hide_reasoning} busy={busy}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
