use clap::Parser;
use colored::*;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use rustyline::DefaultEditor;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Protocol — tells the agent how to request tool calls
// ---------------------------------------------------------------------------

const PROTOCOL: &str = r#"## attacca-cli bridge protocol

You are connected to the user's **local computer** through attacca-cli. To
interact with it, output JSON tool calls inside ```attacca-tool blocks.

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
| run_command | command | Run a shell command |
| create_dir | path | Create a directory |
| file_exists | path | Check a file exists |
| delete_file | path | Delete a file or empty dir |
| read_files | paths[] | Batch read multiple files |

### Rules
1. Output ONE tool call per ```attacca-tool block.
2. You can output text AND tool calls in the same response.
3. After I send back the tool result, continue your reasoning.
4. When the task is complete, state it clearly.
5. Do NOT invent files or guess contents -- always use a tool.
6. Never explain what you would do -- actually do it with a tool."#;

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SessionDto {
    id: String,
    running: bool,
}

#[derive(Deserialize)]
struct MessageDto {
    role: MessageRole,
    text: String,
    cursor: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Deserialize)]
struct ErrorDto {
    code: String,
    message: String,
}

#[derive(Deserialize)]
struct MeDto {
    display_name: String,
    email: String,
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "attacca", version, about = "Chat with Attacca agents -- local bridge mode")]
struct Cli {
    /// Message to send (one-shot mode)
    message: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool model
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ToolCall {
    name: String,
    args: HashMap<String, String>,
}

/// Parse ```attacca-tool blocks from agent text, returning (clean_text, tools).
fn parse_tool_calls(text: &str) -> (String, Vec<ToolCall>) {
    let mut tools = Vec::new();
    let mut clean = text.to_string();
    let start_marker = "```attacca-tool";
    let end_marker = "```";

    loop {
        let start = match clean.find(start_marker) {
            Some(i) => i,
            None => break,
        };
        let content_start = start + start_marker.len();
        let end = match clean[content_start..].find(end_marker) {
            Some(i) => content_start + i,
            None => break,
        };

        let json_str = clean[content_start..end].trim();
        if let Ok(tc) = parse_single_tool(json_str) {
            tools.push(tc);
        }

        let block_end = end + end_marker.len();
        clean.replace_range(start..block_end, "");
    }

    (clean.trim().to_string(), tools)
}

fn parse_single_tool(json: &str) -> Result<ToolCall, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("bad json: {e}"))?;
    let name = v["tool"].as_str().ok_or("missing 'tool'")?.to_string();
    let args_obj = v.get("args").and_then(|a| a.as_object()).ok_or("missing 'args'")?;
    let mut args = HashMap::new();
    for (k, val) in args_obj {
        args.insert(k.clone(), val.as_str().unwrap_or("").to_string());
    }
    Ok(ToolCall { name, args })
}

// ---------------------------------------------------------------------------
// Tool execution (local)
// ---------------------------------------------------------------------------

fn execute_tool(tc: &ToolCall) -> String {
    let empty = String::new();
    match tc.name.as_str() {
        "read_file" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::read_to_string(path) {
                Ok(s) => {
                    if s.len() > 100_000 {
                        format!("[file too large: {} bytes, showing first 100k]\n{}", s.len(), &s[..100_000])
                    } else {
                        format!("[file content ({} bytes)]:\n{}", s.len(), s)
                    }
                }
                Err(e) => format!("[error reading file: {e}]"),
            }
        }
        "write_file" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            let content = tc.args.get("content").unwrap_or(&empty);
            match std::fs::write(path, content) {
                Ok(()) => format!("[OK] wrote {} bytes to {}", content.len(), path),
                Err(e) => format!("[error writing file: {e}]"),
            }
        }
        "edit_file" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            let old = tc.args.get("old_string").unwrap_or(&empty);
            let new = tc.args.get("new_string").unwrap_or(&empty);
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    if content.contains(old.as_str()) {
                        let new_content = content.replace(old.as_str(), new.as_str());
                        match std::fs::write(path, &new_content) {
                            Ok(()) => {
                                let count = content.matches(old.as_str()).count();
                                format!("[OK] replaced {} occurrence(s) in {}", count, path)
                            }
                            Err(e) => format!("[error writing after edit: {e}]"),
                        }
                    } else {
                        format!("[error] string not found in {}", path)
                    }
                }
                Err(e) => format!("[error reading for edit: {e}]"),
            }
        }
        "list_dir" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut items: Vec<String> = Vec::new();
                    for entry in entries.flatten() {
                        let ftype = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            "📁"
                        } else {
                            "📄"
                        };
                        items.push(format!("{} {}", ftype, entry.file_name().to_string_lossy()));
                    }
                    items.sort();
                    format!("[{} entries]:\n{}", items.len(), items.join("\n"))
                }
                Err(e) => format!("[error listing dir: {e}]"),
            }
        }
        "run_command" => {
            let cmd = tc.args.get("command").unwrap_or(&empty);
            match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
                Ok(output) => {
                    let mut result = String::new();
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if !stdout.is_empty() { result.push_str(&format!("[stdout]:\n{stdout}\n")); }
                    if !stderr.is_empty() { result.push_str(&format!("[stderr]:\n{stderr}\n")); }
                    result.push_str(&format!("[exit code: {}]", output.status.code().unwrap_or(-1)));
                    result
                }
                Err(e) => format!("[error running command: {e}]"),
            }
        }
        "create_dir" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::create_dir_all(path) {
                Ok(()) => format!("[OK] created directory {}", path),
                Err(e) => format!("[error creating dir: {e}]"),
            }
        }
        "file_exists" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            let exists = std::path::Path::new(path).exists();
            if exists { format!("[true] {} exists", path) } else { format!("[false] {} does not exist", path) }
        }
        "delete_file" => {
            let path = tc.args.get("path").unwrap_or(&empty);
            match std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path)) {
                Ok(()) => format!("[OK] deleted {}", path),
                Err(e) => format!("[error deleting: {e}]"),
            }
        }
        "read_files" => {
            let paths_str = tc.args.get("paths").unwrap_or(&empty);
            let paths: Vec<String> = if paths_str.starts_with('[') {
                serde_json::from_str::<Vec<String>>(paths_str).unwrap_or(vec![paths_str.clone()])
            } else {
                paths_str.split(',').map(|s| s.trim().to_string()).collect()
            };
            let mut results = Vec::new();
            for p in &paths {
                match std::fs::read_to_string(p) {
                    Ok(s) => results.push(format!("--- {} ---\n{}", p, s)),
                    Err(e) => results.push(format!("--- {} ---\n[error: {e}]", p)),
                }
            }
            results.join("\n")
        }
        other => format!("[unknown tool: {other}]"),
    }
}

/// Returns true for dangerous commands that should default to "no"
fn is_dangerous(tc: &ToolCall) -> bool {
    if tc.name == "run_command" {
        let cmd = tc.args.get("command").map(|s| s.as_str()).unwrap_or("");
        cmd.contains("rm ") || cmd.contains("sudo ") || cmd.contains("dd ") || cmd.contains("mkfs")
            || cmd.contains('>') || (cmd.contains('|') && cmd.contains("rm"))
    } else {
        false
    }
}

fn format_tool(tc: &ToolCall) -> String {
    let args: Vec<String> = tc.args.iter().map(|(k, v)| {
        if v.len() > 60 {
            format!("{}: \"{}...\"", k, &v[..60])
        } else {
            format!("{}: \"{}\"", k, v)
        }
    }).collect();
    format!("{}({})", tc.name, args.join(", "))
}

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

const BASE_URL: &str = "https://attacca.cc";

struct ApiClient {
    inner: Client,
    key: String,
}

impl ApiClient {
    fn from_env() -> Result<Self, String> {
        let key = std::env::var("ATTACCA_API_KEY").map_err(|_| {
            "Set ATTACCA_API_KEY (get one at attacca.cc > Settings > API keys)".to_string()
        })?;
        let inner = Client::builder()
            .user_agent("attacca-cli/0.1.0")
            .build()
            .map_err(|e| format!("reqwest: {e}"))?;
        Ok(Self { inner, key })
    }

    fn bearer_header(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.key).parse().unwrap(),
        );
        h
    }

    async fn get_me(&self) -> Result<MeDto, String> {
        let resp = self.inner.get(format!("{BASE_URL}/v1/me"))
            .headers(self.bearer_header())
            .send().await.map_err(|e| format!("request: {e}"))?;
        if resp.status().is_success() {
            resp.json().await.map_err(|e| format!("json: {e}"))
        } else {
            let err: ErrorDto = resp.json().await.map_err(|_| "unknown error".to_string())?;
            Err(format!("{}: {}", err.code, err.message))
        }
    }

    async fn create_session(&self) -> Result<SessionDto, String> {
        let body = serde_json::json!({"title": "attacca-cli"});
        let resp = self.inner.post(format!("{BASE_URL}/v1/sessions"))
            .headers(self.bearer_header())
            .json(&body)
            .send().await.map_err(|e| format!("request: {e}"))?;
        if resp.status().is_success() {
            resp.json().await.map_err(|e| format!("json: {e}"))
        } else {
            let err: ErrorDto = resp.json().await.map_err(|_| "unknown error".to_string())?;
            Err(format!("{}: {}", err.code, err.message))
        }
    }

    async fn send_message(&self, session_id: &str, msg: &str) -> Result<(), String> {
        let body = serde_json::json!({"message": msg, "timezone": "Asia/Seoul"});
        let resp = self.inner.post(format!("{BASE_URL}/v1/sessions/{session_id}/messages"))
            .headers(self.bearer_header())
            .json(&body)
            .send().await.map_err(|e| format!("request: {e}"))?;
        let status = resp.status();
        if status.is_success() || status.as_u16() == 202 {
            Ok(())
        } else {
            let err: ErrorDto = resp.json().await.map_err(|_| "unknown error".to_string())?;
            Err(format!("{}: {}", err.code, err.message))
        }
    }

    async fn get_session(&self, session_id: &str) -> Result<SessionDto, String> {
        let resp = self.inner.get(format!("{BASE_URL}/v1/sessions/{session_id}"))
            .headers(self.bearer_header())
            .send().await.map_err(|e| format!("request: {e}"))?;
        if resp.status().is_success() {
            resp.json().await.map_err(|e| format!("json: {e}"))
        } else {
            let err: ErrorDto = resp.json().await.map_err(|_| "unknown error".to_string())?;
            Err(format!("{}: {}", err.code, err.message))
        }
    }

    async fn get_messages_after(&self, session_id: &str, after: i64) -> Result<Vec<MessageDto>, String> {
        let url = format!("{BASE_URL}/v1/sessions/{session_id}/messages?after={after}");
        let resp = self.inner.get(&url).headers(self.bearer_header())
            .send().await.map_err(|e| format!("request: {e}"))?;
        if resp.status().is_success() {
            let mut msgs: Vec<MessageDto> = resp.json().await.map_err(|e| format!("json: {e}"))?;
            msgs.reverse();
            Ok(msgs)
        } else {
            let err: ErrorDto = resp.json().await.map_err(|_| "unknown error".to_string())?;
            Err(format!("{}: {}", err.code, err.message))
        }
    }

    async fn wait_until_done(&self, session_id: &str) -> Result<(), String> {
        loop {
            let sess = self.get_session(session_id).await?;
            if !sess.running { return Ok(()); }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// UI helpers
// ---------------------------------------------------------------------------

fn print_error(msg: &str) {
    eprintln!("{} {msg}", "✖".bright_red());
}

fn print_banner(me: &MeDto) {
    println!();
    println!("{}  Attacca CLI -- {} ({})", "◆".bright_cyan(), me.display_name.bold(), me.email);
    println!("{}  Bridge mode: agent can access your local computer via tools", "🔗".bright_cyan());
    println!("{}  Type /help for commands", "◆".bright_cyan());
    println!();
}

fn print_assistant(text: &str) {
    if text.is_empty() { return; }
    for line in text.lines() {
        println!("{} {}", "│".bright_blue(), line);
    }
    println!();
}

fn print_tool_invoke(tc: &ToolCall, is_danger: bool) {
    let icon = if is_danger { "⚠".bright_red().to_string() } else { "🔧".bright_yellow().to_string() };
    println!("{} {}", icon, format_tool(tc).bold());
}

fn ask_approve(danger: bool) -> bool {
    let default = if danger { "n" } else { "Y" };
    print!("{} Execute? [{}/{}] ", "  └".bright_black(),
        default.to_uppercase().green(),
        if default == "Y" { "n".bright_red() } else { "y".bright_green() });
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let input = input.trim().to_lowercase();
    if input.is_empty() { default == "Y" } else { input == "y" }
}

// ---------------------------------------------------------------------------
// Interactive mode
// ---------------------------------------------------------------------------

async fn run_interactive(client: &ApiClient) -> Result<(), String> {
    let me = client.get_me().await?;
    print_banner(&me);

    let mut session_id;
    {
        let s = client.create_session().await?;
        session_id = s.id;
    }
    println!("{} Session: {}", "📁".bright_black(), session_id);
    println!("{} CWD: {}", "📁".bright_black(),
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default());
    println!();

    let mut rl = DefaultEditor::new().map_err(|e| format!("rustyline: {e}"))?;
    let _ = rl.load_history("attacca_history.txt");

    let mut cursor = 0i64;
    let mut first_turn = true;

    loop {
        let prompt = "→ ".bright_green().to_string();
        let line = rl.readline(&prompt);
        match line {
            Ok(input) => {
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() { continue; }
                let _ = rl.add_history_entry(&trimmed);
                let _ = rl.save_history("attacca_history.txt");

                match trimmed.as_str() {
                    "/help" => {
                        println!("{} Commands:", "●".bright_yellow());
                        println!("  /help         Show this");
                        println!("  /quit         Exit");
                        println!("  /new          Start a fresh session");
                        println!("  anything      Send to agent");
                        continue;
                    }
                    "/quit" | "/exit" => break,
                    "/new" => {
                        let s = client.create_session().await?;
                        session_id = s.id;
                        cursor = 0;
                        first_turn = true;
                        println!("{} New session: {}", "📁".bright_black(), session_id);
                        continue;
                    }
                    _ => {}
                }

                let full_msg = if first_turn {
                    first_turn = false;
                    format!("{}\n\n---\n{}", PROTOCOL, trimmed)
                } else {
                    trimmed
                };
                client.send_message(&session_id, &full_msg).await?;

                // Inner tool loop: keep processing tools until agent is idle with no tools
                loop {
                    client.wait_until_done(&session_id).await?;
                    let msgs = client.get_messages_after(&session_id, cursor).await?;
                    if msgs.is_empty() { break; }

                    let mut sent_results = false;

                    for m in &msgs {
                        if let MessageRole::Assistant = m.role {
                            let (text, tools) = parse_tool_calls(&m.text);
                            print_assistant(&text);

                            if tools.is_empty() {
                                if m.cursor > cursor { cursor = m.cursor; }
                                continue;
                            }

                            let mut results = Vec::new();
                            for tc in &tools {
                                let danger = is_dangerous(tc);
                                print_tool_invoke(tc, danger);
                                if ask_approve(danger) {
                                    println!("  └ {}", "✔ running...".bright_green());
                                    let result = execute_tool(tc);
                                    results.push(format!("[Tool \"{}\" result]:\n{}", tc.name, result));
                                } else {
                                    println!("  └ {}", "✖ rejected".bright_red());
                                    results.push(format!("[Tool \"{}\" rejected by user]", tc.name));
                                }
                            }

                            if !results.is_empty() {
                                sent_results = true;
                                let combined = results.join("\n\n");
                                let result_msg = if combined.len() > 100_000 {
                                    format!("{}...(truncated)", &combined[..100_000])
                                } else {
                                    combined
                                };
                                client.send_message(&session_id, &result_msg).await?;
                            }
                        }
                        if m.cursor > cursor { cursor = m.cursor; }
                    }

                    if !sent_results { break; }
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted)
            | Err(rustyline::error::ReadlineError::Eof) => {
                println!();
                break;
            }
            Err(e) => {
                print_error(&format!("readline: {e}"));
                break;
            }
        }
    }

    let _ = rl.save_history("attacca_history.txt");
    println!("{} Bye!", "◆".bright_cyan());
    Ok(())
}

// ---------------------------------------------------------------------------
// One-shot mode
// ---------------------------------------------------------------------------

async fn run_one_shot(client: &ApiClient, message: &str) -> Result<(), String> {
    let session = client.create_session().await?;
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

                        if danger {
                            if !ask_approve(true) {
                                println!("  └ {} rejected", "✖".bright_red());
                                results.push(format!("[Tool \"{}\" rejected by user]", tc.name));
                                continue;
                            }
                        }

                        println!("  └ {}", "✔ running...".bright_green());
                        let result = execute_tool(tc);
                        results.push(format!("[Tool \"{}\" result]:\n{}", tc.name, result));
                    }

                    if !results.is_empty() {
                        let combined = results.join("\n\n");
                        let result_msg = if combined.len() > 100_000 {
                            format!("{}...(truncated)", &combined[..100_000])
                        } else {
                            combined
                        };
                        client.send_message(&session.id, &result_msg).await?;
                    }
                }
            }

            // fetch final thoughts
            if let MessageRole::Assistant = m.role {
                let (text, tools) = parse_tool_calls(&m.text);
                if tools.is_empty() && !text.is_empty() && sent_results {
                    // already printed above
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
    let cli = Cli::parse();

    let client = match ApiClient::from_env() {
        Ok(c) => c,
        Err(e) => { print_error(&e); std::process::exit(1); }
    };

    let result = match cli.message {
        Some(msg) => run_one_shot(&client, &msg).await,
        None => run_interactive(&client).await,
    };

    if let Err(e) = result {
        print_error(&e);
        if e.contains("401") || e.contains("scope") {
            eprintln!("  Get an API key at https://attacca.cc/settings and set ATTACCA_API_KEY");
        }
        std::process::exit(1);
    }
}
