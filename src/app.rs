use crate::api::Api;
use crate::tools::{exec_tool, parse_tools, short, PROTOCOL};
use crate::ui;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::event::MouseEventKind;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Msg {
    pub role: String,
    pub text: String,
    pub raw: Option<String>,
    pub done: bool,
}

#[derive(Clone, Debug)]
pub enum SidebarItem {
    ProjectHeader { id: String, name: String, expanded: bool, session_count: usize },
    Session { title: String, id: String, active: bool },
    NewSession,
}

enum BgEvent {
    NewMsgs { msgs: Vec<Msg>, new_cur: i64 },
    Done,
}

enum Action {
    Send(String),
    Open(String),
    Create,
}

pub struct App {
    pub api: Api,
    pub sid: Option<String>,
    pub cur: i64,
    pub msgs: Vec<Msg>,
    pub input: String,
    pub scroll: usize,
    pub at_end: bool,
    pub busy: bool,
    pub sidebar_items: Vec<SidebarItem>,
    pub sel: usize,
    pub sidebar_scroll: usize,
    pub first: bool,
    pub sessions: Vec<(String, String, String)>,
    expanded_projects: std::collections::HashSet<String>,
    project_names: HashMap<String, String>,
    exit_requested: bool,
    pub focus: Focus,
    pub autocomplete_suggestions: Vec<String>,
    pub autocomplete_idx: Option<usize>,

    bg_tx: mpsc::UnboundedSender<BgEvent>,
    bg_rx: mpsc::UnboundedReceiver<BgEvent>,
    actions: Vec<Action>,
}

#[derive(PartialEq)]
pub enum Focus {
    Chat,
    Sidebar,
}

impl App {
    pub fn new(api: Api) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            api, sid: None, cur: 0, msgs: vec![], input: String::new(),
            scroll: 0, at_end: true, busy: false, sidebar_items: vec![], sel: 0,
            sidebar_scroll: 0, first: true, sessions: vec![],
            expanded_projects: std::collections::HashSet::new(),
            project_names: HashMap::new(),
            exit_requested: false,
            focus: Focus::Chat,
            autocomplete_suggestions: vec![],
            autocomplete_idx: None,
            bg_tx: tx, bg_rx: rx, actions: vec![],
        }
    }

    pub async fn run(&mut self) {
        terminal::enable_raw_mode().ok();
        let mut stdout = io::stdout();
        crossterm::execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture).ok();
        let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)) {
            Ok(t) => t,
            Err(_) => { eprintln!("term init failed"); return; }
        };
        term.clear().ok();
        self.load_sessions().await;
        self.load_projects().await;
        self.add("sys", "attacca — enter:send  tab:autocomplete  y/n:tool  /exit");

        loop {
            if term.draw(|f| ui::draw(f, self)).is_err() { break; }
            if self.exit_requested { break; }

            // drain bg events (poll results)
            while let Ok(ev) = self.bg_rx.try_recv() {
                match ev {
                    BgEvent::NewMsgs { msgs, new_cur } => {
                        self.cur = new_cur;
                        for m in msgs {
                            match m.role.as_str() {
                                "assistant" => {
                                    let (clean, tools) = parse_tools(&m.text);
                                    if !clean.is_empty() { self.add("agent", &clean); }
                                    for j in tools { self.add_tool(&j); }
                                }
                                "user" => {
                                    if !m.text.is_empty() { self.add("user", &m.text); }
                                }
                                _ => {
                                    if !m.text.is_empty() { self.add("sys", &m.text); }
                                }
                            }
                        }
                    }
                    BgEvent::Done => { self.busy = false; }
                }
            }

            // process one action at a time
            if !self.busy && !self.actions.is_empty() {
                let action = self.actions.remove(0);
                self.busy = true;
                match action {
                    Action::Send(msg) => self.send_async(msg).await,
                    Action::Open(sid) => self.open_async(&sid).await,
                    Action::Create => self.create_async().await,
                }
                continue;
            }

            // poll keyboard & mouse
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(k)) => {
                        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                            break;
                        }
                        if (k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat)
                            && !self.handle_key(k.code) { break; }
                    }
                    Ok(Event::Mouse(m)) => {
                        match m.kind {
                            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                                // columns 0-27 = sidebar, 28+ = chat
                                if m.column < 30 {
                                    self.focus = Focus::Sidebar;
                                    let list_row = m.row.saturating_sub(2) as usize;
                                    let idx = list_row + self.sidebar_scroll;
                                    if idx < self.sidebar_items.len() {
                                        self.sel = idx;
                                        self.activate_sidebar_selection();
                                    }
                                } else {
                                    self.focus = Focus::Chat;
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                const S: usize = 3;
                                if m.column < 30 {
                                    self.sidebar_scroll = self.sidebar_scroll.saturating_add(S)
                                        .min(self.sidebar_items.len().saturating_sub(1));
                                    let max_vis = (12usize).min(self.sidebar_items.len());
                                    if self.sel < self.sidebar_scroll { self.sel = self.sidebar_scroll; }
                                    if self.sel >= self.sidebar_scroll + max_vis {
                                        self.sel = self.sidebar_scroll + max_vis - 1;
                                    }
                                } else {
                                    // chat scroll — 3 lines per tick
                                    if self.at_end {
                                        self.at_end = false;
                                        self.scroll = S;
                                    } else {
                                        self.scroll = self.scroll.saturating_add(S);
                                    }
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                const S: usize = 3;
                                if m.column < 30 {
                                    self.sidebar_scroll = self.sidebar_scroll.saturating_sub(S);
                                    if self.sel >= self.sidebar_scroll + 12 {
                                        self.sel = self.sidebar_scroll + 11;
                                    }
                                } else {
                                    // chat scroll — 3 lines per tick
                                    if self.at_end || self.scroll > 0 {
                                        if self.at_end {
                                            self.at_end = false;
                                            self.scroll = 0;
                                        } else {
                                            self.scroll = self.scroll.saturating_sub(S);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
        // cleanup — runs on Ctrl+C or /exit
        terminal::disable_raw_mode().ok();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture);
    }

    fn add(&mut self, role: &str, text: &str) {
        if text.trim().is_empty() { return; }
        self.msgs.push(Msg { role: role.into(), text: text.into(), raw: None, done: false });
        if self.at_end {
            self.scroll = 0; // doesn't matter — at_end overrides
        }
    }

    fn add_tool(&mut self, json: &str) {
        let v: Value = serde_json::from_str(json).unwrap_or_default();
        let tool = v["tool"].as_str().unwrap_or("?");
        let args = v.get("args").and_then(|a| a.as_object())
            .map(|o| o.iter().filter_map(|(k, vv)| Some(format!("{k}={}", vv.as_str()?))).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        self.msgs.push(Msg { role: "tool".into(), text: format!("◆ {tool} {args}"), raw: Some(json.into()), done: false });
        if self.at_end { self.scroll = 0; }
    }

    pub fn rebuild_sidebar(&mut self) {
        use std::collections::BTreeMap;
        let mut project_map: BTreeMap<String, (String, Vec<(String, String)>)> = BTreeMap::new();
        for (pid, title, id) in &self.sessions {
            let entry = project_map.entry(pid.clone()).or_insert_with(|| {
                let name = if pid.is_empty() { "📁 All".into() } else {
                    let known = self.project_names.get(pid).cloned().unwrap_or_default();
                    if known.is_empty() { format!("📁 {}", short(pid)) }
                    else { format!("📁 {} ({})", known, short(pid)) }
                };
                (name, vec![])
            });
            entry.1.push((title.clone(), id.clone()));
        }
        let active_id = self.sid.clone().unwrap_or_default();
        self.sidebar_items.clear();
        for (pid, (pname, sess_list)) in &project_map {
            let expanded = self.expanded_projects.contains(pid.as_str());
            self.sidebar_items.push(SidebarItem::ProjectHeader {
                id: pid.clone(), name: pname.clone(), expanded, session_count: sess_list.len(),
            });
            if expanded {
                for (title, id) in sess_list {
                    self.sidebar_items.push(SidebarItem::Session { title: title.clone(), id: id.clone(), active: *id == active_id });
                }
            }
        }
        self.sidebar_items.push(SidebarItem::NewSession);
        let max = self.sidebar_items.len().saturating_sub(1);
        self.sel = self.sel.min(max);
        // keep scroll in bounds
        self.sidebar_scroll = self.sidebar_scroll.min(max.saturating_sub(1));
    }

    // ── Key handling ──

    fn handle_key(&mut self, code: KeyCode) -> bool {
        // Tab: cycle autocomplete suggestions if any, else toggle focus
        if code == KeyCode::Tab {
            if self.focus == Focus::Chat && !self.autocomplete_suggestions.is_empty() {
                self.cycle_autocomplete();
            } else {
                self.focus = match self.focus {
                    Focus::Chat => Focus::Sidebar,
                    Focus::Sidebar => Focus::Chat,
                };
            }
            return true;
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar(code),
            Focus::Chat => self.handle_chat(code),
        }
    }

    fn handle_sidebar(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Up => {
                if self.sel > 0 { self.sel -= 1; self.clamp_sidebar_scroll(); }
            }
            KeyCode::Down => {
                let max = self.sidebar_items.len().saturating_sub(1);
                if self.sel < max { self.sel += 1; self.clamp_sidebar_scroll(); }
            }
            KeyCode::Enter | KeyCode::Right => {
                if self.sel < self.sidebar_items.len() {
                    self.activate_sidebar_selection();
                }
            }
            KeyCode::Left => {
                for i in (0..self.sel).rev() {
                    if matches!(&self.sidebar_items[i], SidebarItem::ProjectHeader { .. }) {
                        if let SidebarItem::ProjectHeader { id, .. } = &self.sidebar_items[i].clone() {
                            self.expanded_projects.remove(id);
                            self.rebuild_sidebar();
                            self.sel = i;
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn handle_chat(&mut self, code: KeyCode) -> bool {
        const SCROLL_SPEED: usize = 3;
        match code {
            KeyCode::Up => {
                if self.at_end {
                    self.at_end = false;
                    self.scroll = 0;
                } else if self.scroll > 0 {
                    self.scroll = self.scroll.saturating_sub(SCROLL_SPEED);
                }
            }
            KeyCode::Down => {
                if !self.at_end {
                    self.scroll = self.scroll.saturating_add(SCROLL_SPEED);
                }
            }
            KeyCode::PageUp => {
                if self.at_end {
                    self.at_end = false;
                    self.scroll = 0;
                } else {
                    self.scroll = self.scroll.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if !self.at_end {
                    self.scroll = self.scroll.saturating_add(10);
                }
            }
            KeyCode::Home => { self.at_end = false; self.scroll = 0; }
            KeyCode::End => { self.at_end = true; self.scroll = 0; }
            KeyCode::Enter => {
                let m = self.input.trim().to_string();
                if !m.is_empty() {
                    for item in &self.sidebar_items {
                        if let SidebarItem::Session { title, id, .. } = item {
                            if title == &m {
                                self.input.clear();
                                self.actions.push(Action::Open(id.clone()));
                                return true;
                            }
                        }
                    }
                    self.input.clear();
                    match m.as_str() {
                        "/exit" | "/quit" => { self.exit_requested = true; return true; }
                        "/help" | "/h" => { self.add("sys", "enter:send  y/n:tool  ↑↓:scroll"); return true; }
                        "/new" | "/n" => { self.actions.push(Action::Create); return true; }
                        _ => {}
                    }
                    self.add("user", &m);
                    if self.api.key.is_empty() {
                        self.add("sys", "no API key — set ATTACCA_API_KEY");
                        return true;
                    }
                    self.actions.push(Action::Send(m));
                }
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => self.approve(true),
            KeyCode::Char('n') | KeyCode::Char('N') => self.approve(false),
            KeyCode::Char(c) => {
                self.input.push(c);
                self.update_autocomplete();
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.update_autocomplete();
            }
            _ => {}
        }
        true
    }

    fn update_autocomplete(&mut self) {
        self.autocomplete_suggestions.clear();
        self.autocomplete_idx = None;
        let trimmed = self.input.trim();
        if trimmed.starts_with('/') && !trimmed.is_empty() && trimmed.len() > 1 {
            let cmds = ["/exit", "/help", "/sessions", "/new"];
            for cmd in &cmds {
                if cmd.starts_with(trimmed) {
                    self.autocomplete_suggestions.push(cmd.to_string());
                }
            }
        }
    }

    fn cycle_autocomplete(&mut self) {
        let n = self.autocomplete_suggestions.len();
        if n == 0 { return; }
        let next = self.autocomplete_idx.map(|i| (i + 1) % n).unwrap_or(0);
        self.autocomplete_idx = Some(next);
        if let Some(cmd) = self.autocomplete_suggestions.get(next) {
            self.input = cmd.clone();
            self.input.push(' ');
        }
    }

    fn clamp_sidebar_scroll(&mut self) {
        let vis = 12usize;
        if self.sel < self.sidebar_scroll {
            self.sidebar_scroll = self.sel;
        } else if self.sel >= self.sidebar_scroll + vis {
            self.sidebar_scroll = self.sel.saturating_sub(vis) + 1;
        }
    }

    fn activate_sidebar_selection(&mut self) {
        if self.sel >= self.sidebar_items.len() { return; }
        match self.sidebar_items[self.sel].clone() {
            SidebarItem::ProjectHeader { id, ref expanded, .. } => {
                if *expanded { self.expanded_projects.remove(&id); }
                else { self.expanded_projects.insert(id.clone()); }
                self.rebuild_sidebar();
            }
            SidebarItem::Session { id, .. } => {
                self.actions.push(Action::Open(id));
            }
            SidebarItem::NewSession => {
                self.actions.push(Action::Create);
            }
        }
    }

    // ── Async operations ──

    async fn open_async(&mut self, sid: &str) {
        self.sid = Some(sid.to_string());
        self.cur = 0;
        self.first = false;
        self.msgs.clear();
        self.scroll = 0;
        self.at_end = true;
        self.add("sys", &format!("loading {}", short(sid)));
        self.rebuild_sidebar();

        // fetch messages in bg
        let api = self.api.clone();
        let tx = self.bg_tx.clone();
        let s = sid.to_string();
        tokio::spawn(async move {
            let mut new_cur = 0i64;
            let mut new_msgs = Vec::new();
            if let Ok(body) = api.get(&format!("/v1/sessions/{s}/messages?after=0")).await {
                if let Ok(msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                    for m in msgs.iter() {
                        if let Some(c) = m["cursor"].as_i64() { if c > new_cur { new_cur = c; } }
                        let role = m["role"].as_str().unwrap_or("");
                        let text = m["text"].as_str().unwrap_or("");
                        if role == "assistant" || role == "user" {
                            let (clean, _) = parse_tools(text);
                            if !clean.is_empty() {
                                new_msgs.push(Msg { role: role.into(), text: clean, raw: None, done: false });
                            }
                        }
                    }
                }
            }
            if !new_msgs.is_empty() {
                let _ = tx.send(BgEvent::NewMsgs { msgs: new_msgs, new_cur });
            }
            let _ = tx.send(BgEvent::Done);
        });
    }

    async fn create_async(&mut self) {
        self.sid = None;
        self.first = true;
        self.msgs.clear();
        self.scroll = 0;
        self.at_end = true;
        match self.api.post("sessions", &serde_json::json!({"title": "attacca-cli"})).await {
            Ok(body) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    if let Some(id) = v["id"].as_str() {
                        self.sid = Some(id.to_string());
                        self.cur = 0;
                        self.first = true;
                        self.add("sys", "new session");
                        self.busy = false;
                        return;
                    }
                }
                self.add("sys", &format!("create: {body}"));
            }
            Err((c, b)) => {
                self.add("sys", &format!("create: HTTP {c}: {}", b.chars().take(100).collect::<String>()));
            }
        }
        self.busy = false;
    }

    async fn send_async(&mut self, raw: String) {
        if self.sid.is_none() {
            match self.api.post("sessions", &serde_json::json!({"title": "attacca-cli"})).await {
                Ok(body) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&body) {
                        if let Some(id) = v["id"].as_str() {
                            self.sid = Some(id.to_string());
                            self.cur = 0;
                            self.first = true;
                        } else {
                            self.add("sys", &format!("session: {body}")); self.busy = false; return;
                        }
                    } else { self.add("sys", "session: parse error"); self.busy = false; return; }
                }
                Err((c, b)) => {
                    self.add("sys", &format!("session: HTTP {c}: {}", b.chars().take(100).collect::<String>()));
                    self.busy = false; return;
                }
            }
        }
        let Some(ref sid) = self.sid.clone() else { self.busy = false; return; };
        let payload = if self.first {
            self.first = false;
            format!("{PROTOCOL}\n\n---\n{raw}")
        } else {
            raw
        };
        if let Err((c, b)) = self.api.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message": payload, "timezone": "Asia/Seoul"})).await {
            self.add("sys", &format!("send: HTTP {c}: {}", b.chars().take(100).collect::<String>()));
            self.busy = false; return;
        }
        let api = self.api.clone();
        let tx = self.bg_tx.clone();
        let c = self.cur;
        let s = sid.clone();
        tokio::spawn(async move {
            loop {
                match api.get(&format!("/v1/sessions/{s}")).await {
                    Ok(body) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&body) {
                            if !v["running"].as_bool().unwrap_or(true) { break; }
                        }
                    }
                    Err(_) => break,
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if let Ok(body) = api.get(&format!("/v1/sessions/{s}/messages?after={c}")).await {
                if let Ok(raw_msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                    let mut new_cur = c;
                    let mut msgs = Vec::new();
                    for m in &raw_msgs {
                        if let Some(c2) = m["cursor"].as_i64() { if c2 > new_cur { new_cur = c2; } }
                        if m["role"].as_str() == Some("assistant") {
                            if let Some(text) = m["text"].as_str() {
                                msgs.push(Msg { role: "assistant".into(), text: text.into(), raw: None, done: false });
                            }
                        }
                    }
                    if !msgs.is_empty() {
                        let _ = tx.send(BgEvent::NewMsgs { msgs, new_cur });
                    }
                }
            }
            let _ = tx.send(BgEvent::Done);
        });
    }

    // ── Tool approval ──

    fn approve(&mut self, yes: bool) {
        let idx = self.msgs.iter().rposition(|m| m.raw.is_some() && !m.done);
        let Some(i) = idx else { return };
        let json = self.msgs[i].raw.take().unwrap_or_default();
        self.msgs[i].done = true;
        let result = if yes { exec_tool(&json) } else { "skipped".into() };
        self.add("result", &result);
        if !yes || self.sid.is_none() { return; }
        let sid = self.sid.clone().unwrap();
        self.busy = true;
        let api = self.api.clone();
        let tx = self.bg_tx.clone();
        let cur = self.cur;
        tokio::spawn(async move {
            let _ = api.post(&format!("/v1/sessions/{sid}/messages"),
                &serde_json::json!({"message": format!("[tool result]\n{result}"), "timezone": "Asia/Seoul"})).await;
            loop {
                match api.get(&format!("/v1/sessions/{sid}")).await {
                    Ok(body) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&body) {
                            if !v["running"].as_bool().unwrap_or(true) { break; }
                        }
                    }
                    Err(_) => break,
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            if let Ok(body) = api.get(&format!("/v1/sessions/{sid}/messages?after={cur}")).await {
                if let Ok(raw_msgs) = serde_json::from_str::<Vec<Value>>(&body) {
                    let mut new_cur = cur;
                    let mut msgs = Vec::new();
                    for m in &raw_msgs {
                        if let Some(c) = m["cursor"].as_i64() { if c > new_cur { new_cur = c; } }
                        if m["role"].as_str() == Some("assistant") {
                            if let Some(text) = m["text"].as_str() {
                                msgs.push(Msg { role: "assistant".into(), text: text.into(), raw: None, done: false });
                            }
                        }
                    }
                    if !msgs.is_empty() {
                        let _ = tx.send(BgEvent::NewMsgs { msgs, new_cur });
                    }
                }
            }
            let _ = tx.send(BgEvent::Done);
        });
    }

    // ── API ──

    pub async fn load_projects(&mut self) {
        if let Ok(body) = self.api.get("projects").await {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&body) {
                for p in &arr {
                    if let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) {
                        self.project_names.insert(id.to_string(), name.to_string());
                    }
                }
            }
        }
        self.rebuild_sidebar();
    }

    pub async fn load_sessions(&mut self) {
        if self.api.key.is_empty() { return; }
        match self.api.get("sessions").await {
            Ok(body) => {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&body) {
                    self.sessions = arr.iter().map(|s| {
                        let pid = s["project_id"].as_str().unwrap_or("").to_string();
                        let t = s["title"].as_str().unwrap_or("");
                        let id = s["id"].as_str().unwrap_or("");
                        let title = if t.is_empty() || t == "attacca-cli" { "untitled".into() } else { t.into() };
                        (pid, title, id.into())
                    }).collect();
                    if !self.sessions.is_empty() {
                        let first_pid = self.sessions[0].0.clone();
                        if !first_pid.is_empty() { self.expanded_projects.insert(first_pid); }
                    }
                    self.expanded_projects.insert(String::new());
                } else {
                    self.add("sys", &format!("sessions: parse: {}", body.chars().take(80).collect::<String>()));
                }
            }
            Err((c, b)) => {
                self.add("sys", &format!("sessions: HTTP {c}: {}", b.chars().take(80).collect::<String>()));
            }
        }
        self.rebuild_sidebar();
    }
}
