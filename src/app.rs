//! Application state — structured data and enums only.
//!
//! No I/O, no rendering, no event handling. The event loop lives in
//! [`event`](crate::event), input handling in [`handler`](crate::handler),
//! and background tasks in [`bg`](crate::bg).

use crate::transport::Transport;
use std::collections::HashMap;
use tokio::sync::mpsc;

// ── Core message type ──────────────────────────────────────────

/// A single message in the chat view.
#[derive(Clone)]
pub struct Msg {
    pub role: String, // "user" | "agent" | "sys" | "tool" | "result"
    pub text: String,
    pub raw: Option<String>, // raw JSON for pending tool calls
    pub done: bool,          // tool has been approved/skipped
    pub cursor: i64,         // API cursor for dedup
}

// ── Sidebar items ──────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum SidebarItem {
    ProjectHeader {
        id: String,
        name: String,
        expanded: bool,
        session_count: usize,
    },
    Session {
        title: String,
        id: String,
        active: bool,
    },
    NewSession,
}

// ── Background events (bg → main thread) ───────────────────────

pub enum BgEvent {
    NewMsgs {
        sid: String,
        msgs: Vec<Msg>,
        new_cur: i64,
    },
    SessionCreated(String),
    Usage(serde_json::Value),
    Done {
        sid: String,
    },
}

// ── Actions (main thread → bg) ─────────────────────────────────

pub enum Action {
    Send(String),
    Open(String),
    Create,
    Login(String),
    ShowInfo,
    ToolResult(String), // tool execution result to send back to agent
}

// ── Focus ──────────────────────────────────────────────────────

#[derive(PartialEq)]
pub enum Focus {
    Chat,
    Sidebar,
}

// ── Main application state ─────────────────────────────────────

pub struct App {
    /// API transport (cloned for background tasks).
    pub transport: Transport,

    // Session
    pub sid: Option<String>,
    pub cursor: i64,
    pub first: bool, // first message → prepend PROTOCOL

    // Chat messages
    pub msgs: Vec<Msg>,

    // Input
    pub input: String,
    pub autocomplete_suggestions: Vec<String>,
    pub autocomplete_idx: Option<usize>,

    // Scrolling
    pub scroll: usize,
    pub at_end: bool,
    pub input_scroll: usize,

    // Sidebar
    pub sidebar_items: Vec<SidebarItem>,
    pub sidebar_sel: usize,
    pub sidebar_scroll: usize,
    pub expanded_projects: std::collections::HashSet<String>,
    pub sessions: Vec<(String, String, String)>, // (project_id, title, session_id)

    // Data caches
    pub project_names: HashMap<String, String>,
    pub user_name: String,
    pub user_plan: String,
    pub user_credits: String,
    pub current_project_name: String,
    pub usage_credits_used: String,
    pub usage_context_tokens: String,
    pub usage_input_tokens: String,
    pub usage_output_tokens: String,
    pub usage_total_tokens: String,
    pub usage_model: String,

    // State
    pub exit_requested: bool,
    pub focus: Focus,
    busy_count: u32,

    // Channels
    pub bg_tx: mpsc::UnboundedSender<BgEvent>,
    pub bg_rx: mpsc::UnboundedReceiver<BgEvent>,
    pub actions: Vec<Action>,
}

impl App {
    pub fn new(transport: Transport) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            transport,
            sid: None,
            cursor: 0,
            first: true,
            msgs: vec![],
            input: String::new(),
            autocomplete_suggestions: vec![],
            autocomplete_idx: None,
            scroll: 0,
            at_end: true,
            input_scroll: 0,
            sidebar_items: vec![],
            sidebar_sel: 0,
            sidebar_scroll: 0,
            expanded_projects: std::collections::HashSet::new(),
            sessions: vec![],
            project_names: HashMap::new(),
            user_name: String::new(),
            user_plan: String::new(),
            user_credits: String::new(),
            current_project_name: String::new(),
            usage_credits_used: String::new(),
            usage_context_tokens: String::new(),
            usage_input_tokens: String::new(),
            usage_output_tokens: String::new(),
            usage_total_tokens: String::new(),
            usage_model: String::new(),
            exit_requested: false,
            focus: Focus::Chat,
            busy_count: 0,
            bg_tx: tx,
            bg_rx: rx,
            actions: vec![],
        }
    }

    /// Whether any background work is in-flight.
    pub fn busy(&self) -> bool {
        self.busy_count > 0
    }

    /// Increment the busy counter (before spawning a task).
    pub fn inc_busy(&mut self) {
        self.busy_count = self.busy_count.saturating_add(1);
    }

    /// Decrement the busy counter (when a task finishes).
    pub fn dec_busy(&mut self) {
        self.busy_count = self.busy_count.saturating_sub(1);
    }

    /// Check if there's a pending (unapproved) tool call.
    pub fn has_pending_tool(&self) -> bool {
        self.msgs.iter().any(|m| m.raw.is_some() && !m.done)
    }

    // ── Message helpers ────────────────────────────────────────

    /// Append a simple system/user/agent message.
    pub fn add_msg(&mut self, role: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.msgs.push(Msg {
            role: role.into(),
            text: text.into(),
            raw: None,
            done: false,
            cursor: 0,
        });
    }

    /// Insert or update a message by cursor (streaming dedup).
    pub fn upsert_msg(&mut self, cursor: i64, role: &str, text: &str) {
        if cursor > 0 {
            if let Some(existing) = self.msgs.iter_mut().find(|m| m.cursor == cursor) {
                existing.text = text.to_string();
                return;
            }
        }
        self.msgs.push(Msg {
            role: role.into(),
            text: text.into(),
            raw: None,
            done: false,
            cursor,
        });
    }

    /// Add a pending tool-call message (waiting for y/n).
    pub fn add_tool_msg(&mut self, json: &str) {
        let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
        let tool = v["tool"].as_str().unwrap_or("?");
        let args = v
            .get("args")
            .and_then(|a| a.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, vv)| Some(format!("{k}={}", vv.as_str()?)))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        self.msgs.push(Msg {
            role: "tool".into(),
            text: format!("◆ {tool} {args}"),
            raw: Some(json.into()),
            done: false,
            cursor: 0,
        });
    }
}
