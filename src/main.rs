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

const SIDEW: u16 = 26;

// ── API ──

struct Api { inner: Client, key: String, base: String }
impl Api {
    fn from_env() -> Self {
        let key = std::env::var("ATTACCA_API_KEY").unwrap_or_default();
        let base = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| "https://attacca.cc".to_string());
        let inner = Client::builder().user_agent("attacca-cli").build().unwrap_or_default();
        Self { inner, key, base }
    }
    fn url(&self, p: &str) -> String { format!("{}/{}", self.base.trim_end_matches('/'), p.trim_start_matches('/')) }
    fn authd(&self) -> reqwest::RequestBuilder {
        let r = self.inner.get(&self.url("/v1/me"));
        if self.key.is_empty() { r } else { r.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key)).header(reqwest::header::ACCEPT, "application/json") }
    }
    async fn test(&self) -> String {
        let r = self.authd().send().await;
        match r {
            Ok(resp) => {
                let s = resp.status();
                let b = resp.text().await.unwrap_or_default();
                if s.is_success() {
                    if let Ok(v) = serde_json::from_str::<Value>(&b) {
                        let nm = v["display_name"].as_str().unwrap_or("?");
                        format!("✓ connected as {nm}")
                    } else { format!("✓ {} (unexpected body)", s) }
                } else { format!("✖ {s} {b}", b = &b[..b.len().min(120)]) }
            }
            Err(e) => format!("✖ {e}"),
        }
    }
    async fn call(&self, path: &str) -> Result<String, String> {
        let u = self.url(path);
        let r = self.inner.get(&u).header(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key)).header(reqwest::header::ACCEPT, "application/json").send().await.map_err(|e| format!("{e}"))?;
        let s = r.status(); let b = r.text().await.unwrap_or_default();
        if s.is_success() { Ok(b) } else { Err(format!("{s} {b}", b = &b[..b.len().min(120)])) }
    }
    async fn post(&self, path: &str, body: &Value) -> Result<String, String> {
        let u = self.url(path);
        let r = self.inner.post(&u).header(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key)).header(reqwest::header::ACCEPT, "application/json").json(body).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status(); let b = r.text().await.unwrap_or_default();
        if s.is_success() || s.as_u16() == 202 { Ok(b) } else { Err(format!("{s} {b}", b = &b[..b.len().min(120)])) }
    }
    async fn sessions(&self) -> Result<String, String> { self.call("/v1/sessions").await }
    async fn create(&self) -> Result<String, String> { self.post("/v1/sessions", &serde_json::json!({"title":"attacca-cli"})).await }
    async fn send(&self, sid: &str, msg: &str) -> Result<String, String> { self.post(&format!("/v1/sessions/{sid}/messages"), &serde_json::json!({"message":msg,"timezone":"Asia/Seoul"})).await }
    async fn poll(&self, sid: &str) -> Result<bool, String> { let r = self.call(&format!("/v1/sessions/{sid}")).await?; Ok(serde_json::from_str::<Value>(&r).ok().and_then(|v| v["running"].as_bool()).unwrap_or(false)) }
    async fn msgs(&self, sid: &str, after: i64) -> Result<String, String> { self.call(&format!("/v1/sessions/{sid}/messages?after={after}")).await }
    async fn rename(&self, sid: &str, title: &str) -> Result<String, String> { self.post(&format!("/v1/sessions/{sid}"), &serde_json::json!({"title":title})).await }
}

// ── Tools ──

fn parse_tools(text: &str) -> (String, Vec<String>) {
    let mut t = Vec::new(); let mut c = text.to_string();
    loop { let s = c.find("```attacca-tool"); let Some(i) = s else { break }; let cs = i + "```attacca-tool".len(); let e = c[cs..].find("```").map(|x| cs+x).unwrap_or(c.len()); t.push(c[cs..e].trim().to_string()); c.replace_range(i..e+3, ""); }
    (c.trim().to_string(), t)
}

fn exec(j: &str) -> String {
    let v: Value = serde_json::from_str(j).unwrap_or_default();
    let t = v["tool"].as_str().unwrap_or("?"); let a = |k:&str| v["args"][k].as_str().unwrap_or("");
    match t {
        "read_file" => match std::fs::read_to_string(a("path")) { Ok(s)=>format!("{s}"), Err(e)=>format!("err: {e}") },
        "write_file" => match std::fs::write(a("path"), a("content")) { Ok(())=>"ok".into(), Err(e)=>format!("err: {e}") },
        "list_dir" => match std::fs::read_dir(a("path")) { Ok(e)=>{let v:Vec<String>=e.flatten().map(|e| format!("{}{}", if e.file_type().map(|t|t.is_dir()).unwrap_or(false){"📁 "}else{"  "},e.file_name().to_string_lossy())).collect(); v.join("\n")}, Err(e)=>format!("err: {e}") },
        "run_command" => match std::process::Command::new("sh").arg("-c").arg(a("command")).output() { Ok(o)=>{let mut r=String::new();let so=String::from_utf8_lossy(&o.stdout);let se=String::from_utf8_lossy(&o.stderr);if!so.is_empty(){r.push_str(&so);}if!se.is_empty(){r.push_str(&format!("\nerr:\n{se}"));}r}, Err(e)=>format!("err: {e}") },
        "file_exists" => std::path::Path::new(a("path")).exists().to_string(),
        _ => format!("? {t}"),
    }
}

fn short(s: &str) -> String { if s.len()>8{s[..8].to_string()}else{s.to_string()} }

// ── App ──

struct Msg { role: String, text: String, raw: Option<String>, done: bool }
struct Sess { title: String, id: String }

struct App {
    api: Api,
    sid: Option<String>,
    cur: i64,
    msgs: Vec<Msg>,
    buf: String,
    scroll: usize,
    busy: bool,
    sides: Vec<Sess>,
    sel: usize,
    side: bool,
    status: String,
    first: bool,
}

impl App {
    fn new(api: Api) -> Self { Self { api, sid:None, cur:0, msgs:vec![], buf:String::new(), scroll:0, busy:false, sides:vec![], sel:0, side:false, status:String::new(), first:true } }

    fn add(&mut self, r:&str, t:&str) { if t.trim().is_empty(){return} self.msgs.push(Msg{role:r.into(),text:t.into(),raw:None,done:false}); self.scroll=self.msgs.len().saturating_sub(1); }

    fn add_tool(&mut self, j:&str) {
        let v:Value=serde_json::from_str(j).unwrap_or_default();
        let t=v["tool"].as_str().unwrap_or("?"); let a=v.get("args").and_then(|a|a.as_object()).map(|o|o.iter().filter_map(|(k,vv)|Some(format!("{}={}",k,vv.as_str()?))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        self.msgs.push(Msg{role:"tool".into(),text:format!("◆ {t} {a}"),raw:Some(j.into()),done:false});
    }

    fn render(&mut self, f:&mut Frame) {
        let a=f.area();
        f.render_widget(Paragraph::new("").style(Style::new().bg(Color::Rgb(15,15,22))), a);

        let c=Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(SIDEW),Constraint::Min(30)]).split(a);
        self.draw_side(f,c[0]); self.draw_chat(f,c[1]);
    }

    fn draw_side(&self,f:&mut Frame,area:Rect) {
        let bg=Style::new().bg(Color::Rgb(20,20,30));
        f.render_widget(Paragraph::new("").style(bg),area);
        f.render_widget(Paragraph::new("  Sessions").style(bg.fg(Color::White).add_modifier(Modifier::BOLD)), Rect::new(area.x,area.y,area.width,1));

        let mut items:Vec<ListItem>=vec![];
        for s in &self.sides {
            let act=self.sid.as_ref().map(|id|id==&s.id).unwrap_or(false);
            let dot=if act{"●"}else{" "};
            let title=if s.title.len()>18{format!("{}…",&s.title[..17])}else{s.title.clone()};
            let st=if act{Style::new().fg(Color::Rgb(100,180,255)).add_modifier(Modifier::BOLD).bg(Color::Rgb(20,20,30))}else{Style::new().fg(Color::White).bg(Color::Rgb(20,20,30))};
            items.push(ListItem::new(Line::from(vec![Span::styled(format!(" {dot} {title}"),st)])));
        }
        let ns=if self.sel==self.sides.len()&&self.side{"▸"}else{" "};
        items.push(ListItem::new(Line::from(vec![Span::styled(format!(" {ns} + new"),Style::new().fg(Color::Rgb(80,200,120)).bg(Color::Rgb(20,20,30)))])));

        f.render_widget(List::new(items).style(bg).highlight_style(Style::new().bg(Color::Rgb(30,30,40))), Rect::new(area.x,area.y+2,area.width,area.height.saturating_sub(5)));
        let hint=if self.side{" tab: chat"}else{" tab: side"};
        f.render_widget(Paragraph::new(hint).style(Style::new().fg(Color::Rgb(120,120,140)).bg(Color::Rgb(20,20,30))), Rect::new(area.x,area.height.saturating_sub(3),area.width,1));

        // status line
        if !self.status.is_empty() {
            f.render_widget(Paragraph::new(format!(" {}",self.status)).style(Style::new().fg(Color::Rgb(180,180,180)).bg(Color::Rgb(20,20,30))), Rect::new(area.x,area.y+1,area.width,1));
        }
    }

    fn draw_chat(&self,f:&mut Frame,area:Rect) {
        let c=Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1),Constraint::Min(3),Constraint::Length(3)]).split(area);
        let sid_d=self.sid.as_ref().map(|s|short(s)).unwrap_or_default();
        let st=if self.busy{format!(" {sid_d}  ···")}else{format!(" {sid_d}  {}msgs",self.msgs.len())};
        f.render_widget(Paragraph::new(st).style(Style::new().fg(Color::White).bg(Color::Rgb(28,28,40))),c[0]);

        let mut lines:Vec<Line>=vec![];
        for m in &self.msgs {
            match m.role.as_str() {
                "user"=>{lines.push(Line::from(vec![Span::styled(" you",Style::new().fg(Color::Rgb(80,200,120)).bold())]));for l in m.text.lines(){lines.push(Line::from(Span::raw(format!(" │ {l}"))))}}
                "agent"=>{for(i,l)in m.text.lines().enumerate(){lines.push(Line::from(vec![if i==0{Span::styled(format!(" ─ {l}"),Style::new().fg(Color::Rgb(100,180,255)))}else{Span::raw(format!(" {l}"))}]))}}
                "tool" if m.done=>{}
                "tool"=>{lines.push(Line::from(vec![Span::styled(&m.text,Style::new().fg(Color::Rgb(220,190,80)).bold())]));lines.push(Line::from(vec![Span::styled("  [y] run  [n] skip",Style::new().fg(Color::Rgb(120,120,140)))]))}
                "result"=>{lines.push(Line::from(vec![Span::styled(format!(" └ {}",m.text.lines().next().unwrap_or("")),Style::new().fg(Color::Rgb(120,120,140)))]))}
                _=>{for l in m.text.lines(){lines.push(Line::from(Span::raw(format!(" {l}"))))}}
            }
        }
        if lines.is_empty(){lines.push(Line::from(vec![Span::styled(" enter:send · tab:sidebar · y/n:tools · q:quit",Style::new().fg(Color::Rgb(120,120,140)))]))}
        let off=self.scroll.saturating_sub(12).min(self.msgs.len().saturating_sub(5));
        f.render_widget(Paragraph::new(Text::from(lines)).scroll((off as u16,0)).style(Style::new().bg(Color::Rgb(15,15,22))),c[1]);

        let inp=if self.buf.is_empty(){vec![Span::styled(" type here",Style::new().fg(Color::Rgb(120,120,140)))]}else{vec![Span::raw(format!(" {}",self.buf))]};
        f.render_widget(Paragraph::new(Text::from(Line::from(inp))).block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(Color::Rgb(28,28,40)))).style(Style::new().bg(Color::Rgb(28,28,40))),c[2]);
    }

    async fn handle(&mut self, ev:Event) -> bool {
        match ev{Event::Key(k) if k.kind==KeyEventKind::Press=>{
            if self.side { match k.code {
                KeyCode::Tab|KeyCode::Esc => self.side=false,
                KeyCode::Up|KeyCode::Char('k')=>self.sel=self.sel.saturating_sub(1),
                KeyCode::Down|KeyCode::Char('j')=>self.sel=self.sel.saturating_add(1).min(self.sides.len()),
                KeyCode::Enter=>{let id=self.sides.get(self.sel).map(|s|s.id.clone());if let Some(id)=id{self.open(&id).await;self.side=false;}else{self.create().await;}}
                _=>{}
            } } else { match k.code {
                KeyCode::Tab => { self.side=true; if self.sides.is_empty(){self.refresh().await} }
                KeyCode::Enter => { let m=self.buf.trim().to_string(); if!m.is_empty(){self.buf.clear();self.send(m).await} }
                KeyCode::Char('y')|KeyCode::Char('Y')=>self.approve(true).await,
                KeyCode::Char('n')|KeyCode::Char('N')=>self.approve(false).await,
                KeyCode::Char('q')=>return false,
                KeyCode::Char(c)=>self.buf.push(c),
                KeyCode::Backspace=>{self.buf.pop();}
                KeyCode::Up=>self.scroll=self.scroll.saturating_sub(1),
                KeyCode::Down=>self.scroll=self.scroll.saturating_add(1).min(self.msgs.len().saturating_sub(1)),
                _=>{}
            }}
        }_=>{}}
        true
    }

    async fn refresh(&mut self) {
        match self.api.sessions().await {
            Ok(r) => {
                if let Ok(Value::Array(arr))=serde_json::from_str::<Value>(&r) {
                    self.sides=arr.iter().map(|s|{let t=s["title"].as_str().unwrap_or("");let i=s["id"].as_str().unwrap_or("");Sess{title:if t.is_empty()||t=="attacca-cli"{"untitled".into()}else{t.into()},id:i.into()}}).collect();
                }
            }
            Err(e) => { self.status = format!("sessions: {e}"); }
        }
    }

    async fn open(&mut self, sid:&str) {
        self.sid=Some(sid.into()); self.cur=0; self.first=false; self.msgs.clear(); self.scroll=0;
        if let Ok(r)=self.api.msgs(sid,0).await {
            if let Ok(v)=serde_json::from_str::<Vec<Value>>(&r) {
                for m in v.iter().rev(){
                    let role=m["role"].as_str().unwrap_or(""); let text=m["text"].as_str().unwrap_or("");
                    if let Some(c)=m["cursor"].as_i64(){if c>self.cur{self.cur=c}}
                    if role=="assistant"||role=="user"{let(t,_)=parse_tools(text);if!t.is_empty(){self.add(role,&t)}}
                }
            }
        }
        self.add("system",&format!("resumed {}",short(sid)));
    }

    async fn create(&mut self) {
        match self.api.create().await {
            Ok(r) => {
                if let Ok(v)=serde_json::from_str::<Value>(&r) {
                    if let Some(id)=v["id"].as_str() { self.sid=Some(id.into()); self.cur=0; self.first=true; self.msgs.clear(); self.scroll=0; self.add("system","new"); self.refresh().await; return; }
                }
                self.add("system",&format!("create: {r}"));
            }
            Err(e) => { self.add("system",&format!("create: {e}")); }
        }
        self.side=false;
    }

    async fn send(&mut self, raw:String) {
        if raw=="/q"||raw=="/quit"||raw=="/exit"{std::process::exit(0)}
        if raw=="/h"||raw=="/help"{self.add("system","enter:send tab:side y/n:tool q:quit /test:api");return}
        if raw=="/test"||raw=="/t"{let r=self.api.test().await;self.status=r.clone();self.add("system",&r);return}

        self.add("user",&raw); self.busy=true;

        if self.sid.is_none() {
            match self.api.create().await {
                Ok(r)=>{if let Ok(v)=serde_json::from_str::<Value>(&r){if let Some(id)=v["id"].as_str(){self.sid=Some(id.into());self.cur=0;self.first=true;self.refresh().await}else{self.add("system",&format!("err: {r}"));self.busy=false;return}}else{self.add("system","parse err");self.busy=false;return}}
                Err(e)=>{self.add("system",&format!("err: {e}"));self.busy=false;return}
            }
        }

        let sid=self.sid.as_ref().unwrap().clone();
        let msg=if self.first{
            self.first=false;
            let n=raw.split_whitespace().take(5).collect::<Vec<_>>().join(" "); let n=if n.len()>40{format!("{}...",&n[..37])}else{n};
            let _=self.api.rename(&sid,&n).await; self.refresh().await;
            format!("{}\n\n---\n{}",r#"You are connected to the user's computer. Use ```attacca-tool blocks to call tools: read_file, write_file, list_dir, run_command. Never invent content."#,raw)
        }else{raw};

        if let Err(e)=self.api.send(&sid,&msg).await{self.add("system",&format!("send: {e}"));self.busy=false;return}

        loop { match self.api.poll(&sid).await { Ok(running)=>{if!running{break}} Err(_)=>{break} } tokio::time::sleep(Duration::from_millis(500)).await; }

        match self.api.msgs(&sid,self.cur).await {
            Ok(r)=>{if let Ok(v)=serde_json::from_str::<Vec<Value>>(&r){for m in &v{if let Some(c)=m["cursor"].as_i64(){if c>self.cur{self.cur=c}}if m["role"].as_str()==Some("assistant"){let text=m["text"].as_str().unwrap_or("");let(t,tools)=parse_tools(text);if!t.is_empty(){self.add("agent",&t)}for j in tools{self.add_tool(&j)}}}}}
            Err(e)=>{self.add("system",&format!("read: {e}"))}
        }
        self.busy=false;
    }

    async fn approve(&mut self, yes:bool) {
        let i=self.msgs.iter().rposition(|m|m.raw.is_some()&&!m.done); let Some(idx)=i else{return};
        let json=self.msgs[idx].raw.take().unwrap_or_default(); self.msgs[idx].done=true;
        let r=if yes{exec(&json)}else{"skipped".into()}; self.add("result",&r);
        if let Some(sid)=self.sid.clone() {
            self.busy=true; let _=self.api.send(&sid,&format!("[tool result]\n{r}")).await;
            loop{match self.api.poll(&sid).await{Ok(run)=>{if!run{break}}Err(_)=>{break}}tokio::time::sleep(Duration::from_millis(500)).await}
            if let Ok(r)=self.api.msgs(&sid,self.cur).await{if let Ok(v)=serde_json::from_str::<Vec<Value>>(&r){for m in &v{if let Some(c)=m["cursor"].as_i64(){if c>self.cur{self.cur=c}}if m["role"].as_str()==Some("assistant"){let t=m["text"].as_str().unwrap_or("");let(tx,tools)=parse_tools(t);if!tx.is_empty(){self.add("agent",&tx)}for j in tools{self.add_tool(&j)}}}}}
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

    // show connection test
    let test = api.test().await;
    eprintln!("{test}");

    terminal::enable_raw_mode().ok();
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).ok();
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)).ok();
    if term.is_none() { eprintln!("term err"); return; }
    let mut term = term.unwrap();

    let mut app = App::new(api);
    app.add("system", "enter:send · tab:sidebar · y/n:tools · /test:api");

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
