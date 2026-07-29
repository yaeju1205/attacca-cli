use serde_json::Value;
use std::path::Path;

/// Protocol instructions sent to the Attacca agent on first turn.
pub const PROTOCOL: &str = r#"
You are connected to the user's computer via attacca-cli.

To access the user's local machine, use ```attacca-tool blocks:

```attacca-tool
{"tool": "read_file", "args": {"path": "/path/to/file"} }
```

Available tools:
- read_file(path) — read any text file
- write_file(path, content) — write/overwrite
- list_dir(path) — list directory contents
- run_command(command) — run shell command (grep, find, git, cat, ls, sed)
- file_exists(path) — check if exists

Automatic approval: read_file, and commands starting with "find", "grep"
run without asking. File paths relative to the current directory are
resolved against the CLI's working directory.

Rules: Read before writing. Never invent file contents."#;

/// Extract ```attacca-tool blocks from agent response text.
/// Returns (clean_text, vec_of_json_strings).
pub fn parse_tools(text: &str) -> (String, Vec<String>) {
    let mut tools = Vec::new();
    let mut clean = text.to_string();
    loop {
        let start = clean.find("```attacca-tool");
        let Some(s) = start else { break };
        let cs = s + "```attacca-tool".len();
        let end = clean[cs..].find("```").map(|x| cs + x).unwrap_or(clean.len());
        tools.push(clean[cs..end].trim().to_string());
        let block_end = if end + 3 <= clean.len() { end + 3 } else { clean.len() };
        clean.replace_range(s..block_end, "");
    }
    (clean.trim().to_string(), tools)
}

/// Whether a tool should auto-approve without waiting for y/n.
///
/// Safe tools: `read_file`, and `run_command` starting with `find` or `grep`.
pub fn is_safe_tool(json: &str) -> bool {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    let tool = v["tool"].as_str().unwrap_or("");
    match tool {
        "read_file" => true,
        "file_exists" => true,
        "run_command" => {
            let cmd = v["args"]["command"].as_str().unwrap_or("");
            cmd.trim_start().starts_with("find ")
                || cmd.trim_start().starts_with("grep ")
        }
        _ => false,
    }
}

/// Resolve a file path, defaulting to the current working directory
/// when the given path is relative.
pub fn resolve_path(path: &str) -> String {
    if path.is_empty() {
        // default to CWD
        return cwd();
    }
    let p = Path::new(path);
    if p.is_absolute() {
        path.to_string()
    } else {
        format!("{}/{}", cwd().trim_end_matches('/'), path.trim_start_matches("./"))
    }
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

/// Execute a local tool from its JSON description.
pub fn exec_tool(json: &str) -> String {
    let v: Value = serde_json::from_str(json).unwrap_or_default();
    let tool = v["tool"].as_str().unwrap_or("?");
    let arg = |k: &str| v["args"][k].as_str().unwrap_or("");

    match tool {
        "read_file" => read_file(&resolve_path(arg("path"))),
        "write_file" => write_file(&resolve_path(arg("path")), arg("content")),
        "list_dir" => list_dir(&resolve_path(arg("path"))),
        "run_command" => run_cmd(arg("command")),
        "file_exists" => std::path::Path::new(&resolve_path(arg("path"))).exists().to_string(),
        _ => format!("? unknown tool: {tool}"),
    }
}

fn read_file(path: &str) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            if s.len() > 50000 {
                format!("[truncated to 50k]{}", &s[..50000])
            } else {
                s
            }
        }
        Err(e) => format!("err: {e}"),
    }
}

fn write_file(path: &str, content: &str) -> String {
    match std::fs::write(path, content) {
        Ok(()) => "ok".into(),
        Err(e) => format!("err: {e}"),
    }
}

fn list_dir(path: &str) -> String {
    match std::fs::read_dir(path) {
        Ok(entries) => {
            let items: Vec<String> = entries
                .flatten()
                .map(|e| {
                    let icon = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        "📁 "
                    } else {
                        "  "
                    };
                    format!("{icon}{}", e.file_name().to_string_lossy())
                })
                .collect();
            items.join("\n")
        }
        Err(e) => format!("err: {e}"),
    }
}

fn run_cmd(cmd: &str) -> String {
    match std::process::Command::new("sh").arg("-c").arg(cmd).output() {
        Ok(o) => {
            let mut r = String::from_utf8_lossy(&o.stdout).to_string();
            let se = String::from_utf8_lossy(&o.stderr);
            if !se.is_empty() {
                r.push_str(&format!("\n[err]\n{se}"));
            }
            r
        }
        Err(e) => format!("err: {e}"),
    }
}

/// Shorten a UUID for display (UTF-8 safe).
pub fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tools_empty() {
        let (text, tools) = parse_tools("hello world");
        assert_eq!(text, "hello world");
        assert!(tools.is_empty());
    }

    #[test]
    fn test_parse_tools_single() {
        let (text, tools) = parse_tools("say hi\n```attacca-tool\n{\"tool\":\"read_file\",\"args\":{\"path\":\"x\"}}\n```\nok");
        assert!(text.contains("say hi"));
        assert!(text.contains("ok"));
        assert_eq!(tools.len(), 1);
        assert!(tools[0].contains("\"read_file\""));
    }

    #[test]
    fn test_parse_tools_multiple() {
        let input = "a\n```attacca-tool\n{\"tool\":\"read_file\",\"args\":{\"path\":\"x\"}}\n```\nb\n```attacca-tool\n{\"tool\":\"list_dir\",\"args\":{\"path\":\".\"}}\n```\nc";
        let (text, tools) = parse_tools(input);
        assert!(text.contains("a"));
        assert!(text.contains("b"));
        assert!(text.contains("c"));
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn test_short() {
        assert_eq!(short("abc12345def67890"), "abc12345");
        assert_eq!(short("abc"), "abc");
    }

    #[test]
    fn test_exec_unknown_tool() {
        let result = exec_tool(r#"{"tool":"nonexistent","args":{}}"#);
        assert!(result.contains("unknown"));
    }

    #[test]
    fn test_exec_missing_tool() {
        let result = exec_tool(r#"{}"#);
        assert!(result.contains("unknown"));
    }

    #[test]
    fn test_is_safe_read_file() {
        assert!(is_safe_tool(r#"{"tool":"read_file","args":{"path":"x"}}"#));
    }

    #[test]
    fn test_is_safe_find() {
        assert!(is_safe_tool(r#"{"tool":"run_command","args":{"command":"find . -name '*.rs'"}}"#));
    }

    #[test]
    fn test_is_safe_grep() {
        assert!(is_safe_tool(r#"{"tool":"run_command","args":{"command":"grep -r 'foo' ."}}"#));
    }

    #[test]
    fn test_is_not_safe_write() {
        assert!(!is_safe_tool(r#"{"tool":"write_file","args":{"path":"x","content":"y"}}"#));
    }

    #[test]
    fn test_is_not_safe_rm() {
        assert!(!is_safe_tool(r#"{"tool":"run_command","args":{"command":"rm -rf /"}}"#));
    }

    #[test]
    fn test_resolve_path_absolute() {
        let r = resolve_path("/etc/passwd");
        assert_eq!(r, "/etc/passwd");
    }

    #[test]
    fn test_resolve_path_empty() {
        let r = resolve_path("");
        assert!(r.contains("/attacca-cli") || r.contains('/'));
    }

    #[test]
    fn test_resolve_path_relative() {
        let r = resolve_path("foo/bar.txt");
        assert!(r.contains("foo/bar.txt"));
        assert!(r.starts_with('/')); // absolute result
    }
}
