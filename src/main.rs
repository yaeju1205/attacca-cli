use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
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

#[derive(Deserialize, Default, Clone)]
struct SessionDto { id:String, #[serde(default)] title:String, #[serde(default)] status:String, #[serde(default)] running:bool, #[serde(default)] created_at:String, #[serde(default)] updated_at:String }
#[derive(Deserialize, Clone)] struct MessageDto { #[serde(default)] role:String, #[serde(default)] text:String, #[serde(default)] cursor:i64 }
#[derive(Deserialize, Default)] struct MeDto { #[serde(default)] display_name:String, #[serde(default)] email:String }
#[derive(Deserialize, Clone)] struct ProjectDto { id:String, name:String, #[serde(default)] is_default:bool }
#[derive(Deserialize, Clone)] struct AgentDto { id:String, name:String }

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
    async fn list_sessions(&self) -> Result<Vec<SessionDto>, String> { api_call(self.inner.get(self.url("/v1/sessions")).headers(self.bearer())).await }
    async fn list_projects(&self) -> Result<Vec<ProjectDto>, String> { api_call(self.inner.get(self.url("/v1/projects")).headers(self.bearer())).await }
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
    async fn get_msgs_after(&self, sid:&str, after:i64) -> Result<Vec<MessageDto>, String> {
        api_call(self.inner.get(self.url(&format!("/v1/sessions/{sid}/messages?after={after}"))).headers(self.bearer())).await
            .map(|mut m:Vec<MessageDto>| { m.reverse(); m })
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
    let empty=String::new();
    let a=|k| v["args"][k].as_str().unwrap_or(&empty);
    match tool {
        "read_file" => match std::fs::read_to_string(a("path")) {
            Ok(s) if s.len()>50000 => format!("[{} bytes, first 50k]\n{}", s.len(), &s[..50000]),
            Ok(s) => format!("[{} bytes]\n{}", s.len(), s),
            Err(e) => format!("[error: {e}]"),
        },
        "write_file" => match std::fs::write(a("path"), a("content")) { Ok(()) => "[OK] wrote".into(), Err(e)=>format!("[error: {e}]") },
        "edit_file" => {
            let (p,o,n)=(a("path"),a("old_string"),a("new_string"));
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
            Ok(o) => { let mut r=String::new(); let so=String::from_utf8_lossy(&o.stdout); let se=String::from_utf8_lossy(&o.stderr); if !so.is_empty() { r.push_str(&format!("[out]\n{so}\n")); } if !se.is_empty() { r.push_str(&format!("[err]\n{se}\n")); } r.push_str(&format!("[exit:{}]", o.status.code().unwrap_or(-1))); r }
            Err(e) => format!("[error: {e}]"),
        },
        "file_exists" => if std::path::Path::new(a("path")).exists() { "[true] exists".into() } else { "[false] not found".into() },
        "create_dir" => match std::fs::create_dir_all(a("path")) { Ok(())=> "[OK] created".into(), Err(e)=>format!("[error: {e}]") },
        "delete_file" => match std::fs::remove_file(a("path")).or_else(|_|std::fs::remove_dir(a("path"))) { Ok(())=> "[OK] deleted".into(), Err(e)=>format!("[error: {e}]") },
        _ => format!("[unknown: {tool}]"),
    }
}

fn is_dangerous(j:&str) -> bool {
    let v:Value=serde_json::from_str(j).unwrap_or_default();
    if v["tool"].as_str()==Some("run_command") {
        let c=v["args"]["command"].as_str().unwrap_or(""); c.contains("rm ")||c.contains("sudo ")||c.contains("dd ")||c.contains("mkfs")||c.contains('>')
    } else { false }
}

// ── Chat message ──

#[derive(Clone)]
struct ChatMsg { role:String, text:String, tool_json:Option<String>, is_danger:bool, approved:bool, rejected:bool }

// ── TUI App ──

struct TuiApp {
    client: ApiClient,
    session_id: Option<String>,
    cursor: i64,
    project_id: Option<String>,
    agent_id: Option<String>,
    first_turn: bool,
    chat_msgs: Vec<ChatMsg>,
    input: String,
    status: String,
    mode: Mode,
    scroll: usize,
    input_mode: bool,
    modal_idx: usize,
}

#[derive(PartialEq)]
enum Mode { Chat, Picker, Exit }

impl TuiApp {
    fn new(client:ApiClient) -> Self { Self { client, session_id:None, cursor:0, project_id:None, agent_id:None, first_turn:true, chat_msgs:Vec::new(), input:String::new(), status:"시작".into(), mode:Mode::Chat, scroll:0, input_mode:true, modal_idx:0 } }

    fn add(&mut self, r:&str, t:&str) { self.chat_msgs.push(ChatMsg { role:r.into(), text:t.into(), tool_json:None, is_danger:false, approved:false, rejected:false }); self.scroll=self.chat_msgs.len().saturating_sub(1); }
    fn add_tool(&mut self, j:&str) {
        let v:Value=serde_json::from_str(j).unwrap_or_default();
        let t=v["tool"].as_str().unwrap_or("?");
        let a=v.get("args").and_then(|a|a.as_object()).map(|o| o.iter().filter_map(|(k,vv)| Some(format!("{}={}",k,vv.as_str()?))).collect::<Vec<_>>().join(" ")).unwrap_or_default();
        self.chat_msgs.push(ChatMsg { role:"tool".into(), text:format!("🔧 {t} {a}"), tool_json:Some(j.into()), is_danger:is_dangerous(j), approved:false, rejected:false });
        self.scroll=self.chat_msgs.len().saturating_sub(1);
    }

    fn render(&self, f:&mut Frame) {
        let size=f.area();
        if size.width<40||size.height<10 { f.render_widget(Paragraph::new("too small").centered().red(), size); return; }

        let c=Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1),Constraint::Min(3),Constraint::Length(3)]).split(size);

        // status
        let st=if let Some(sid)=&self.session_id { let s=if sid.len()>8{&sid[..8]}else{sid.as_str()}; format!(" Attacca CLI  │ {s}  │ {} msgs  │ {}", self.chat_msgs.len(), self.status) } else { " Attacca CLI  │ /sessions 로 시작".into() };
        f.render_widget(Paragraph::new(st).style(Style::new().fg(Color::White).bg(Color::DarkGray)), c[0]);

        // chat
        let mut lines:Vec<Line>=Vec::new();
        for m in &self.chat_msgs {
            match m.role.as_str() {
                "user" => { lines.push(Line::from(vec![Span::styled(" You", Style::new().fg(Color::Green).bold())])); for l in m.text.lines() { lines.push(Line::from(Span::raw(format!(" │ {l}")))); } }
                "agent" => { lines.push(Line::from(vec![Span::styled(" Agent", Style::new().fg(Color::Cyan).bold())])); for l in m.text.lines() { lines.push(Line::from(Span::raw(format!(" │ {l}")))); } }
                "tool" if m.approved || m.rejected => { lines.push(Line::from(vec![Span::styled(&m.text, if m.rejected { Style::new().fg(Color::DarkGray) } else { Style::new().fg(Color::Green) })])); }
                "tool" => { lines.push(Line::from(vec![Span::styled(&m.text, if m.is_danger { Style::new().fg(Color::Red).bold() } else { Style::new().fg(Color::Yellow) })])); }
                _ => {}
            }
        }
        if lines.is_empty() { lines.push(Line::from(Span::styled(" 메시지를 입력하세요", Style::new().fg(Color::DarkGray).italic()))); }

        let off=self.scroll.saturating_sub(10).min(self.chat_msgs.len().saturating_sub(10));
        f.render_widget(
            Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(Color::DarkGray))).scroll((off as u16,0)),
            c[1]);

        // input
        let inp=if self.input.is_empty() { vec![Span::styled("입력...",Style::new().fg(Color::DarkGray).italic())] } else { vec![Span::raw(&self.input)] };
        f.render_widget(Paragraph::new(Text::from(Line::from(inp))).block(Block::default().borders(Borders::TOP).border_style(Style::new().fg(Color::DarkGray)).title(" 입력 ").title_style(Style::new().fg(Color::Green))), c[2]);

        // modal
        if self.mode==Mode::Picker {
            let (x,y,w,h)=(size.width/6,size.height/4,size.width*2/3,size.height/2);
            let a=Rect::new(x,y,w.max(40),h.max(10));
            f.render_widget(Clear, a);
            f.render_widget(Paragraph::new("세션/프로젝트 선택 (Tab=목록, Esc=닫기)").block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).style(Style::new().bg(Color::Black))).centered(), a);
        }
    }

    async fn handle(&mut self, ev:Event) -> bool {
        match ev {
            Event::Key(k) if k.kind==KeyEventKind::Press => {
                if self.mode==Mode::Exit { return false; }
                if self.mode==Mode::Picker { match k.code { KeyCode::Esc=>self.mode=Mode::Chat, _=>{} } return true; }
                match k.code {
                    KeyCode::Esc => self.input_mode=false,
                    KeyCode::Enter if self.input_mode => { let msg=self.input.trim().to_string(); self.input.clear(); if !msg.is_empty() { self.handle_input(msg).await; } }
                    KeyCode::Char(c) if self.input_mode => self.input.push(c),
                    KeyCode::Backspace if self.input_mode => { self.input.pop(); }
                    KeyCode::Up => self.scroll=self.scroll.saturating_sub(1),
                    KeyCode::Down => self.scroll=self.scroll.saturating_add(1).min(self.chat_msgs.len().saturating_sub(1)),
                    KeyCode::PageUp => self.scroll=self.scroll.saturating_sub(5),
                    KeyCode::PageDown => self.scroll=self.scroll.saturating_add(5).min(self.chat_msgs.len().saturating_sub(1)),
                    KeyCode::Tab => self.mode=Mode::Picker,
                    KeyCode::Char('q') if !self.input_mode => return false,
                    KeyCode::Char(c) if !self.input_mode => { self.input_mode=true; self.input.push(c); }
                    _ if !self.input_mode => self.input_mode=true,
                    _ => {}
                }
            }
            Event::Mouse(m) => {
                if let MouseEventKind::Down(MouseButton::Left)=m.kind {
                    // Check for tool approve click - simplified: press 'y'/'n'
                }
            }
            _ => {}
        }
        true
    }

    async fn handle_input(&mut self, raw:String) {
        match raw.as_str() {
            "/quit"|"/exit"|"/q" => self.mode=Mode::Exit,
            "/help"|"/h" => self.add("system","Enter:전송  ↑↓:스크롤  Tab:세션  Esc:일반  q:종료  y/n:툴승인"),
            "/sessions"|"/session" => self.mode=Mode::Picker,
            "/new" => {
                if let Ok(s)=self.client.create_session(self.project_id.as_deref(),self.agent_id.as_deref()).await {
                    let sid=s.id.clone();
                    self.session_id=Some(sid.clone()); self.cursor=0; self.first_turn=true; self.chat_msgs.clear(); self.scroll=0;
                    self.add("system",&format!("새 세션: {sid}"));
                }
            }
            _ => {
                self.add("user",&raw);
                if self.session_id.is_none() {
                    if let Ok(s)=self.client.create_session(self.project_id.as_deref(),self.agent_id.as_deref()).await {
                        self.session_id=Some(s.id.clone()); self.cursor=0; self.first_turn=true;
                        self.status="세션 생성됨".into();
                    } else { self.add("system","세션 생성 실패"); return; }
                }
                let sid=self.session_id.as_ref().unwrap().clone();
                let msg=if self.first_turn { self.first_turn=false; let n=self.input_cap(&raw,40); let _=self.client.rename_session(&sid,&n).await; format!("{}\n\n---\n{}",PROTOCOL,raw) } else { raw };
                self.status="전송...".into();
                if let Err(e)=self.client.send_message(&sid,&msg).await { self.add("system",&format!("실패: {e}")); return; }
                loop { let s=self.client.get_session(&sid).await.unwrap_or_default(); if !s.running { break; } tokio::time::sleep(Duration::from_millis(500)).await; }
                self.status="응답...".into();
                if let Ok(msgs)=self.client.get_msgs_after(&sid,self.cursor).await {
                    for m in &msgs { if m.cursor>self.cursor { self.cursor=m.cursor; } if m.role=="assistant" { let (t,tools)=parse_tool_calls(&m.text); if !t.is_empty(){self.add("agent",&t);} for j in tools {self.add_tool(&j);} } }
                }
                self.status="완료".into();
            }
        }
    }

    fn input_cap(&self, s:&str, m:usize) -> String {
        let w=s.split_whitespace().take(5).collect::<Vec<_>>().join(" ");
        if w.len()>m { format!("{}...", &w[..m.saturating_sub(3)]) } else { w }
    }

    async fn process_tools(&mut self) {}
    async fn approve_last_tool(&mut self, yes:bool) {
        let sid=match &self.session_id { Some(s)=>s.clone(), None=>return };
        // Find last unprocessed tool
        let target=self.chat_msgs.iter().rposition(|m| m.role=="tool" && !m.approved && !m.rejected);
        let Some(idx)=target else { return };
        self.chat_msgs[idx].approved=yes;
        if !yes { self.chat_msgs[idx].rejected=true; }
        let result=if yes { self.chat_msgs[idx].tool_json.as_ref().map(|j| execute_tool(j)).unwrap_or_default() } else { "rejected".into() };
        self.status="결과 전송...".into();
        if let Err(e)=self.client.send_message(&sid,&format!("[Tool result]\n{result}")).await { self.add("system",&format!("실패: {e}")); return; }
        loop { let s=self.client.get_session(&sid).await.unwrap_or_default(); if !s.running { break; } tokio::time::sleep(Duration::from_millis(500)).await; }
        if let Ok(msgs)=self.client.get_msgs_after(&sid,self.cursor).await {
            for m2 in &msgs { if m2.cursor>self.cursor { self.cursor=m2.cursor; } if m2.role=="assistant" { let(t,tools)=parse_tool_calls(&m2.text); if !t.is_empty(){self.add("agent",&t);} for j in tools {self.add_tool(&j);} } }
        }
        self.status="완료".into();
    }
}

// ── Main ──

#[derive(Parser)]
#[command(name="attacca",version,about="Attacca CLI — TUI bridge")]
struct Cli {
    #[arg(short='P',long,env="ATTACCA_PROJECT")] project:Option<String>,
    #[arg(short='S',long,env="ATTACCA_SESSION")] session:Option<String>,
    #[arg(short='A',long,env="ATTACCA_AGENT")] agent:Option<String>,
}

#[tokio::main]
async fn main() {
    let _=dotenvy::dotenv();
    let cli=Cli::parse();
    let client=match ApiClient::from_env() { Ok(c)=>c, Err(e)=>{ eprintln!("✖ {e}"); std::process::exit(1); } };

    terminal::enable_raw_mode().ok();
    let mut stdout=io::stdout();
    crossterm::execute!(stdout,EnterAlternateScreen,crossterm::event::EnableMouseCapture).ok();
    let mut term=Terminal::new(ratatui::backend::CrosstermBackend::new(stdout)).unwrap();

    let mut app=TuiApp::new(client);
    // auto-create session
    if let Some(sid)=&cli.session {
        app.session_id=Some(sid.clone()); app.cursor=0;
        app.add("system",&format!("세션 재개: {sid}"));
    } else if let Ok(s)=app.client.create_session(None,None).await {
        app.session_id=Some(s.id.clone());
        app.add("system",&format!("세션 생성됨"));
    } else { app.add("system","세션 생성 실패"); }

    loop {
        let _=term.draw(|f| app.render(f));
        app.process_tools().await;

        if event::poll(Duration::from_millis(100)).ok().unwrap_or(false) {
            if let Ok(ev)=event::read() {
                // Check for y/n keypress for tool approval
                if let Event::Key(k)=&ev { if k.kind==KeyEventKind::Press {
                    match k.code {
                        KeyCode::Char('y')|KeyCode::Char('Y') => { app.approve_last_tool(true).await; continue; }
                        KeyCode::Char('n')|KeyCode::Char('N') => { app.approve_last_tool(false).await; continue; }
                        _ => {}
                    }
                }}
                if !app.handle(ev).await { break; }
            }
        }
    }

    terminal::disable_raw_mode().ok();
    crossterm::execute!(io::stdout(),LeaveAlternateScreen,crossterm::event::DisableMouseCapture).ok();
    println!("Bye!");
}
