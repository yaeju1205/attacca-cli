#![allow(dead_code)]
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use reqwest::Client;
use serde_json::Value;
use std::io;
use std::time::Duration;

// ═══════════════════════════════════════════════════════════════
// Protocol
// ═══════════════════════════════════════════════════════════════

const PROTOCOL: &str = r#"You are connected to the user's computer via attacca-cli bridge.

To access the user's local machine, output JSON tool calls inside ```attacca-tool blocks:

```attacca-tool
{"tool": "read_file", "args": {"path": "/home/user/project/src/main.rs"} }
```

Available tools:
- read_file(path) — read any text file
- write_file(path, content) — write/overwrite a file
- edit_file(path, old_string, new_string) — find/replace
- list_dir(path) — list directory
- run_command(command) — run any shell command (grep, find, sed, git, cargo, cat, mkdir, ls, diff, etc.)
- create_dir(path) — create directory
- file_exists(path) — check existence
- delete_file(path) — delete file or dir
- read_files(paths) — read multiple files at once

Rules: Read before writing. Use run_command for searching. Never invent content."#;

// ═══════════════════════════════════════════════════════════════
// API Client — synchronous-style for simplicity
// ═══════════════════════════════════════════════════════════════

struct Api {
    inner: Client,
    key: String,
    base: String,
}

impl Api {
    fn from_env() -> Result<Self, String> {
        let key = std::env::var("ATTACCA_API_KEY").map_err(|_| "ATTACCA_API_KEY not set".to_string())?;
        let base = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| "https://attacca.cc".to_string());
        let inner = Client::builder().user_agent("attacca-cli").build().map_err(|e| format!("{e}"))?;
        Ok(Self { inner, key, base })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }

    async fn get(&self, path: &str) -> Result<String, String> {
        let r = self.inner.get(&self.url(path)).headers(self.headers()).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status();
        let body = r.text().await.unwrap_or_default();
        if s.is_success() { Ok(body) } else { Err(format!("{s} {body}", body = &body[..body.len().min(160)])) }
    }

    async fn post(&self, path: &str, json: &Value) -> Result<String, String> {
        let r = self.inner.post(&self.url(path)).headers(self.headers()).json(json).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status();
        let body = r.text().await.unwrap_or_default();
        if s.is_success() || s.as_u16() == 202 { Ok(body) } else { Err(format!("{s} {body}", body = &body[..body.len().min(160)])) }
    }

    async fn patch(&self, path: &str, json: &Value) -> Result<String, String> {
        let r = self.inner.patch(&self.url(path)).headers(self.headers()).json(json).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status();
        if s.is_success() { r.text().await.map_err(|e| format!("{e}")) } else { Err(format!("{s}")) }
    }

    async fn me(&self) -> Result<String, String> { self.get("/v1/me").await }
    async fn sessions(&self) -> Result<String, String> { self.get("/v1/sessions").await }
    async fn projects(&self) -> Result<String, String> { self.get("/v1/projects").await }
    async fn create_session(&self, pid: Option<&str>) -> Result<String, String> {
        let mut body = serde_json::json!({"title":"attacca-cli"});
        if let Some(p) = pid { body["project_id"] = serde_json::json!(p); }
        self.post("/v1/sessions", &body).await
    }
    async fn rename(&self, sid: &str, title: &str) -> Result<String, String> {
        self.patch(&format!("/v1/sessions/{sid}"), &serde_json::json!({"title": title})).await
    }
    async fn send(&self, sid: &str, msg: &str) -> Result<String, String> {
        self.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message": msg, "timezone": "Asia/Seoul"})).await
    }
    async fn session_get(&self, sid: &str) -> Result<String, String> {
        self.get(&format!("/v1/sessions/{sid}")).await
    }
    async fn msgs(&self, sid: &str, after: i64) -> Result<String, String> {
        self.get(&format!("/v1/sessions/{sid}/messages?after={after}")).await
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool parsing & execution
// ═══════════════════════════════════════════════════════════════

fn parse_tools(text: &str) -> (String, Vec<String>) {
    let mut tools = Vec::new();
    let mut clean = text.to_string();
    loop {
        let s = match clean.find("```attacca-tool") { Some(i) => i, None => break };
        let cs = s + "```attacca-tool".len();
        let e = match clean[cs..].find("```") { Some(i) => cs + i, None => break };
        tools.push(clean[cs..e].trim().to_string());
        clean.replace_range(s..e + 3, "");
    }
    (clean.trim().to_string(), tools)
}

fn exec_tool(json: &str) -> String {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    let t = v["tool"].as_str().unwrap_or("?");
    let a = |k: &str| v["args"][k].as_str().unwrap_or("");
    match t {
        "read_file" => match std::fs::read_to_string(a("path")) {
            Ok(s) if s.len() > 50000 => format!("[{}b]\n{}", s.len(), &s[..50000]),
            Ok(s) => format!("[content {}b]\n{s}", s.len()),
            Err(e) => format!("[error: {e}]"),
        },
        "write_file" => match std::fs::write(a("path"), a("content")) {
            Ok(()) => "ok".into(),
            Err(e) => format!("[error: {e}]"),
        },
        "edit_file" => match std::fs::read_to_string(a("path")) {
            Ok(c) if c.contains(a("old_string")) => {
                let n = c.replace(a("old_string"), a("new_string"));
                let cnt = c.matches(a("old_string")).count();
                match std::fs::write(a("path"), &n) { Ok(()) => format!("replaced {cnt}"), Err(e) => format!("[error: {e}]") }
            }
            Ok(_) => "not found".into(),
            Err(e) => format!("[error: {e}]"),
        },
        "list_dir" => match std::fs::read_dir(a("path")) {
            Ok(e) => { let mut v: Vec<String> = e.flatten().map(|e| format!("{}{}", if e.file_type().map(|t| t.is_dir()).unwrap_or(false) { "📁 " } else { "  " }, e.file_name().to_string_lossy())).collect(); v.sort(); format!("[{}]\n{}", v.len(), v.join("\n")) }
            Err(e) => format!("[error: {e}]"),
        },
        "run_command" => match std::process::Command::new("sh").arg("-c").arg(a("command")).output() {
            Ok(o) => {
                let mut r = String::new();
                let so = String::from_utf8_lossy(&o.stdout);
                let se = String::from_utf8_lossy(&o.stderr);
                if !so.is_empty() { r.push_str(&format!("{so}\n")); }
                if !se.is_empty() { r.push_str(&format!("[err]\n{se}\n")); }
                r.push_str(&format!("[exit:{}]", o.status.code().unwrap_or(-1)));
                r
            }
            Err(e) => format!("[error: {e}]"),
        },
        "file_exists" => (std::path::Path::new(a("path")).exists()).to_string(),
        "create_dir" => match std::fs::create_dir_all(a("path")) { Ok(()) => "ok".into(), Err(e) => format!("[error: {e}]") },
        "delete_file" => match std::fs::remove_file(a("path")).or_else(|_| std::fs::remove_dir(a("path"))) { Ok(()) => "ok".into(), Err(e) => format!("[error: {e}]") },
        "read_files" => {
            let ps = a("paths");
            let pv: Vec<&str> = if ps.starts_with('[') { serde_json::from_str(ps).unwrap_or_default() } else { ps.split(',').collect() };
            pv.iter().map(|p| format!("--- {p} ---\n{}", std::fs::read_to_string(p).unwrap_or_default())).collect::<Vec<_>>().join("\n")
        }
        _ => format!("[unknown: {t}]"),
    }
}

fn is_dangerous(json: &str) -> bool {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    if v["tool"].as_str() == Some("run_command") {
        let c = v["args"]["command"].as_str().unwrap_or("");
        c.contains("rm ") || c.contains("sudo ") || c.contains("dd ") || c.contains("mkfs") || c.contains('>')
    } else { false }
}

fn short(s: &str) -> String { if s.len() > 8 { s[..8].to_string() } else { s.to_string() } }
fn timer(s: &str) -> String { if s.len() > 19 { s[..16].to_string().replace("T", " ") } else { s.to_string() } }

// ═══════════════════════════════════════════════════════════════
// Message model
// ═══════════════════════════════════════════════════════════════

struct Msg {
    role: String,     // "user" | "agent" | "tool" | "result"
    text: String,
    raw_json: Option<String>,
    approved: bool,
}

// ═══════════════════════════════════════════════════════════════
// TUI App
// ═══════════════════════════════════════════════════════════════

#[derive(PartialEq)]
enum Mode { Chat, Sessions, Projects }

struct Tui {
    api: Api,
    sid: Option<String>,
    cursor: i64,
    pid: Option<String>,
    first: bool,
    msgs: Vec<Msg>,
    buf: String,
    mode: Mode,
    scroll: usize,
    busy: bool,
    items: Vec<(String, String)>, // (title, id) for pickers
    sel: usize,
}

impl Tui {
    fn new(api: Api) -> Self {
        Self {
            api,
            sid: None, cursor: 0, pid: None, first: true,
            msgs: Vec::new(), buf: String::new(),
            mode: Mode::Chat, scroll: 0, busy: false,
            items: Vec::new(), sel: 0,
        }
    }

    fn log(&mut self, role: &str, text: &str) {
        if text.trim().is_empty() { return; }
        self.msgs.push(Msg { role: role.into(), text: text.into(), raw_json: None, approved: false });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    fn add_tool(&mut self, json: &str) {
        let v: Value = serde_json::from_str(json).unwrap_or_default();
        let t = v["tool"].as_str().unwrap_or("?");
        let args = v.get("args").and_then(|a| a.as_object()).map(|o| o.iter().filter_map(|(k, vv)| Some(format!("{}={}", k, vv.as_str()?))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        self.msgs.push(Msg { role: "tool".into(), text: format!("◇ {t} {args}"), raw_json: Some(json.into()), approved: false });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    // ── rendering ──

    fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.width < 40 || area.height < 10 { f.render_widget(Paragraph::new("too small").centered().red(), area); return; }

        match self.mode {
            Mode::Chat => self.draw_chat(f, area),
            Mode::Sessions => self.draw_picker(f, area, "Sessions", "💬"),
            Mode::Projects => self.draw_picker(f, area, "Projects", "📁"),
        }
    }

    fn draw_chat(&self, f: &mut Frame, area: Rect) {
        let c = Layout::default().direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)])
            .split(area);

        // status
        let status = match &self.sid {
            Some(sid) => format!(" attacca  {short}  {n}msgs  {b}", short = short(sid), n = self.msgs.len(), b = if self.busy { "…" } else { "✓" }),
            None => " attacca  no session  Tab: sessions".into(),
        };
        f.render_widget(Paragraph::new(status).style(Style::new().fg(Color::White).bg(Color::Rgb(30, 30, 30))), c[0]);

        // messages
        let mut lines: Vec<Line> = Vec::new();
        for m in &self.msgs {
            match m.role.as_str() {
                "user" => {
                    lines.push(Line::from(vec![Span::styled("  you", Style::new().fg(Color::Rgb(80, 200, 120)).bold())]));
                    for l in m.text.lines() { lines.push(Line::from(Span::raw(format!("  │ {l}")))); }
                }
                "agent" => {
                    lines.push(Line::from(vec![Span::styled("  ──", Style::new().fg(Color::Rgb(100, 180, 255)).bold())]));
                    for l in m.text.lines() { lines.push(Line::from(Span::raw(format!("  {l}")))); }
                }
                "tool" if m.approved => {}
                "tool" => {
                    let danger = m.raw_json.as_ref().map(|j| is_dangerous(j)).unwrap_or(false);
                    lines.push(Line::from(vec![Span::styled(&m.text, if danger { Style::new().fg(Color::Red).bold() } else { Style::new().fg(Color::Yellow).bold() })]));
                    lines.push(Line::from(vec![
                        Span::styled("  [", Style::new().fg(Color::DarkGray)),
                        Span::styled("y", Style::new().fg(Color::Green).bold()),
                        Span::styled("] run  [", Style::new().fg(Color::DarkGray)),
                        Span::styled("n", Style::new().fg(Color::Red).bold()),
                        Span::styled("] skip", Style::new().fg(Color::DarkGray)),
                    ]));
                }
                "result" => {
                    lines.push(Line::from(vec![Span::styled(format!("  └ {t}", t = m.text.lines().next().unwrap_or("")), Style::new().fg(Color::Rgb(120, 120, 120)))]));
                }
                _ => { for l in m.text.lines() { lines.push(Line::from(Span::raw(format!("  {l}")))); } }
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled("  Enter to send  Tab: sessions  y/n: run tools  q: quit", Style::new().fg(Color::DarkGray))));
        }

        let off = self.scroll.saturating_sub(12).min(self.msgs.len().saturating_sub(5));
        f.render_widget(
            Paragraph::new(Text::from(lines)).scroll((off as u16, 0)),
            c[1],
        );

        // input
        let inp = if self.buf.is_empty() {
            vec![Span::styled(" type here…", Style::new().fg(Color::DarkGray))]
        } else { vec![Span::raw(&self.buf)] };
        f.render_widget(
            Paragraph::new(Text::from(Line::from(inp)))
                .block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(Color::Rgb(50, 50, 50))))
                .style(Style::new().bg(Color::Rgb(20, 20, 20))),
            c[2],
        );
    }

    fn draw_picker(&self, f: &mut Frame, area: Rect, title: &str, icon: &str) {
        let a = Rect::new(4, 3, area.width.saturating_sub(8), area.height.saturating_sub(6));
        f.render_widget(Clear, a);
        let mut items: Vec<ListItem> = Vec::new();
        for (i, (name, id)) in self.items.iter().enumerate() {
            let m = if i == self.sel { "▸" } else { " " };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("{m} {icon} "), Style::new().fg(Color::Cyan)),
                Span::raw(name),
                Span::styled(format!("  {}", short(id)), Style::new().fg(Color::DarkGray)),
            ])));
        }
        if items.is_empty() {
            items.push(ListItem::new(Line::from(vec![Span::styled("  (empty)", Style::new().fg(Color::DarkGray))])));
        }
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("{} ✨ new", if self.items.len() == self.sel { "▸" } else { " " }), Style::new().fg(Color::Green)),
        ])));

        f.render_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(format!(" {title} ")).title_style(Style::new().fg(Color::Cyan).bold()))
                .highlight_style(Style::new().bg(Color::Rgb(40, 40, 60))),
            a,
        );
    }

    // ── event handling ──

    async fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                match self.mode {
                    Mode::Sessions | Mode::Projects => match k.code {
                        KeyCode::Esc => { self.mode = Mode::Chat; self.sel = 0; }
                        KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => self.sel = self.sel.saturating_add(1),
                        KeyCode::Enter => {
                            if self.sel < self.items.len() {
                                let id = self.items[self.sel].1.clone();
                                if self.mode == Mode::Sessions { self.open(&id).await; }
                                else { self.pid = Some(id); self.load_sessions().await; self.mode = Mode::Sessions; }
                            } else {
                                self.create().await;
                            }
                        }
                        KeyCode::Char('n') => { self.create().await; }
                        _ => {}
                    },
                    Mode::Chat => match k.code {
                        KeyCode::Enter => {
                            let m = self.buf.trim().to_string();
                            if !m.is_empty() { self.buf.clear(); self.send(m).await; }
                        }
                        KeyCode::Char('y') | KeyCode::Char('Y') => self.approve(true).await,
                        KeyCode::Char('n') | KeyCode::Char('N') => self.approve(false).await,
                        KeyCode::Char('q') => return false,
                        KeyCode::Char(c) => self.buf.push(c),
                        KeyCode::Backspace => { self.buf.pop(); }
                        KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                        KeyCode::Down => self.scroll = self.scroll.saturating_add(1).min(self.msgs.len().saturating_sub(1)),
                        KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                        KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10).min(self.msgs.len().saturating_sub(1)),
                        KeyCode::Tab => self.load_sessions().await,
                        KeyCode::Esc => {}
                        _ => {}
                    },
                }
            }
            _ => {}
        }
        true
    }

    // ── API helpers ──

    async fn load_sessions(&mut self) {
        let r = self.api.sessions().await.unwrap_or_else(|e| e);
        if let Ok(v) = serde_json::from_str::<Vec<Value>>(&r) {
            self.items = v.iter().map(|s| {
                let title = s["title"].as_str().unwrap_or("(?)");
                let id = s["id"].as_str().unwrap_or("");
                let running = s["running"].as_bool().unwrap_or(false);
                let icon = if running { "▶" } else { "💬" };
                let t = if title.is_empty() || title == "attacca-cli" { "(untitled)" } else { title };
                (format!("{icon} {t}"), id.to_string())
            }).collect();
        } else {
            self.items = vec![("(failed to load)".into(), String::new())];
        }
        self.sel = 0;
        self.mode = Mode::Sessions;
    }

    async fn open(&mut self, sid: &str) {
        self.sid = Some(sid.to_string());
        self.cursor = 0;
        self.first = false;
        self.msgs.clear();
        self.scroll = 0;
        // load old messages
        if let Ok(r) = self.api.msgs(sid, 0).await {
            if let Ok(v) = serde_json::from_str::<Vec<Value>>(&r) {
                for m in v.iter().rev() {
                    let role = m["role"].as_str().unwrap_or("");
                    let text = m["text"].as_str().unwrap_or("");
                    if let Some(c) = m["cursor"].as_i64() { if c > self.cursor { self.cursor = c; } }
                    if role == "assistant" || role == "user" {
                        let (t, _) = parse_tools(text);
                        if !t.is_empty() { self.log(if role == "user" { "user" } else { "agent" }, &t); }
                    }
                }
            }
        }
        self.log("system", &format!("resumed session {s}", s = short(sid)));
        self.mode = Mode::Chat;
    }

    async fn create(&mut self) {
        let r = self.api.create_session(self.pid.as_deref()).await.unwrap_or_else(|e| e);
        if let Ok(v) = serde_json::from_str::<Value>(&r) {
            if let Some(id) = v["id"].as_str() {
                self.sid = Some(id.to_string());
                self.cursor = 0;
                self.first = true;
                self.msgs.clear();
                self.scroll = 0;
                self.log("system", "new session");
                self.mode = Mode::Chat;
                return;
            }
        }
        self.log("system", &format!("create failed: {r}"));
        self.mode = Mode::Chat;
    }

    async fn send(&mut self, raw: String) {
        // slash cmds
        if raw == "/q" || raw == "/quit" || raw == "/exit" { std::process::exit(0); }
        if raw == "/h" || raw == "/help" { self.log("system", "Enter=send ↑↓=scroll Tab=sessions y/n=tools q=quit"); return; }
        if raw == "/sessions" || raw == "/tab" { self.load_sessions().await; return; }
        if raw == "/new" { self.create().await; return; }

        self.log("user", &raw);
        self.busy = true;

        // ensure session
        if self.sid.is_none() {
            let r = self.api.create_session(self.pid.as_deref()).await.unwrap_or_else(|e| e);
            if let Ok(v) = serde_json::from_str::<Value>(&r) {
                if let Some(id) = v["id"].as_str() { self.sid = Some(id.to_string()); self.cursor = 0; self.first = true; }
                else { self.log("system", &format!("session error: {r}")); self.busy = false; return; }
            } else { self.log("system", &format!("session error: {r}")); self.busy = false; return; }
        }
        let sid = self.sid.as_ref().unwrap().clone();

        let msg = if self.first {
            self.first = false;
            let n = raw.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
            let n = if n.len() > 40 { format!("{}...", &n[..37]) } else { n };
            let _ = self.api.rename(&sid, &n).await;
            format!("{}\n\n---\n{}", PROTOCOL, raw)
        } else { raw };

        if let Err(e) = self.api.send(&sid, &msg).await {
            self.log("system", &format!("send error: {e}"));
            self.busy = false;
            return;
        }

        // wait for agent
        loop {
            let r = self.api.session_get(&sid).await.unwrap_or_else(|e| e);
            if let Ok(v) = serde_json::from_str::<Value>(&r) {
                if !v["running"].as_bool().unwrap_or(true) { break; }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // read reply
        let r = self.api.msgs(&sid, self.cursor).await.unwrap_or_else(|e| e);
        if let Ok(v) = serde_json::from_str::<Vec<Value>>(&r) {
            for m in &v {
                if let Some(c) = m["cursor"].as_i64() { if c > self.cursor { self.cursor = c; } }
                if m["role"].as_str() == Some("assistant") {
                    let text = m["text"].as_str().unwrap_or("");
                    let (t, tools) = parse_tools(text);
                    if !t.is_empty() { self.log("agent", &t); }
                    for j in tools { self.add_tool(&j); }
                }
            }
        }
        self.busy = false;
    }

    async fn approve(&mut self, yes: bool) {
        let idx = self.msgs.iter().rposition(|m| m.raw_json.is_some() && !m.approved);
        let Some(i) = idx else { return };

        let json = self.msgs[i].raw_json.take().unwrap_or_default();
        self.msgs[i].approved = true;

        let result = if yes { exec_tool(&json) } else { "skipped".into() };
        self.log("result", &result);

        // send result back
        if let Some(sid) = self.sid.clone() {
            self.busy = true;
            let _ = self.api.send(&sid, &format!("[tool result]\n{result}")).await;
            loop {
                let r = self.api.session_get(&sid).await.unwrap_or_else(|e| e);
                if let Ok(v) = serde_json::from_str::<Value>(&r) {
                    if !v["running"].as_bool().unwrap_or(true) { break; }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let r = self.api.msgs(&sid, self.cursor).await.unwrap_or_else(|e| e);
            if let Ok(v) = serde_json::from_str::<Vec<Value>>(&r) {
                for m in &v {
                    if let Some(c) = m["cursor"].as_i64() { if c > self.cursor { self.cursor = c; } }
                    if m["role"].as_str() == Some("assistant") {
                        let text = m["text"].as_str().unwrap_or("");
                        let (t, tools) = parse_tools(text);
                        if !t.is_empty() { self.log("agent", &t); }
                        for j in tools { self.add_tool(&j); }
                    }
                }
            }
            self.busy = false;
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════

#[derive(Parser)]
#[command(name = "attacca", version, about = "attacca-cli")]
struct Cli {
    #[arg(short = 'P', long, env = "ATTACCA_PROJECT")] project: Option<String>,
    #[arg(short = 'S', long, env = "ATTACCA_SESSION")] session: Option<String>,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _cli = Cli::parse();

    let api = match Api::from_env() {
        Ok(a) => a,
        Err(e) => { eprintln!("✖ {e}"); std::process::exit(1); }
    };

    // test connection
    match api.me().await {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                let name = v["display_name"].as_str().unwrap_or("?");
                eprintln!("✓ connected as {name}");
            } else {
                eprintln!("✓ connected (response: {body})", body = &body[..body.len().min(100)]);
            }
        }
        Err(e) => {
            eprintln!("✖ API error: {e}");
            eprintln!("  Check ATTACCA_API_KEY and ATTACCA_API_URL");
            eprintln!("  URL: {}", api.base);
            std::process::exit(1);
        }
    }

    // terminal setup
    terminal::enable_raw_mode().unwrap();
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture).ok();
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)).unwrap();
    term.clear().unwrap();

    let mut tui = Tui::new(api);
    tui.log("system", "Enter: send  Tab: sessions  y/n: tools  q: quit");

    loop {
        term.draw(|f| tui.render(f)).ok();
        if event::poll(Duration::from_millis(100)).ok().unwrap_or(false) {
            if let Ok(ev) = event::read() {
                if !tui.handle(ev).await { break; }
            }
        }
    }

    terminal::disable_raw_mode().unwrap();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture).ok();
    println!("bye!");
}
