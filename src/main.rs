#![allow(dead_code)]
use clap::Parser;
use colored::*;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use rustyline::completion::{Completer, Pair};
use rustyline::config::{CompletionType, Config};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::{self, Validator};
use rustyline::history::DefaultHistory;
use rustyline::{Context, Helper};
use rustyline::{Editor, Result as RlResult};
use std::collections::HashMap;

const PROTOCOL: &str = r#"## attacca-cli bridge protocol

You are connected to the user's **local computer** through attacca-cli. To
interact with it, output JSON tool calls inside ```attacca-tool blocks.
You can output MULTIPLE tool calls in the same response -- they run in
sequence, each getting its result sent back.

Example:
```attacca-tool
{"tool": "read_file", "args": {"path": "/home/user/hello.txt"} }
```

### Available tools

| Tool | Args | Description |
|------|------|-------------|
| read_file | path | Read a text file |
| write_file | path, content | Write a new file or overwrite |
| edit_file | path, old_string, new_string | Find-and-replace in a file |
| list_dir | path | List a directory |
| run_command | command | Run ANY shell command (use for grep, find, cat, ls, mkdir, cp, mv, wc, diff, sort, head, tail, sed, awk, etc.) |
| create_dir | path | Create a directory |
| file_exists | path | Check a file exists |
| delete_file | path | Delete a file or empty dir |
| read_files | paths[] | Batch read MULTIPLE files at once |
| batch_read | paths[] | Alias for read_files |

### Rules
1. Plan ahead: read ALL needed files first before writing anything.
2. Use run_command with grep, find, glob patterns for searching.
3. Use read_files to batch-read many files at once.
4. After getting results, continue your reasoning directly.
5. Do NOT invent file contents -- always read them first.
6. Do NOT explain what you would do -- actually do it with tools."#;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct SessionDto {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
}

#[derive(Deserialize)]
struct MessageDto {
    #[serde(default)]
    role: MessageRole,
    #[serde(default)]
    text: String,
    #[serde(default)]
    cursor: i64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum MessageRole {
    #[default]
    User,
    Assistant,
    System,
    Tool,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize, Default)]
struct MeDto {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Deserialize, Default, Debug)]
struct AgentDto {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize, Default, Debug, Clone)]
struct ProjectDto {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    is_default: bool,
}

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

async fn json_or_body<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T, String> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("read body: {e}"))?;
    match serde_json::from_str::<T>(&body) {
        Ok(val) => Ok(val),
        Err(e) => {
            let preview = if body.len() > 300 { format!("{}...", &body[..300]) } else { body.clone() };
            Err(format!("Status: {status}\nJSON decode: {e}\nRaw: {preview}"))
        }
    }
}

async fn api_call<T: serde::de::DeserializeOwned>(req: reqwest::RequestBuilder) -> Result<T, String> {
    let resp = req.send().await.map_err(|e| format!("request failed: {e}"))?;
    let status = resp.status();
    if status.is_success() { json_or_body::<T>(resp).await }
    else {
        let body = resp.text().await.unwrap_or_default();
        let preview = if body.len() > 300 { format!("{}...", &body[..300]) } else { body };
        Err(format!("HTTP {status}\nResponse: {preview}"))
    }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "attacca", version, about = "Chat with Attacca agents -- local bridge mode")]
struct Cli {
    #[arg(help = "Message to send (one-shot mode)")]
    message: Option<String>,

    #[arg(short = 'P', long, env = "ATTACCA_PROJECT", help = "Project name or UUID")]
    project: Option<String>,

    #[arg(short = 'S', long, env = "ATTACCA_SESSION", help = "Session UUID (resume existing)")]
    session: Option<String>,

    #[arg(short = 'A', long, env = "ATTACCA_AGENT", help = "Agent UUID")]
    agent: Option<String>,

    #[arg(long, help = "Show debug info (URLs, responses)")]
    debug: bool,
}

// ---------------------------------------------------------------------------
// Tool model
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ToolCall { name: String, args: HashMap<String, String> }

fn parse_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    let mut tools = Vec::new();
    let mut clean = text.to_string();
    loop {
        let start = match clean.find("```attacca-tool") { Some(i) => i, None => break };
        let cs = start + "```attacca-tool".len();
        let end = match clean[cs..].find("```") { Some(i) => cs + i, None => break };
        let json_str = clean[cs..end].trim();
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            if let (Some(name), Some(args_obj)) = (v["tool"].as_str(), v.get("args").and_then(|a| a.as_object())) {
                let args = args_obj.iter().map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string())).collect();
                tools.push(ToolCall { name: name.to_string(), args });
            }
        }
        let be = end + "```".len();
        clean.replace_range(start..be, "");
    }
    (clean.trim().to_string(), tools)
}

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

fn execute_tool(tc: &ToolCall) -> String {
    let empty = String::new();
    match tc.name.as_str() {
        "read_file" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::read_to_string(path) {
                Ok(s) if s.len() > 100_000 => format!("[file too large: {} bytes, first 100k]\n{}", s.len(), &s[..100_000]),
                Ok(s) => format!("[file content ({} bytes)]:\n{}", s.len(), s),
                Err(e) => format!("[error: {e}]"),
            }
        }
        "write_file" => {
            let (path, content) = (tc.args.get("path").unwrap_or(&empty), tc.args.get("content").unwrap_or(&empty));
            match std::fs::write(path, content) { Ok(()) => format!("[OK] wrote {} bytes", content.len()), Err(e) => format!("[error: {e}]") }
        }
        "edit_file" => {
            let (path, old, new) = (tc.args.get("path").unwrap_or(&empty), tc.args.get("old_string").unwrap_or(&empty), tc.args.get("new_string").unwrap_or(&empty));
            match std::fs::read_to_string(path) {
                Ok(c) if c.contains(old.as_str()) => {
                    let nc = c.replace(old.as_str(), new.as_str());
                    let count = c.matches(old.as_str()).count();
                    match std::fs::write(path, &nc) { Ok(()) => format!("[OK] {} replacements in {}", count, path), Err(e) => format!("[error: {e}]") }
                }
                Ok(_) => format!("[error] not found in {}", path),
                Err(e) => format!("[error: {e}]"),
            }
        }
        "list_dir" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut items: Vec<String> = entries.flatten().map(|e| {
                        format!("{} {}", if e.file_type().map(|t| t.is_dir()).unwrap_or(false) { "📁" } else { "📄" }, e.file_name().to_string_lossy())
                    }).collect();
                    items.sort();
                    format!("[{} items]\n{}", items.len(), items.join("\n"))
                }
                Err(e) => format!("[error: {e}]"),
            }
        }
        "run_command" => {
            let cmd = tc.args.get("command").unwrap_or(&empty);
            match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
                Ok(o) => {
                    let mut r = String::new();
                    let so = String::from_utf8_lossy(&o.stdout);
                    let se = String::from_utf8_lossy(&o.stderr);
                    if !so.is_empty() { r.push_str(&format!("[out]:\n{so}\n")); }
                    if !se.is_empty() { r.push_str(&format!("[err]:\n{se}\n")); }
                    r.push_str(&format!("[exit: {}]", o.status.code().unwrap_or(-1)));
                    r
                }
                Err(e) => format!("[error: {e}]"),
            }
        }
        "create_dir" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::create_dir_all(path) { Ok(()) => format!("[OK] created"), Err(e) => format!("[error: {e}]") }
        }
        "file_exists" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            if std::path::Path::new(path).exists() { format!("[true] exists") } else { format!("[false] not found") }
        }
        "delete_file" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path)) { Ok(()) => format!("[OK] deleted"), Err(e) => format!("[error: {e}]") }
        }
        "read_files" => {
            let ps = tc.args.get("paths").unwrap_or(&empty);
            let paths: Vec<String> = if ps.starts_with('[') { serde_json::from_str(ps).unwrap_or(vec![ps.clone()]) } else { ps.split(',').map(|s| s.trim().to_string()).collect() };
            let mut r = Vec::new();
            for p in &paths { match std::fs::read_to_string(p) { Ok(s) => r.push(format!("--- {} ---\n{}", p, s)), Err(e) => r.push(format!("--- {} ---\n[error: {e}]", p)) } }
            r.join("\n")
        }
        other => format!("[unknown: {other}]"),
    }
}

fn is_dangerous(tc: &ToolCall) -> bool {
    if tc.name == "run_command" {
        let cmd = tc.args.get("command").map(|s| s.as_str()).unwrap_or("");
        cmd.contains("rm ") || cmd.contains("sudo ") || cmd.contains("dd ") || cmd.contains("mkfs") || cmd.contains('>') || (cmd.contains('|') && cmd.contains("rm"))
    } else { false }
}

fn format_tool(tc: &ToolCall) -> String {
    let args: Vec<String> = tc.args.iter().map(|(k, v)| {
        if v.len() > 60 { format!("{}: \"{}...\"", k, &v[..60]) } else { format!("{}: \"{}\"", k, v) }
    }).collect();
    format!("{}({})", tc.name, args.join(", "))
}

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

const DEFAULT_API_URL: &str = "https://attacca.cc/api/v1";

struct ApiClient { inner: Client, key: String, base_url: String, debug: bool }

impl ApiClient {
    fn from_env(debug: bool) -> Result<Self, String> {
        let key = std::env::var("ATTACCA_API_KEY").map_err(|_| "Set ATTACCA_API_KEY (or .env)\n  Get at https://attacca.cc/settings/api-keys".to_string())?;
        let base_url = std::env::var("ATTACCA_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string());
        let inner = Client::builder().user_agent("attacca-cli/0.1.0").build().map_err(|e| format!("reqwest: {e}"))?;
        Ok(Self { inner, key, base_url, debug })
    }

    fn log(&self, msg: &str) {
        if self.debug { eprintln!("{} {}", "🔍".bright_black(), msg.bright_black()); }
    }

    fn bearer(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.key).parse().unwrap());
        h.insert(reqwest::header::ACCEPT, "application/json".parse().unwrap());
        h
    }

    fn url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        let p = if base.contains("/api/v1") && path.starts_with("/v1/") { &path[3..] } else { path };
        format!("{}/{}", base, p.trim_start_matches('/'))
    }

    async fn get_me(&self) -> Result<MeDto, String> {
        let url = self.url("/v1/me");
        self.log(&format!("GET {url}"));
        api_call(self.inner.get(&url).headers(self.bearer())).await
    }
    async fn list_agents(&self) -> Result<Vec<AgentDto>, String> {
        let url = self.url("/v1/agents");
        self.log(&format!("GET {url}"));
        api_call(self.inner.get(&url).headers(self.bearer())).await
    }
    async fn list_projects(&self) -> Result<Vec<ProjectDto>, String> {
        let url = self.url("/v1/projects");
        self.log(&format!("GET {url}"));
        api_call(self.inner.get(&url).headers(self.bearer())).await
    }
    async fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<SessionDto>, String> {
        let url = self.url("/v1/sessions");
        self.log(&format!("GET {url}"));
        let mut req = self.inner.get(&url).headers(self.bearer());
        if let Some(pid) = project_id { req = req.query(&[("project_id", pid)]); }
        api_call(req).await
    }

    async fn create_session(&self, project_id: Option<&str>, agent_id: Option<&str>) -> Result<SessionDto, String> {
        let url = self.url("/v1/sessions");
        self.log(&format!("POST {url}"));
        let mut body = serde_json::json!({"title": "attacca-cli"});
        if let Some(pid) = project_id { body["project_id"] = serde_json::json!(pid); }
        if let Some(aid) = agent_id { body["agent_id"] = serde_json::json!(aid); }
        api_call(self.inner.post(&url).headers(self.bearer()).json(&body)).await
    }

    async fn send_message(&self, session_id: &str, msg: &str) -> Result<(), String> {
        let url = self.url(&format!("/v1/sessions/{session_id}/messages"));
        self.log(&format!("POST {url} ({} bytes)", msg.len()));
        let body = serde_json::json!({"message": msg, "timezone": "Asia/Seoul"});
        let resp = self.inner.post(&url).headers(self.bearer()).json(&body).send().await.map_err(|e| format!("request: {e}"))?;
        let s = resp.status();
        if s.is_success() || s.as_u16() == 202 { Ok(()) }
        else { let b = resp.text().await.unwrap_or_default(); Err(format!("HTTP {s}\n{}", &b[..b.len().min(300)])) }
    }

    async fn get_session(&self, session_id: &str) -> Result<SessionDto, String> {
        let url = self.url(&format!("/v1/sessions/{session_id}"));
        self.log(&format!("GET {url}"));
        api_call(self.inner.get(&url).headers(self.bearer())).await
    }

    async fn get_messages_after(&self, session_id: &str, after: i64) -> Result<Vec<MessageDto>, String> {
        let url = self.url(&format!("/v1/sessions/{session_id}/messages?after={after}"));
        self.log(&format!("GET {url}"));
        api_call(self.inner.get(&url).headers(self.bearer())).await
            .map(|mut msgs: Vec<MessageDto>| { msgs.reverse(); msgs })
    }

    async fn wait_until_done(&self, session_id: &str) -> Result<(), String> {
        self.log(&format!("waiting for session {session_id}..."));
        loop {
            let s = self.get_session(session_id).await?;
            if !s.running { self.log("session done"); return Ok(()); }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    /// Diagnose: test all major API endpoints
    async fn diagnose(&self) {
        println!("\n{} Diagnostics:", "🔍".bright_cyan());
        println!("  API URL: {}", self.base_url);

        let tests: Vec<(&str, &str)> = vec![
            ("GET /me", "/v1/me"),
            ("GET /sessions", "/v1/sessions"),
            ("GET /projects", "/v1/projects"),
            ("GET /agents", "/v1/agents"),
        ];

        for (name, path) in &tests {
            let url = self.url(path);
            print!("  {} {:<20} ... ", name, "");
            use std::io::Write; std::io::stdout().flush().ok();
            let resp = self.inner.get(&url).headers(self.bearer()).send().await;
            match resp {
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    let preview = if body.len() > 100 { format!("{}...", &body[..100]) } else { body };
                    if status.is_success() {
                        println!("{}", "✅".bright_green());
                        if self.debug { println!("  └ {}", preview.bright_black()); }
                    } else {
                        println!("{} HTTP {}", "❌".bright_red(), status);
                        println!("  └ {}", preview);
                    }
                }
                Err(e) => println!("{} {e}", "💥".bright_red()),
            }
        }

        // Try to create a test session
        println!();
        let url = self.url("/v1/sessions");
        print!("  {} POST /sessions (create test) ... ", "🔧".bright_yellow());
        use std::io::Write; std::io::stdout().flush().ok();
        let resp = self.inner.post(&url).headers(self.bearer())
            .json(&serde_json::json!({"title": "attacca-cli-diagnose"})).send().await;
        match resp {
            Ok(r) => {
                let status = r.status();
                if status.is_success() {
                    let body: serde_json::Value = r.json().await.unwrap_or_default();
                    let sid = body["id"].as_str().unwrap_or("?");
                    println!("{} session_id={}", "✅".bright_green(), sid);
                } else {
                    let body = r.text().await.unwrap_or_default();
                    println!("{} HTTP {}", "❌".bright_red(), status);
                    println!("  └ {}", &body[..body.len().min(200)]);
                }
            }
            Err(e) => println!("{} {e}", "💥".bright_red()),
        }
    }

    async fn resolve_project(&self, project: &str) -> Result<String, String> {
        if project.len() == 36 && project.contains('-') { return Ok(project.to_string()); }
        let ps = self.list_projects().await?;
        for p in &ps { if p.name == project { return Ok(p.id.clone()); } }
        Err(format!("Project '{project}' not found. Available:\n  {}", ps.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join("\n  ")))
    }

    /// Find last cursor in a session (for resuming). Returns 0 if no messages.
    async fn get_last_cursor(&self, session_id: &str) -> Result<i64, String> {
        let msgs = self.get_messages_after(session_id, 0).await?;
        Ok(msgs.iter().map(|m| m.cursor).max().unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------
// Picker helpers
// ---------------------------------------------------------------------------

fn ask_number(max: usize) -> Option<usize> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let n = input.trim().parse::<usize>().ok()?;
    if n == 0 || n > max { None } else { Some(n - 1) }
}

fn short_id(id: &str) -> String { if id.len() > 8 { id[..8].to_string() } else { id.to_string() } }

fn fmt_time(iso: &str) -> &str {
    if iso.len() > 19 { &iso[..19] } else { iso }
}

// ---------------------------------------------------------------------------
// UI helpers
// ---------------------------------------------------------------------------

fn print_error(msg: &str) { eprintln!("✖ {}", msg.bright_red()); }
fn print_assistant(text: &str) { if text.is_empty() { return; } for l in text.lines() { println!("{} {}", "│".bright_blue(), l); } println!(); }

fn print_tool_invoke(tc: &ToolCall, is_danger: bool) {
    let icon = if is_danger { "⚠".bright_red().to_string() } else { "🔧".bright_yellow().to_string() };
    println!("{} {}", icon, format_tool(tc).bold());
}

fn ask_approve(danger: bool) -> bool {
    let default = if danger { "n" } else { "Y" };
    print!("  └ Execute? [{}/{}] ", default.to_uppercase().green(), if default == "Y" { "n".bright_red() } else { "y".bright_green() });
    use std::io::Write; std::io::stdout().flush().ok();
    let mut i = String::new(); std::io::stdin().read_line(&mut i).ok();
    let i = i.trim().to_lowercase();
    if i.is_empty() { default == "Y" } else { i == "y" }
}

// ---------------------------------------------------------------------------
// Slash command autocomplete helper
// ---------------------------------------------------------------------------

const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/help", "Show this help"),
    ("/new", "Create a new session"),
    ("/sessions", "Switch to a different session"),
    ("/diagnose", "Test API connection"),
    ("/quit", "Exit the program"),
    ("/exit", "Exit the program"),
];

struct SlashHelper;

impl Completer for SlashHelper {
    type Candidate = Pair;

    fn complete(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> RlResult<(usize, Vec<Pair>)> {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('/') {
            return Ok((pos, vec![]));
        }
        let candidates: Vec<Pair> = SLASH_COMMANDS
            .iter()
            .filter(|(cmd, _)| cmd.starts_with(trimmed))
            .map(|(cmd, desc)| Pair {
                display: format!("{} ( {})", cmd, desc),
                replacement: format!("{} ", cmd),
            })
            .collect();
        let start_pos = line.find('/').unwrap_or(pos);
        Ok((start_pos, candidates))
    }
}

impl Hinter for SlashHelper {
    type Hint = String;
    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None // skip hints for now, focus on completing
    }
}

impl Highlighter for SlashHelper {}

impl Validator for SlashHelper {
    fn validate(&self, _ctx: &mut validate::ValidationContext) -> RlResult<validate::ValidationResult> {
        Ok(validate::ValidationResult::Valid(None))
    }
}

impl Helper for SlashHelper {}

// ---------------------------------------------------------------------------
// Interactive session
// ---------------------------------------------------------------------------

type SessionInfo = (String, i64, bool); // (session_id, cursor, first_turn)

/// Pick or create a session. Returns (session_id, cursor, is_first_turn).
async fn pick_session(client: &ApiClient, project_id: Option<String>, agent_id: Option<String>) -> Result<SessionInfo, String> {
    println!();
    println!("{} Pick a session:", "💬".bright_cyan());

    let sessions = client.list_sessions(project_id.as_deref()).await.unwrap_or_default();

    // Show only non-running sessions (settled)
    let settled: Vec<&SessionDto> = sessions.iter().filter(|s| !s.running && s.status != "deleted").collect();

    if settled.is_empty() {
        println!("{}  No existing sessions — creating a new one.", "  ".bright_black());
        let s = client.create_session(project_id.as_deref(), agent_id.as_deref()).await?;
        return Ok((s.id, 0, true));
    }

    // Show 10 most recent
    let count = settled.len().min(10);
    println!("{}  Recent sessions:", "  ".bright_black());
    for (i, s) in settled[..count].iter().enumerate() {
        let title = if s.title.is_empty() { "(untitled)" } else { &s.title };
        let age = fmt_time(&s.updated_at);
        let sid = short_id(&s.id);
        println!("  {}. {} [{}] {} — {}", (i + 1).to_string().bright_green(), "💬".bright_black(), sid, title.bold(), age.bright_black());
    }
    if count < settled.len() { println!("  ... and {} older sessions", settled.len() - count); }

    println!("  {}. {} Create a new session", (count + 1).to_string().bright_green(), "✨".bright_cyan());
    print!("{}  Choose [1-{}]: ", "→".bright_green(), count + 1);
    use std::io::Write; std::io::stdout().flush().ok();

    match ask_number(count + 1) {
        Some(idx) if idx < count => {
            let sess = settled[idx];
            let cursor = client.get_last_cursor(&sess.id).await.unwrap_or(0);
            println!("{}  Resuming session {} ({})", "  ↻".bright_black(), sess.id, sess.title);
            Ok((sess.id.clone(), cursor, false))
        }
        _ => {
            let s = client.create_session(project_id.as_deref(), agent_id.as_deref()).await?;
            println!("{}  Created session {}", "  ✚".bright_green(), s.id);
            Ok((s.id, 0, true))
        }
    }
}

async fn run_interactive(client: &ApiClient, project_id: Option<String>, session_id: Option<String>, agent_id: Option<String>) -> Result<(), String> {
    let me = client.get_me().await.unwrap_or_default();
    let projects = client.list_projects().await.ok().unwrap_or_default();
    let agents = client.list_agents().await.ok().unwrap_or_default();

    let project_name = project_id.as_ref().and_then(|pid| projects.iter().find(|p| p.id == *pid).map(|p| p.name.as_str()));
    let agent_name = agent_id.as_ref().and_then(|aid| agents.iter().find(|a| a.id == *aid).map(|a| a.name.as_str()));

    // Banner
    println!();
    if me.email.is_empty() {
        println!("{}  Attacca CLI (offline — check API key)", "◆".bright_cyan());
    } else {
        println!("{}  Attacca CLI — {} ({})", "◆".bright_cyan(), me.display_name.bold(), me.email);
    }
    if let Some(pn) = project_name { println!("{}  Project: {}", "📁".bright_cyan(), pn.bold()); }
    if let Some(an) = agent_name { println!("{}  Agent: {}", "🤖".bright_cyan(), an.bold()); }
    println!("{}  Bridge mode: agent can access your local computer via tools", "🔗".bright_cyan());
    println!("{}  Type /help\n", "◆".bright_cyan());

    // Pick or resume session
    let (mut session_id, mut cursor, mut first_turn) = if let Some(sid) = session_id {
        let cur = client.get_last_cursor(&sid).await.unwrap_or(0);
        println!("{} Resuming session {}", "↻".bright_black(), sid);
        (sid, cur, false)
    } else {
        match pick_session(client, project_id.clone(), agent_id.clone()).await {
            Ok(ok) => ok,
            Err(e) => { print_error(&format!("session: {e}")); return Ok(()); }
        }
    };

    let config = Config::builder()
        .completion_type(CompletionType::List)
        .build();
    let mut rl: Editor<SlashHelper, DefaultHistory> = Editor::with_config(config)
        .map_err(|e| format!("rustyline: {e}"))?;
    rl.set_helper(Some(SlashHelper));
    let _ = rl.load_history("attacca_history.txt");

    loop {
        let prompt = "→ ".bright_green().to_string();
        match rl.readline(&prompt) {
            Ok(input) => {
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() { continue; }
                let _ = rl.add_history_entry(&trimmed);
                let _ = rl.save_history("attacca_history.txt");

                match trimmed.as_str() {
                    "/help" => {
                        println!("{} Commands:", "●".bright_yellow());
                        println!("  /help      Show this");
                        println!("  /new       New session");
                        println!("  /sessions  Switch session");
                        println!("  /diagnose  Test API connection");
                        println!("  /quit      Exit");
                        println!();
                        println!("{} Flags:", "●".bright_yellow());
                        println!("  -P, --project   Project name/UUID");
                        println!("  -S, --session   Session UUID (resume)");
                        println!("  -A, --agent     Agent UUID");
                        println!("  --debug         Show API URLs");
                        continue;
                    }
                    "/quit" | "/exit" => break,
                    "/new" => {
                        match client.create_session(project_id.as_deref(), agent_id.as_deref()).await {
                            Ok(s) => { session_id = s.id; cursor = 0; first_turn = true;
                                println!("{} New session: {}", "💬".bright_black(), session_id); }
                            Err(e) => { print_error(&format!("create session: {e}")); }
                        }
                        continue;
                    }
                    "/sessions" => {
                        match pick_session(client, project_id.clone(), agent_id.clone()).await {
                            Ok((sid, cur, ft)) => { session_id = sid; cursor = cur; first_turn = ft; }
                            Err(e) => { print_error(&format!("pick session: {e}")); }
                        }
                        continue;
                    }
                    "/diagnose" => {
                        client.diagnose().await;
                        continue;
                    }
                    _ => {}
                }

                let full_msg = if first_turn { first_turn = false; format!("{}\n\n---\n{}", PROTOCOL, trimmed) } else { trimmed };
                if let Err(e) = client.send_message(&session_id, &full_msg).await {
                    print_error(&format!("send: {e}"));
                    continue;
                }

                'tool_loop: loop {
                    if let Err(e) = client.wait_until_done(&session_id).await {
                        print_error(&e); break;
                    }
                    let msgs = match client.get_messages_after(&session_id, cursor).await {
                        Ok(m) => m,
                        Err(e) => { print_error(&e); break; }
                    };
                    if msgs.is_empty() { break; }
                    let mut sent_results = false;

                    for m in &msgs {
                        if let MessageRole::Assistant = m.role {
                            let (text, tools) = parse_tool_calls(&m.text);
                            print_assistant(&text);
                            if tools.is_empty() { if m.cursor > cursor { cursor = m.cursor; } continue; }

                            let mut results = Vec::new();
                            for tc in &tools {
                                let danger = is_dangerous(tc);
                                print_tool_invoke(tc, danger);
                                if ask_approve(danger) {
                                    println!("  └ {}", "✔".bright_green());
                                    let result = execute_tool(tc);
                                    results.push(format!("[Tool \"{}\"]\n{}", tc.name, result));
                                } else { println!("  └ {}", "✖".bright_red()); results.push(format!("[Tool \"{}\" rejected]", tc.name)); }
                            }
                            if !results.is_empty() {
                                sent_results = true;
                                let combined = results.join("\n\n");
                                let msg = if combined.len() > 100_000 { format!("{}...(truncated)", &combined[..100_000]) } else { combined };
                                if let Err(e) = client.send_message(&session_id, &msg).await {
                                    print_error(&format!("send result: {e}"));
                                    break 'tool_loop;
                                }
                            }
                        }
                        if m.cursor > cursor { cursor = m.cursor; }
                    }
                    if !sent_results { break; }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) | Err(rustyline::error::ReadlineError::Eof) => { println!(); break; }
            Err(e) => { print_error(&format!("readline: {e}")); break; }
        }
    }

    let _ = rl.save_history("attacca_history.txt");
    println!("{} Bye!", "◆".bright_cyan());
    Ok(())
}

// ---------------------------------------------------------------------------
// One-shot
// ---------------------------------------------------------------------------

async fn run_one_shot(client: &ApiClient, message: &str, project_id: Option<String>, agent_id: Option<String>) -> Result<(), String> {
    let session = client.create_session(project_id.as_deref(), agent_id.as_deref()).await?;
    let full_msg = format!("{}\n\n---\n{}", PROTOCOL, message);
    client.send_message(&session.id, &full_msg).await?;
    let mut cursor = 0i64;

    loop {
        client.wait_until_done(&session.id).await?;
        let msgs = client.get_messages_after(&session.id, cursor).await?;
        if msgs.is_empty() { break; }
        let mut sent_results = false;

        for m in &msgs {
            if let MessageRole::Assistant = m.role {
                let (text, tools) = parse_tool_calls(&m.text);
                if !text.is_empty() { println!("{}", text); }
                if !tools.is_empty() {
                    sent_results = true;
                    let mut results = Vec::new();
                    for tc in &tools {
                        let danger = is_dangerous(tc);
                        print_tool_invoke(tc, danger);
                        if danger && !ask_approve(true) { println!("  └ {} rejected", "✖".bright_red()); results.push(format!("[Tool \"{}\" rejected]", tc.name)); continue; }
                        println!("  └ {}", "✔".bright_green());
                        results.push(format!("[Tool \"{}\"]\n{}", tc.name, execute_tool(tc)));
                    }
                    if !results.is_empty() {
                        let combined = results.join("\n\n");
                        let msg = if combined.len() > 100_000 { format!("{}...(truncated)", &combined[..100_000]) } else { combined };
                        client.send_message(&session.id, &msg).await?;
                    }
                }
            }
            if m.cursor > cursor { cursor = m.cursor; }
        }
        let sess = client.get_session(&session.id).await?;
        if !sess.running && !sent_results { break; }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();

    let client = match ApiClient::from_env(cli.debug) {
        Ok(c) => c,
        Err(e) => { print_error(&e); std::process::exit(1); }
    };

    let project_id = match &cli.project {
        Some(p) => match client.resolve_project(p).await {
            Ok(id) => Some(id),
            Err(e) => { print_error(&e); std::process::exit(1); }
        },
        None => None,
    };

    let result = match cli.message {
        Some(msg) => run_one_shot(&client, &msg, project_id, cli.agent).await,
        None => run_interactive(&client, project_id, cli.session, cli.agent).await,
    };

    if let Err(e) = result {
        print_error(&e);
        if e.contains("401") || e.contains("scope") { eprintln!("  Get an API key at https://attacca.cc/settings"); }
        std::process::exit(1);
    }
}
