#![allow(dead_code)]
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::io;
use std::time::Duration;

const PROTOCOL: &str = r#"## attacca-cli — dual-computer bridge

You have access to the user's **local computer** through attacca-cli.
Output JSON tool calls inside ```attacca-tool blocks.

Example:
```attacca-tool
{"tool": "read_file", "args": {"path": "/home/user/hello.txt"} }
```

Tools: read_file, write_file, edit_file, list_dir, run_command,
       create_dir, file_exists, delete_file, read_files

Run ANY command with run_command: grep, find, cat, ls, mkdir, cp,
mv, sed, awk, git, cargo, npm. Do NOT invent file contents."#;

// ── DTOs ──

#[derive(Deserialize, Default, Clone, Debug)]
struct SessionDto { id:String, #[serde(default)] title:String, #[serde(default)] status:String, #[serde(default)] running:bool, #[serde(default)] created_at:String, #[serde(default)] updated_at:String }
#[derive(Deserialize, Clone, Debug)] struct MessageDto { #[serde(default)] role:String, #[serde(default)] text:String, #[serde(default)] cursor:i64 }
#[derive(Deserialize, Default)] struct MeDto { #[serde(default)] display_name:String, #[serde(default)] email:String }
#[derive(Deserialize, Clone, Debug)] struct ProjectDto { id:String, name:String, #[serde(default)] is_default:bool }

// ── API client ──

struct ApiClient { inner:Client, key:String, base_url:String }
impl ApiClient {
    fn from_env() -> Result<Self, String> {
        let key = std::env::var("ATTACCA_API_KEY").map_err(|_| "Set ATTACCA_API_KEY in .env".to_string())?;
        let base_url = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| "https://attacca.cc/api/v1".to_string());
        let inner = Client::builder().user_agent("attacca-cli/0.1.0").build().map_err(|e| format!("reqwest: {e}"))?;
        Ok(Self { inner, key, base_url })
    }
    fn url(&self, path:&str) -> String {
        let base = self.base_url.trim_end_matches('/');
        let p = if base.contains("/api/v1") && path.starts_with("/v1/") { &path[3..] } else { path };
        format!("{}/{}", base, p.trim_start_matches('/'))
    }
    fn bearer(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }
    async fn get_me(&self) -> Result<MeDto, String> { api_call(self.inner.get(self.url("/v1/me")).headers(self.bearer())).await }
    async fn list_sessions(&self) -> Vec<SessionDto> { api_call(self.inner.get(self.url("/v1/sessions")).headers(self.bearer())).await.unwrap_or_default() }
    async fn list_projects(&self) -> Vec<ProjectDto> { api_call(self.inner.get(self.url("/v1/projects")).headers(self.bearer())).await.unwrap_or_default() }
    async fn create_session(&self, pid:Option<&str>, aid:Option<&str>) -> Result<SessionDto, String> {
        let mut body = serde_json::json!({"title":"attacca-cli"});
        if let Some(p) = pid { body["project_id"]=serde_json::json!(p); }
        if let Some(a) = aid { body["agent_id"]=serde_json::json!(a); }
        api_call(self.inner.post(self.url("/v1/sessions")).headers(self.bearer()).json(&body)).await
    }
    async fn rename_session(&self, sid:&str, title:&str) -> Result<(), String> {
        let r = self.inner.patch(&self.url(&format!("/v1/sessions/{sid}"))).headers(self.bearer())
            .json(&serde_json::json!({"title":title})).send().await.map_err(|e| format!("{e}"))?;
        if r.status().is_success() { Ok(()) } else { Err(format!("HTTP {}", r.status())) }
    }
    async fn send_message(&self, sid:&str, msg:&str) -> Result<(), String> {
        let r = self.inner.post(self.url(&format!("/v1/sessions/{sid}/messages"))).headers(self.bearer())
            .json(&serde_json::json!({"message":msg,"timezone":"Asia/Seoul"})).send().await.map_err(|e| format!("{e}"))?;
        let s=r.status(); if s.is_success()||s.as_u16()==202 { Ok(()) } else { Err(format!("HTTP {s}")) }
    }
    async fn get_session(&self, sid:&str) -> Result<SessionDto, String> {
        api_call(self.inner.get(self.url(&format!("/v1/sessions/{sid}"))).headers(self.bearer())).await
    }
    async fn get_msgs_after(&self, sid:&str, after:i64) -> Vec<MessageDto> {
        let r = api_call::<Vec<MessageDto>>(self.inner.get(self.url(&format!("/v1/sessions/{sid}/messages?after={after}"))).headers(self.bearer())).await;
        r.map(|mut m| { m.reverse(); m }).unwrap_or_default()
    }
}

async fn api_call<T: serde::de::DeserializeOwned>(req: reqwest::RequestBuilder) -> Result<T, String> {
    let r = req.send().await.map_err(|e| format!("{e}"))?;
    let s = r.status();
    if s.is_success() { serde_json::from_str(&r.text().await.unwrap_or_default()).map_err(|e| format!("JSON: {e}")) }
    else { Err(format!("HTTP {s}")) }
}

// ── Tool execution ──

fn parse_tool_calls(text:&str) -> (String, Vec<String>) {
    let mut tools=Vec::new(); let mut clean=text.to_string();
    loop {
        let s=match clean.find("```attacca-tool") { Some(i)=>i, None=>break };
        let cs=s+"```attacca-tool".len();
        let e=match clean[cs..].find("```") { Some(i)=>cs+i, None=>break };
        tools.push(clean[s..e].to_string());
        let be=e+3; clean.replace_range(s..be, "");
    }
    (clean.trim().to_string(), tools)
}

fn execute_tool(j:&str) -> String {
    let v:Value=serde_json::from_str(j).unwrap_or_default();
    let tool=v["tool"].as_str().unwrap_or("?");
    let a=|k:&str| v["args"][k].as_str().unwrap_or("");
    match tool {
        "read_file" => match std::fs::read_to_string(a("path")) {
            Ok(s) if s.len()>50000 => format!("[{} bytes, first 50k]\n{}", s.len(), &s[..50000]),
            Ok(s) => format!("[{} bytes]\n{}", s.len(), s),
            Err(e) => format!("[error: {e}]"),
        },
        "write_file" => match std::fs::write(a("path"), a("content")) { Ok(()) => "[OK] wrote".into(), Err(e)=>format!("[error: {e}]") },
        "edit_file" => {
            let(p,o,n)=(a("path"),a("old_string"),a("new_string"));
            match std::fs::read_to_string(p) {
                Ok(c) if c.contains(o) => { let nc=c.replace(o,n); let cnt=c.matches(o).count();
                    match std::fs::write(p,&nc) { Ok(())=>format!("[OK] {cnt} replacements"), Err(e)=>format!("[error: {e}]") } }
                Ok(_) => "[error] not found".into(), Err(e)=>format!("[error: {e}]")
            }
        }
        "list_dir" => match std::fs::read_dir(a("path")) {
            Ok(entries) => { let mut v:Vec<String>=entries.flatten().map(|e| format!("{}{}", if e.file_type().map(|t|t.is_dir()).unwrap_or(false){"📁 "}else{"📄 "},e.file_name().to_string_lossy())).collect(); v.sort(); format!("[{}]\n{}",v.len(),v.join("\n")) }
            Err(e) => format!("[error: {e}]"),
        },
        "run_command" => match std::process::Command::new("sh").arg("-c").arg(a("command")).output() {
            Ok(o) => { let mut r=String::new(); let so=String::from_utf8_lossy(&o.stdout); let se=String::from_utf8_lossy(&o.stderr); if !so.is_empty() { r.push_str(&format!("[out]\n{so}\n")); } if !se.is_empty() { r.push_str(&format!("[err]\n{se}\n")); } r.push_str(&format!("[exit:{}]",o.status.code().unwrap_or(-1))); r }
            Err(e) => format!("[error: {e}]"),
        },
        "file_exists" => if std::path::Path::new(a("path")).exists() { "[true] exists".into() } else { "[false] not found".into() },
        "create_dir" => match std::fs::create_dir_all(a("path")) { Ok(())=> "[OK] created".into(), Err(e)=>format!("[error: {e}]") },
        "delete_file" => match std::fs::remove_file(a("path")).or_else(|_|std::fs::remove_dir(a("path"))) { Ok(())=> "[OK] deleted".into(), Err(e)=>format!("[error: {e}]") },
        "read_files" => { let paths_str=a("paths"); let ps:Vec<&str>=if paths_str.starts_with('['){serde_json::from_str(paths_str).unwrap_or_default()}else{paths_str.split(',').collect()}; ps.iter().map(|p| format!("--- {p} ---\n{}",std::fs::read_to_string(p).unwrap_or_default())).collect::<Vec<_>>().join("\n") }
        _ => format!("[unknown: {tool}]"),
    }
}

fn is_dangerous(j:&str) -> bool {
    let v:Value=serde_json::from_str(j).unwrap_or_default();
    if v["tool"].as_str()==Some("run_command") {
        let c=v["args"]["command"].as_str().unwrap_or(""); c.contains("rm ")||c.contains("sudo ")||c.contains("dd ")||c.contains("mkfs")||c.contains('>')
    } else { false }
}

fn fmt_sid(s:&str) -> String { if s.len()>8 { s[..8].to_string() } else { s.to_string() } }
fn fmt_time(s:&str) -> String { if s.len()>19 { s[..16].to_string().replace("T"," ") } else { s.to_string() } }

// ── TUI App ──

#[derive(Clone)]
struct ChatMsg { role:String, text:String, is_tool:bool, approved:bool, rejected:bool }

#[derive(PartialEq)]
enum Page { Chat, SessionPicker, ProjectPicker }

struct App {
    client: ApiClient,
    sid: Option<String>,
    cursor: i64,
    pid: Option<String>,
    aid: Option<String>,
    first: bool,
    msgs: Vec<ChatMsg>,
    input: String,
    page: Page,
    scroll: usize,
    busy: bool,
    user: String,

    // picker state
    sessions: Vec<SessionDto>,
    projects: Vec<ProjectDto>,
    pick_sel: usize,
    pick_loading: bool,
}

impl App {
    fn new(client:ApiClient) -> Self {
        Self {
            client,
            sid: None, cursor: 0, pid: None, aid: None, first: true,
            msgs: Vec::new(), input: String::new(),
            page: Page::Chat, scroll: 0, busy: false, user: String::new(),
            sessions: Vec::new(), projects: Vec::new(), pick_sel: 0, pick_loading: false,
        }
    }

    fn add(&mut self, r:&str, t:&str) {
        if t.trim().is_empty() { return; }
        self.msgs.push(ChatMsg { role:r.into(), text:t.into(), is_tool:false, approved:false, rejected:false });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    fn add_tool(&mut self, j:&str) {
        let v:Value=serde_json::from_str(j).unwrap_or_default();
        let tool=v["tool"].as_str().unwrap_or("?");
        let args=v.get("args").and_then(|a|a.as_object()).map(|o| o.iter().filter_map(|(k,vv)| Some(format!("{}={}",k,vv.as_str()?))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        let text = format!("🔧 {tool} {args}");
        self.msgs.push(ChatMsg { role:"tool".into(), text, is_tool:true, approved:false, rejected:false });
        self.scroll = self.msgs.len().saturating_sub(1);
    }

    fn render(&mut self, f:&mut Frame) {
        let size = f.area();
        if size.width<40||size.height<10 { f.render_widget(Paragraph::new("Terminal too small").centered().red(), size); return; }

        let chunks = Layout::default().direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(3)])
            .split(size);

        // status bar
        let status = match self.page {
            Page::Chat => {
                let s = self.sid.as_ref().map(|s| fmt_sid(s)).unwrap_or_else(|| "없음".into());
                let busy = if self.busy { "⏳" } else { "✓" };
                format!(" Attacca  │ {s}  │ {}개 메시지  {busy}", self.msgs.len())
            }
            Page::SessionPicker => " 세션 선택  (↑↓ 선택, Enter 열기, Esc 뒤로, n 새세션)".into(),
            Page::ProjectPicker => " 프로젝트 선택  (↑↓ 선택, Enter 열기, Esc 뒤로)".into(),
        };
        f.render_widget(Paragraph::new(status).style(Style::new().fg(Color::White).bg(Color::DarkGray)), chunks[0]);

        // main area
        match self.page {
            Page::Chat => self.render_chat(f, chunks[1], chunks[2]),
            Page::SessionPicker => self.render_session_picker(f, size),
            Page::ProjectPicker => self.render_project_picker(f, size),
        }
    }

    fn render_chat(&self, f:&mut Frame, chat_area:Rect, input_area:Rect) {
        // chat messages
        let mut lines:Vec<Line> = Vec::new();
        for m in &self.msgs {
            match m.role.as_str() {
                "user" => {
                    lines.push(Line::from(vec![Span::styled("  You", Style::new().fg(Color::Green).bold())]));
                    for l in m.text.lines() { lines.push(Line::from(Span::raw(format!("  │ {l}")))); }
                }
                "agent" => {
                    lines.push(Line::from(vec![Span::styled("  Agent", Style::new().fg(Color::Cyan).bold())]));
                    for l in m.text.lines() { lines.push(Line::from(Span::raw(format!("  │ {l}")))); }
                }
                "tool" if m.approved || m.rejected => {
                    if m.rejected { lines.push(Line::from(vec![Span::styled(format!("  ✖ {}. 거절됨", &m.text[..4]), Style::new().fg(Color::DarkGray))])); }
                }
                "tool" => {
                    let style = if m.is_tool && serde_json::from_str::<Value>(&m.text.replace("🔧 ","")).ok().map(|v|is_dangerous(&v.to_string())).unwrap_or(false) { Style::new().fg(Color::Red).bold() } else { Style::new().fg(Color::Yellow) };
                    lines.push(Line::from(vec![Span::styled(&m.text, style)]));
                    lines.push(Line::from(vec![Span::styled("  └ [Y] 승인  [N] 거절", Style::new().fg(Color::DarkGray))]));
                }
                _ => {}
            }
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled("  메시지를 입력하고 Enter를 누르세요", Style::new().fg(Color::DarkGray).italic())));
        }

        let off = self.scroll.saturating_sub(10).min(self.msgs.len().saturating_sub(10));
        f.render_widget(
            Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(Color::DarkGray))).scroll((off as u16,0)),
            chat_area,
        );

        // input bar
        let inp = if self.input.is_empty() {
            vec![Span::styled("메시지 입력...", Style::new().fg(Color::DarkGray).italic())]
        } else { vec![Span::raw(&self.input)] };
        f.render_widget(
            Paragraph::new(Text::from(Line::from(inp))).block(
                Block::default().borders(Borders::TOP)
                .border_style(Style::new().fg(Color::DarkGray))
                .title(" 입력 ").title_style(Style::new().fg(Color::Green))
            ),
            input_area,
        );
    }

    fn render_session_picker(&self, f:&mut Frame, size:Rect) {
        let (x,y,w,h) = (2, 2, size.width.saturating_sub(4), size.height.saturating_sub(8));
        let area = Rect::new(x, y, w.max(40), h);
        f.render_widget(Clear, area);

        let mut items:Vec<ListItem> = Vec::new();
        if self.pick_loading {
            items.push(ListItem::new("로딩 중..."));
        } else {
            for (i, s) in self.sessions.iter().enumerate() {
                let icon = if s.running { "▶" } else { "💬" };
                let title = if s.title.is_empty() || s.title=="attacca-cli" { "(제목 없음)" } else { &s.title };
                let time = fmt_time(&s.updated_at);
                let marker = if i==self.pick_sel { "→" } else { " " };
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(format!("{marker} {icon} "), Style::new().fg(Color::Cyan)),
                    Span::raw(format!("{title}")),
                    Span::styled(format!("  {time}",), Style::new().fg(Color::DarkGray)),
                    Span::styled(format!(" [{}]", fmt_sid(&s.id)), Style::new().fg(Color::DarkGray).italic()),
                ])));
            }
            if self.sessions.is_empty() {
                items.push(ListItem::new(Line::from(vec![Span::styled("  (세션 없음 — Enter로 새로 만들기)", Style::new().fg(Color::DarkGray))])));
            }
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ✨", if self.sessions.len()==self.pick_sel { "→" } else { " " }), Style::new().fg(Color::Green)),
                Span::styled(" 새 세션 만들기", Style::new().fg(Color::Green)),
            ])));
            items.push(ListItem::new(Line::from(vec![
                Span::styled("  📁 ", Style::new().fg(Color::Blue)),
                Span::styled("프로젝트 선택", Style::new().fg(Color::Blue)),
            ])));
        }

        let list = List::new(items).block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Sessions ").title_style(Style::new().fg(Color::Cyan)))
            .highlight_style(Style::new().bg(Color::DarkGray));
        f.render_widget(list, area);
    }

    fn render_project_picker(&self, f:&mut Frame, size:Rect) {
        let (x,y,w,h) = (2, 2, size.width.saturating_sub(4), size.height.saturating_sub(8));
        let area = Rect::new(x, y, w.max(40), h);
        f.render_widget(Clear, area);

        let mut items:Vec<ListItem> = Vec::new();
        for (i, p) in self.projects.iter().enumerate() {
            let marker = if i==self.pick_sel { "→" } else { " " };
            items.push(ListItem::new(Line::from(vec![
                Span::raw(format!("{marker} 📁 {name}", name=p.name.as_str())),
            ])));
        }
        items.push(ListItem::new(Line::from(vec![
            Span::styled("  💬 ", Style::new().fg(Color::Cyan)),
            Span::styled("세션 선택", Style::new().fg(Color::Cyan)),
        ])));

        let list = List::new(items).block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Projects ").title_style(Style::new().fg(Color::Blue)))
            .highlight_style(Style::new().bg(Color::DarkGray));
        f.render_widget(list, area);
    }

    async fn handle(&mut self, ev:Event) -> bool {
        match ev {
            Event::Key(k) if k.kind==KeyEventKind::Press => {
                match self.page {
                    Page::SessionPicker => {
                        match k.code {
                            KeyCode::Esc => { self.page = Page::Chat; self.pick_sel=0; }
                            KeyCode::Up | KeyCode::Char('k') => { self.pick_sel = self.pick_sel.saturating_sub(1); }
                            KeyCode::Down | KeyCode::Char('j') => { self.pick_sel = self.pick_sel.saturating_add(1); }
                            KeyCode::Enter => {
                                if self.pick_sel < self.sessions.len() {
                                    // open session
                                    let s = &self.sessions[self.pick_sel];
                                    let sid = s.id.clone();
                                    self.sid = Some(sid.clone());
                                    self.cursor = 0;
                                    self.first = false;
                                    self.msgs.clear();
                                    self.scroll = 0;
                                    self.page = Page::Chat;
                                    // load existing messages
                                    let msgs = self.client.get_msgs_after(&sid, 0).await;
                                    for m in &msgs { if m.role=="assistant" || m.role=="user" {
                                        let (t, tools) = parse_tool_calls(&m.text);
                                        if !t.is_empty() { self.add(if m.role=="user" { "user" } else { "agent" }, &t); }
                                        for _ in tools {}
                                    } else if m.role!="user" {
                                        let (t,_) = parse_tool_calls(&m.text);
                                        if !t.is_empty() { self.add("agent", &t); }
                                    } if m.cursor > self.cursor { self.cursor = m.cursor; } }
                                    self.add("system", &format!("세션 재개: {sid}"));
                                } else if self.pick_sel == self.sessions.len() {
                                    // new session
                                    if let Ok(s) = self.client.create_session(self.pid.as_deref(), self.aid.as_deref()).await {
                                        self.sid = Some(s.id); self.cursor = 0; self.first = true; self.msgs.clear(); self.scroll = 0; self.page = Page::Chat;
                                        self.add("system", "새 세션 시작");
                                    }
                                } else {
                                    // projects
                                    self.projects = self.client.list_projects().await;
                                    self.pick_sel = 0;
                                    self.page = Page::ProjectPicker;
                                }
                            }
                            KeyCode::Char('n') => {
                                if let Ok(s) = self.client.create_session(self.pid.as_deref(), self.aid.as_deref()).await {
                                    self.sid = Some(s.id); self.cursor = 0; self.first = true; self.msgs.clear(); self.scroll = 0; self.page = Page::Chat;
                                    self.add("system", "새 세션 시작");
                                }
                            }
                            _ => {}
                        }
                    }
                    Page::ProjectPicker => {
                        match k.code {
                            KeyCode::Esc => { self.page = Page::SessionPicker; self.pick_sel = self.sessions.len(); }
                            KeyCode::Up | KeyCode::Char('k') => { self.pick_sel = self.pick_sel.saturating_sub(1); }
                            KeyCode::Down | KeyCode::Char('j') => { self.pick_sel = self.pick_sel.saturating_add(1); }
                            KeyCode::Enter => {
                                if self.pick_sel < self.projects.len() {
                                    self.pid = Some(self.projects[self.pick_sel].id.clone());
                                    self.page = Page::SessionPicker;
                                    self.pick_sel = 0;
                                    self.sessions = self.client.list_sessions().await;
                                } else {
                                    self.page = Page::SessionPicker;
                                    self.pick_sel = self.sessions.len();
                                }
                            }
                            _ => {}
                        }
                    }
                    Page::Chat => {
                        match k.code {
                            KeyCode::Enter => {
                                let msg = self.input.trim().to_string();
                                if !msg.is_empty() { self.input.clear(); self.send(msg).await; }
                            }
                            KeyCode::Char('y') | KeyCode::Char('Y') => { self.approve_tool(true).await; }
                            KeyCode::Char('n') | KeyCode::Char('N') => { self.approve_tool(false).await; }
                            KeyCode::Char('q') => return false,
                            KeyCode::Char('/') => if self.input.is_empty() { self.input.push('/'); }
                            KeyCode::Char(c) => self.input.push(c),
                            KeyCode::Backspace => { self.input.pop(); }
                            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                            KeyCode::Down => self.scroll = self.scroll.saturating_add(1).min(self.msgs.len().saturating_sub(1)),
                            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
                            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5).min(self.msgs.len().saturating_sub(1)),
                            KeyCode::Tab => {
                                self.sessions = self.client.list_sessions().await;
                                self.pick_sel = 0;
                                self.page = Page::SessionPicker;
                            }
                            KeyCode::Esc => {}
                            _ => {}
                        }
                    }
                }
            }
            Event::Mouse(m) => {
                if let MouseEventKind::Down(MouseButton::Left)=m.kind {
                    // click on [Y] or [N] in tool prompt — handled by y/n keys
                }
            }
            _ => {}
        }
        true
    }

    async fn send(&mut self, raw:String) {
        if raw.starts_with('/') {
            match raw.as_str() {
                "/quit"|"/exit"|"/q" => std::process::exit(0),
                "/help"|"/h" => { self.add("system","Enter:전송  ↑↓:스크롤  Tab:세션  y/n:툴승인  q:종료"); return; }
                "/sessions"|"/session" => { self.sessions=self.client.list_sessions().await; self.pick_sel=0; self.page=Page::SessionPicker; return; }
                "/new" => {
                    if let Ok(s)=self.client.create_session(self.pid.as_deref(),self.aid.as_deref()).await {
                        self.sid=Some(s.id); self.cursor=0; self.first=true; self.msgs.clear(); self.scroll=0;
                        self.add("system","새 세션");
                    } else { self.add("system","세션 생성 실패"); }
                    return;
                }
                _ => {}
            }
        }

        self.add("user", &raw);
        self.busy = true;

        // ensure session
        if self.sid.is_none() {
            if let Ok(s)=self.client.create_session(self.pid.as_deref(),self.aid.as_deref()).await {
                let sid = s.id;
                self.sid=Some(sid.clone());
                self.cursor=0; self.first=true;
            } else { self.add("system","세션 생성 실패"); self.busy=false; return; }
        }

        let sid = self.sid.as_ref().unwrap().clone();
        let msg = if self.first {
            self.first=false;
            let n = raw.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
            let n = if n.len()>40 { format!("{}...",&n[..37]) } else { n };
            let _ = self.client.rename_session(&sid, &n).await;
            format!("{}\n\n---\n{}", PROTOCOL, raw)
        } else { raw };

        if let Err(e)=self.client.send_message(&sid,&msg).await {
            self.add("system", &format!("전송 실패: {e}")); self.busy=false; return;
        }

        // wait
        loop { if let Ok(s)=self.client.get_session(&sid).await { if !s.running { break; } } tokio::time::sleep(Duration::from_millis(500)).await; }

        let msgs = self.client.get_msgs_after(&sid, self.cursor).await;
        for m in &msgs {
            if m.cursor > self.cursor { self.cursor = m.cursor; }
            if m.role == "assistant" {
                let (t, tools) = parse_tool_calls(&m.text);
                if !t.is_empty() { self.add("agent", &t); }
                for j in tools { self.add_tool(&j); }
            }
        }
        self.busy = false;
    }

    async fn approve_tool(&mut self, yes:bool) {
        let idx = self.msgs.iter().rposition(|m| m.is_tool && !m.approved && !m.rejected);
        let Some(i) = idx else { return };
        if yes {
            self.msgs[i].approved = true;
            let result = execute_tool(&self.msgs[i].text.replacen("🔧 ","",1));
            self.add("tool_result", &result);
        } else {
            self.msgs[i].rejected = true;
            self.add("system", "툴 거절됨");
        }

        // send result back to agent
        if let Some(sid) = &self.sid.clone() {
            let result_text = if yes { self.msgs.last().map(|m| m.text.clone()).unwrap_or_default() } else { "rejected by user".into() };
            self.busy = true;
            if self.client.send_message(sid, &format!("[Tool result]\n{result_text}")).await.is_ok() {
                loop { if let Ok(s)=self.client.get_session(sid).await { if !s.running { break; } } tokio::time::sleep(Duration::from_millis(500)).await; }
                let msgs = self.client.get_msgs_after(sid, self.cursor).await;
                for m in &msgs {
                    if m.cursor > self.cursor { self.cursor = m.cursor; }
                    if m.role == "assistant" {
                        let (t, tools) = parse_tool_calls(&m.text);
                        if !t.is_empty() { self.add("agent", &t); }
                        for j in tools { self.add_tool(&j); }
                    }
                }
            }
            self.busy = false;
        }
    }
}

// ── Main ──

#[derive(Parser)]
#[command(name="attacca", version, about="Attacca CLI")]
struct Cli {
    #[arg(short='P',long,env="ATTACCA_PROJECT")] project:Option<String>,
    #[arg(short='S',long,env="ATTACCA_SESSION")] session:Option<String>,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let client = match ApiClient::from_env() { Ok(c)=>c, Err(e)=>{ eprintln!("✖ {e}"); std::process::exit(1); } };

    terminal::enable_raw_mode().ok();
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture).ok();
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)).unwrap();

    let mut app = App::new(client);

    // init
    if let Some(sid) = &cli.session {
        app.sid = Some(sid.clone());
        app.first = false;
        // load messages
        let msgs = app.client.get_msgs_after(sid, 0).await;
        for m in &msgs {
            if m.cursor > app.cursor { app.cursor = m.cursor; }
            if m.role == "assistant" { let (t,_) = parse_tool_calls(&m.text); if !t.is_empty() { app.add("agent", &t); } }
        }
        app.add("system", &format!("세션 재개: {}", fmt_sid(sid)));
    } else {
        app.add("system", "Tab으로 세션 선택, Enter로 메시지 전송");
    }

    loop {
        let _ = term.draw(|f| app.render(f));

        if event::poll(Duration::from_millis(100)).ok().unwrap_or(false) {
            if let Ok(ev) = event::read() {
                if !app.handle(ev).await { break; }
            }
        }
    }

    terminal::disable_raw_mode().ok();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture).ok();
}
