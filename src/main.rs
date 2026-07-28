#![allow(dead_code)]
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use reqwest::Client;
use serde_json::Value;
use std::io;
use std::time::Duration;

const BG: Color = Color::Rgb(13, 13, 20);
const SIDE: Color = Color::Rgb(20, 20, 30);
const SURFACE: Color = Color::Rgb(28, 28, 40);
const GREEN: Color = Color::Rgb(80, 200, 120);
const BLUE: Color = Color::Rgb(100, 180, 255);
const GRAY: Color = Color::Rgb(120, 120, 140);
const YELLOW: Color = Color::Rgb(220, 190, 80);
const SIDEW: u16 = 24;

const PROTOCOL: &str = r#"You are connected to the user's computer via attacca-cli bridge.

To access the user's local machine, output JSON tool calls inside ```attacca-tool blocks:

```attacca-tool
{"tool": "read_file", "args": {"path": "/home/user/project/src/main.rs"} }
```

Available tools: read_file, write_file, edit_file, list_dir, run_command, create_dir, file_exists, delete_file, read_files

Use run_command for grep, find, sed, git, cargo, cat, mkdir, ls, diff. Never invent content."#;

// ── API ──

struct Api { inner: Client, key: String, base: String }
impl Api {
    fn from_env() -> Result<Self, String> {
        let key = std::env::var("ATTACCA_API_KEY").map_err(|_| "ATTACCA_API_KEY not set".to_string())?;
        let base = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| "https://attacca.cc".to_string());
        let inner = Client::builder().user_agent("attacca-cli").build().map_err(|e| format!("{e}"))?;
        Ok(Self { inner, key, base })
    }
    fn url(&self, p: &str) -> String { format!("{}/{}", self.base.trim_end_matches('/'), p.trim_start_matches('/')) }
    fn hdrs(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }
    async fn get(&self, p: &str) -> Result<String, String> {
        let r = self.inner.get(&self.url(p)).headers(self.hdrs()).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status(); let b = r.text().await.unwrap_or_default();
        if s.is_success() { Ok(b) } else { Err(format!("{s} {b}", b = &b[..b.len().min(120)])) }
    }
    async fn post(&self, p: &str, j: &Value) -> Result<String, String> {
        let r = self.inner.post(&self.url(p)).headers(self.hdrs()).json(j).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status(); let b = r.text().await.unwrap_or_default();
        if s.is_success() || s.as_u16() == 202 { Ok(b) } else { Err(format!("{s} {b}", b = &b[..b.len().min(120)])) }
    }
    async fn patch(&self, p: &str, j: &Value) -> Result<String, String> {
        let r = self.inner.patch(&self.url(p)).headers(self.hdrs()).json(j).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status();
        if s.is_success() { r.text().await.map_err(|e| format!("{e}")) } else { Err(format!("{s}")) }
    }
    async fn me(&self) -> Result<String, String> { self.get("/v1/me").await }
    async fn sessions(&self) -> Result<String, String> { self.get("/v1/sessions").await }
    async fn create_ses(&self, pid: Option<&str>) -> Result<String, String> {
        let mut b = serde_json::json!({"title":"attacca-cli"});
        if let Some(p) = pid { b["project_id"] = serde_json::json!(p); }
        self.post("/v1/sessions", &b).await
    }
    async fn rename(&self, sid: &str, title: &str) -> Result<String, String> {
        self.patch(&format!("/v1/sessions/{sid}"), &serde_json::json!({"title": title})).await
    }
    async fn send(&self, sid: &str, msg: &str) -> Result<String, String> {
        self.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message": msg, "timezone": "Asia/Seoul"})).await
    }
    async fn sget(&self, sid: &str) -> Result<String, String> { self.get(&format!("/v1/sessions/{sid}")).await }
    async fn msgs(&self, sid: &str, after: i64) -> Result<String, String> {
        self.get(&format!("/v1/sessions/{sid}/messages?after={after}")).await
    }
}

// ── Tools ──

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

fn exec_tool(j: &str) -> String {
    let v: Value = serde_json::from_str(j).unwrap_or_default();
    let t = v["tool"].as_str().unwrap_or("?");
    let a = |k: &str| v["args"][k].as_str().unwrap_or("");
    match t {
        "read_file" => match std::fs::read_to_string(a("path")) {
            Ok(s) if s.len() > 50000 => format!("[{}b]\n{}", s.len(), &s[..50000]),
            Ok(s) => format!("[{}b]\n{s}", s.len()),
            Err(e) => format!("[error: {e}]"),
        },
        "write_file" => match std::fs::write(a("path"), a("content")) { Ok(()) => "ok".into(), Err(e) => format!("[error: {e}]") },
        "edit_file" => match std::fs::read_to_string(a("path")) {
            Ok(c) if c.contains(a("old_string")) => {
                let n = c.replace(a("old_string"), a("new_string"));
                let cnt = c.matches(a("old_string")).count();
                match std::fs::write(a("path"), &n) { Ok(()) => format!("replaced {cnt}"), Err(e) => format!("[error: {e}]") }
            }
            Ok(_) => "not found".into(), Err(e) => format!("[error: {e}]"),
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
        "file_exists" => std::path::Path::new(a("path")).exists().to_string(),
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

fn danger(j: &str) -> bool {
    let v: Value = serde_json::from_str(j).unwrap_or_default();
    if v["tool"].as_str() == Some("run_command") {
        let c = v["args"]["command"].as_str().unwrap_or("");
        c.contains("rm ") || c.contains("sudo ") || c.contains("dd ") || c.contains("mkfs") || c.contains('>')
    } else { false }
}

fn short(s: &str) -> String { if s.len() > 8 { s[..8].to_string() } else { s.to_string() } }

// ── Messages ──

struct Msg { role: String, text: String, j: Option<String>, done: bool }

// ── App ──

struct SideSession { title: String, id: String, active: bool }

struct App {
    api: Api,
    sid: Option<String>,
    cur: i64,
    first: bool,
    msgs: Vec<Msg>,
    buf: String,
    scroll: usize,
    busy: bool,
    sides: Vec<SideSession>,
    sels: usize,
    focus: Focus,
    err: Option<String>,
}

enum Focus { Main, Side }

impl App {
    fn new(api: Api) -> Self {
        Self {
            api, sid: None, cur: 0, first: true,
            msgs: Vec::new(), buf: String::new(),
            scroll: 0, busy: false,
            sides: Vec::new(), sels: 0, focus: Focus::Main,
            err: None,
        }
    }

    fn add(&mut self, r: &str, t: &str) {
        if t.trim().is_empty() { return; }
        self.msgs.push(Msg { role: r.into(), text: t.into(), j: None, done: false });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    fn add_tool(&mut self, json: &str) {
        let v: Value = serde_json::from_str(json).unwrap_or_default();
        let t = v["tool"].as_str().unwrap_or("?");
        let args = v.get("args").and_then(|a| a.as_object()).map(|o| o.iter().filter_map(|(k, vv)| Some(format!("{}={}", k, vv.as_str()?))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        self.msgs.push(Msg { role: "tool".into(), text: format!("◇ {t} {args}"), j: Some(json.into()), done: false });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    // ── render ──

    fn render(&mut self, f: &mut Frame) {
        let a = f.area();
        if a.width < 50 || a.height < 10 {
            f.render_widget(Paragraph::new("too small").centered().red(), a);
            return;
        }

        // background
        f.render_widget(Paragraph::new("").style(Style::new().bg(BG)), a);

        // main split: sidebar | chat
        let main = Layout::default().direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEW), Constraint::Min(30)])
            .split(a);

        self.draw_sidebar(f, main[0]);
        self.draw_chat(f, main[1]);
    }

    fn draw_sidebar(&self, f: &mut Frame, area: Rect) {
        // sidebar bg
        f.render_widget(Paragraph::new("").style(Style::new().bg(SIDE)), area);

        // title
        let title_style = Style::new().fg(Color::White).bg(SIDE).add_modifier(Modifier::BOLD);
        f.render_widget(Paragraph::new("  Sessions").style(title_style), Rect::new(area.x, area.y, area.width, 1));

        // separator
        f.render_widget(Paragraph::new("").style(Style::new().bg(SURFACE)), Rect::new(area.x, area.y + 1, area.width, 1));

        // list area
        let list_area = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(8));

        let mut items: Vec<ListItem> = Vec::new();
        for s in self.sides.iter() {
            let is_current = self.sid.as_ref().map(|id| id == &s.id).unwrap_or(false);
            let prefix = if is_current { "● " } else { "  " };
            let mut span_style = Style::new().fg(Color::White).bg(SIDE);
            if is_current { span_style = span_style.fg(BLUE).add_modifier(Modifier::BOLD); }
            let title = if s.title.len() > 18 { format!("{}…", &s.title[..17]) } else { s.title.clone() };
            items.push(ListItem::new(Line::from(vec![Span::styled(format!("{prefix}{title}"), span_style)])));
        }

        if items.is_empty() {
            items.push(ListItem::new(Line::from(vec![Span::styled("  (none)", Style::new().fg(GRAY).bg(SIDE))])));
        }

        // new session button
        let is_new = self.sels == self.sides.len();
        let new_prefix = if is_new && matches!(self.focus, Focus::Side) { "▸ " } else { "  " };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("{new_prefix}+ new"), Style::new().fg(GREEN).bg(SIDE)),
        ])));

        f.render_widget(
            List::new(items)
                .style(Style::new().bg(SIDE))
                .highlight_style(Style::new().bg(SURFACE)),
            list_area,
        );

        // bottom hint
        let hint = match self.focus {
            Focus::Side => "  tab: main",
            Focus::Main => "  tab: side",
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(hint, Style::new().fg(GRAY).bg(SIDE))])),
            Rect::new(area.x, area.height.saturating_sub(3), area.width, 1),
        );
    }

    fn draw_chat(&self, f: &mut Frame, area: Rect) {
        let c = Layout::default().direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)])
            .split(area);

        // status
        let sid_display = self.sid.as_ref().map(|s| short(s)).unwrap_or_else(|| "—".into());
        let st = if self.busy {
            format!("  {sid_display}  ···")
        } else {
            format!("  {sid_display}  {} msgs", self.msgs.len())
        };
        f.render_widget(Paragraph::new(st).style(Style::new().fg(Color::White).bg(SURFACE)), c[0]);

        // messages
        let mut lines: Vec<Line> = Vec::new();
        for m in &self.msgs {
            match m.role.as_str() {
                "user" => {
                    lines.push(Line::from(vec![Span::styled("  you", Style::new().fg(GREEN).bold())]));
                    for l in m.text.lines() { lines.push(Line::from(Span::raw(format!("  │ {l}")))); }
                }
                "agent" => {
                    for (i, l) in m.text.lines().enumerate() {
                        if i == 0 { lines.push(Line::from(vec![Span::styled(format!("  ─ {l}"), Style::new().fg(BLUE))])); }
                        else { lines.push(Line::from(Span::raw(format!("  {l}")))); }
                    }
                }
                "tool" if m.done => {}
                "tool" => {
                    let d = m.j.as_ref().map(|j| danger(j)).unwrap_or(false);
                    lines.push(Line::from(vec![Span::styled(&m.text, if d { Style::new().fg(Color::Red).bold() } else { Style::new().fg(YELLOW).bold() })]));
                    lines.push(Line::from(vec![
                        Span::styled("  [y] run  [n] skip", Style::new().fg(GRAY)),
                    ]));
                }
                "result" => {
                    let first = m.text.lines().next().unwrap_or("");
                    if first.len() > 80 { lines.push(Line::from(vec![Span::styled(format!("  └ {}…", &first[..77]), Style::new().fg(GRAY))])); }
                    else { lines.push(Line::from(vec![Span::styled(format!("  └ {first}"), Style::new().fg(GRAY))])); }
                }
                _ => { for l in m.text.lines() { lines.push(Line::from(Span::raw(format!("  {l}")))); } }
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled("  enter to send · tab: sidebar · y/n: tools · q: quit", Style::new().fg(GRAY))));
        }

        let off = self.scroll.saturating_sub(12).min(self.msgs.len().saturating_sub(5));
        f.render_widget(
            Paragraph::new(Text::from(lines)).scroll((off as u16, 0)).style(Style::new().bg(BG)),
            c[1],
        );

        // input
        let inp = if self.buf.is_empty() {
            vec![Span::styled("  type here…", Style::new().fg(GRAY))]
        } else { vec![Span::raw(format!("  {}", self.buf))] };
        f.render_widget(
            Paragraph::new(Text::from(Line::from(inp)))
                .block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(SURFACE)))
                .style(Style::new().bg(SURFACE)),
            c[2],
        );
    }

    // ── events ──

    async fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                match self.focus {
                    Focus::Side => match k.code {
                        KeyCode::Tab | KeyCode::Esc => { self.focus = Focus::Main; }
                        KeyCode::Up | KeyCode::Char('k') => { self.sels = self.sels.saturating_sub(1); }
                        KeyCode::Down | KeyCode::Char('j') => { self.sels = self.sels.saturating_add(1).min(self.sides.len()); }
                        KeyCode::Enter => {
                            if self.sels < self.sides.len() {
                                let id = self.sides[self.sels].id.clone();
                                self.open(&id).await;
                                self.focus = Focus::Main;
                            } else {
                                self.create().await;
                            }
                        }
                        KeyCode::Char('n') => { self.create().await; }
                        _ => {}
                    },
                    Focus::Main => match k.code {
                        KeyCode::Tab => {
                            self.focus = Focus::Side;
                            if self.sides.is_empty() { self.load_sessions().await; }
                        }
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
                        _ => {}
                    },
                }
            }
            _ => {}
        }
        true
    }

    async fn load_sessions(&mut self) {
        match self.api.sessions().await {
            Ok(r) => {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&r) {
                    let current = self.sid.clone();
                    self.sides = arr.iter().map(|s| {
                        let title = s["title"].as_str().unwrap_or("");
                        let id = s["id"].as_str().unwrap_or("");
                        let t = if title.is_empty() || title == "attacca-cli" { "(untitled)" } else { title };
                        SideSession { title: t.to_string(), id: id.to_string(), active: current.as_ref().map(|c| c == id).unwrap_or(false) }
                    }).collect();
                    self.sels = 0;
                }
            }
            Err(e) => { self.err = Some(e); }
        }
    }

    async fn open(&mut self, sid: &str) {
        self.sid = Some(sid.to_string());
        self.cur = 0;
        self.first = false;
        self.msgs.clear();
        self.scroll = 0;

        if let Ok(r) = self.api.msgs(sid, 0).await {
            if let Ok(v) = serde_json::from_str::<Vec<Value>>(&r) {
                for m in v.iter().rev() {
                    let role = m["role"].as_str().unwrap_or("");
                    let text = m["text"].as_str().unwrap_or("");
                    if let Some(c) = m["cursor"].as_i64() { if c > self.cur { self.cur = c; } }
                    if role == "assistant" || role == "user" {
                        let (t, _) = parse_tools(text);
                        if !t.is_empty() { self.add(role, &t); }
                    }
                }
            }
        }
        // update sidebar active state
        for s in &mut self.sides { s.active = s.id == sid; }
        self.add("system", &format!("resumed {s}", s = short(sid)));
    }

    async fn create(&mut self) {
        match self.api.create_ses(None).await {
            Ok(r) => {
                if let Ok(v) = serde_json::from_str::<Value>(&r) {
                    if let Some(id) = v["id"].as_str() {
                        self.sid = Some(id.to_string());
                        self.cur = 0;
                        self.first = true;
                        self.msgs.clear();
                        self.scroll = 0;
                        self.load_sessions().await;
                        self.add("system", "new session");
                        return;
                    }
                }
                self.add("system", &format!("create: {r}"));
            }
            Err(e) => { self.add("system", &format!("create: {e}")); }
        }
        self.focus = Focus::Main;
    }

    async fn send(&mut self, raw: String) {
        if raw == "/q" || raw == "/quit" || raw == "/exit" { std::process::exit(0); }
        if raw == "/h" || raw == "/help" { self.add("system", "enter:send · tab:sidebar · y/n:tools · q:quit"); return; }
        if raw == "/new" { self.create().await; return; }

        self.add("user", &raw);
        self.busy = true;

        if self.sid.is_none() {
            match self.api.create_ses(None).await {
                Ok(r) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&r) {
                        if let Some(id) = v["id"].as_str() { self.sid = Some(id.to_string()); self.cur = 0; self.first = true; self.load_sessions().await; }
                        else { self.add("system", &format!("session: {r}")); self.busy = false; return; }
                    } else { self.add("system", &format!("session parse err")); self.busy = false; return; }
                }
                Err(e) => { self.add("system", &format!("session: {e}")); self.busy = false; return; }
            }
        }

        let sid = self.sid.as_ref().unwrap().clone();
        let msg = if self.first {
            self.first = false;
            let n = raw.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
            let n = if n.len() > 40 { format!("{}...", &n[..37]) } else { n };
            let _ = self.api.rename(&sid, &n).await;
            self.load_sessions().await;
            format!("{}\n\n---\n{}", PROTOCOL, raw)
        } else { raw };

        match self.api.send(&sid, &msg).await {
            Ok(_) => {}
            Err(e) => { self.add("system", &format!("send: {e}")); self.busy = false; return; }
        }

        loop {
            match self.api.sget(&sid).await {
                Ok(r) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&r) {
                        if !v["running"].as_bool().unwrap_or(true) { break; }
                    }
                }
                Err(_) => { break; }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        match self.api.msgs(&sid, self.cur).await {
            Ok(r) => {
                if let Ok(v) = serde_json::from_str::<Vec<Value>>(&r) {
                    for m in &v {
                        if let Some(c) = m["cursor"].as_i64() { if c > self.cur { self.cur = c; } }
                        if m["role"].as_str() == Some("assistant") {
                            let text = m["text"].as_str().unwrap_or("");
                            let (t, tools) = parse_tools(text);
                            if !t.is_empty() { self.add("agent", &t); }
                            for j in tools { self.add_tool(&j); }
                        }
                    }
                }
            }
            Err(e) => { self.add("system", &format!("read: {e}")); }
        }
        self.busy = false;
    }

    async fn approve(&mut self, yes: bool) {
        let idx = self.msgs.iter().rposition(|m| m.j.is_some() && !m.done);
        let Some(i) = idx else { return };

        let json = self.msgs[i].j.take().unwrap_or_default();
        self.msgs[i].done = true;

        let result = if yes { exec_tool(&json) } else { "skipped".into() };
        self.add("result", &result);

        if let Some(sid) = self.sid.clone() {
            self.busy = true;
            let _ = self.api.send(&sid, &format!("[tool result]\n{result}")).await;
            loop {
                match self.api.sget(&sid).await {
                    Ok(r) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&r) {
                            if !v["running"].as_bool().unwrap_or(true) { break; }
                        }
                    }
                    Err(_) => { break; }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            match self.api.msgs(&sid, self.cur).await {
                Ok(r) => {
                    if let Ok(v) = serde_json::from_str::<Vec<Value>>(&r) {
                        for m in &v {
                            if let Some(c) = m["cursor"].as_i64() { if c > self.cur { self.cur = c; } }
                            if m["role"].as_str() == Some("assistant") {
                                let text = m["text"].as_str().unwrap_or("");
                                let (t, tools) = parse_tools(text);
                                if !t.is_empty() { self.add("agent", &t); }
                                for j in tools { self.add_tool(&j); }
                            }
                        }
                    }
                }
                Err(_) => {}
            }
            self.busy = false;
        }
    }
}

// ── Main ──

#[derive(Parser)]
#[command(name = "attacca", version, about = "attacca-cli")]
struct Cli { #[arg(short = 'S', long, env = "ATTACCA_SESSION")] session: Option<String> }

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _cli = Cli::parse();

    let api = match Api::from_env() {
        Ok(a) => a,
        Err(e) => { eprintln!("✖ {e}"); std::process::exit(1); }
    };

    match api.me().await {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                eprintln!("✓ {}", v["display_name"].as_str().unwrap_or("connected"));
            }
        }
        Err(e) => {
            eprintln!("✖ API: {e}");
            eprintln!("  See ATTACCA_API_KEY / ATTACCA_API_URL");
            std::process::exit(1);
        }
    }

    terminal::enable_raw_mode().unwrap();
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).ok();
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)).unwrap();
    term.clear().unwrap();

    let mut app = App::new(api);
    app.load_sessions().await;
    app.add("system", "enter: send · tab: sidebar · y/n: tools · q: quit");

    loop {
        term.draw(|f| app.render(f)).ok();
        if event::poll(Duration::from_millis(100)).ok().unwrap_or(false) {
            if let Ok(ev) = event::read() {
                if !app.handle(ev).await { break; }
            }
        }
    }

    terminal::disable_raw_mode().unwrap();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen).ok();
}
