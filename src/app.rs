use crate::api::Api;
use crate::tools::{exec_tool, parse_tools, short, PROTOCOL};
use crate::ui;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use serde_json::Value;
use std::io;
use std::time::Duration;

#[derive(Clone)]
pub struct Msg {
    pub role: String,
    pub text: String,
    pub raw: Option<String>,
    pub done: bool,
}

/// A flattened sidebar item for rendering.
#[derive(Clone, Debug)]
pub enum SidebarItem {
    ProjectHeader { id: String, name: String, expanded: bool, session_count: usize },
    Session { title: String, id: String, active: bool },
    NewSession,
}

pub struct App {
    pub api: Api,
    pub sid: Option<String>,
    pub cur: i64,
    pub msgs: Vec<Msg>,
    pub input: String,
    pub scroll: usize,
    pub busy: bool,
    pub sidebar_items: Vec<SidebarItem>,
    pub sel: usize,
    pub show_sidebar: bool,
    pub first: bool,
    // raw data
    pub sessions: Vec<(String, String, String)>, // (project_id, title, id)
    expanded_projects: std::collections::HashSet<String>,
}

impl App {
    pub fn new(api: Api) -> Self {
        Self {
            api,
            sid: None,
            cur: 0,
            msgs: vec![],
            input: String::new(),
            scroll: 0,
            busy: false,
            sidebar_items: vec![],
            sel: 0,
            show_sidebar: false,
            first: true,
            sessions: vec![],
            expanded_projects: std::collections::HashSet::new(),
        }
    }

    pub async fn run(&mut self) {
        terminal::enable_raw_mode().ok();
        let mut stdout = io::stdout();
        crossterm::execute!(stdout, EnterAlternateScreen).ok();
        let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)) {
            Ok(t) => t,
            Err(_) => { eprintln!("term init failed"); return; }
        };
        term.clear().ok();
        self.load_sessions().await;
        self.add("sys", "attacca — enter:send  tab:sidebar  y/n:tool  q:quit");
        loop {
            if term.draw(|f| ui::draw(f, self)).is_err() { break; }
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(k)) => {
                        if (k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat)
                            && !self.handle_key(k.code).await { break; }
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

    fn add(&mut self, role: &str, text: &str) {
        if text.trim().is_empty() { return; }
        self.msgs.push(Msg { role: role.into(), text: text.into(), raw: None, done: false });
        self.scroll = usize::MAX;
    }

    fn add_tool(&mut self, json: &str) {
        let v: Value = serde_json::from_str(json).unwrap_or_default();
        let tool = v["tool"].as_str().unwrap_or("?");
        let args = v.get("args").and_then(|a| a.as_object())
            .map(|o| o.iter().filter_map(|(k, vv)| Some(format!("{k}={}", vv.as_str()?))).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        self.msgs.push(Msg { role: "tool".into(), text: format!("◆ {tool} {args}"), raw: Some(json.into()), done: false });
        self.scroll = usize::MAX;
    }

    /// Rebuild flat sidebar_items from raw sessions + expand state.
    pub fn rebuild_sidebar(&mut self) {
        use std::collections::BTreeMap;
        let mut project_map: BTreeMap<String, (String, Vec<(String, String)>)> = BTreeMap::new();
        for (pid, title, id) in &self.sessions {
            let entry = project_map.entry(pid.clone()).or_insert_with(|| {
                let name = if pid.is_empty() { "📁 All".into() } else { format!("📁 {}", short(pid)) };
                (name, vec![])
            });
            entry.1.push((title.clone(), id.clone()));
        }

        let active_id = self.sid.clone().unwrap_or_default();
        self.sidebar_items.clear();

        for (pid, (pname, sess_list)) in &project_map {
            let expanded = self.expanded_projects.contains(pid.as_str());
            self.sidebar_items.push(SidebarItem::ProjectHeader {
                id: pid.clone(),
                name: pname.clone(),
                expanded,
                session_count: sess_list.len(),
            });
            if expanded {
                for (title, id) in sess_list {
                    let active = *id == active_id;
                    self.sidebar_items.push(SidebarItem::Session { title: title.clone(), id: id.clone(), active });
                }
            }
        }
        self.sidebar_items.push(SidebarItem::NewSession);
        self.sel = self.sel.min(self.sidebar_items.len().saturating_sub(1));
    }

    async fn handle_key(&mut self, code: KeyCode) -> bool {
        if self.show_sidebar {
            match code {
                KeyCode::Tab | KeyCode::Esc => { self.show_sidebar = false; return true; }
                KeyCode::Up => { self.sel = self.sel.saturating_sub(1); return true; }
                KeyCode::Down => { self.sel = self.sel.saturating_add(1).min(self.sidebar_items.len().saturating_sub(1)); return true; }
                KeyCode::Enter | KeyCode::Right => {
                    if self.sel < self.sidebar_items.len() {
                        match self.sidebar_items[self.sel].clone() {
                            SidebarItem::ProjectHeader { id, ref expanded, .. } => {
                                if *expanded {
                                    self.expanded_projects.remove(&id);
                                } else {
                                    self.expanded_projects.insert(id.clone());
                                }
                                self.rebuild_sidebar();
                            }
                            SidebarItem::Session { id, .. } => {
                                self.open(&id).await;
                                self.show_sidebar = false;
                            }
                            SidebarItem::NewSession => {
                                self.create().await;
                                self.show_sidebar = false;
                            }
                        }
                    }
                    return true;
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
                    return true;
                }
                _ => {}
            }
        } else {
            match code {
                KeyCode::Tab => {
                    self.show_sidebar = true;
                    self.rebuild_sidebar();
                    self.sel = self.sel.min(self.sidebar_items.len().saturating_sub(1));
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
                KeyCode::Up => {
                    if self.scroll > 0 && self.scroll != usize::MAX { self.scroll -= 1; }
                    else if self.scroll == usize::MAX { self.scroll = 0; }
                }
                KeyCode::Down => { if self.scroll != usize::MAX { self.scroll += 1; } }
                KeyCode::PageUp => { self.scroll = if self.scroll != usize::MAX { self.scroll.saturating_sub(10) } else { 0 }; }
                KeyCode::PageDown => { if self.scroll != usize::MAX { self.scroll = self.scroll.saturating_add(10); } }
                KeyCode::Home => { self.scroll = 0; }
                KeyCode::End => { self.scroll = usize::MAX; }
                _ => {}
            }
        }
        true
    }

    // ── API ──

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
                        if !first_pid.is_empty() {
                            self.expanded_projects.insert(first_pid);
                        }
                    }
                    self.expanded_projects.insert(String::new());
                } else {
                    self.add("sys", &format!("sessions: unexpected: {}", body.chars().take(80).collect::<String>()));
                }
            }
            Err((c, b)) => {
                self.add("sys", &format!("sessions: HTTP {c}: {}", b.chars().take(80).collect::<String>()));
            }
        }
        self.rebuild_sidebar();
    }

    pub async fn open(&mut self, sid: &str) {
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

    pub async fn create(&mut self) {
        let payload = serde_json::json!({"title": "attacca-cli"});
        match self.api.post("sessions", &payload).await {
            Ok(body) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body) {
                    if let Some(id) = v["id"].as_str() {
                        self.sid = Some(id.into());
                        self.cur = 0;
                        self.first = true;
                        self.msgs.clear();
                        self.scroll = 0;
                        self.add("sys", "new session");
                        self.load_sessions().await;
                        return;
                    }
                }
                self.add("sys", &format!("create: {body}"));
            }
            Err((c, b)) => {
                self.add("sys", &format!("create: HTTP {c}: {}", b.chars().take(100).collect::<String>()));
            }
        }
        self.show_sidebar = false;
    }

    // ── Chat ──

    async fn send(&mut self, raw: String) {
        match raw.as_str() {
            "/q" | "/quit" | "/exit" => std::process::exit(0),
            "/h" | "/help" => { self.add("sys", "enter:send  tab:sessions  y/n:tool  ↑↓:scroll"); return; }
            "/sessions" | "/s" => { self.load_sessions().await; self.show_sidebar = true; return; }
            "/new" | "/n" => { self.create().await; return; }
            _ => {}
        }
        if self.api.key.is_empty() { self.add("sys", "no API key — set ATTACCA_API_KEY"); return; }
        self.add("user", &raw);
        self.busy = true;
        if self.sid.is_none() {
            match self.api.post("sessions", &serde_json::json!({"title": "attacca-cli"})).await {
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
        let payload = if self.first { self.first = false; format!("{PROTOCOL}\n\n---\n{raw}") } else { raw };
        if let Err((c, b)) = self.api.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message": payload, "timezone": "Asia/Seoul"})).await {
            self.add("sys", &format!("send: HTTP {c}: {}", b.chars().take(100).collect::<String>())); self.busy = false; return;
        }
        loop {
            match self.api.get(&format!("/v1/sessions/{sid}")).await {
                Ok(body) => { if let Ok(v) = serde_json::from_str::<Value>(&body) { if !v["running"].as_bool().unwrap_or(true) { break; } } }
                Err(_) => break,
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
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
        if !yes { return; }
        if let Some(sid) = self.sid.clone() {
            self.busy = true;
            let _ = self.api.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message": format!("[tool result]\n{result}"), "timezone": "Asia/Seoul"})).await;
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
