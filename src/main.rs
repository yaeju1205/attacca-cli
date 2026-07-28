#![allow(dead_code)]

mod api;
mod tools;

use api::Api;
use tools::{exec_tool, parse_tools, short, PROTOCOL};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::io;
use std::time::Duration;

// ── color palette ──

const BG: Color = Color::Rgb(13, 13, 20);
const SIDE: Color = Color::Rgb(18, 18, 28);
const TOP: Color = Color::Rgb(28, 28, 40);
const GREEN: Color = Color::Rgb(80, 200, 120);
const BLUE: Color = Color::Rgb(100, 180, 255);
const GRAY: Color = Color::Rgb(120, 120, 140);
const YELLOW: Color = Color::Rgb(220, 190, 80);
const SIDEW: u16 = 26;

// ── message model ──

struct Msg {
    role: String,       // "user" | "agent" | "tool" | "result" | "sys"
    text: String,
    tool_json: Option<String>,
    done: bool,
}

// ── app state ──

struct App {
    api: Api,
    sid: Option<String>,
    cursor: i64,
    msgs: Vec<Msg>,
    input: String,
    scroll: usize,
    busy: bool,

    // sidebar
    sessions: Vec<(String, String)>, // (title, id)
    sel: usize,
    focus_side: bool,

    // flow control
    first: bool,
}

impl App {
    fn new(api: Api) -> Self {
        Self {
            api,
            sid: None,
            cursor: 0,
            msgs: Vec::new(),
            input: String::new(),
            scroll: 0,
            busy: false,
            sessions: Vec::new(),
            sel: 0,
            focus_side: false,
            first: true,
        }
    }

    fn chat(&mut self, role: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.msgs.push(Msg {
            role: role.into(),
            text: text.into(),
            tool_json: None,
            done: false,
        });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    fn tool_box(&mut self, json: &str) {
        let v: Value = serde_json::from_str(json).unwrap_or_default();
        let t = v["tool"].as_str().unwrap_or("?");
        let args = v
            .get("args")
            .and_then(|a| a.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, vv)| Some(format!("{k}={}", vv.as_str()?)))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        self.msgs.push(Msg {
            role: "tool".into(),
            text: format!("◆ {t} {args}"),
            tool_json: Some(json.into()),
            done: false,
        });
    }

    // ── render ──

    fn draw(&self, f: &mut Frame) {
        let a = f.area();
        if a.width < 50 || a.height < 10 {
            f.render_widget(Paragraph::new("resize terminal").centered().red(), a);
            return;
        }
        f.render_widget(Paragraph::new("").style(Style::new().bg(BG)), a);

        let c = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEW), Constraint::Min(30)])
            .split(a);
        self.sidebar(f, c[0]);
        self.chat_panel(f, c[1]);
    }

    fn sidebar(&self, f: &mut Frame, area: Rect) {
        f.render_widget(Paragraph::new("").style(Style::new().bg(SIDE)), area);
        f.render_widget(
            Paragraph::new("  sessions").style(Style::new().fg(Color::White).add_modifier(Modifier::BOLD).bg(SIDE)),
            Rect::new(area.x, area.y, area.width, 1),
        );

        let mut items: Vec<ListItem> = Vec::new();
        for s in &self.sessions {
            let active = self.sid.as_ref().map(|id| id == &s.1).unwrap_or(false);
            let title = if s.0.len() > 18 {
                format!("{}…", &s.0[..17])
            } else {
                s.0.clone()
            };
            let style = if active {
                Style::new().fg(BLUE).add_modifier(Modifier::BOLD).bg(SIDE)
            } else {
                Style::new().fg(Color::White).bg(SIDE)
            };
            let dot = if active { "●" } else { " " };
            items.push(ListItem::new(Line::from(vec![Span::styled(
                format!(" {dot} {title}"),
                style,
            )])));
        }
        let ns = if self.sel == self.sessions.len() && self.focus_side {
            "▸"
        } else {
            " "
        };
        items.push(ListItem::new(Line::from(vec![Span::styled(
            format!(" {ns}+ new"),
            Style::new().fg(GREEN).bg(SIDE),
        )])));

        let list_area = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(6));
        f.render_widget(List::new(items).style(Style::new().bg(SIDE)), list_area);

        // hint at bottom
        f.render_widget(
            Paragraph::new(if self.focus_side { "  ► chat" } else { "  ► side" })
                .style(Style::new().fg(GRAY).bg(SIDE)),
            Rect::new(area.x, area.height.saturating_sub(3), area.width, 1),
        );

        // key status
        let key_info = if self.api.key.is_empty() {
            "  no key"
        } else {
            "  key ✓"
        };
        f.render_widget(
            Paragraph::new(key_info).style(Style::new().fg(GRAY).bg(SIDE)),
            Rect::new(area.x, area.height.saturating_sub(2), area.width, 1),
        );
    }

    fn chat_panel(&self, f: &mut Frame, area: Rect) {
        let c = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)])
            .split(area);

        // status bar
        let sid_short = self.sid.as_ref().map(|s| short(s)).unwrap_or_default();
        let busy = if self.busy { " ···" } else { "" };
        f.render_widget(
            Paragraph::new(format!(" {}{}", sid_short, busy)).style(Style::new().fg(Color::White).bg(TOP)),
            c[0],
        );

        // messages
        let mut lines: Vec<Line> = Vec::new();
        for m in &self.msgs {
            match m.role.as_str() {
                "user" => {
                    lines.push(Line::from(vec![Span::styled(" you", Style::new().fg(GREEN).bold())]));
                    for l in m.text.lines() {
                        lines.push(Line::from(Span::raw(format!(" │ {l}"))));
                    }
                }
                "agent" => {
                    for (i, l) in m.text.lines().enumerate() {
                        if i == 0 {
                            lines.push(Line::from(vec![Span::styled(format!(" ─ {l}"), Style::new().fg(BLUE))]));
                        } else {
                            lines.push(Line::from(Span::raw(format!(" {l}"))));
                        }
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
                _ => {
                    for l in m.text.lines() {
                        lines.push(Line::from(Span::raw(format!(" {l}"))));
                    }
                }
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                " enter:send · tab:side · y/n:tool · /test",
                Style::new().fg(GRAY),
            )]));
        }
        let off = self.scroll.saturating_sub(12).min(self.msgs.len().saturating_sub(5));
        f.render_widget(
            Paragraph::new(Text::from(lines)).scroll((off as u16, 0)).style(Style::new().bg(BG)),
            c[1],
        );

        // input box
        let inp = if self.input.is_empty() {
            vec![Span::styled(" type here", Style::new().fg(GRAY))]
        } else {
            vec![Span::raw(format!(" {}", self.input))]
        };
        f.render_widget(
            Paragraph::new(Text::from(Line::from(inp)))
                .block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(TOP)))
                .style(Style::new().bg(TOP)),
            c[2],
        );
    }

    // ── event loop ──

    async fn on_key(&mut self, code: KeyCode) -> bool {
        if self.focus_side {
            match code {
                KeyCode::Tab | KeyCode::Esc => self.focus_side = false,
                KeyCode::Up => self.sel = self.sel.saturating_sub(1),
                KeyCode::Down => self.sel = self.sel.saturating_add(1).min(self.sessions.len()),
                KeyCode::Enter => {
                    if self.sel < self.sessions.len() {
                        let id = self.sessions[self.sel].1.clone();
                        self.open_session(&id).await;
                        self.focus_side = false;
                    } else {
                        self.new_session().await;
                    }
                }
                _ => {}
            }
            return true;
        }

        // main focus
        match code {
            KeyCode::Tab => {
                self.focus_side = true;
                if self.sessions.is_empty() {
                    self.load_sessions().await;
                }
            }
            KeyCode::Enter => {
                let m = self.input.trim().to_string();
                if !m.is_empty() {
                    self.input.clear();
                    self.send_msg(m).await;
                }
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => self.approve_tool(true).await,
            KeyCode::Char('n') | KeyCode::Char('N') => self.approve_tool(false).await,
            KeyCode::Char('q') => return false,
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_add(1).min(self.msgs.len().saturating_sub(1)),
            _ => {}
        }
        true
    }

    async fn on_mouse(&mut self, col: u16, row: u16) {
        // Click on sidebar session
        if col < SIDEW && row >= 3 && row < self.sessions.len() as u16 + 3 {
            let idx = (row - 3) as usize;
            if idx < self.sessions.len() {
                let id = self.sessions[idx].1.clone();
                self.open_session(&id).await;
            }
        }
        // Click on + new
        if col < SIDEW && row as usize == self.sessions.len() + 3 {
            self.new_session().await;
        }
    }

    // ── API operations ──

    async fn load_sessions(&mut self) {
        if self.api.key.is_empty() {
            self.chat("sys", "no API key — set ATTACCA_API_KEY");
            return;
        }
        match self.api.get("/v1/sessions").await {
            Ok(body) => {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&body) {
                    self.sessions = arr
                        .iter()
                        .map(|s| {
                            let t = s["title"].as_str().unwrap_or("");
                            let id = s["id"].as_str().unwrap_or("");
                            let title = if t.is_empty() || t == "attacca-cli" {
                                "untitled".into()
                            } else {
                                t.into()
                            };
                            (title, id.into())
                        })
                        .collect();
                    self.chat("sys", &format!("{} sessions", self.sessions.len()));
                    self.sel = 0;
                }
            }
            Err((code, body)) => {
                let preview = body.chars().take(100).collect::<String>();
                self.chat("sys", &format!("sessions: HTTP {code}: {preview}"));
            }
        }
    }

    async fn open_session(&mut self, sid: &str) {
        self.sid = Some(sid.into());
        self.cursor = 0;
        self.first = false;
        self.msgs.clear();
        self.scroll = 0;

        // load history
        if let Ok(body) = self.api.get(&format!("/v1/sessions/{sid}/messages?after=0")).await {
            if let Ok(msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                for m in msgs.iter().rev() {
                    if let Some(c) = m["cursor"].as_i64() {
                        if c > self.cursor {
                            self.cursor = c;
                        }
                    }
                    let role = m["role"].as_str().unwrap_or("");
                    let text = m["text"].as_str().unwrap_or("");
                    if role == "assistant" || role == "user" {
                        let (clean, _) = parse_tools(text);
                        if !clean.is_empty() {
                            self.chat(role, &clean);
                        }
                    }
                }
            }
        }
        self.chat("sys", &format!("opened {}", short(sid)));
    }

    async fn new_session(&mut self) {
        match self.api.post("/v1/sessions", &serde_json::json!({"title":"attacca-cli"})).await {
            Ok(body) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    if let Some(id) = v["id"].as_str() {
                        self.sid = Some(id.into());
                        self.cursor = 0;
                        self.first = true;
                        self.msgs.clear();
                        self.scroll = 0;
                        self.chat("sys", "new session");
                        self.load_sessions().await;
                        return;
                    }
                }
                self.chat("sys", &format!("create: {body}"));
            }
            Err((code, body)) => {
                self.chat("sys", &format!("create: HTTP {code}: {}", body.chars().take(100).collect::<String>()));
            }
        }
        self.focus_side = false;
    }

    async fn send_msg(&mut self, raw: String) {
        // slash commands
        if raw.starts_with('/') {
            match raw.as_str() {
                "/q" | "/quit" | "/exit" => std::process::exit(0),
                "/h" | "/help" => {
                    self.chat("sys", "enter:send tab:side y/n:tool q:quit /test:probe api");
                    return;
                }
                "/test" => {
                    self.chat("sys", "probing API endpoints...");
                    let results = self.api.diagnose().await;
                    for r in &results {
                        let icon = if r.ok { "✓" } else { " " };
                        let note = if r.ok { "  ← OK" } else { "" };
                        self.chat("sys", &format!("{icon} {url} → HTTP {s}{note}", url = r.url, s = r.status));
                    }
                    return;
                }
                "/sessions" => {
                    self.load_sessions().await;
                    self.focus_side = true;
                    return;
                }
                _ => {}
            }
        }

        if self.api.key.is_empty() {
            self.chat("sys", "set ATTACCA_API_KEY first");
            return;
        }

        self.chat("user", &raw);
        self.busy = true;

        // ensure session exists
        if self.sid.is_none() {
            match self.api.post("/v1/sessions", &serde_json::json!({"title":"attacca-cli"})).await {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&body) {
                        if let Some(id) = v["id"].as_str() {
                            self.sid = Some(id.into());
                            self.cursor = 0;
                            self.first = true;
                        } else {
                            self.chat("sys", &format!("session: {body}"));
                            self.busy = false;
                            return;
                        }
                    } else {
                        self.chat("sys", "session: parse error");
                        self.busy = false;
                        return;
                    }
                }
                Err((code, body)) => {
                    self.chat("sys", &format!("session: HTTP {code}: {}", body.chars().take(100).collect::<String>()));
                    self.busy = false;
                    return;
                }
            }
        }

        let sid = self.sid.as_ref().unwrap().clone();

        let payload = if self.first {
            self.first = false;
            format!("{PROTOCOL}\n\n---\n{raw}")
        } else {
            raw
        };

        // send
        if let Err((code, body)) = self
            .api
            .post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message": payload, "timezone": "Asia/Seoul"}))
            .await
        {
            self.chat("sys", &format!("send: HTTP {code}: {}", body.chars().take(100).collect::<String>()));
            self.busy = false;
            return;
        }

        // wait for agent
        loop {
            match self.api.get(&format!("/v1/sessions/{sid}")).await {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&body) {
                        if !v["running"].as_bool().unwrap_or(true) {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // read response
        match self.api.get(&format!("/v1/sessions/{sid}/messages?after={}", self.cursor)).await {
            Ok(body) => {
                if let Ok(msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                    for m in &msgs {
                        if let Some(c) = m["cursor"].as_i64() {
                            if c > self.cursor {
                                self.cursor = c;
                            }
                        }
                        if m["role"].as_str() == Some("assistant") {
                            let text = m["text"].as_str().unwrap_or("");
                            let (clean, tools) = parse_tools(text);
                            if !clean.is_empty() {
                                self.chat("agent", &clean);
                            }
                            for j in tools {
                                self.tool_box(&j);
                            }
                        }
                    }
                }
            }
            Err((code, body)) => {
                self.chat("sys", &format!("read: HTTP {code}: {}", body.chars().take(100).collect::<String>()));
            }
        }
        self.busy = false;
    }

    async fn approve_tool(&mut self, yes: bool) {
        let idx = self.msgs.iter().rposition(|m| m.tool_json.is_some() && !m.done);
        let Some(i) = idx else { return };

        let json = self.msgs[i].tool_json.take().unwrap_or_default();
        self.msgs[i].done = true;

        let result = if yes { exec_tool(&json) } else { "skipped".into() };
        self.chat("result", &result);

        // send result back to agent
        if let Some(sid) = self.sid.clone() {
            self.busy = true;
            let _ = self
                .api
                .post(
                    &format!("/v1/sessions/{sid}/messages"),
                    &serde_json::json!({"message": format!("[tool result]\n{result}"), "timezone": "Asia/Seoul"}),
                )
                .await;

            // wait for agent to respond
            loop {
                match self.api.get(&format!("/v1/sessions/{sid}")).await {
                    Ok(body) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&body) {
                            if !v["running"].as_bool().unwrap_or(true) {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            // read next response
            if let Ok(body) = self.api.get(&format!("/v1/sessions/{sid}/messages?after={}", self.cursor)).await {
                if let Ok(msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                    for m in &msgs {
                        if let Some(c) = m["cursor"].as_i64() {
                            if c > self.cursor {
                                self.cursor = c;
                            }
                        }
                        if m["role"].as_str() == Some("assistant") {
                            let text = m["text"].as_str().unwrap_or("");
                            let (clean, tools) = parse_tools(text);
                            if !clean.is_empty() {
                                self.chat("agent", &clean);
                            }
                            for j in tools {
                                self.tool_box(&j);
                            }
                        }
                    }
                }
            }
            self.busy = false;
        }
    }
}

// ── main ──

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let api = Api::from_env();

    // startup test
    eprintln!("{}", api.whoami().await);
    eprintln!("  base: {}", api.base);
    eprintln!("  key:  {}", if api.key.is_empty() { "not set" } else { "set" });

    // init terminal
    terminal::enable_raw_mode().ok();
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture).ok();
    let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("terminal init failed");
            return;
        }
    };
    term.clear().ok();

    let mut app = App::new(api);
    app.chat("sys", "enter: send · tab: side panel · y/n: approve tool · /test: probe API");

    // main loop
    loop {
        if term.draw(|f| app.draw(f)).is_err() {
            break;
        }

        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(k)) => {
                    if k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat {
                        if !app.on_key(k.code).await {
                            break;
                        }
                    }
                }
                Ok(Event::Mouse(m)) => {
                    if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                        app.on_mouse(m.column, m.row).await;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    }

    // cleanup
    terminal::disable_raw_mode().ok();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture).ok();
}
