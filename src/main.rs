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
use serde::Deserialize;
use serde_json::Value;
use std::io;
use std::time::Duration;

const PROTOCOL: &str = r#"## attacca-cli — dual-computer bridge

You have access to the user's **local computer**.
Output JSON tool calls inside ```attacca-tool blocks.

Example:
```attacca-tool
{"tool": "read_file", "args": {"path": "/home/user/hello.txt"} }
```

Tools: read_file, write_file, edit_file, list_dir, run_command, create_dir, file_exists, delete_file, read_files

Use run_command for: grep, find, cat, ls, mkdir, cp, mv, sed, awk, git, cargo, npm. Do NOT invent file contents."#;

// ── DTOs ──

#[derive(Deserialize, Default, Clone, Debug)]
struct SessionDto { id:String, #[serde(default)] title:String, #[serde(default)] running:bool, #[serde(default)] updated_at:String }
#[derive(Deserialize, Clone, Debug)] struct MessageDto { #[serde(default)] role:String, #[serde(default)] text:String, #[serde(default)] cursor:i64 }
#[derive(Deserialize, Clone, Debug)] struct ProjectDto { id:String, name:String }

// ── API client ──

struct ApiClient { inner:Client, key:String, base_url:String }
impl ApiClient {
    fn from_env() -> Result<Self, String> {
        let key = std::env::var("ATTACCA_API_KEY").map_err(|_| "Set ATTACCA_API_KEY in .env".to_string())?;
        let base_url = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| "https://attacca.cc".to_string());
        let inner = Client::builder().user_agent("attacca-cli/0.1.0").build().map_err(|e| format!("{e}"))?;
        Ok(Self { inner, key, base_url })
    }
    fn url(&self, p:&str) -> String { format!("{}/{}", self.base_url.trim_end_matches('/'), p.trim_start_matches('/')) }
    fn bearer(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }
    async fn raw_get(&self, url:&str) -> Result<String,String> {
        let r = self.inner.get(url).headers(self.bearer()).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status(); let body = r.text().await.unwrap_or_default();
        if s.is_success() { Ok(body) } else { Err(format!("HTTP {s} {body}", body=&body[..body.len().min(200)])) }
    }
    async fn raw_post(&self, url:&str, json:&Value) -> Result<String,String> {
        let r = self.inner.post(url).headers(self.bearer()).json(json).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status(); let body = r.text().await.unwrap_or_default();
        if s.is_success() || s.as_u16()==202 { Ok(body) } else { Err(format!("HTTP {s} {body}", body=&body[..body.len().min(200)])) }
    }
    async fn raw_patch(&self, url:&str, json:&Value) -> Result<String,String> {
        let r = self.inner.patch(url).headers(self.bearer()).json(json).send().await.map_err(|e| format!("{e}"))?;
        let s = r.status(); let body = r.text().await.unwrap_or_default();
        if s.is_success() { Ok(body) } else { Err(format!("HTTP {s}")) }
    }

    async fn test_conn(&self) -> Result<String,String> {
        self.raw_get(&self.url("/v1/me")).await
    }
    async fn list_sessions_raw(&self) -> String {
        self.raw_get(&self.url("/v1/sessions")).await.unwrap_or_else(|e| e)
    }
    async fn list_projects_raw(&self) -> String {
        self.raw_get(&self.url("/v1/projects")).await.unwrap_or_else(|e| e)
    }
    async fn create_session_raw(&self, pid:Option<&str>) -> String {
        let mut body = serde_json::json!({"title":"attacca-cli"});
        if let Some(p) = pid { body["project_id"]=serde_json::json!(p); }
        self.raw_post(&self.url("/v1/sessions"), &body).await.unwrap_or_else(|e| e)
    }
    async fn send_msg_raw(&self, sid:&str, msg:&str) -> Result<String,String> {
        self.raw_post(&self.url(&format!("/v1/sessions/{sid}/messages")), &serde_json::json!({"message":msg,"timezone":"Asia/Seoul"})).await
    }
    async fn get_session_raw(&self, sid:&str) -> String {
        self.raw_get(&self.url(&format!("/v1/sessions/{sid}"))).await.unwrap_or_else(|e| e)
    }
    async fn get_msgs_raw(&self, sid:&str, after:i64) -> String {
        self.raw_get(&self.url(&format!("/v1/sessions/{sid}/messages?after={after}"))).await.unwrap_or_else(|e| e)
    }
    async fn rename_raw(&self, sid:&str, title:&str) -> Result<String,String> {
        self.raw_patch(&self.url(&format!("/v1/sessions/{sid}")), &serde_json::json!({"title":title})).await
    }
}

// ── Tool execution ──

fn parse_tool_calls(text:&str) -> (String, Vec<String>) {
    let mut tools=Vec::new(); let mut clean=text.to_string();
    loop {
        let s=match clean.find("```attacca-tool") { Some(i)=>i, None=>break };
        let cs=s+"```attacca-tool".len();
        let e=match clean[cs..].find("```") { Some(i)=>cs+i, None=>break };
        tools.push(clean[cs..e].trim().to_string());
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
        "read_files" => { let ps=a("paths"); let pv:Vec<&str>=if ps.starts_with('['){serde_json::from_str(ps).unwrap_or_default()}else{ps.split(',').collect()}; pv.iter().map(|p| format!("--- {p} ---\n{}",std::fs::read_to_string(p).unwrap_or_default())).collect::<Vec<_>>().join("\n") }
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
struct ChatMsg { role:String, text:String, tool_json:Option<String>, approved:bool }

#[derive(PartialEq)]
enum Page { Chat, SessionPicker, ProjectPicker }

struct App {
    client: ApiClient,
    sid: Option<String>,
    cursor: i64,
    pid: Option<String>,
    first: bool,
    msgs: Vec<ChatMsg>,
    input: String,
    page: Page,
    scroll: usize,
    busy: bool,
    sessions: Vec<SessionDto>,
    projects: Vec<ProjectDto>,
    pick_sel: usize,
}

impl App {
    fn new(client:ApiClient) -> Self { Self { client, sid:None, cursor:0, pid:None, first:true, msgs:Vec::new(), input:String::new(), page:Page::Chat, scroll:0, busy:false, sessions:Vec::new(), projects:Vec::new(), pick_sel:0 } }
    fn add(&mut self, r:&str, t:&str) { if t.trim().is_empty(){return;} self.msgs.push(ChatMsg{role:r.into(),text:t.into(),tool_json:None,approved:false}); self.scroll=self.msgs.len().saturating_sub(1); }
    fn add_tool(&mut self, j:&str) {
        let v:Value=serde_json::from_str(j).unwrap_or_default();
        let t=v["tool"].as_str().unwrap_or("?");
        let args=v.get("args").and_then(|a|a.as_object()).map(|o| o.iter().filter_map(|(k,vv)| Some(format!("{}={}",k,vv.as_str()?))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        self.msgs.push(ChatMsg{role:"tool".into(),text:format!("🔧 {t} {args}"),tool_json:Some(j.into()),approved:false});
        self.scroll=self.msgs.len().saturating_sub(1);
    }

    fn render(&mut self, f:&mut Frame) {
        let size=f.area();
        if size.width<40||size.height<10{f.render_widget(Paragraph::new("too small").centered().red(),size);return;}
        let c=Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1),Constraint::Min(3),Constraint::Length(3)]).split(size);
        let status=match self.page{
            Page::Chat=>{let s=self.sid.as_ref().map(|s|fmt_sid(s)).unwrap_or_else(||"없음".into()); let b=if self.busy{"⏳"}else{"✓"}; format!(" Attacca │ {s} │ {} 메시지 {b}",self.msgs.len())}
            Page::SessionPicker=>" 세션선택 (↑↓ Enter Esc n=새세션)".into(),
            Page::ProjectPicker=>" 프로젝트선택 (↑↓ Enter Esc)".into(),
        };
        f.render_widget(Paragraph::new(status).style(Style::new().fg(Color::White).bg(Color::DarkGray)),c[0]);
        match self.page{Page::Chat=>self.render_chat(f,c[1],c[2]),Page::SessionPicker=>self.render_picker(f,size),Page::ProjectPicker=>self.render_projects(f,size),}
    }

    fn render_chat(&self,f:&mut Frame,ca:Rect,ia:Rect){
        let mut lines:Vec<Line>=Vec::new();
        for m in &self.msgs {
            match m.role.as_str() {
                "user"=>{lines.push(Line::from(vec![Span::styled(" You",Style::new().fg(Color::Green).bold())]));for l in m.text.lines(){lines.push(Line::from(Span::raw(format!(" │ {l}"))));}}
                "agent"=>{lines.push(Line::from(vec![Span::styled(" Agent",Style::new().fg(Color::Cyan).bold())]));for l in m.text.lines(){lines.push(Line::from(Span::raw(format!(" │ {l}"))));}}
                "tool" if m.approved => { lines.push(Line::from(vec![Span::styled(" ✓ done",Style::new().fg(Color::Green))])); }
                "tool" => {
                    let danger=m.tool_json.as_ref().map(|j|is_dangerous(j)).unwrap_or(false);
                    lines.push(Line::from(vec![Span::styled(&m.text,if danger{Style::new().fg(Color::Red).bold()}else{Style::new().fg(Color::Yellow)})]));
                    lines.push(Line::from(vec![Span::styled("  [Y] approve  [N] reject",Style::new().fg(Color::DarkGray))]));
                }
                "result"=>{lines.push(Line::from(Span::styled(format!(" {}",m.text),Style::new().fg(Color::DarkGray))));}
                _=>{}
            }
        }
        if lines.is_empty(){lines.push(Line::from(Span::styled(" Enter: send  Tab: sessions  y/n: approve tools  q: quit",Style::new().fg(Color::DarkGray).italic())));}
        let off=self.scroll.saturating_sub(10).min(self.msgs.len().saturating_sub(10));
        f.render_widget(Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(Color::DarkGray))).scroll((off as u16,0)),ca);
        let inp=if self.input.is_empty(){vec![Span::styled("type here...",Style::new().fg(Color::DarkGray).italic())]}else{vec![Span::raw(&self.input)]};
        f.render_widget(Paragraph::new(Text::from(Line::from(inp))).block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(Color::DarkGray)).title(" input ").title_style(Style::new().fg(Color::Green))),ia);
    }

    fn render_picker(&self,f:&mut Frame,size:Rect){
        let a=Rect::new(2,2,size.width.saturating_sub(4),size.height.saturating_sub(8)); f.render_widget(Clear,a);
        let mut items:Vec<ListItem>=Vec::new();
        for(i,s)in self.sessions.iter().enumerate(){let t=if s.title.is_empty()||s.title=="attacca-cli"{"(untitled)"}else{&s.title};let m=if i==self.pick_sel{"→"}else{" "};let r=if s.running{"▶"}else{"💬"};items.push(ListItem::new(Line::from(vec![Span::raw(format!("{m} {r} {t}")),Span::styled(format!(" {}",&fmt_time(&s.updated_at)[..16]),Style::new().fg(Color::DarkGray))])));}
        items.push(ListItem::new(Line::from(vec![Span::styled(format!("{} ✨ new session",if self.sessions.len()==self.pick_sel{"→"}else{" "}), Style::new().fg(Color::Green))])));
        items.push(ListItem::new(Line::from(vec![Span::styled(format!("{} 📁 projects",if self.sessions.len()+1==self.pick_sel{"→"}else{" "}), Style::new().fg(Color::Blue))])));
        f.render_widget(List::new(items).block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Sessions ").title_style(Style::new().fg(Color::Cyan))).highlight_style(Style::new().bg(Color::DarkGray)),a);
    }
    fn render_projects(&self,f:&mut Frame,size:Rect){
        let a=Rect::new(2,2,size.width.saturating_sub(4),size.height.saturating_sub(8)); f.render_widget(Clear,a);
        let mut items:Vec<ListItem>=Vec::new();
        for(i,p)in self.projects.iter().enumerate(){let m=if i==self.pick_sel{"→"}else{" "};items.push(ListItem::new(Line::from(vec![Span::raw(format!("{m} 📁 {}",p.name))])));}
        if self.projects.is_empty(){items.push(ListItem::new("(no projects)"));}
        items.push(ListItem::new(Line::from(vec![Span::styled(format!("{} 💬 back to sessions",if self.projects.len()==self.pick_sel{"→"}else{" "}), Style::new().fg(Color::Cyan))])));
        f.render_widget(List::new(items).block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(" Projects ").title_style(Style::new().fg(Color::Blue))).highlight_style(Style::new().bg(Color::DarkGray)),a);
    }

    async fn handle(&mut self, ev:Event) -> bool {
        match ev{Event::Key(k) if k.kind==KeyEventKind::Press=>{match self.page{
            Page::SessionPicker=>match k.code{
                KeyCode::Esc=>{self.page=Page::Chat;self.pick_sel=0;}
                KeyCode::Up|KeyCode::Char('k')=>{self.pick_sel=self.pick_sel.saturating_sub(1);}
                KeyCode::Down|KeyCode::Char('j')=>{self.pick_sel=self.pick_sel.saturating_add(1);}
                KeyCode::Enter=>{
                    if self.pick_sel<self.sessions.len(){let id=self.sessions[self.pick_sel].id.clone();self.open_session(&id).await;}
                    else if self.pick_sel==self.sessions.len(){self.create_new_session().await;}
                    else{let r=self.client.list_projects_raw().await;self.projects=serde_json::from_str(&r).unwrap_or_default();self.pick_sel=0;self.page=Page::ProjectPicker;}
                }
                KeyCode::Char('n')=>{self.create_new_session().await;}
                _=>{}
            },
            Page::ProjectPicker=>match k.code{
                KeyCode::Esc=>{self.page=Page::SessionPicker;self.pick_sel=self.sessions.len();}
                KeyCode::Up|KeyCode::Char('k')=>{self.pick_sel=self.pick_sel.saturating_sub(1);}
                KeyCode::Down|KeyCode::Char('j')=>{self.pick_sel=self.pick_sel.saturating_add(1);}
                KeyCode::Enter=>{
                    if self.pick_sel<self.projects.len(){self.pid=Some(self.projects[self.pick_sel].id.clone());self.page=Page::SessionPicker;self.pick_sel=0;let r=self.client.list_sessions_raw().await;self.sessions=serde_json::from_str(&r).unwrap_or_default();}
                    else{self.page=Page::SessionPicker;self.pick_sel=self.sessions.len();}
                }
                _=>{}
            },
            Page::Chat=>match k.code{
                KeyCode::Enter=>{let m=self.input.trim().to_string();if!m.is_empty(){self.input.clear();self.send(m).await;}}
                KeyCode::Char('q')=>return false,
                KeyCode::Char('y')|KeyCode::Char('Y')=>{self.approve(true).await;}
                KeyCode::Char('n')|KeyCode::Char('N')=>{self.approve(false).await;}
                KeyCode::Char(c)=>self.input.push(c),
                KeyCode::Backspace=>{self.input.pop();}
                KeyCode::Up=>self.scroll=self.scroll.saturating_sub(1),
                KeyCode::Down=>self.scroll=self.scroll.saturating_add(1).min(self.msgs.len().saturating_sub(1)),
                KeyCode::PageUp=>self.scroll=self.scroll.saturating_sub(5),
                KeyCode::PageDown=>self.scroll=self.scroll.saturating_add(5).min(self.msgs.len().saturating_sub(1)),
                KeyCode::Tab=>{let r=self.client.list_sessions_raw().await;self.sessions=serde_json::from_str(&r).unwrap_or_default();self.pick_sel=0;self.page=Page::SessionPicker;}
                _=>{}
            },
        }}_=>{}}
        true
    }

    async fn open_session(&mut self, sid:&str){
        self.sid=Some(sid.into());self.cursor=0;self.first=false;self.msgs.clear();self.scroll=0;self.page=Page::Chat;
        let msgs:String=self.client.get_msgs_raw(sid,0).await;
        if let Ok(parsed)=serde_json::from_str::<Vec<MessageDto>>(&msgs){
            for m in &parsed{if m.cursor>self.cursor{self.cursor=m.cursor;}if m.role=="assistant"{let(t,_)=parse_tool_calls(&m.text);if!t.is_empty(){self.add("agent",&t);}}}
        }
        self.add("system",&format!("resumed: {}",fmt_sid(sid)));
    }

    async fn create_new_session(&mut self){
        let r=self.client.create_session_raw(self.pid.as_deref()).await;
        if let Ok(s)=serde_json::from_str::<SessionDto>(&r){self.sid=Some(s.id);self.cursor=0;self.first=true;self.msgs.clear();self.scroll=0;self.page=Page::Chat;self.add("system","new session");}
        else{self.add("system",&format!("create failed: {r}"));}
    }

    async fn send(&mut self, raw:String){
        // slash commands
        if raw=="/quit"||raw=="/exit"||raw=="/q"{std::process::exit(0)}
        if raw=="/help"||raw=="/h"{self.add("system","Enter=send ↑↓=scroll Tab=sessions y/n=tools q=quit");return;}
        if raw=="/sessions"||raw=="/session"{let r=self.client.list_sessions_raw().await;self.sessions=serde_json::from_str(&r).unwrap_or_default();self.pick_sel=0;self.page=Page::SessionPicker;return;}
        if raw=="/new"{self.create_new_session().await;return;}

        self.add("user",&raw);self.busy=true;

        // ensure session
        if self.sid.is_none(){
            let r=self.client.create_session_raw(self.pid.as_deref()).await;
            if let Ok(s)=serde_json::from_str::<SessionDto>(&r){self.sid=Some(s.id);self.cursor=0;self.first=true;}
            else{self.add("system",&format!("session: {r}"));self.busy=false;return;}
        }
        let sid=self.sid.as_ref().unwrap().clone();

        let msg=if self.first{
            self.first=false;
            let n=raw.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
            let n=if n.len()>40{format!("{}...",&n[..37])}else{n};
            let _=self.client.rename_raw(&sid,&n).await;
            format!("{}\n\n---\n{}",PROTOCOL,raw)
        }else{raw};

        if let Err(e)=self.client.send_msg_raw(&sid,&msg).await{self.add("system",&format!("send: {e}"));self.busy=false;return;}

        // wait for completion
        loop{
            let r=self.client.get_session_raw(&sid).await;
            if let Ok(s)=serde_json::from_str::<SessionDto>(&r){if!s.running{break;}}
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // read new messages
        let r=self.client.get_msgs_raw(&sid,self.cursor).await;
        if let Ok(parsed)=serde_json::from_str::<Vec<MessageDto>>(&r){
            for m in &parsed{
                if m.cursor>self.cursor{self.cursor=m.cursor;}
                if m.role=="assistant"{let(t,tools)=parse_tool_calls(&m.text);if!t.is_empty(){self.add("agent",&t);}for j in tools{self.add_tool(&j);}}
            }
        }else{self.add("system",&format!("msg read failed: {r}"));}
        self.busy=false;
    }

    async fn approve(&mut self, yes:bool){
        let idx=self.msgs.iter().rposition(|m|m.tool_json.is_some()&&!m.approved);
        let Some(i)=idx else{return};

        let json=self.msgs[i].tool_json.take().unwrap_or_default();
        self.msgs[i].approved=true;

        let result=if yes{execute_tool(&json)}else{"rejected by user".into()};
        self.add("result",&format!("↳ {result}"));

        // send back
        if let Some(sid)=&self.sid.clone(){
            self.busy=true;
            let _=self.client.send_msg_raw(&sid,&format!("[Tool result]\n{result}")).await;
            loop{
                let r=self.client.get_session_raw(&sid).await;
                if let Ok(s)=serde_json::from_str::<SessionDto>(&r){if!s.running{break;}}
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let r=self.client.get_msgs_raw(&sid,self.cursor).await;
            if let Ok(parsed)=serde_json::from_str::<Vec<MessageDto>>(&r){
                for m in &parsed{if m.cursor>self.cursor{self.cursor=m.cursor;}if m.role=="assistant"{let(t,tools)=parse_tool_calls(&m.text);if!t.is_empty(){self.add("agent",&t);}for j in tools{self.add_tool(&j);}}}
            }
            self.busy=false;
        }
    }
}

// ── Main ──

#[derive(Parser)]
#[command(name="attacca",version,about="Attacca CLI")]
struct Cli {
    #[arg(short='P',long,env="ATTACCA_PROJECT")] project:Option<String>,
    #[arg(short='S',long,env="ATTACCA_SESSION")] session:Option<String>,
}

#[tokio::main]
async fn main() {
    let _=dotenvy::dotenv();
    let cli=Cli::parse();
    let client=match ApiClient::from_env(){Ok(c)=>c,Err(e)=>{eprintln!("✖ {e}");std::process::exit(1);}};

    // connection test
    match client.test_conn().await {
        Ok(body)=>{
            if let Ok(v)=serde_json::from_str::<Value>(&body){
                let name=v["display_name"].as_str().unwrap_or("?");
                let email=v["email"].as_str().unwrap_or("?");
                eprintln!("✓ Connected as {name} ({email})");
            }else{eprintln!("✓ Connected (auth OK)");}
        }
        Err(e)=>{eprintln!("✖ API connection failed: {e}");
            eprintln!("  Check: ATTACCA_API_KEY and ATTACCA_API_URL");
            eprintln!("  Default URL: {}",client.base_url);
            std::process::exit(1);
        }
    }

    terminal::enable_raw_mode().ok();
    let mut stdout=io::stdout();
    crossterm::execute!(stdout,EnterAlternateScreen,crossterm::event::EnableMouseCapture).ok();
    let mut term=Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)).unwrap();

    let mut app=App::new(client);
    if let Some(sid)=&cli.session{app.open_session(sid).await;}
    else{app.add("system","Tab: sessions  Enter: send");}

    loop{
        let _=term.draw(|f|app.render(f));
        if event::poll(Duration::from_millis(100)).ok().unwrap_or(false){
            if let Ok(ev)=event::read(){if!app.handle(ev).await{break;}}
        }
    }

    terminal::disable_raw_mode().ok();
    crossterm::execute!(io::stdout(),LeaveAlternateScreen,crossterm::event::DisableMouseCapture).ok();
}
