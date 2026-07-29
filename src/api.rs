//! Domain-specific API operations built on top of [`Transport`].
//!
//! Each public function takes a `Transport` (or `&Transport`) and returns
//! structured data — no raw HTTP details leak out.

use crate::transport::Transport;
use serde_json::Value;

// ── User / Auth ────────────────────────────────────────────────

/// Fetch `display_name` for an auth smoke test.
pub async fn whoami(api: &Transport) -> String {
    match api.get("me").await {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                format!(
                    "✓ {}",
                    v["display_name"].as_str().unwrap_or("?")
                )
            } else {
                "✓ ok".to_string()
            }
        }
        Err((s, b)) => {
            format!(
                "✖ HTTP {s}: {}",
                b.chars().take(60).collect::<String>()
            )
        }
    }
}

/// Rich user info as structured fields: `(name, email, plan, credits)`.
pub async fn whoami_detailed(api: &Transport) -> (String, String, String, String) {
    match api.get("me").await {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                let name = v["display_name"].as_str().unwrap_or("").to_string();
                let email = v["email"].as_str().unwrap_or("").to_string();
                let plan = v["plan"].as_str().unwrap_or("").to_string();
                let credits = v["credits"].as_str().unwrap_or("").to_string();
                (name, email, plan, credits)
            } else {
                Default::default()
            }
        }
        Err(_) => Default::default(),
    }
}

// ── Sessions ───────────────────────────────────────────────────

/// Fetch session usage/metadata as a raw JSON Value.
pub async fn get_session_usage(api: &Transport, sid: &str) -> Value {
    match api.get(&format!("/v1/sessions/{sid}")).await {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => Value::Null,
    }
}

/// Create a new session and return its id.
pub async fn create_session(api: &Transport, title: &str) -> Option<String> {
    match api
        .post("sessions", &serde_json::json!({ "title": title }))
        .await
    {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                v["id"].as_str().map(String::from)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

/// Send a message to a session.
pub async fn send_message(api: &Transport, sid: &str, msg: &str) -> bool {
    api.post(
        &format!("/v1/sessions/{sid}/messages"),
        &serde_json::json!({ "message": msg, "timezone": "Asia/Seoul" }),
    )
    .await
    .is_ok()
}

/// Fetch raw messages (as JSON Values) for a session after cursor 0.
/// We always use `after=0` so the caller deduplicates by cursor.
pub async fn fetch_messages_raw(api: &Transport, sid: &str) -> Vec<Value> {
    match api.get(&format!("/v1/sessions/{sid}/messages?after=0")).await {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => vec![],
    }
}

/// Check whether a session is still `running`.
pub async fn session_is_running(api: &Transport, sid: &str) -> bool {
    match api.get(&format!("/v1/sessions/{sid}")).await {
        Ok(body) => {
            if let Ok(v) = serde_json::from_str::<Value>(&body) {
                v["running"].as_bool().unwrap_or(true)
            } else {
                false // parse fail → assume done
            }
        }
        Err(_) => false,
    }
}

// ── Projects ───────────────────────────────────────────────────

/// Fetch all projects and return a `(id → name)` map.
pub async fn fetch_projects(api: &Transport) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(body) = api.get("projects").await {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&body) {
            for p in &arr {
                if let (Some(id), Some(name)) = (p["id"].as_str(), p["name"].as_str()) {
                    map.insert(id.to_string(), name.to_string());
                }
            }
        }
    }
    map
}

/// A minimal session summary from the sessions list.
#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub project_id: String,
    pub title: String,
    pub id: String,
}

/// Fetch the sessions list.
pub async fn fetch_sessions(api: &Transport) -> Vec<SessionSummary> {
    if api.key.is_empty() {
        return vec![];
    }
    match api.get("sessions").await {
        Ok(body) => {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&body) {
                arr.iter()
                    .map(|s| {
                        let pid = s["project_id"].as_str().unwrap_or("").to_string();
                        let t = s["title"].as_str().unwrap_or("");
                        let id = s["id"].as_str().unwrap_or("");
                        let title = if t.is_empty() || t == "attacca-cli" {
                            "untitled".into()
                        } else {
                            t.into()
                        };
                        SessionSummary { project_id: pid, title, id: id.into() }
                    })
                    .collect()
            } else {
                vec![]
            }
        }
        Err(_) => vec![],
    }
}

// ── Helpers ────────────────────────────────────────────────────

/// Extract a string from a JSON Value, handling string, i64, and f64.
pub fn field_val(v: &Value) -> String {
    v.as_str()
        .map(String::from)
        .or_else(|| v.as_i64().map(|n| n.to_string()))
        .or_else(|| v.as_f64().map(|n| format!("{:.1}", n)))
        .unwrap_or_default()
}
