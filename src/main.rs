mod api;
mod tools;

use api::Api;
use tools::{exec_tool, parse_tools, short, PROTOCOL};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::io;
use std::time::Duration;

const BG: Color = Color::Rgb(13, 13, 20);
const SIDE: Color = Color::Rgb(18, 18, 28);
const TOP: Color = Color::Rgb(28, 28, 40);
const GREEN: Color = Color::Rgb(80, 200, 120);
const BLUE: Color = Color::Rgb(100, 180, 255);
const GRAY: Color = Color::Rgb(120, 120, 140);
const YELLOW: Color = Color::Rgb(220, 190, 80);
const SIDEW: u16 = 26;

struct Msg { role: String, text: String, raw: Option<String>, done: bool }

struct App {
    api: Api,
    sid: Option<String>,
    cur: i64,
    msgs: Vec<Msg>,
    input: String,
    scroll: usize,
    busy: bool,
    sessions: Vec<(String, String)>,
    sel: usize,
    side: bool,
    first: bool,
}

impl App {
    fn new(api: Api) -> Self {
        Self {
            api, sid: None, cur: 0, msgs: vec![], input: String::new(),
            scroll: 0, busy: false, sessions: vec![], sel: 0, side: false, first: true,
        }
    }

    fn add(&mut self, r: &str, t: &str) {
        if t.trim().is_empty() { return; }
        self.msgs.push(Msg { role: r.into(), text: t.into(), raw: None, done: false });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    fn add_tool(&mut self, j: &str) {
        let v: Value = serde_json::from_str(j).unwrap_or_default();
        let t = v["tool"].as_str().unwrap_or("?");
        let args = v.get("args").and_then(|a| a.as_object())
            .map(|o| o.iter().filter_map(|(k, vv)| Some(format!("{k}={}", vv.as_str()?))).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        self.msgs.push(Msg { role: "tool".into(), text: format!("◆ {t} {args}"), raw: Some(j.into()), done: false });
    }

    // ── RENDER ──

    fn draw(&self, f: &mut Frame) {
        let a = f.area();
        if a.width < 50 || a.height < 10 { return; }
        f.render_widget(Paragraph::new("").style(Style::new().bg(BG)), a);
        let c = Layout::default().direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEW), Constraint::Min(30)]).split(a);
        self.sidebar(f, c[0]);
        self.chat(f, c[1]);
    }

    fn sidebar(&self, f: &mut Frame, area: Rect) {
        f.render_widget(Paragraph::new("").style(Style::new().bg(SIDE)), area);
        f.render_widget(
            Paragraph::new("  sessions").style(Style::new().fg(Color::White).add_modifier(Modifier::BOLD).bg(SIDE)),
            Rect::new(area.x, area.y, area.width, 1));

        let mut items: Vec<ListItem> = Vec::new();
        let n = self.sessions.len();
        for (i, s) in self.sessions.iter().enumerate() {
            let active = self.sid.as_ref().map(|id| id == &s.1).unwrap_or(false);
            let title = if s.0.len() > 18 { format!("{}…", &s.0[..17]) } else { s.0.clone() };
            let dot = if active { "●" } else { " " };
            let highlight = self.side && i == self.sel;
            let style = if active { Style::new().fg(BLUE).add_modifier(Modifier::BOLD) } else if highlight { Style::new().fg(Color::White).add_modifier(Modifier::BOLD) } else { Style::new().fg(GRAY) };
            items.push(ListItem::new(Line::from(vec![Span::styled(format!(" {dot} {title}"), style.bg(SIDE))])));
        }
        let at_new = self.side && self.sel >= n;
        items.push(ListItem::new(Line::from(vec![
            Span::styled(if at_new { " ▸ + new" } else { "   + new" }, Style::new().fg(GREEN).bg(SIDE)),
        ])));

        let la = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(6));
        f.render_widget(List::new(items).style(Style::new().bg(SIDE)), la);

        let hint = if self.side { "  ◄ chat" } else { "  side ►" };
        f.render_widget(Paragraph::new(Line::from(vec![Span::styled(hint, Style::new().fg(GRAY).bg(SIDE))])),
            Rect::new(area.x, area.height.saturating_sub(3), area.width, 1));

        let key_info = if self.api.key.is_empty() { "  no key" } else { "  key ✓" };
        f.render_widget(Paragraph::new(Line::from(vec![Span::styled(key_info, Style::new().fg(GRAY).bg(SIDE))])),
            Rect::new(area.x, area.height.saturating_sub(2), area.width, 1));
    }

    fn chat(&self, f: &mut Frame, area: Rect) {
        let c = Layout::default().direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)]).split(area);

        let sid_s = self.sid.as_ref().map(|s| short(s)).unwrap_or_default();
        f.render_widget(Paragraph::new(format!(" {}{}", sid_s, if self.busy {" ···"} else {""}))
            .style(Style::new().fg(Color::White).bg(TOP)), c[0]);

        let mut lines: Vec<Line> = Vec::new();
        for m in &self.msgs {
            match m.role.as_str() {
                "user" => {
                    lines.push(Line::from(vec![Span::styled(" you", Style::new().fg(GREEN).bold())]));
                    for l in m.text.lines() { lines.push(Line::from(Span::raw(format!(" │ {l}")))); }
                }
                "agent" => {
                    for (i, l) in m.text.lines().enumerate() {
                        if i == 0 { lines.push(Line::from(vec![Span::styled(format!(" ─ {l}"), Style::new().fg(BLUE))])); }
                        else { lines.push(Line::from(Span::raw(format!(" {l}")))); }
                    }
                }
                "tool" if m.done => {}
                "tool" => {
                    lines.push(Line::from(vec![Span::styled(&m.text, Style::new().fg(YELLOW).bold())]));
                    lines.push(Line::from(vec![Span::styled("  [y] run  [n] skip", Style::new().fg(GRAY))]));
                }
                "result" => {
                    let first = m.text.lines().next().unwrap_or("");
                    lines.push(Line::from(vec![Span::styled(format!(" └ {first}"), Style::new().fg(GRAY))]));
                }
                _ => { for l in m.text.lines() { lines.push(Line::from(Span::raw(format!(" {l}")))); } }
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(vec![Span::styled(" enter:send · tab:side · y/n:tool", Style::new().fg(GRAY))]));
        }
        let max = self.msgs.len();
        let off = if max > 5 { self.scroll.saturating_sub(12).min(max.saturating_sub(5)) } else { 0 };
        f.render_widget(Paragraph::new(Text::from(lines)).scroll((off as u16, 0)).style(Style::new().bg(BG)), c[1]);

        let inp = if self.input.is_empty() { vec![Span::styled(" type here", Style::new().fg(GRAY))] } else { vec![Span::raw(format!(" {}", self.input))] };
        f.render_widget(
            Paragraph::new(Text::from(Line::from(inp)))
                .block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(TOP)))
                .style(Style::new().bg(TOP)), c[2]);
    }

    // ── INPUT ──

    async fn key(&mut self, code: KeyCode) -> bool {
        if self.side {
            match code {
                KeyCode::Tab | KeyCode::Esc => { self.side = false; return true; }
                KeyCode::Up => { self.sel = self.sel.saturating_sub(1); return true; }
                KeyCode::Down => { self.sel = self.sel.saturating_add(1).min(self.sessions.len()); return true; }
                KeyCode::Enter => {
                    if self.sel < self.sessions.len() {
                        let id = self.sessions[self.sel].1.clone();
                        self.open(&id).await;
                        self.side = false;
                    } else {
                        self.create().await;
                    }
                    return true;
                }
                _ => {}
            }
        } else {
            match code {
                KeyCode::Tab => {
                    self.side = true;
                    self.sel = self.sel.min(self.sessions.len());
                }
                KeyCode::Enter => {
                    let m = self.input.trim().to_string();
                    if !m.is_empty() { self.input.clear(); self.send(m).await; }
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => self.approve(true).await,
                KeyCode::Char('n') | KeyCode::Char('N') => self.approve(false).await,
                KeyCode::Char('q') => return false,
                KeyCode::Char(c) => self.input.push(c),
                KeyCode::Backspace => { self.input.pop(); }
                KeyCode::Up => if self.scroll > 0 { self.scroll -= 1; }
                KeyCode::Down => if self.scroll + 1 < self.msgs.len() { self.scroll += 1; }
                _ => {}
            }
        }
        true
    }

    // ── API ──

    async fn load(&mut self) {
        if self.api.key.is_empty() { return; }
        match self.api.get("sessions").await {
            Ok(body) => {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&body) {
                    self.sessions = arr.iter().map(|s| {
                        let t = s["title"].as_str().unwrap_or("");
                        let id = s["id"].as_str().unwrap_or("");
                        let title = if t.is_empty() || t == "attacca-cli" { "untitled".into() } else { t.into() };
                        (title, id.into())
                    }).collect();
                    self.sel = 0;
                } else {
                    self.add("sys", &format!("sessions: unexpected: {}", body.chars().take(80).collect::<String>()));
                }
            }
            Err((c, b)) => {
                self.add("sys", &format!("sessions: HTTP {c}: {}", b.chars().take(80).collect::<String>()));
            }
        }
    }

    async fn open(&mut self, sid: &str) {
        self.sid = Some(sid.into());
        self.cur = 0;
        self.first = false;
        self.msgs.clear();
        self.scroll = 0;

        if let Ok(body) = self.api.get(&format!("/v1/sessions/{sid}/messages?after=0")).await {
            if let Ok(msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                for m in msgs.iter().rev() {
                    if let Some(c) = m["cursor"].as_i64() { if c > self.cur { self.cur = c; } }
                    let role = m["role"].as_str().unwrap_or("");
                    let text = m["text"].as_str().unwrap_or("");
                    if role == "assistant" || role == "user" {
                        let (clean, _) = parse_tools(text);
                        if !clean.is_empty() { self.add(role, &clean); }
                    }
                }
            }
        }
        self.add("sys", &format!("opened {}", short(sid)));
    }

    async fn create(&mut self) {
        match self.api.post("sessions", &serde_json::json!({"title":"attacca-cli"})).await {
            Ok(body) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    if let Some(id) = v["id"].as_str() {
                        self.sid = Some(id.into());
                        self.cur = 0;
                        self.first = true;
                        self.msgs.clear();
                        self.scroll = 0;
                        self.add("sys", "new session");
                        self.load().await;
                        return;
                    }
                }
                self.add("sys", &format!("create: {body}"));
            }
            Err((c, b)) => {
                self.add("sys", &format!("create: HTTP {c}: {}", b.chars().take(100).collect::<String>()));
            }
        }
        self.side = false;
    }

    async fn send(&mut self, raw: String) {
        if raw == "/q" || raw == "/quit" || raw == "/exit" { std::process::exit(0); }
        if raw == "/h" || raw == "/help" { self.add("sys", "enter:send  tab:side  y/n:tool  q:quit"); return; }
        if raw == "/sessions" { self.load().await; self.side = true; return; }

        if self.api.key.is_empty() { self.add("sys", "no API key"); return; }

        self.add("user", &raw);
        self.busy = true;

        // ensure session
        if self.sid.is_none() {
            match self.api.post("sessions", &serde_json::json!({"title":"attacca-cli"})).await {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&body) {
                        if let Some(id) = v["id"].as_str() { self.sid = Some(id.into()); self.cur = 0; self.first = true; }
                        else { self.add("sys", &format!("session: {body}")); self.busy = false; return; }
                    } else { self.add("sys", "session: parse error"); self.busy = false; return; }
                }
                Err((c, b)) => { self.add("sys", &format!("session: HTTP {c}: {}", b.chars().take(100).collect::<String>())); self.busy = false; return; }
            }
        }

        let sid = self.sid.as_ref().unwrap().clone();
        let payload = if self.first {
            self.first = false;
            format!("{PROTOCOL}\n\n---\n{raw}")
        } else { raw };

        if let Err((c, b)) = self.api.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message":payload, "timezone":"Asia/Seoul"})).await {
            self.add("sys", &format!("send: HTTP {c}: {}", b.chars().take(100).collect::<String>()));
            self.busy = false; return;
        }

        // wait
        loop {
            match self.api.get(&format!("/v1/sessions/{sid}")).await {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&body) {
                        if !v["running"].as_bool().unwrap_or(true) { break; }
                    }
                }
                Err(_) => break,
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // read
        match self.api.get(&format!("/v1/sessions/{sid}/messages?after={}", self.cur)).await {
            Ok(body) => {
                if let Ok(msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                    for m in &msgs {
                        if let Some(c) = m["cursor"].as_i64() { if c > self.cur { self.cur = c; } }
                        if m["role"].as_str() == Some("assistant") {
                            let text = m["text"].as_str().unwrap_or("");
                            let (clean, tools) = parse_tools(text);
                            if !clean.is_empty() { self.add("agent", &clean); }
                            for j in tools { self.add_tool(&j); }
                        }
                    }
                }
            }
            Err((c, b)) => { self.add("sys", &format!("read: HTTP {c}: {}", b.chars().take(100).collect::<String>())); }
        }
        self.busy = false;
    }

    async fn approve(&mut self, yes: bool) {
        let idx = self.msgs.iter().rposition(|m| m.raw.is_some() && !m.done);
        let Some(i) = idx else { return };
        let json = self.msgs[i].raw.take().unwrap_or_default();
        self.msgs[i].done = true;
        let result = if yes { exec_tool(&json) } else { "skipped".into() };
        self.add("result", &result);

        if let Some(sid) = self.sid.clone() {
            self.busy = true;
            let _ = self.api.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message": format!("[tool result]\n{result}"), "timezone":"Asia/Seoul"})).await;
            loop {
                match self.api.get(&format!("/v1/sessions/{sid}")).await {
                    Ok(body) => { if let Ok(v) = serde_json::from_str::<Value>(&body) { if !v["running"].as_bool().unwrap_or(true) { break; } } }
                    Err(_) => break,
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if let Ok(body) = self.api.get(&format!("/v1/sessions/{sid}/messages?after={}", self.cur)).await {
                if let Ok(msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                    for m in &msgs {
                        if let Some(c) = m["cursor"].as_i64() { if c > self.cur { self.cur = c; } }
                        if m["role"].as_str() == Some("assistant") {
                            let text = m["text"].as_str().unwrap_or("");
                            let (clean, tools) = parse_tools(text);
                            if !clean.is_empty() { self.add("agent", &clean); }
                            for j in tools { self.add_tool(&j); }
                        }
                    }
                }
            }
            self.busy = false;
        }
    }
}

// ── MAIN ──

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let api = Api::from_env();

    eprintln!("{}", api.whoami().await);
    eprintln!("  key: {}", if api.key.is_empty() { "not set" } else { "set" });

    terminal::enable_raw_mode().ok();
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).ok();
    let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)) {
        Ok(t) => t,
        Err(_) => { eprintln!("term init failed"); return; }
    };
    term.clear().ok();

    let mut app = App::new(api);
    app.add("sys", "enter:send · tab:side · y/n:tool · q:quit");

    loop {
        if term.draw(|f| app.draw(f)).is_err() { break; }

        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(k)) => {
                    if k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat {
                        if !app.key(k.code).await { break; }
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    }

    terminal::disable_raw_mode().ok();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen).ok();
}
