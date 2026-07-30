//! Application state — structured data, enums, and the turn-frame reducer.
//!
//! No I/O, no rendering, no task spawning. The event loop lives in
//! [`event`](crate::event), input handling in [`handler`](crate::handler),
//! background tasks in [`bg`](crate::bg), and the connection in
//! [`zyris_client`](crate::zyris_client).

use crate::auth::Authenticator;
use crate::brief::{self, NodeBrief};
use crate::util::short;
use crate::zyris_client::ApiSlot;

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zyris_attacca::{
    ZAgent, ZDeltaKind, ZMe, ZNewSession, ZSession, ZSessionEvent, ZTurnFrame, ZUsage,
};

// ── Core message type ──────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MsgKind {
    Sys,
    User,
    Agent,
    Reasoning,
    Tool,
    Result,
}

/// A single card in the chat view.
#[derive(Clone, Debug)]
pub struct Msg {
    pub kind: MsgKind,
    pub text: String,
    /// Still growing from token deltas. Rendered with a cursor and settled by the durable event.
    pub streaming: bool,
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
        running: bool,
    },
    NewSession,
}

/// One row of the session list, flattened from [`ZSession`].
#[derive(Clone, Debug)]
pub struct SessionRow {
    pub project_id: String,
    pub title: String,
    pub id: String,
    pub running: bool,
}

// ── Background events (bg → main thread) ───────────────────────

pub type BgTx = mpsc::UnboundedSender<BgEvent>;

pub enum BgEvent {
    Connected(Box<ZMe>),
    Disconnected(String),
    Projects(Vec<zyris_attacca::ZProject>),
    Sessions(Vec<ZSession>),
    Agents(Vec<ZAgent>),
    SessionCreated(Box<ZSession>),
    /// The `turn_events` head: the session's state at subscribe time.
    StreamHead {
        session_id: String,
        running: bool,
    },
    Frame {
        session_id: String,
        frame: ZTurnFrame,
    },
    Usage(Box<ZUsage>),
    /// The node runtime gave up for good. Distinct from `Disconnected`, which is transient.
    NodeStopped {
        message: String,
        needs_operator: bool,
    },
    Notice(String),
    /// One in-flight request finished, whatever its outcome.
    Done,
}

// ── Actions (main thread → bg) ─────────────────────────────────

pub enum Action {
    Send(String),
    Open(String),
    Create,
    Cancel,
    Logout,
    ShowInfo,
    RefreshSessions,
}

// ── State persistence ─────────────────────────────────────────

use serde::{Deserialize, Serialize};

/// Snapshot of app state persisted between restarts.
#[derive(Serialize, Deserialize)]
pub struct SavedState {
    pub sessions: Vec<SavedSession>,
    pub last_sid: Option<String>,
    pub last_project_name: String,
    pub sidebar_sel: usize,
    pub sidebar_scroll: usize,
}

#[derive(Serialize, Deserialize)]
pub struct SavedSession {
    pub project_id: String,
    pub title: String,
    pub id: String,
}

impl App {
    pub fn save_state(&self) {
        let Some(path) = state_path() else { return };
        let state = SavedState {
            sessions: self
                .sessions
                .iter()
                .map(|s| SavedSession {
                    project_id: s.project_id.clone(),
                    title: s.title.clone(),
                    id: s.id.clone(),
                })
                .collect(),
            last_sid: self.sid.clone(),
            last_project_name: self.current_project_name.clone(),
            sidebar_sel: self.sidebar_sel,
            sidebar_scroll: self.sidebar_scroll,
        };
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            let _ = std::fs::write(&path, &json);
        }
    }

    /// Load saved state and return it (caller applies it after construction).
    pub fn load_state() -> Option<SavedState> {
        let path = state_path()?;
        let json = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&json).ok()
    }
}

fn state_path() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir().map(|d| d.join("attacca-cli"))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share/attacca-cli")))?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("state.json"))
}

// ── Focus ──────────────────────────────────────────────────────

#[derive(PartialEq)]
pub enum Focus {
    Chat,
    Sidebar,
}

// ── Transcript: the turn-frame reducer ─────────────────────────

/// The chat transcript and the turn-frame reducer that maintains it.
///
/// Split out from [`App`] so the reduction is testable without a terminal or a connection: every
/// interesting decision about deltas, durable events and deduplication lives here.
pub struct Transcript {
    pub msgs: Vec<Msg>,
    /// Highest durable cursor seen, which is what a re-subscribe resumes from.
    pub cur: i64,
    pub running: bool,
}

impl Transcript {
    pub fn new() -> Transcript {
        Transcript {
            msgs: Vec::new(),
            cur: 0,
            running: false,
        }
    }

    pub fn push(&mut self, kind: MsgKind, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.msgs.push(Msg {
            kind,
            text: text.to_string(),
            streaming: false,
        });
    }

    /// Grow the tail card in place, or open a new one.
    ///
    /// Unlike [`push`](Self::push) this accepts whitespace: the first chunk of a turn is often
    /// empty, and dropping it would leave the card unopened so the rest of the turn arrived with
    /// nowhere to land.
    fn push_delta(&mut self, kind: MsgKind, text: &str) {
        match self.msgs.last_mut() {
            Some(last) if last.streaming && last.kind == kind => last.text.push_str(text),
            _ => self.msgs.push(Msg {
                kind,
                text: text.to_string(),
                streaming: true,
            }),
        }
    }

    /// Reconcile a durable message with whatever the deltas already streamed.
    ///
    /// The event is the canonical text for content the deltas showed progressively, so it replaces
    /// the streaming card rather than following it. Appending instead would duplicate every reply.
    fn settle(&mut self, kind: MsgKind, text: &str) {
        if let Some(last) = self.msgs.last_mut() {
            if last.streaming && last.kind == kind {
                last.text = text.to_string();
                last.streaming = false;
                return;
            }
        }
        self.push(kind, text);
    }

    /// The user's own message is echoed optimistically on send and then arrives again as a durable
    /// event. Matching on text over the recent tail keeps one copy without needing an id the event
    /// payload is not guaranteed to carry.
    fn settle_user(&mut self, text: &str) {
        let already = self
            .msgs
            .iter()
            .rev()
            .take(4)
            .any(|m| m.kind == MsgKind::User && m.text.trim() == text.trim());
        if !already {
            self.push(MsgKind::User, text);
        }
    }

    fn finish_streaming(&mut self) {
        for m in self.msgs.iter_mut() {
            m.streaming = false;
        }
    }

    pub fn apply_frame(&mut self, frame: ZTurnFrame, debug_events: bool) {
        match frame {
            ZTurnFrame::Delta { kind, text } => {
                let kind = match kind {
                    ZDeltaKind::Assistant => MsgKind::Agent,
                    ZDeltaKind::Reasoning => MsgKind::Reasoning,
                };
                self.push_delta(kind, &text);
            }
            ZTurnFrame::Status { running } => {
                self.running = running;
                if !running {
                    self.finish_streaming();
                }
            }
            ZTurnFrame::Event { cursor, event } => {
                // A re-subscribe resumes from the last cursor seen, and every turn boundary is a
                // re-subscribe. Skipping what has already been applied makes replay idempotent
                // whether the server treats `after` as exclusive or inclusive — otherwise an
                // inclusive `after` would re-render the boundary message on every turn.
                if self.cur > 0 && cursor <= self.cur {
                    return;
                }
                self.cur = self.cur.max(cursor);
                self.apply_event(event, debug_events);
            }
        }
    }

    fn apply_event(&mut self, event: ZSessionEvent, debug_events: bool) {
        match classify(&event.kind) {
            Some(MsgKind::Tool) => {
                let name = string_field(&event.payload, &["name", "tool", "tool_name"])
                    .unwrap_or_else(|| "tool".to_string());
                self.push(
                    MsgKind::Tool,
                    &format!("◆ {name}{}", tool_args(&event.payload)),
                );
            }
            Some(MsgKind::Result) => {
                let text = text_of(&event.payload).unwrap_or_else(|| "ok".to_string());
                self.push(MsgKind::Result, &text);
            }
            Some(MsgKind::User) => {
                if let Some(text) = text_of(&event.payload) {
                    // A session an older build started carries the node brief in front of its first
                    // message. Stripping here covers both the echo during the turn and every later
                    // replay of the session's history.
                    self.settle_user(brief::strip(&text));
                }
            }
            Some(kind) => {
                if let Some(text) = text_of(&event.payload) {
                    self.settle(kind, &text);
                }
            }
            // The payload is an untyped `Value` and the kind vocabulary is the server's, so an
            // unrecognised kind is skipped rather than guessed at. `ATTACCA_DEBUG_EVENTS=1` is how
            // you find out what a deployment actually emits.
            None => {
                if debug_events {
                    self.push(
                        MsgKind::Sys,
                        &format!("· {} {}", event.kind, compact(&event.payload)),
                    );
                }
            }
        }
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Transcript::new()
    }
}

/// Map a durable event kind onto a card. Substring matching on purpose — the vocabulary lives
/// server-side, and a deployment that renames `assistant_message` to `assistant_text` should keep
/// rendering rather than fall silent.
fn classify(kind: &str) -> Option<MsgKind> {
    let kind = kind.to_ascii_lowercase();
    if kind.contains("tool") {
        // Checked before the rest so `assistant_tool_call` reads as a tool, not as prose.
        if kind.contains("result") || kind.contains("output") || kind.contains("return") {
            return Some(MsgKind::Result);
        }
        return Some(MsgKind::Tool);
    }
    if kind.contains("reasoning") || kind.contains("thinking") {
        return Some(MsgKind::Reasoning);
    }
    if kind.contains("assistant") {
        return Some(MsgKind::Agent);
    }
    if kind.contains("user") {
        return Some(MsgKind::User);
    }
    None
}

/// Pull display text out of an event payload, trying the spellings a message event plausibly uses.
fn text_of(payload: &Value) -> Option<String> {
    for key in ["text", "message", "content", "output", "result"] {
        match payload.get(key) {
            Some(Value::String(s)) if !s.trim().is_empty() => return Some(s.clone()),
            // Content-block arrays: concatenate the text parts and ignore the rest.
            Some(Value::Array(parts)) => {
                let joined = parts
                    .iter()
                    .filter_map(|p| match p {
                        Value::String(s) => Some(s.as_str()),
                        _ => p.get("text").and_then(Value::as_str),
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if !joined.trim().is_empty() {
                    return Some(joined);
                }
            }
            _ => {}
        }
    }
    payload
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn string_field(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|k| payload.get(k).and_then(Value::as_str))
        .find(|s| !s.trim().is_empty())
        .map(str::to_string)
}

fn tool_args(payload: &Value) -> String {
    let args = ["args", "input", "arguments", "params"]
        .iter()
        .filter_map(|k| payload.get(k))
        .find_map(Value::as_object);
    match args {
        Some(map) if !map.is_empty() => {
            let rendered = map
                .iter()
                .map(|(k, v)| match v {
                    Value::String(s) => format!("{k}={s}"),
                    other => format!("{k}={other}"),
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!(" {}", ellipsize(&rendered, 160))
        }
        _ => String::new(),
    }
}

fn compact(payload: &Value) -> String {
    ellipsize(&payload.to_string(), 120)
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// A session title carried by a durable event, if one is.
///
/// The kind vocabulary is the server's, so this goes by payload shape instead: any event carrying a
/// non-empty `title` is taken to be renaming the session. Attacca titles a session from its first
/// turn, so this is what makes that appear in the sidebar without asking.
pub fn title_in(frame: &ZTurnFrame) -> Option<String> {
    let ZTurnFrame::Event { event, .. } = frame else {
        return None;
    };
    event
        .payload
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// What the sidebar shows for a session title.
///
/// New sessions are created untitled so Attacca's title agent names them from their first message,
/// and until it does they read back under Attacca's own placeholder, which is shown as-is. The
/// `attacca-cli` case is legacy: this client used to send that as a title, permanently, which cost
/// every session it created a real name. Sessions from those builds still carry it.
fn display_title(title: Option<String>) -> String {
    match title {
        Some(t) if !t.trim().is_empty() && t != "attacca-cli" => t,
        _ => "untitled".to_string(),
    }
}

/// What an `ATTACCA_PROJECT` / `ATTACCA_AGENT` reference came to.
#[derive(Debug, PartialEq, Eq)]
pub enum RefChoice {
    /// Unset: the caller falls back to its own default.
    Unset,
    Resolved(String),
    /// Asked for, but matching no id or name.
    Unresolved(String),
}

/// Resolve an id-or-name reference against a `(id, name)` list.
///
/// A UUID resolves on its own without consulting `known`. That matters because `known` is filled in
/// asynchronously on connect: checking the list first meant a perfectly good id failed to resolve
/// during the round trip after launch, and the session quietly landed somewhere else. A name has no
/// choice but to wait for the list, since a name is only meaningful against it.
pub fn resolve_ref(wanted: &str, known: &[(String, String)]) -> RefChoice {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return RefChoice::Unset;
    }
    if looks_like_uuid(wanted) || known.iter().any(|(id, _)| id == wanted) {
        return RefChoice::Resolved(wanted.to_string());
    }
    match known
        .iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(wanted))
    {
        Some((id, _)) => RefChoice::Resolved(id.clone()),
        None => RefChoice::Unresolved(wanted.to_string()),
    }
}

/// A canonical dashed UUID, which is what Attacca ids are. Anything else still resolves through the
/// name list, so this only decides whether that list can be skipped.
fn looks_like_uuid(s: &str) -> bool {
    let mut parts = s.split('-');
    for len in [8usize, 4, 4, 4, 12] {
        match parts.next() {
            Some(p) if p.len() == len && p.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

fn env_str(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

// ── Main application state ─────────────────────────────────────

pub struct App {
    /// The chat transcript and its reducer.
    pub chat: Transcript,

    // Session
    pub sid: Option<String>,
    /// Session ID to restore on next server sessions list.
    pub pending_restore_sid: Option<String>,

    // Input
    pub input: String,
    /// Byte offset of the insertion point in `input`. Always a char boundary.
    pub input_cursor: usize,
    pub autocomplete_suggestions: Vec<String>,
    pub autocomplete_idx: Option<usize>,

    // Scrolling
    pub scroll: usize,
    pub at_end: bool,
    pub input_scroll: usize,
    /// Max top-row offset for the input box, in wrapped *visual* rows.
    /// Recomputed each frame in [`crate::ui`] and read by the input scroll
    /// handler so Ctrl+↑/↓ clamps against wrapped lines, not just `\n` lines.
    pub input_max_scroll: usize,
    /// Screen coordinate for the hardware cursor, set each frame.
    pub cursor_screen: Option<(u16, u16)>,

    // Sidebar
    pub sidebar_items: Vec<SidebarItem>,
    pub sidebar_sel: usize,
    pub sidebar_scroll: usize,
    pub expanded_projects: HashSet<String>,
    pub sessions: Vec<SessionRow>,

    // Data caches
    pub project_names: HashMap<String, String>,
    pub project_order: Vec<String>,
    pub agents: Vec<ZAgent>,
    pub me: Option<ZMe>,
    pub connected: bool,
    pub current_project_name: String,
    pub usage_model: String,
    pub usage_credits_used: String,
    pub usage_context_tokens: String,
    pub usage_input_tokens: String,
    pub usage_output_tokens: String,
    pub usage_total_tokens: String,

    // State
    pub exit_requested: bool,
    pub focus: Focus,
    busy_count: u32,
    pub debug_events: bool,
    /// Reasoning cards carry two rows of chrome each and reasoning is frequent, so a long chat can
    /// become mostly "thinking". `ATTACCA_HIDE_REASONING=1` drops them from the view.
    pub hide_reasoning: bool,
    /// Set by `/login`. Handled in the main loop rather than in the handler, because it has to
    /// suspend the terminal and await a person.
    pub login_requested: bool,
    /// The node runtime stopped for good; the loop draws once more and exits.
    pub node_stopped: Option<String>,
    /// Whether that stop is something a person has to fix, which decides the process exit code.
    pub node_needs_operator: bool,
    pub node_brief: NodeBrief,
    pub auth: Arc<Authenticator>,
    pub slot: ApiSlot,
    pub stream: Option<JoinHandle<()>>,

    // Channels
    pub bg_tx: BgTx,
    pub bg_rx: mpsc::UnboundedReceiver<BgEvent>,
    pub actions: Vec<Action>,
}

impl App {
    pub fn new(
        bg_tx: BgTx,
        bg_rx: mpsc::UnboundedReceiver<BgEvent>,
        slot: ApiSlot,
        auth: Arc<Authenticator>,
        node_brief: NodeBrief,
    ) -> Self {
        Self {
            chat: Transcript::new(),
            sid: None,
            pending_restore_sid: None,
            input: String::new(),
            input_cursor: 0,
            autocomplete_suggestions: vec![],
            autocomplete_idx: None,
            scroll: 0,
            at_end: true,
            input_scroll: 0,
            input_max_scroll: 0,
            cursor_screen: None,
            sidebar_items: vec![],
            sidebar_sel: 0,
            sidebar_scroll: 0,
            expanded_projects: HashSet::new(),
            sessions: vec![],
            project_names: HashMap::new(),
            project_order: vec![],
            agents: vec![],
            me: None,
            connected: false,
            current_project_name: String::new(),
            usage_model: String::new(),
            usage_credits_used: String::new(),
            usage_context_tokens: String::new(),
            usage_input_tokens: String::new(),
            usage_output_tokens: String::new(),
            usage_total_tokens: String::new(),
            exit_requested: false,
            focus: Focus::Chat,
            busy_count: 0,
            debug_events: std::env::var_os("ATTACCA_DEBUG_EVENTS").is_some(),
            hide_reasoning: std::env::var_os("ATTACCA_HIDE_REASONING").is_some(),
            login_requested: false,
            node_stopped: None,
            node_needs_operator: false,
            node_brief,
            auth,
            slot,
            stream: None,
            bg_tx,
            bg_rx,
            actions: vec![],
        }
    }

    /// Something is happening: a one-shot request, or a turn producing tokens. Drives the spinner.
    pub fn busy(&self) -> bool {
        self.busy_count > 0 || self.chat.running
    }

    /// One-shot requests only. The action queue gates on this rather than on [`busy`](Self::busy),
    /// or a follow-up message would be held back for the whole of a streaming turn.
    pub fn requests_in_flight(&self) -> bool {
        self.busy_count > 0
    }

    pub fn inc_busy(&mut self) {
        self.busy_count = self.busy_count.saturating_add(1);
    }

    pub fn dec_busy(&mut self) {
        self.busy_count = self.busy_count.saturating_sub(1);
    }

    /// Append a system line to the transcript.
    pub fn push_sys(&mut self, text: &str) {
        self.chat.push(MsgKind::Sys, text);
    }

    // ── Session state ──────────────────────────────────────────

    /// Point the UI at a session, keeping whatever is already on screen.
    ///
    /// Used for a session this process just created: the optimistic user echo must survive, and the
    /// replay from cursor 0 is empty anyway. The caller starts the feed.
    pub fn attach_session(&mut self, sid: String) {
        self.stop_stream();
        self.sid = Some(sid);
        self.chat.cur = 0;
        self.sync_current_project();
        self.rebuild_sidebar();
    }

    /// Switch to an existing session, clearing the view first.
    pub fn reset_for_session(&mut self, sid: &str) {
        self.chat = Transcript::new();
        self.scroll = 0;
        self.at_end = true;
        self.push_sys(&format!("session {}", short(sid)));
    }

    pub fn stop_stream(&mut self) {
        // Dropping the `Streaming` sends `s_cancel`, so the server stops producing for a session
        // nobody is looking at any more.
        if let Some(handle) = self.stream.take() {
            handle.abort();
        }
    }

    /// Push the open session's live state onto its sidebar row.
    ///
    /// `list_sessions` reports `running` only as of the moment it was called; the turn feed knows in
    /// real time, and this is the row it knows about.
    pub fn sync_open_row(&mut self) {
        let Some(sid) = self.sid.clone() else { return };
        let running = self.chat.running;
        let mut changed = false;
        for row in self.sessions.iter_mut() {
            if row.id == sid && row.running != running {
                row.running = running;
                changed = true;
            }
        }
        if changed {
            self.rebuild_sidebar();
        }
    }

    pub fn retitle_open_row(&mut self, title: &str) {
        let Some(sid) = self.sid.clone() else { return };
        let title = display_title(Some(title.to_string()));
        let mut changed = false;
        for row in self.sessions.iter_mut() {
            if row.id == sid && row.title != title {
                row.title = title.clone();
                changed = true;
            }
        }
        if changed {
            self.rebuild_sidebar();
        }
    }

    /// The project label in the input-box info bar, for whichever session is open.
    pub fn sync_current_project(&mut self) {
        let Some(sid) = self.sid.as_deref() else {
            self.current_project_name.clear();
            return;
        };
        self.current_project_name = self
            .sessions
            .iter()
            .find(|r| r.id == sid)
            .filter(|r| !r.project_id.is_empty())
            .map(|r| {
                self.project_names
                    .get(&r.project_id)
                    .cloned()
                    .unwrap_or_else(|| short(&r.project_id))
            })
            .unwrap_or_default();
    }

    pub fn replace_sessions(&mut self, sessions: Vec<ZSession>) {
        self.sessions = sessions
            .into_iter()
            .map(|s| SessionRow {
                project_id: s.project_id.unwrap_or_default(),
                title: display_title(s.title),
                id: s.id,
                running: s.running,
            })
            .collect();
        self.sync_current_project();
        self.rebuild_sidebar();
    }

    pub fn insert_session(&mut self, session: ZSession) {
        self.sessions.insert(
            0,
            SessionRow {
                project_id: session.project_id.unwrap_or_default(),
                title: display_title(session.title),
                id: session.id,
                running: session.running,
            },
        );
        self.sync_current_project();
        self.rebuild_sidebar();
    }

    pub fn apply_usage(&mut self, usage: &ZUsage) {
        let n = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_default();
        self.usage_model = usage.model.clone().unwrap_or_default();
        self.usage_credits_used = usage.credits_used.clone().unwrap_or_default();
        self.usage_context_tokens = n(usage.context_tokens);
        self.usage_input_tokens = n(usage.input_tokens);
        self.usage_output_tokens = n(usage.output_tokens);
        self.usage_total_tokens = n(usage.total_tokens);
    }

    // ── Session creation ───────────────────────────────────────

    /// The `(id, name)` pairs `ATTACCA_PROJECT` resolves against.
    fn project_refs(&self) -> Vec<(String, String)> {
        self.project_names
            .iter()
            .map(|(id, name)| (id.clone(), name.clone()))
            .collect()
    }

    fn agent_refs(&self) -> Vec<(String, String)> {
        self.agents
            .iter()
            .map(|a| (a.id.clone(), a.name.clone()))
            .collect()
    }

    /// The session about to be created, with any diagnostics posted, or `None` when there is no
    /// agent to create it against.
    ///
    /// A reference the user asked for but that resolves to nothing is called out rather than
    /// swallowed: silently substituting a default is how a typo puts a session somewhere surprising
    /// with nothing to explain it. Resolution runs only when a session is actually being created,
    /// so an ongoing conversation does not reprint the diagnostic on every message.
    pub fn new_session_spec(&mut self) -> Option<ZNewSession> {
        let project_id = match resolve_ref(&env_str("ATTACCA_PROJECT"), &self.project_refs()) {
            RefChoice::Resolved(id) => Some(id),
            RefChoice::Unset => None,
            RefChoice::Unresolved(wanted) => {
                self.push_sys(&format!(
                    "ATTACCA_PROJECT \"{wanted}\" matched no project — using the account default"
                ));
                None
            }
        };

        let agent_id = match resolve_ref(&env_str("ATTACCA_AGENT"), &self.agent_refs()) {
            RefChoice::Resolved(id) => Some(id),
            RefChoice::Unset => self.agents.first().map(|a| a.id.clone()),
            RefChoice::Unresolved(wanted) => {
                self.push_sys(&format!(
                    "ATTACCA_AGENT \"{wanted}\" matched no agent — using the first one"
                ));
                self.agents.first().map(|a| a.id.clone())
            }
        };

        Some(ZNewSession {
            agent_id: agent_id?,
            // Deliberately unset. Attacca names a session from its first message, in that message's
            // own language, and a title supplied here would be permanent and would opt the session
            // out of that for good — which is what the old `attacca-cli` placeholder cost.
            title: None,
            project_id,
            preamble: Some(self.node_brief.preamble()),
        })
    }

    // ── Sidebar ────────────────────────────────────────────────

    pub fn rebuild_sidebar(&mut self) {
        let mut order: Vec<String> = self.project_order.clone();
        for row in &self.sessions {
            if !order.contains(&row.project_id) {
                order.push(row.project_id.clone());
            }
        }

        let active_id = self.sid.clone().unwrap_or_default();
        self.sidebar_items.clear();
        for pid in &order {
            let sessions: Vec<&SessionRow> = self
                .sessions
                .iter()
                .filter(|s| &s.project_id == pid)
                .collect();
            if sessions.is_empty() && !self.project_names.contains_key(pid) {
                continue;
            }
            let name = match self.project_names.get(pid) {
                Some(name) => format!("📁 {name}"),
                None if pid.is_empty() => "📁 (no project)".to_string(),
                None => format!("📁 {}", short(pid)),
            };
            let expanded = self.expanded_projects.contains(pid.as_str());
            self.sidebar_items.push(SidebarItem::ProjectHeader {
                id: pid.clone(),
                name,
                expanded,
                session_count: sessions.len(),
            });
            if expanded {
                for s in sessions {
                    self.sidebar_items.push(SidebarItem::Session {
                        title: s.title.clone(),
                        id: s.id.clone(),
                        active: s.id == active_id,
                        running: s.running,
                    });
                }
            }
        }
        self.sidebar_items.push(SidebarItem::NewSession);
        let max = self.sidebar_items.len().saturating_sub(1);
        self.sidebar_sel = self.sidebar_sel.min(max);
        self.sidebar_scroll = self.sidebar_scroll.min(max.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zyris_attacca::ZDeltaKind;

    fn delta(kind: ZDeltaKind, text: &str) -> ZTurnFrame {
        ZTurnFrame::Delta {
            kind,
            text: text.to_string(),
        }
    }

    fn event(cursor: i64, kind: &str, payload: Value) -> ZTurnFrame {
        ZTurnFrame::Event {
            cursor,
            event: ZSessionEvent {
                seq: cursor,
                cursor,
                kind: kind.to_string(),
                payload,
                created_at: None,
            },
        }
    }

    fn msg(cursor: i64, kind: &str, text: &str) -> ZTurnFrame {
        event(cursor, kind, serde_json::json!({ "text": text }))
    }

    // ── Deltas and settling ────────────────────────────────────

    #[test]
    fn deltas_grow_a_single_card() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Assistant, "Hel"), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "lo!"), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].text, "Hello!");
        assert!(t.msgs[0].streaming);
    }

    #[test]
    fn a_durable_message_settles_the_streamed_card_instead_of_duplicating_it() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Assistant, "Hel"), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "lo!"), false);
        t.apply_frame(msg(3, "assistant_message", "Hello!"), false);

        assert_eq!(t.msgs.len(), 1, "{:?}", t.msgs);
        assert_eq!(t.msgs[0].text, "Hello!");
        assert!(!t.msgs[0].streaming);
        assert_eq!(t.cur, 3);
    }

    #[test]
    fn a_durable_message_with_no_deltas_is_appended() {
        let mut t = Transcript::new();
        t.apply_frame(msg(1, "assistant_message", "no deltas here"), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].kind, MsgKind::Agent);
        assert!(!t.msgs[0].streaming);
    }

    #[test]
    fn reasoning_does_not_merge_into_the_assistant_card() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Reasoning, "hmm…"), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "Hello!"), false);

        assert_eq!(t.msgs.len(), 2);
        assert_eq!(t.msgs[0].kind, MsgKind::Reasoning);
        assert_eq!(t.msgs[1].kind, MsgKind::Agent);
    }

    #[test]
    fn an_empty_first_delta_still_opens_a_card() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Assistant, ""), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "then text"), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].text, "then text");
    }

    #[test]
    fn status_stops_the_turn_and_settles_any_open_card() {
        let mut t = Transcript::new();
        t.apply_frame(ZTurnFrame::Status { running: true }, false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "half a th"), false);
        assert!(t.running);

        t.apply_frame(ZTurnFrame::Status { running: false }, false);
        assert!(!t.running);
        assert!(!t.msgs[0].streaming, "a stopped turn must not blink forever");
    }

    // ── The user echo ──────────────────────────────────────────

    #[test]
    fn the_optimistic_user_echo_is_not_duplicated_by_its_durable_event() {
        let mut t = Transcript::new();
        t.push(MsgKind::User, "what is in main.rs?");
        t.apply_frame(msg(1, "user_message", "what is in main.rs?"), false);

        assert_eq!(t.msgs.len(), 1, "{:?}", t.msgs);
    }

    #[test]
    fn a_replayed_user_message_with_no_echo_is_shown() {
        let mut t = Transcript::new();
        t.apply_frame(msg(1, "user_message", "from history"), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].kind, MsgKind::User);
    }

    #[test]
    fn the_node_brief_is_stripped_from_a_replayed_first_message() {
        let brief = NodeBrief {
            node_name: "box".into(),
            file_root: "/tmp".into(),
            terminal: true,
        };
        let sent = format!("{}{}", brief.preamble(), "what is in main.rs?");

        let mut t = Transcript::new();
        t.apply_frame(msg(1, "user_message", &sent), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].text, "what is in main.rs?");
    }

    // ── Cursors and replay ─────────────────────────────────────

    #[test]
    fn the_cursor_only_moves_forward() {
        let mut t = Transcript::new();
        t.apply_frame(msg(5, "assistant_message", "five"), false);
        t.apply_frame(msg(2, "assistant_message", "two"), false);

        assert_eq!(t.cur, 5);
    }

    #[test]
    fn a_replayed_boundary_event_is_dropped() {
        // An inclusive `after` re-delivers the event the subscription resumed from. Without the
        // skip, every turn boundary would re-render its last message.
        let mut t = Transcript::new();
        t.apply_frame(msg(3, "assistant_message", "Hello!"), false);
        assert_eq!(t.msgs.len(), 1);

        t.apply_frame(msg(3, "assistant_message", "Hello!"), false);
        assert_eq!(t.msgs.len(), 1, "{:?}", t.msgs);
    }

    // ── Tools and unknown kinds ────────────────────────────────

    #[test]
    fn tool_events_render_as_cards_and_results() {
        let mut t = Transcript::new();
        t.apply_frame(
            event(
                1,
                "assistant_tool_call",
                serde_json::json!({ "name": "read_file", "args": { "path": "src/main.rs" } }),
            ),
            false,
        );
        t.apply_frame(
            event(
                2,
                "tool_result",
                serde_json::json!({ "output": "fn main() {}" }),
            ),
            false,
        );

        assert_eq!(t.msgs[0].kind, MsgKind::Tool);
        assert!(t.msgs[0].text.contains("read_file"));
        assert!(t.msgs[0].text.contains("path=src/main.rs"));
        assert_eq!(t.msgs[1].kind, MsgKind::Result);
        assert_eq!(t.msgs[1].text, "fn main() {}");
    }

    #[test]
    fn an_unknown_kind_is_ignored_unless_debugging() {
        let mut quiet = Transcript::new();
        quiet.apply_frame(event(1, "sparkles", serde_json::json!({ "a": 1 })), false);
        assert!(quiet.msgs.is_empty());

        let mut loud = Transcript::new();
        loud.apply_frame(event(1, "sparkles", serde_json::json!({ "a": 1 })), true);
        assert_eq!(loud.msgs.len(), 1);
        assert!(loud.msgs[0].text.contains("sparkles"));
    }

    #[test]
    fn classify_covers_the_plausible_spellings() {
        assert_eq!(classify("assistant_message"), Some(MsgKind::Agent));
        assert_eq!(classify("ASSISTANT_TEXT"), Some(MsgKind::Agent));
        assert_eq!(classify("user_message"), Some(MsgKind::User));
        assert_eq!(classify("reasoning_delta"), Some(MsgKind::Reasoning));
        assert_eq!(classify("thinking"), Some(MsgKind::Reasoning));
        // `tool` wins over `assistant` so a tool call does not read as prose.
        assert_eq!(classify("assistant_tool_call"), Some(MsgKind::Tool));
        assert_eq!(classify("tool_result"), Some(MsgKind::Result));
        assert_eq!(classify("tool_output"), Some(MsgKind::Result));
        assert_eq!(classify("sparkles"), None);
    }

    #[test]
    fn text_of_tries_each_spelling_and_flattens_content_blocks() {
        assert_eq!(
            text_of(&serde_json::json!({ "message": "hi" })).as_deref(),
            Some("hi")
        );
        assert_eq!(
            text_of(&serde_json::json!({
                "content": [{ "type": "text", "text": "a" }, { "type": "image" }, "b"]
            }))
            .as_deref(),
            Some("ab")
        );
        assert_eq!(text_of(&serde_json::json!({ "text": "   " })), None);
        assert_eq!(text_of(&serde_json::json!({})), None);
    }

    // ── Titles ─────────────────────────────────────────────────

    #[test]
    fn a_title_in_an_event_payload_is_picked_up() {
        let frame = event(1, "anything", serde_json::json!({ "title": "  Rollout  " }));
        assert_eq!(title_in(&frame).as_deref(), Some("Rollout"));
    }

    #[test]
    fn a_frame_with_no_title_is_left_alone() {
        assert_eq!(title_in(&msg(1, "assistant_message", "hi")), None);
        assert_eq!(title_in(&delta(ZDeltaKind::Assistant, "hi")), None);
        assert_eq!(
            title_in(&event(1, "x", serde_json::json!({ "title": "  " }))),
            None
        );
    }

    #[test]
    fn display_title_shows_the_real_title_and_hides_the_legacy_placeholder() {
        assert_eq!(display_title(Some("Rollout".into())), "Rollout");
        assert_eq!(display_title(Some("attacca-cli".into())), "untitled");
        assert_eq!(display_title(Some("  ".into())), "untitled");
        assert_eq!(display_title(None), "untitled");
    }

    // ── Reference resolution ───────────────────────────────────

    fn known() -> Vec<(String, String)> {
        vec![(
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_string(),
            "Rollout".to_string(),
        )]
    }

    #[test]
    fn an_unset_reference_falls_through_to_the_default() {
        assert_eq!(resolve_ref("", &known()), RefChoice::Unset);
        assert_eq!(resolve_ref("   ", &known()), RefChoice::Unset);
    }

    #[test]
    fn a_name_resolves_case_insensitively_to_its_id() {
        assert_eq!(
            resolve_ref("rollout", &known()),
            RefChoice::Resolved("3f2504e0-4f89-11d3-9a0c-0305e82c3301".into())
        );
    }

    /// The list is filled asynchronously on connect, so a UUID must resolve before it arrives —
    /// otherwise the first session of a run silently lands somewhere else.
    #[test]
    fn a_uuid_resolves_without_the_list() {
        assert_eq!(
            resolve_ref("3f2504e0-4f89-11d3-9a0c-0305e82c3301", &[]),
            RefChoice::Resolved("3f2504e0-4f89-11d3-9a0c-0305e82c3301".into())
        );
    }

    #[test]
    fn a_reference_matching_nothing_is_reported_rather_than_substituted() {
        assert_eq!(
            resolve_ref("typo", &known()),
            RefChoice::Unresolved("typo".into())
        );
    }

    #[test]
    fn looks_like_uuid_accepts_only_the_canonical_form() {
        assert!(looks_like_uuid("3f2504e0-4f89-11d3-9a0c-0305e82c3301"));
        assert!(!looks_like_uuid("3f2504e04f8911d39a0c0305e82c3301"));
        assert!(!looks_like_uuid("3f2504e0-4f89-11d3-9a0c-0305e82c3301-extra"));
        assert!(!looks_like_uuid("zzzzzzzz-4f89-11d3-9a0c-0305e82c3301"));
        assert!(!looks_like_uuid("Rollout"));
    }
}
