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

const PROTO: &str = r#"You are connected to the user's computer.
Use ```attacca-tool blocks to call tools:
read_file, write_file, list_dir, run_command, file_exists.
Never invent content."#;

// ── API with minimal abstraction ──

struct Api {
    inner: Client,
    key: String,
    pub base: String,
}

impl Api {
    fn from_env() -> Self {
        let key = std::env::var("ATTACCA_API_KEY").unwrap_or_default();
        let base = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| "https://attacca.cc".to_string());
        let inner = Client::builder().user_agent("attacca-cli/0.1").build().unwrap_or_default();
        Self { inner, key, base }
    }

    fn url(&self, p: &str) -> String { format!("{}/{}", self.base.trim_end_matches('/'), p.trim_start_matches('/')) }

    async fn get_json(&self, p: &str) -> Result<String, (u16, String)> {
        let url = self.url(p);
        let req = self.inner.get(&url)
            .header("Authorization", &format!("Bearer {}", self.key))
            .header("Accept", "application/json");
        let resp = req.send().await.map_err(|e| (0, format!("{e}")))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() { Ok(body) } else { Err((status.as_u16(), body)) }
    }

    async fn post_json(&self, p: &str, j: &Value) -> Result<String, (u16, String)> {
        let url = self.url(p);
        let req = self.inner.post(&url)
            .header("Authorization", &format!("Bearer {}", self.key))
            .header("Accept", "application/json")
            .json(j);
        let resp = req.send().await.map_err(|e| (0, format!("{e}")))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() || status.as_u16() == 202 { Ok(body) } else { Err((status.as_u16(), body)) }
    }
}

// ── Tool parsing ──

fn parse_tools(text: &str) -> (String, Vec<String>) {
    let mut t = vec![]; let mut c = text.to_string();
    loop {
        let i = c.find("```attacca-tool");
        let Some(s) = i else { break };
        let cs = s + "```attacca-tool".len();
        let e = c[cs..].find("```").map(|x| cs+x).unwrap_or(c.len());
        t.push(c[cs..e].trim().to_string());
        c.replace_range(s..e+3, "");
    }
    (c.trim().to_string(), t)
}

fn exec(j: &str) -> String {
    let v: Value = serde_json::from_str(j).unwrap_or_default();
    let t = v["tool"].as_str().unwrap_or("?");
    let a = |k: &str| v["args"][k].as_str().unwrap_or("");
    match t {
        "read_file" => match std::fs::read_to_string(a("path")) { Ok(s) => s, Err(e) => format!("err: {e}") },
        "write_file" => match std::fs::write(a("path"), a("content")) { Ok(()) => "ok".into(), Err(e) => format!("err: {e}") },
        "list_dir" => match std::fs::read_dir(a("path")) { Ok(e) => e.flatten().map(|e| format!("{}{}", if e.file_type().map(|t|t.is_dir()).unwrap_or(false){"📁 "}else{"  "}, e.file_name().to_string_lossy())).collect::<Vec<_>>().join("\n"), Err(e) => format!("err: {e}") },
        "run_command" => match std::process::Command::new("sh").arg("-c").arg(a("command")).output() { Ok(o) => { let mut r = String::from_utf8_lossy(&o.stdout).to_string(); let se = String::from_utf8_lossy(&o.stderr); if !se.is_empty() { r.push_str(&format!("\n[err]\n{se}")); } r }, Err(e) => format!("err: {e}") },
        _ => format!("? {t}"),
    }
}

fn short(s: &str) -> String { if s.len()>8 { s[..8].to_string() } else { s.to_string() } }

// ── Msg ──

struct Msg { role: String, text: String, raw: Option<String>, done: bool }

// ── App ──

struct App {
    api: Api,
    sid: Option<String>,
    cur: i64,
    msgs: Vec<Msg>,
    buf: String,
    scroll: usize,
    busy: bool,
    side_sessions: Vec<(String,String)>, // (title, id)
    side_sel: usize,
    side_focus: bool,
    first: bool,
}

impl App {
    fn new(api: Api) -> Self {
        Self {
            api, sid:None, cur:0, msgs:vec![], buf:String::new(),
            scroll:0, busy:false, side_sessions:vec![], side_sel:0,
            side_focus:false, first:true,
        }
    }

    fn add(&mut self, r:&str, t:&str) {
        if t.trim().is_empty(){return}
        self.msgs.push(Msg{role:r.into(),text:t.into(),raw:None,done:false});
        self.scroll=self.msgs.len().saturating_sub(1);
    }

    // ── render ──

    fn render(&mut self, f:&mut Frame) {
        let a=f.area();
        f.render_widget(Paragraph::new("").style(Style::new().bg(Color::Rgb(13,13,20))), a);
        let c=Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(26),Constraint::Min(30)]).split(a);
        self.draw_side(f,c[0]); self.draw_chat(f,c[1]);
    }

    fn draw_side(&self,f:&mut Frame,area:Rect) {
        let bg=Color::Rgb(18,18,28);
        f.render_widget(Paragraph::new("").style(Style::new().bg(bg)), area);
        f.render_widget(Paragraph::new("  sessions").style(Style::new().fg(Color::White).add_modifier(Modifier::BOLD).bg(bg)), Rect::new(area.x,area.y,area.width,1));

        let mut items:Vec<ListItem>=vec![];
        for s in &self.side_sessions {
            let act=self.sid.as_ref().map(|id|id==&s.1).unwrap_or(false);
            let title=if s.0.len()>18{format!("{}…",&s.0[..17])}else{s.0.clone()};
            let style=if act{Style::new().fg(Color::Rgb(100,180,255)).add_modifier(Modifier::BOLD).bg(bg)}else{Style::new().fg(Color::White).bg(bg)};
            items.push(ListItem::new(Line::from(vec![Span::styled(if act{format!(" ● {title}")}else{format!("   {title}")}, style)])));
        }
        let ns=if self.side_sel==self.side_sessions.len()&&self.side_focus{"▸"}else{" "};
        items.push(ListItem::new(Line::from(vec![Span::styled(format!(" {ns}+ new"),Style::new().fg(Color::Rgb(80,200,120)).bg(bg))])));

        f.render_widget(List::new(items).style(Style::new().bg(bg)), Rect::new(area.x,area.y+2,area.width,area.height.saturating_sub(6)));
        f.render_widget(Paragraph::new(if self.side_focus{" tab→chat"}else{" tab→side"}).style(Style::new().fg(Color::Rgb(100,100,120)).bg(bg)), Rect::new(area.x,area.height.saturating_sub(3),area.width,1));

        // key info
        let key_status = if self.api.key.is_empty() { "  no API key" } else { "  key set" };
        f.render_widget(Paragraph::new(key_status).style(Style::new().fg(Color::Rgb(120,120,140)).bg(bg)), Rect::new(area.x,area.height.saturating_sub(2),area.width,1));
    }

    fn draw_chat(&self,f:&mut Frame,area:Rect) {
        let top=Color::Rgb(28,28,40); let bg=Color::Rgb(13,13,20);
        let c=Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1),Constraint::Min(3),Constraint::Length(3)]).split(area);
        let s=self.sid.as_ref().map(|s|short(s)).unwrap_or_default();
        f.render_widget(Paragraph::new(format!(" {s}{}",if self.busy{" ···"}else{""})).style(Style::new().fg(Color::White).bg(top)),c[0]);

        let mut lines:Vec<Line>=vec![];
        for m in &self.msgs {
            match m.role.as_str() {
                "user"=>{lines.push(Line::from(vec![Span::styled(" you",Style::new().fg(Color::Rgb(80,200,120)).bold())]));for l in m.text.lines(){lines.push(Line::from(Span::raw(format!(" │ {l}"))))}}
                "agent"=>{for(i,l)in m.text.lines().enumerate(){lines.push(Line::from(vec![if i==0{Span::styled(format!(" ─ {l}"),Style::new().fg(Color::Rgb(100,180,255)))}else{Span::raw(format!(" {l}"))}]))}}
                "tool" if m.done=>{}
                "tool"=>{lines.push(Line::from(vec![Span::styled(&m.text,Style::new().fg(Color::Rgb(220,190,80)).bold())]));lines.push(Line::from(vec![Span::styled("  [y]/[n]",Style::new().fg(Color::Rgb(120,120,140)))]))}
                "result"=>{lines.push(Line::from(vec![Span::styled(format!(" └ {}",m.text.lines().next().unwrap_or("")),Style::new().fg(Color::Rgb(120,120,140)))]))}
                _=>{for l in m.text.lines(){lines.push(Line::from(Span::raw(format!(" {l}"))))}}
            }
        }
        if lines.is_empty(){lines.push(Line::from(vec![Span::styled(" enter:send · tab:side · y/n:tool · /test:api",Style::new().fg(Color::Rgb(120,120,140)))]))}
        let off=self.scroll.saturating_sub(12).min(self.msgs.len().saturating_sub(5));
        f.render_widget(Paragraph::new(Text::from(lines)).scroll((off as u16,0)).style(Style::new().bg(bg)),c[1]);

        let inp=if self.buf.is_empty(){vec![Span::styled(" type here",Style::new().fg(Color::Rgb(120,120,140)))]}else{vec![Span::raw(format!(" {}",self.buf))]};
        f.render_widget(Paragraph::new(Text::from(Line::from(inp))).block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(top))).style(Style::new().bg(top)),c[2]);
    }

    // ── events ──

    async fn handle(&mut self, ev:Event) -> bool {
        match ev {
            Event::Key(k) if k.kind==KeyEventKind::Press => {
                if self.side_focus {
                    match k.code {
                        KeyCode::Tab|KeyCode::Esc => self.side_focus=false,
                        KeyCode::Up => self.side_sel=self.side_sel.saturating_sub(1),
                        KeyCode::Down => self.side_sel=self.side_sel.saturating_add(1).min(self.side_sessions.len()),
                        KeyCode::Enter => {
                            if self.side_sel<self.side_sessions.len() {
                                let id=self.side_sessions[self.side_sel].1.clone();
                                self.open(&id).await;
                                self.side_focus=false;
                            } else { self.create().await; }
                        }
                        _ => {}
                    }
                } else {
                    match k.code {
                        KeyCode::Tab => { self.side_focus=true; if self.side_sessions.is_empty(){self.reload().await} }
                        KeyCode::Enter => { let m=self.buf.trim().to_string(); if!m.is_empty(){self.buf.clear();self.send(m).await} }
                        KeyCode::Char('y')|KeyCode::Char('Y')=>self.approve(true).await,
                        KeyCode::Char('n')|KeyCode::Char('N')=>self.approve(false).await,
                        KeyCode::Char('q')=>return false,
                        KeyCode::Char(c)=>self.buf.push(c),
                        KeyCode::Backspace=>{self.buf.pop();}
                        KeyCode::Up=>self.scroll=self.scroll.saturating_sub(1),
                        KeyCode::Down=>self.scroll=self.scroll.saturating_add(1).min(self.msgs.len().saturating_sub(1)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        true
    }

    // ── API ──

    async fn reload(&mut self) {
        let key_ok = !self.api.key.is_empty();
        if !key_ok { self.add("system", "No API key. Set ATTACCA_API_KEY in .env or env"); return; }

        match self.api.get_json("/v1/sessions").await {
            Ok(r) => {
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&r) {
                    self.side_sessions = arr.iter().map(|s| {
                        let t = s["title"].as_str().unwrap_or("");
                        let id = s["id"].as_str().unwrap_or("");
                        let title = if t.is_empty() || t=="attacca-cli" { "untitled".into() } else { t.into() };
                        (title, id.into())
                    }).collect();
                    self.add("system", &format!("{} sessions loaded", self.side_sessions.len()));
                } else { self.add("system", &format!("unexpected: {r}")); }
            }
            Err((code, body)) => {
                let preview = if body.len() > 100 { format!("{}...", &body[..100]) } else { body };
                self.add("system", &format!("sessions: HTTP {code}: {preview}"));
            }
        }
    }

    async fn open(&mut self, sid:&str) {
        self.sid=Some(sid.into()); self.cur=0; self.first=false; self.msgs.clear(); self.scroll=0;
        if let Ok(r)=self.api.get_json(&format!("/v1/sessions/{sid}/messages?after=0")).await {
            if let Ok(v)=serde_json::from_str::<Vec<Value>>(&r) {
                for m in v.iter().rev(){
                    if let Some(c)=m["cursor"].as_i64(){if c>self.cur{self.cur=c}}
                    let role=m["role"].as_str().unwrap_or("");
                    let text=m["text"].as_str().unwrap_or("");
                    if role=="assistant"||role=="user"{let(t,_)=parse_tools(text);if!t.is_empty(){self.add(role,&t)}}
                }
            }
        }
        self.add("system",&format!("opened {}",short(sid)));
    }

    async fn create(&mut self) {
        match self.api.post_json("/v1/sessions", &serde_json::json!({"title":"attacca-cli"})).await {
            Ok(r) => {
                if let Ok(v)=serde_json::from_str::<Value>(&r) {
                    if let Some(id)=v["id"].as_str() {
                        self.sid=Some(id.into()); self.cur=0; self.first=true; self.msgs.clear(); self.scroll=0;
                        self.add("system","new session");
                        self.reload().await;
                        return;
                    }
                }
                self.add("system",&format!("create: {r}"));
            }
            Err((code,body)) => self.add("system",&format!("create: HTTP {code}: {}", &body[..body.len().min(100)])),
        }
        self.side_focus=false;
    }

    async fn send(&mut self, raw:String) {
        if raw=="/q"||raw=="/quit"||raw=="/exit"{std::process::exit(0)}
        if raw=="/h"||raw=="/help"{self.add("system","enter:send tab:side y/n:tool q:quit /test:api");return}

        // /test: try multiple API URLs
        if raw=="/test"||raw=="/t"{
            let my_base = self.api.base.clone();
            let urls = ["/v1/me","/v1/sessions"];
            let bases = [my_base.as_str(), "https://attacca.cc", "https://attacca.cc/api/v1"];
            for base in &bases {
                for path in &urls {
                    let url = format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));
                    let req = self.api.inner.get(&url).header("Authorization", &format!("Bearer {}", self.api.key)).header("Accept", "application/json");
                    match req.send().await {
                        Ok(r) => {
                            let s=r.status(); let b=r.text().await.unwrap_or_default();
                            self.add("system", &format!("{url} → HTTP {s}: {}", &b[..b.len().min(80)]));
                        }
                        Err(e) => { self.add("system", &format!("{url} → err: {e}")); }
                    }
                }
            }
            return;
        }

        let key_ok = !self.api.key.is_empty();
        if !key_ok { self.add("system", "Set ATTACCA_API_KEY first"); return; }

        self.add("user",&raw); self.busy=true;

        if self.sid.is_none() {
            match self.api.post_json("/v1/sessions", &serde_json::json!({"title":"attacca-cli"})).await {
                Ok(r) => {
                    if let Ok(v)=serde_json::from_str::<Value>(&r) {
                        if let Some(id)=v["id"].as_str() { self.sid=Some(id.into()); self.cur=0; self.first=true; self.reload().await; }
                        else { self.add("system",&format!("session: {r}")); self.busy=false; return; }
                    } else { self.add("system","session: parse err"); self.busy=false; return; }
                }
                Err((c,b)) => { self.add("system",&format!("session: HTTP {c}: {}", &b[..b.len().min(100)])); self.busy=false; return; }
            }
        }

        let sid=self.sid.as_ref().unwrap().clone();
        let msg = if self.first {
            self.first=false;
            format!("{}\n\n---\n{}", PROTO, raw)
        } else { raw };

        match self.api.post_json(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message":msg,"timezone":"Asia/Seoul"})).await {
            Ok(_) => {}
            Err((c,b)) => { self.add("system",&format!("send: HTTP {c}: {}", &b[..b.len().min(100)])); self.busy=false; return; }
        }

        loop {
            match self.api.get_json(&format!("/v1/sessions/{sid}")).await {
                Ok(r) => { if let Ok(v)=serde_json::from_str::<Value>(&r) { if !v["running"].as_bool().unwrap_or(true) { break; } } }
                Err(_) => { break; }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        match self.api.get_json(&format!("/v1/sessions/{sid}/messages?after={}",self.cur)).await {
            Ok(r) => {
                if let Ok(v)=serde_json::from_str::<Vec<Value>>(&r) {
                    for m in &v {
                        if let Some(c)=m["cursor"].as_i64(){if c>self.cur{self.cur=c}}
                        if m["role"].as_str()==Some("assistant") {
                            let text=m["text"].as_str().unwrap_or("");
                            let(t,tools)=parse_tools(text);
                            if!t.is_empty(){self.add("agent",&t)}
                            for j in tools{self.add_tool(&j)}
                        }
                    }
                }
            }
            Err((c,b)) => { self.add("system",&format!("read: HTTP {c}: {}", &b[..b.len().min(100)])); }
        }
        self.busy=false;
    }

    fn add_tool(&mut self, j:&str) {
        let v:Value=serde_json::from_str(j).unwrap_or_default();
        let t=v["tool"].as_str().unwrap_or("?");
        let a=v.get("args").and_then(|a|a.as_object()).map(|o|o.iter().filter_map(|(k,vv)|Some(format!("{}={}",k,vv.as_str()?))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        self.msgs.push(Msg{role:"tool".into(),text:format!("◆ {t} {a}"),raw:Some(j.into()),done:false});
    }

    async fn approve(&mut self, yes:bool) {
        let i=self.msgs.iter().rposition(|m|m.raw.is_some()&&!m.done); let Some(idx)=i else{return};
        let json=self.msgs[idx].raw.take().unwrap_or_default(); self.msgs[idx].done=true;
        let r=if yes{exec(&json)}else{"skipped".into()}; self.add("result",&r);
        if let Some(sid)=self.sid.clone() {
            self.busy=true; let _=self.api.post_json(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message":format!("[tool result]\n{r}"),"timezone":"Asia/Seoul"})).await;
            loop{match self.api.get_json(&format!("/v1/sessions/{sid}")).await{Ok(r)=>{if let Ok(v)=serde_json::from_str::<Value>(&r){if!v["running"].as_bool().unwrap_or(true){break}}}Err(_)=>{break}}tokio::time::sleep(Duration::from_millis(500)).await}
            if let Ok(r)=self.api.get_json(&format!("/v1/sessions/{sid}/messages?after={}",self.cur)).await{if let Ok(v)=serde_json::from_str::<Vec<Value>>(&r){for m in &v{if let Some(c)=m["cursor"].as_i64(){if c>self.cur{self.cur=c}}if m["role"].as_str()==Some("assistant"){let t=m["text"].as_str().unwrap_or("");let(tx,tools)=parse_tools(t);if!tx.is_empty(){self.add("agent",&tx)}for j in tools{self.add_tool(&j)}}}}}
            self.busy=false;
        }
    }
}

// ── Main ──

#[derive(Parser)]
#[command(name = "attacca")]
struct Cli {}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let _cli = Cli::parse();

    let api = Api::from_env();

    // startup test
    eprint!("testing API... ");
    let test = api.get_json("/v1/me").await;
    match &test {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<Value>(body) {
                eprintln!("✓ {}", v["display_name"].as_str().unwrap_or("ok"));
            } else { eprintln!("✓ (response)"); }
        }
        Err((code, body)) => {
            eprintln!("HTTP {code}: {}", &body[..body.len().min(80)]);
            eprintln!("  URL: {}/v1/me", api.base);
            eprintln!("  key set: {}", !api.key.is_empty());
            eprintln!("  (enter /test in TUI to try more URLs)");
        }
    }

    terminal::enable_raw_mode().ok();
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).ok();
    let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)) {
        Ok(t) => t,
        Err(_) => { eprintln!("term err"); return; }
    };
    term.clear().unwrap();

    let mut app = App::new(api);
    app.add("system", "enter:send · tab:side · /test:api · y/n:tool · q:quit");

    loop {
        term.draw(|f| app.render(f)).ok();
        if event::poll(Duration::from_millis(100)).ok().unwrap_or(false) {
            if let Ok(ev) = event::read() {
                if !app.handle(ev).await { break; }
            }
        }
    }

    terminal::disable_raw_mode().ok();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen).ok();
}
