use crate::auth::Authenticator;
use crate::brief::{self, NodeBrief};
use crate::util::short;
use crate::ui;
use crate::zyris_client::{
    spawn_session_stream, Api, ApiSlot, BgEvent, BgTx, SESSION_LIMIT,
};

use crossterm::event::MouseEventKind;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zyris_attacca::{
    AttaccaApi, ZAgent, ZDeltaKind, ZMe, ZNewSession, ZSessionEvent, ZSessionFilter, ZTurnFrame,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MsgKind {
    Sys,
    User,
    Agent,
    Reasoning,
    Tool,
    Result,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub kind: MsgKind,
    pub text: String,
    /// Still growing from token deltas. Rendered with a cursor and settled by the durable event.
    pub streaming: bool,
}

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

#[derive(Clone, Debug)]
pub struct SessionRow {
    pub project_id: String,
    pub title: String,
    pub id: String,
    pub running: bool,
}

enum Action {
    Send(String),
    Open(String),
    Create,
    Cancel,
    Logout,
}

/// The chat transcript and the turn-frame reducer that maintains it.
///
/// Split out from `App` so the reduction is testable without a terminal or a connection: every
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
                // whether the server treats `after` as exclusive or inclusive - otherwise an
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
                self.push(MsgKind::Tool, &format!("◆ {name}{}", tool_args(&event.payload)));
            }
            Some(MsgKind::Result) => {
                let text = text_of(&event.payload).unwrap_or_else(|| "ok".to_string());
                self.push(MsgKind::Result, &text);
            }
            Some(MsgKind::User) => {
                if let Some(text) = text_of(&event.payload) {
                    // The first message of a session this CLI started carries the node brief in
                    // front of it. Stripping here covers both the echo back during the turn and
                    // every later replay of the session's history.
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

/// Map a durable event kind onto a card. Substring matching on purpose - the vocabulary lives
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
fn title_in(frame: &ZTurnFrame) -> Option<String> {
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

/// What `ATTACCA_PROJECT` came to.
#[derive(Debug, PartialEq, Eq)]
enum ProjectChoice {
    /// Unset: the server files the session under the account's default project.
    AccountDefault,
    Resolved(String),
    /// Asked for, but matching no project id or name.
    Unresolved(String),
}

/// Resolve `ATTACCA_PROJECT` against the projects `list_projects` returned.
///
/// A UUID resolves on its own without consulting `known`. That matters because `known` is filled in
/// asynchronously on connect: checking the cache first meant a perfectly good project id failed to
/// resolve during the round trip after launch, and the session quietly landed in the default project.
/// A name has no choice but to wait for the cache, since a name is only meaningful against it.
fn resolve_project(wanted: &str, known: &HashMap<String, String>) -> ProjectChoice {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return ProjectChoice::AccountDefault;
    }
    if looks_like_uuid(wanted) || known.contains_key(wanted) {
        return ProjectChoice::Resolved(wanted.to_string());
    }
    match known
        .iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(wanted))
    {
        Some((id, _)) => ProjectChoice::Resolved(id.clone()),
        None => ProjectChoice::Unresolved(wanted.to_string()),
    }
}

/// A canonical dashed UUID, which is what Attacca ids are. Anything else still resolves through the
/// project cache, so this only decides whether the cache can be skipped.
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

fn enter_tui() {
    terminal::enable_raw_mode().ok();
    crossterm::execute!(
        io::stdout(),
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .ok();
}

fn leave_tui() {
    terminal::disable_raw_mode().ok();
    let _ = crossterm::execute!(
        io::stdout(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
}

pub struct App {
    pub chat: Transcript,
    pub sid: Option<String>,
    pub input: String,
    pub scroll: usize,
    pub at_end: bool,
    pub sidebar_items: Vec<SidebarItem>,
    pub sel: usize,
    pub sidebar_scroll: usize,
    pub sessions: Vec<SessionRow>,
    pub me: Option<ZMe>,
    pub connected: bool,
    pub focus: Focus,
    pub autocomplete_suggestions: Vec<String>,
    pub autocomplete_idx: Option<usize>,

    project_names: HashMap<String, String>,
    project_order: Vec<String>,
    agents: Vec<ZAgent>,
    expanded_projects: HashSet<String>,
    exit_requested: bool,
    /// One-shot requests in flight. Turn state comes from the stream, not from here.
    busy_count: u32,
    debug_events: bool,
    stream: Option<JoinHandle<()>>,
    auth: Arc<Authenticator>,
    /// Set by `/login`. Handled in the main loop rather than in `dispatch`, because it has to
    /// suspend the terminal and await a person.
    login_requested: bool,
    node_brief: NodeBrief,

    slot: ApiSlot,
    bg_tx: BgTx,
    bg_rx: mpsc::UnboundedReceiver<BgEvent>,
    actions: Vec<Action>,
}

#[derive(PartialEq)]
pub enum Focus {
    Chat,
    Sidebar,
}

const COMMANDS: [&str; 9] = [
    "/exit", "/help", "/sessions", "/new", "/cancel", "/login", "/whoami", "/logout", "/tools",
];

impl App {
    pub fn new(
        bg_tx: BgTx,
        bg_rx: mpsc::UnboundedReceiver<BgEvent>,
        slot: ApiSlot,
        auth: Arc<Authenticator>,
        node_brief: NodeBrief,
    ) -> App {
        App {
            chat: Transcript::new(),
            sid: None,
            input: String::new(),
            scroll: 0,
            at_end: true,
            sidebar_items: vec![],
            sel: 0,
            sidebar_scroll: 0,
            sessions: vec![],
            me: None,
            connected: false,
            focus: Focus::Chat,
            autocomplete_suggestions: vec![],
            autocomplete_idx: None,
            project_names: HashMap::new(),
            project_order: vec![],
            agents: vec![],
            expanded_projects: HashSet::new(),
            exit_requested: false,
            busy_count: 0,
            debug_events: std::env::var_os("ATTACCA_DEBUG_EVENTS").is_some(),
            stream: None,
            auth,
            login_requested: false,
            node_brief,
            slot,
            bg_tx,
            bg_rx,
            actions: vec![],
        }
    }

    /// Something is in flight: a one-shot request, or a turn producing tokens.
    pub fn busy(&self) -> bool {
        self.busy_count > 0 || self.chat.running
    }

    pub async fn run(&mut self) {
        enter_tui();
        let mut term = match Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout())) {
            Ok(t) => t,
            Err(_) => {
                eprintln!("term init failed");
                return;
            }
        };
        term.clear().ok();
        self.chat.push(
            MsgKind::Sys,
            "── attacca ── enter:send  tab:autocomplete  /help ──",
        );
        self.rebuild_sidebar();

        loop {
            if term.draw(|f| ui::draw(f, self)).is_err() {
                break;
            }
            if self.exit_requested {
                break;
            }

            if self.login_requested {
                self.login_requested = false;
                self.relogin(&mut term).await;
            }

            // drain ALL pending bg events
            while let Ok(ev) = self.bg_rx.try_recv() {
                self.apply_bg(ev);
            }

            // drain action queue
            while self.busy_count == 0 && !self.actions.is_empty() {
                let action = self.actions.remove(0);
                self.dispatch(action);
            }

            // consume ALL pending terminal events - zero-blocking poll loop
            // then sleep only when truly idle (no pending events at all)
            let mut had_event = false;
            loop {
                match event::poll(Duration::from_secs(0)) {
                    Ok(true) => {
                        had_event = true;
                        match event::read() {
                            Ok(Event::Key(k)) => {
                                if k.code == KeyCode::Char('c')
                                    && k.modifiers.contains(KeyModifiers::CONTROL)
                                {
                                    self.exit_requested = true;
                                    break;
                                }
                                if (k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat)
                                    && !self.handle_key(k.code)
                                {
                                    self.exit_requested = true;
                                    break;
                                }
                            }
                            Ok(Event::Mouse(m)) => {
                                match m.kind {
                                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                                        if m.column < 30 {
                                            self.focus = Focus::Sidebar;
                                            let list_row = m.row.saturating_sub(2) as usize;
                                            let idx = list_row + self.sidebar_scroll;
                                            if idx < self.sidebar_items.len() {
                                                self.sel = idx;
                                                self.activate_sidebar_selection();
                                            }
                                        } else {
                                            self.focus = Focus::Chat;
                                        }
                                    }
                                    MouseEventKind::ScrollDown => {
                                        const S: usize = 3;
                                        if m.column < 30 {
                                            self.sidebar_scroll = self
                                                .sidebar_scroll
                                                .saturating_add(S)
                                                .min(self.sidebar_items.len().saturating_sub(1));
                                            let mv = (12usize).min(self.sidebar_items.len());
                                            if self.sel < self.sidebar_scroll {
                                                self.sel = self.sidebar_scroll;
                                            }
                                            if self.sel >= self.sidebar_scroll + mv {
                                                self.sel = self.sidebar_scroll + mv - 1;
                                            }
                                        } else if !self.at_end {
                                            if self.scroll > S {
                                                self.scroll -= S;
                                            } else {
                                                self.at_end = true;
                                                self.scroll = 0;
                                            }
                                        }
                                    }
                                    MouseEventKind::ScrollUp => {
                                        const S: usize = 3;
                                        if m.column < 30 {
                                            self.sidebar_scroll = self.sidebar_scroll.saturating_sub(S);
                                            if self.sel >= self.sidebar_scroll + 12 {
                                                self.sel = self.sidebar_scroll + 11;
                                            }
                                        } else if self.at_end {
                                            self.at_end = false;
                                            self.scroll = S;
                                        } else {
                                            self.scroll = self.scroll.saturating_add(S);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Ok(_) => {}
                            Err(_) => {
                                self.exit_requested = true;
                                break;
                            }
                        }
                    }
                    Ok(false) => break, // no more pending events - exit spin loop
                    Err(_) => {
                        self.exit_requested = true;
                        break;
                    }
                }
                if self.exit_requested {
                    break;
                }
            }
            if self.exit_requested {
                break;
            }

            // if we processed an event, redraw immediately (loop next iter)
            // if no events, brief sleep to prevent 100% CPU
            if !had_event {
                tokio::time::sleep(Duration::from_millis(8)).await;
            }
        }
        self.stop_stream();
        // cleanup - runs on Ctrl+C or /exit
        leave_tui();
    }

    /// Re-enroll this node, interactively.
    ///
    /// The alternate screen is dropped for the duration because the device grant prints its code
    /// with `println!` - that is deliberate on zyris's part so the code survives `RUST_LOG=error`,
    /// and it means the only way to show it is to give the terminal back. The wait is unbounded by
    /// design: it ends when a person approves the code, or gives up.
    async fn relogin<B: ratatui::backend::Backend + io::Write>(
        &mut self,
        term: &mut Terminal<B>,
    ) {
        leave_tui();
        println!("\n── attacca: re-authenticating ──");
        println!("   asking for: {}", self.auth.scopes().join(", "));

        let outcome = self.auth.relogin().await;

        enter_tui();
        term.clear().ok();

        match outcome {
            Ok(prefix) => {
                self.chat
                    .push(MsgKind::Sys, &format!("logged in - credential {prefix}…"));
                // The runner only reads credentials when it dials, so the live connection has to go
                // for the new one to be presented. It redials after its backoff on its own.
                if let Some(live) = self.slot.get() {
                    live.conn.close("re-authenticating");
                    self.chat.push(MsgKind::Sys, "reconnecting…");
                }
            }
            Err(e) => {
                self.chat.push(MsgKind::Sys, &format!("login failed: {e}"));
                self.chat.push(
                    MsgKind::Sys,
                    "still connected on the previous credential, if there was one",
                );
            }
        }
    }

    // ── Background events ──

    fn apply_bg(&mut self, ev: BgEvent) {
        match ev {
            BgEvent::Connected(me) => {
                self.connected = true;
                self.chat.push(
                    MsgKind::Sys,
                    &format!("connected - {} <{}>", me.display_name, me.email),
                );
                self.me = Some(*me);
            }
            BgEvent::Disconnected(reason) => {
                self.connected = false;
                self.chat
                    .push(MsgKind::Sys, &format!("disconnected - {reason}"));
            }
            BgEvent::Projects(projects) => {
                self.project_names.clear();
                self.project_order.clear();
                for p in projects {
                    if p.is_default {
                        self.expanded_projects.insert(p.id.clone());
                    }
                    self.project_order.push(p.id.clone());
                    self.project_names.insert(p.id, p.name);
                }
                self.rebuild_sidebar();
            }
            BgEvent::Sessions(sessions) => {
                self.sessions = sessions
                    .into_iter()
                    .map(|s| SessionRow {
                        project_id: s.project_id.unwrap_or_default(),
                        title: display_title(s.title),
                        id: s.id,
                        running: s.running,
                    })
                    .collect();
                self.rebuild_sidebar();
                // This list replaces the rows wholesale, and its `running` is only true as of when
                // the request was made. The turn feed is more current, so it wins.
                self.sync_open_row();
            }
            BgEvent::Agents(agents) => {
                self.agents = agents;
            }
            BgEvent::SessionCreated(session) => {
                self.chat
                    .push(MsgKind::Sys, &format!("new session {}", short(&session.id)));
                self.attach_session(session.id.clone());
                self.sessions.insert(
                    0,
                    SessionRow {
                        project_id: session.project_id.unwrap_or_default(),
                        title: display_title(session.title),
                        id: session.id,
                        running: session.running,
                    },
                );
                self.rebuild_sidebar();
            }
            BgEvent::StreamHead {
                session_id,
                running,
            } => {
                if self.sid.as_deref() == Some(session_id.as_str()) {
                    self.chat.running = running;
                    self.sync_open_row();
                }
            }
            BgEvent::Frame { session_id, frame } => {
                // A stream task is aborted on session switch, but a frame already in the channel
                // can outlive the abort - so the session it belongs to is checked here too.
                if self.sid.as_deref() == Some(session_id.as_str()) {
                    // Read out of the frame before it is consumed by the reducer.
                    let title = title_in(&frame);
                    let was_running = self.chat.running;

                    self.chat.apply_frame(frame, self.debug_events);

                    if let Some(title) = title {
                        self.retitle_open_row(&title);
                    }
                    if self.chat.running != was_running {
                        self.sync_open_row();
                        // A turn ending is the moment a server-side auto-title lands, and the only
                        // push signal available for "the session list may have moved on". There is
                        // no account-wide event stream in `attacca_api` v1 to subscribe to instead.
                        if was_running && !self.chat.running {
                            self.refresh_sessions();
                        }
                    }
                }
            }
            BgEvent::Notice(text) => {
                self.chat.push(MsgKind::Sys, &text);
            }
            BgEvent::Done => {
                self.busy_count = self.busy_count.saturating_sub(1);
            }
        }
    }

    /// Push the open session's live state onto its sidebar row.
    ///
    /// `list_sessions` reports `running` only as of the moment it was called; the turn feed knows in
    /// real time, and this is the row it knows about.
    fn sync_open_row(&mut self) {
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

    fn retitle_open_row(&mut self, title: &str) {
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

    /// Re-read the session list. Event-driven, not on a timer — see the call site.
    fn refresh_sessions(&mut self) {
        self.spawn_rpc(move |api, tx| async move {
            match api
                .list_sessions(ZSessionFilter {
                    project_id: None,
                    limit: Some(SESSION_LIMIT),
                })
                .await
            {
                Ok(sessions) => {
                    let _ = tx.send(BgEvent::Sessions(sessions));
                }
                // Quiet on purpose: this runs off a turn ending, and a failure here costs a slightly
                // stale sidebar, which is not worth a line in the transcript every turn.
                Err(e) => tracing::warn!(error = %e, "list_sessions refresh failed"),
            }
        });
    }

    // ── Actions ──

    fn dispatch(&mut self, action: Action) {
        match action {
            Action::Send(text) => self.send(text),
            Action::Open(sid) => self.open_session(sid),
            Action::Create => self.create_session(),
            Action::Cancel => self.cancel_turn(),
            Action::Logout => self.logout(),
        }
    }

    /// Run a request against the live connection, guaranteeing exactly one `Done`.
    ///
    /// The single `Done` is what keeps `busy_count` honest across every early return - including
    /// the not-connected path, which has no task to run at all.
    fn spawn_rpc<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(Api, BgTx) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.busy_count += 1;
        let tx = self.bg_tx.clone();
        let Some(live) = self.slot.get() else {
            let _ = tx.send(BgEvent::Notice("not connected yet".into()));
            let _ = tx.send(BgEvent::Done);
            return;
        };
        tokio::spawn(async move {
            f(live.api, tx.clone()).await;
            let _ = tx.send(BgEvent::Done);
        });
    }

    /// Like [`spawn_rpc`](Self::spawn_rpc) for work that needs no connection.
    fn spawn_task<F, Fut>(&mut self, f: F)
    where
        F: FnOnce(BgTx) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.busy_count += 1;
        let tx = self.bg_tx.clone();
        tokio::spawn(async move {
            f(tx.clone()).await;
            let _ = tx.send(BgEvent::Done);
        });
    }

    fn send(&mut self, text: String) {
        let sid = self.sid.clone();
        // Only resolved when a session actually has to be created, so a project diagnostic is not
        // reprinted on every message of an ongoing conversation.
        let (agent_id, project_id) = match sid {
            Some(_) => (None, None),
            None => self.session_seed(),
        };

        let new_session = self.new_session_spec(agent_id, project_id);

        self.spawn_rpc(move |api, tx| async move {
            let sid = match sid {
                Some(sid) => sid,
                None => {
                    let Some(new_session) = new_session else {
                        let _ = tx.send(BgEvent::Notice(
                            "no agent available - set ATTACCA_AGENT or create one in Attacca".into(),
                        ));
                        return;
                    };
                    match api.create_session_with(new_session).await {
                        Ok(session) => {
                            let id = session.id.clone();
                            let _ = tx.send(BgEvent::SessionCreated(session));
                            id
                        }
                        Err(e) => {
                            let _ = tx.send(BgEvent::Notice(format!("create_session: {e}")));
                            return;
                        }
                    }
                }
            };
            if let Err(e) = api.send_message(sid, text, vec![]).await {
                let _ = tx.send(BgEvent::Notice(format!("send_message: {e}")));
            }
        });
    }

    fn create_session(&mut self) {
        // Clear before the request, and stop the old feed first: a stream still running for the
        // previous session would repopulate the transcript we just emptied.
        self.stop_stream();
        self.sid = None;
        self.chat = Transcript::new();
        self.scroll = 0;
        self.at_end = true;
        self.chat.push(MsgKind::Sys, "creating session…");
        self.rebuild_sidebar();

        let (agent_id, project_id) = self.session_seed();
        let new_session = self.new_session_spec(agent_id, project_id);
        self.spawn_rpc(move |api, tx| async move {
            let Some(new_session) = new_session else {
                let _ = tx.send(BgEvent::Notice(
                    "no agent available - set ATTACCA_AGENT or create one in Attacca".into(),
                ));
                return;
            };
            match api.create_session_with(new_session).await {
                Ok(session) => {
                    let _ = tx.send(BgEvent::SessionCreated(session));
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::Notice(format!("create_session: {e}")));
                }
            }
        });
    }

    fn cancel_turn(&mut self) {
        let Some(sid) = self.sid.clone() else {
            self.chat.push(MsgKind::Sys, "no session to cancel");
            return;
        };
        self.spawn_rpc(move |api, tx| async move {
            match api.cancel_turn(sid).await {
                Ok(()) => {
                    let _ = tx.send(BgEvent::Notice("turn cancelled".into()));
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::Notice(format!("cancel_turn: {e}")));
                }
            }
        });
    }

    fn logout(&mut self) {
        let auth = self.auth.clone();
        self.spawn_task(move |tx| async move {
            let msg = match auth.logout().await {
                Ok(()) => "credential cleared - /login now, or restart".to_string(),
                Err(e) => format!("logout: {e}"),
            };
            let _ = tx.send(BgEvent::Notice(msg));
        });
    }

    /// Point the UI at a session and start its turn feed, keeping whatever is already on screen.
    ///
    /// Used for a session this process just created: the optimistic user echo must survive, and the
    /// replay from cursor 0 is empty anyway.
    fn attach_session(&mut self, sid: String) {
        self.stop_stream();
        self.sid = Some(sid.clone());
        self.chat.cur = 0;
        self.rebuild_sidebar();
        self.stream = Some(spawn_session_stream(
            sid,
            self.slot.clone(),
            self.bg_tx.clone(),
        ));
    }

    /// Switch to an existing session. `after = 0` replays the durable log, so this is also the
    /// history load - there is no separate history request.
    fn open_session(&mut self, sid: String) {
        self.chat = Transcript::new();
        self.scroll = 0;
        self.at_end = true;
        self.chat
            .push(MsgKind::Sys, &format!("session {}", short(&sid)));
        self.attach_session(sid);
    }

    fn stop_stream(&mut self) {
        // Dropping the `Streaming` sends `s_cancel`, so the server stops producing for a session
        // nobody is looking at any more.
        if let Some(handle) = self.stream.take() {
            handle.abort();
        }
    }

    fn default_agent_id(&self) -> Option<String> {
        std::env::var("ATTACCA_AGENT")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| self.agents.first().map(|a| a.id.clone()))
    }

    /// The agent and project a session is about to be created under, with any diagnostics posted.
    ///
    /// A project the user asked for but that resolves to nothing is called out rather than swallowed:
    /// silently substituting the account default is how a typo puts a session somewhere surprising
    /// with nothing to explain it.
    fn session_seed(&mut self) -> (Option<String>, Option<String>) {
        let wanted = std::env::var("ATTACCA_PROJECT").unwrap_or_default();
        let project_id = match resolve_project(&wanted, &self.project_names) {
            ProjectChoice::Resolved(id) => Some(id),
            ProjectChoice::AccountDefault => None,
            ProjectChoice::Unresolved(wanted) => {
                self.chat.push(
                    MsgKind::Sys,
                    &format!(
                        "ATTACCA_PROJECT \"{wanted}\" matched no project — using the account default"
                    ),
                );
                None
            }
        };
        (self.default_agent_id(), project_id)
    }

    /// The session to create, or `None` when there is no agent to create it against.
    ///
    /// `title` is deliberately left unset. Attacca names a session from its first message, in that
    /// message's own language, and a title supplied here would be permanent and would opt the session
    /// out of that for good — so the placeholder this client used to send cost every session its real
    /// name.
    fn new_session_spec(
        &self,
        agent_id: Option<String>,
        project_id: Option<String>,
    ) -> Option<ZNewSession> {
        Some(ZNewSession {
            agent_id: agent_id?,
            title: None,
            project_id,
            // System instructions for this session alone, appended to the agent's own on every turn.
            // Better than the message prefix this used to be: it applies to the whole conversation
            // rather than one turn, and it never appears in the transcript.
            preamble: Some(self.node_brief.preamble()),
        })
    }

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
            let sessions: Vec<&SessionRow> =
                self.sessions.iter().filter(|s| &s.project_id == pid).collect();
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
        self.sel = self.sel.min(max);
        self.sidebar_scroll = self.sidebar_scroll.min(max.saturating_sub(1));
    }

    // ── Key handling ──

    fn handle_key(&mut self, code: KeyCode) -> bool {
        // Tab: cycle autocomplete suggestions if any, else toggle focus
        if code == KeyCode::Tab {
            if self.focus == Focus::Chat && !self.autocomplete_suggestions.is_empty() {
                self.cycle_autocomplete();
            } else {
                self.focus = match self.focus {
                    Focus::Chat => Focus::Sidebar,
                    Focus::Sidebar => Focus::Chat,
                };
            }
            return true;
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar(code),
            Focus::Chat => self.handle_chat(code),
        }
    }

    fn handle_sidebar(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Up => {
                if self.sel > 0 {
                    self.sel -= 1;
                    self.clamp_sidebar_scroll();
                }
            }
            KeyCode::Down => {
                let max = self.sidebar_items.len().saturating_sub(1);
                if self.sel < max {
                    self.sel += 1;
                    self.clamp_sidebar_scroll();
                }
            }
            KeyCode::Enter | KeyCode::Right => {
                if self.sel < self.sidebar_items.len() {
                    self.activate_sidebar_selection();
                }
            }
            KeyCode::Left => {
                for i in (0..self.sel).rev() {
                    if let SidebarItem::ProjectHeader { id, .. } = &self.sidebar_items[i] {
                        let id = id.clone();
                        self.expanded_projects.remove(&id);
                        self.rebuild_sidebar();
                        self.sel = i;
                        break;
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn handle_chat(&mut self, code: KeyCode) -> bool {
        const SCROLL_SPEED: usize = 3;
        // scroll = how many lines above the bottom we've scrolled
        match code {
            KeyCode::Up => {
                if self.at_end {
                    self.at_end = false;
                    self.scroll = SCROLL_SPEED;
                } else {
                    self.scroll = self.scroll.saturating_add(SCROLL_SPEED);
                }
            }
            KeyCode::Down => {
                if !self.at_end {
                    if self.scroll > SCROLL_SPEED {
                        self.scroll = self.scroll.saturating_sub(SCROLL_SPEED);
                    } else {
                        self.at_end = true;
                        self.scroll = 0;
                    }
                }
            }
            KeyCode::PageUp => {
                if self.at_end {
                    self.at_end = false;
                    self.scroll = 10;
                } else {
                    self.scroll = self.scroll.saturating_add(10);
                }
            }
            KeyCode::PageDown => {
                if !self.at_end {
                    if self.scroll > 10 {
                        self.scroll = self.scroll.saturating_sub(10);
                    } else {
                        self.at_end = true;
                        self.scroll = 0;
                    }
                }
            }
            KeyCode::Home => {
                self.at_end = false;
                self.scroll = 9999;
            }
            KeyCode::End => {
                self.at_end = true;
                self.scroll = 0;
            }
            KeyCode::Enter => {
                let m = self.input.trim().to_string();
                if !m.is_empty() {
                    for item in &self.sidebar_items {
                        if let SidebarItem::Session { title, id, .. } = item {
                            if title == &m {
                                let id = id.clone();
                                self.input.clear();
                                self.actions.push(Action::Open(id));
                                return true;
                            }
                        }
                    }
                    self.input.clear();
                    self.autocomplete_suggestions.clear();
                    self.autocomplete_idx = None;
                    if m.starts_with('/') {
                        return self.handle_command(&m);
                    }
                    self.chat.push(MsgKind::User, &m);
                    self.actions.push(Action::Send(m));
                }
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.update_autocomplete();
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.update_autocomplete();
            }
            _ => {}
        }
        true
    }

    /// `false` ends the program.
    fn handle_command(&mut self, cmd: &str) -> bool {
        match cmd {
            "/exit" | "/quit" => {
                self.exit_requested = true;
            }
            "/help" | "/h" => {
                self.chat.push(MsgKind::Sys, "── Commands ──────────────────────────");
                self.chat.push(MsgKind::Sys, "  /exit       Exit the program");
                self.chat.push(MsgKind::Sys, "  /help       Show this help");
                self.chat.push(MsgKind::Sys, "  /new        Create a new session");
                self.chat.push(MsgKind::Sys, "  /cancel     Stop the running turn");
                self.chat.push(MsgKind::Sys, "  /login      Authorize this node again");
                self.chat.push(MsgKind::Sys, "  /whoami     Show identity and granted scopes");
                self.chat.push(MsgKind::Sys, "  /logout     Forget this node's credential");
                self.chat.push(MsgKind::Sys, "  /tools      What the server announces");
                self.chat.push(MsgKind::Sys, "  /sessions   Toggle sidebar focus");
                self.chat.push(MsgKind::Sys, "");
                self.chat.push(MsgKind::Sys, "── Keys ─────────────────────────────");
                self.chat.push(MsgKind::Sys, "  Enter      Send message");
                self.chat.push(MsgKind::Sys, "  Tab        Focus sidebar / autocomplete");
                self.chat.push(MsgKind::Sys, "  ↑↓         Scroll chat history");
                self.chat.push(MsgKind::Sys, "  Ctrl+C     Exit");
            }
            "/new" | "/n" => self.actions.push(Action::Create),
            "/cancel" => self.actions.push(Action::Cancel),
            "/login" => {
                self.chat.push(MsgKind::Sys, "authorizing - see the terminal");
                self.login_requested = true;
            }
            "/logout" => self.actions.push(Action::Logout),
            "/sessions" => {
                self.focus = Focus::Sidebar;
            }
            // The sidebar can only push what `turn_events` carries, because that is the one stream
            // `attacca_api` v1 declares. If a deployment announces something account-wide, this is
            // where it would show up — the announced tool list is never compared against the crate's
            // declaration, so a newer server may offer more than `zyris-attacca` knows about.
            "/tools" => match self.slot.get() {
                Some(live) => {
                    for cap in live.conn.peer_descriptors() {
                        self.chat.push(
                            MsgKind::Sys,
                            &format!("{} v{}", cap.name, cap.version),
                        );
                        for tool in &cap.tools {
                            self.chat.push(
                                MsgKind::Sys,
                                &format!("  {} ({:?})", tool.name, tool.transfer),
                            );
                        }
                    }
                }
                None => self.chat.push(MsgKind::Sys, "not connected yet"),
            },
            "/whoami" => match &self.me {
                Some(me) => {
                    let text = format!("{} <{}>", me.display_name, me.email);
                    self.chat.push(MsgKind::Sys, &text);
                    let scopes = if me.scopes.is_empty() {
                        "none granted".to_string()
                    } else {
                        me.scopes.join(", ")
                    };
                    self.chat.push(MsgKind::Sys, &format!("scopes: {scopes}"));
                }
                None => self.chat.push(MsgKind::Sys, "not connected yet"),
            },
            other => {
                self.chat
                    .push(MsgKind::Sys, &format!("unknown command: {other}"));
            }
        }
        true
    }

    fn update_autocomplete(&mut self) {
        self.autocomplete_suggestions.clear();
        self.autocomplete_idx = None;
        let trimmed = self.input.trim();
        if trimmed.starts_with('/') && trimmed.len() > 1 {
            for cmd in &COMMANDS {
                if cmd.starts_with(trimmed) {
                    self.autocomplete_suggestions.push(cmd.to_string());
                }
            }
        }
    }

    fn cycle_autocomplete(&mut self) {
        let n = self.autocomplete_suggestions.len();
        if n == 0 {
            return;
        }
        let next = self.autocomplete_idx.map(|i| (i + 1) % n).unwrap_or(0);
        self.autocomplete_idx = Some(next);
        if let Some(cmd) = self.autocomplete_suggestions.get(next) {
            self.input = cmd.clone();
            self.input.push(' ');
        }
    }

    fn clamp_sidebar_scroll(&mut self) {
        let vis = 12usize;
        if self.sel < self.sidebar_scroll {
            self.sidebar_scroll = self.sel;
        } else if self.sel >= self.sidebar_scroll + vis {
            self.sidebar_scroll = self.sel.saturating_sub(vis) + 1;
        }
    }

    fn activate_sidebar_selection(&mut self) {
        if self.sel >= self.sidebar_items.len() {
            return;
        }
        match self.sidebar_items[self.sel].clone() {
            SidebarItem::ProjectHeader { id, expanded, .. } => {
                if expanded {
                    self.expanded_projects.remove(&id);
                } else {
                    self.expanded_projects.insert(id);
                }
                self.rebuild_sidebar();
            }
            SidebarItem::Session { id, .. } => {
                self.actions.push(Action::Open(id));
            }
            SidebarItem::NewSession => {
                self.actions.push(Action::Create);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    fn delta(kind: ZDeltaKind, text: &str) -> ZTurnFrame {
        ZTurnFrame::Delta {
            kind,
            text: text.to_string(),
        }
    }

    #[test]
    fn deltas_grow_a_single_card() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Assistant, "Hel"), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "lo, "), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "world"), false);

        assert_eq!(t.msgs.len(), 1, "each delta must not open its own card");
        assert_eq!(t.msgs[0].text, "Hello, world");
        assert_eq!(t.msgs[0].kind, MsgKind::Agent);
        assert!(t.msgs[0].streaming);
    }

    /// The bug this guards: appending the durable event after the deltas shows every reply twice.
    #[test]
    fn a_durable_message_settles_the_streamed_card_instead_of_duplicating_it() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Assistant, "Hel"), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "lo"), false);
        t.apply_frame(event(7, "assistant_message", json!({"text": "Hello"})), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].text, "Hello");
        assert!(!t.msgs[0].streaming);
        assert_eq!(t.cur, 7);
    }

    #[test]
    fn a_durable_message_with_no_deltas_is_appended() {
        let mut t = Transcript::new();
        t.apply_frame(event(3, "assistant_message", json!({"text": "cached"})), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].text, "cached");
        assert!(!t.msgs[0].streaming);
    }

    #[test]
    fn reasoning_does_not_merge_into_the_assistant_card() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Reasoning, "thinking…"), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "answer"), false);

        assert_eq!(t.msgs.len(), 2);
        assert_eq!(t.msgs[0].kind, MsgKind::Reasoning);
        assert_eq!(t.msgs[1].kind, MsgKind::Agent);
    }

    /// The user's message is echoed on send and then replayed as a durable event.
    #[test]
    fn the_optimistic_user_echo_is_not_duplicated_by_its_durable_event() {
        let mut t = Transcript::new();
        t.push(MsgKind::User, "hello there");
        t.apply_frame(event(1, "user_message", json!({"text": "hello there"})), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].kind, MsgKind::User);
    }

    /// The first message of a CLI-started session is sent as `brief + text`, and the durable event
    /// echoes the whole thing back. Only the person's own words may reach the screen — and they must
    /// still match the optimistic echo, or the message renders twice.
    #[test]
    fn the_node_brief_is_stripped_from_the_first_user_message() {
        let preamble = crate::brief::NodeBrief {
            node_name: "build-box".into(),
            file_root: "/tmp".into(),
            terminal: true,
        }
        .preamble();

        let mut t = Transcript::new();
        t.push(MsgKind::User, "what is in main.rs?");
        t.apply_frame(
            event(
                1,
                "user_message",
                json!({"text": format!("{preamble}what is in main.rs?")}),
            ),
            false,
        );

        assert_eq!(t.msgs.len(), 1, "{:?}", t.msgs);
        assert_eq!(t.msgs[0].text, "what is in main.rs?");
        assert!(
            !t.msgs[0].text.contains("build-box"),
            "the brief must not reach the screen"
        );
    }

    /// Replaying a briefed session from cursor 0, with no optimistic echo to match against.
    #[test]
    fn a_replayed_briefed_message_shows_only_the_users_text() {
        let preamble = crate::brief::NodeBrief {
            node_name: "build-box".into(),
            file_root: "/tmp".into(),
            terminal: false,
        }
        .preamble();

        let mut t = Transcript::new();
        t.apply_frame(
            event(1, "user_message", json!({"text": format!("{preamble}hello")})),
            false,
        );

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].text, "hello");
    }

    #[test]
    fn a_replayed_user_message_with_no_echo_is_shown() {
        let mut t = Transcript::new();
        t.apply_frame(event(1, "user_message", json!({"text": "from history"})), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].kind, MsgKind::User);
        assert_eq!(t.msgs[0].text, "from history");
    }

    #[test]
    fn status_stops_the_turn_and_settles_any_open_card() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Assistant, "partial"), false);
        t.apply_frame(ZTurnFrame::Status { running: true }, false);
        assert!(t.running);

        t.apply_frame(ZTurnFrame::Status { running: false }, false);
        assert!(!t.running);
        assert!(
            !t.msgs[0].streaming,
            "a card left streaming would keep showing a cursor forever"
        );
    }

    #[test]
    fn an_empty_first_delta_still_opens_a_card() {
        let mut t = Transcript::new();
        t.apply_frame(delta(ZDeltaKind::Assistant, ""), false);
        t.apply_frame(delta(ZDeltaKind::Assistant, "text"), false);

        assert_eq!(t.msgs.len(), 1);
        assert_eq!(t.msgs[0].text, "text");
    }

    #[test]
    fn the_cursor_only_moves_forward() {
        let mut t = Transcript::new();
        t.apply_frame(event(9, "assistant_message", json!({"text": "a"})), false);
        t.apply_frame(event(4, "assistant_message", json!({"text": "b"})), false);
        assert_eq!(t.cur, 9, "a resume must not rewind onto an older cursor");
        assert_eq!(t.msgs.len(), 1, "an already-applied cursor must not render again");
    }

    /// Every turn boundary re-subscribes from the last cursor seen. If the server treats `after` as
    /// inclusive, the boundary event arrives a second time and must not be rendered twice.
    #[test]
    fn a_replayed_boundary_event_is_dropped() {
        let mut t = Transcript::new();
        t.apply_frame(event(7, "assistant_message", json!({"text": "first turn"})), false);
        t.apply_frame(ZTurnFrame::Status { running: false }, false);
        // …re-subscribe with after = 7, and the server echoes cursor 7 back.
        t.apply_frame(event(7, "assistant_message", json!({"text": "first turn"})), false);
        t.apply_frame(event(8, "assistant_message", json!({"text": "second turn"})), false);

        assert_eq!(t.msgs.len(), 2);
        assert_eq!(t.msgs[0].text, "first turn");
        assert_eq!(t.msgs[1].text, "second turn");
    }

    #[test]
    fn tool_events_render_as_cards_and_results() {
        let mut t = Transcript::new();
        t.apply_frame(
            event(1, "tool_call", json!({"name": "exec", "args": {"cmd": "ls"}})),
            false,
        );
        t.apply_frame(event(2, "tool_result", json!({"output": "a\nb"})), false);

        assert_eq!(t.msgs[0].kind, MsgKind::Tool);
        assert!(t.msgs[0].text.contains("exec"), "{}", t.msgs[0].text);
        assert!(t.msgs[0].text.contains("cmd=ls"), "{}", t.msgs[0].text);
        assert_eq!(t.msgs[1].kind, MsgKind::Result);
        assert_eq!(t.msgs[1].text, "a\nb");
    }

    #[test]
    fn an_unknown_kind_is_ignored_unless_debugging() {
        let mut t = Transcript::new();
        t.apply_frame(event(1, "session_renamed", json!({"title": "x"})), false);
        assert!(t.msgs.is_empty());
        assert_eq!(t.cur, 1, "an ignored event still advances the resume cursor");

        let mut debug = Transcript::new();
        debug.apply_frame(event(1, "session_renamed", json!({"title": "x"})), true);
        assert_eq!(debug.msgs.len(), 1);
        assert_eq!(debug.msgs[0].kind, MsgKind::Sys);
        assert!(debug.msgs[0].text.contains("session_renamed"));
    }

    #[test]
    fn classify_covers_the_plausible_spellings() {
        assert_eq!(classify("assistant_message"), Some(MsgKind::Agent));
        assert_eq!(classify("AssistantText"), Some(MsgKind::Agent));
        assert_eq!(classify("user_message"), Some(MsgKind::User));
        assert_eq!(classify("reasoning_block"), Some(MsgKind::Reasoning));
        assert_eq!(classify("thinking"), Some(MsgKind::Reasoning));
        // A tool call reads as a tool even when the kind also names the assistant.
        assert_eq!(classify("assistant_tool_call"), Some(MsgKind::Tool));
        assert_eq!(classify("tool_result"), Some(MsgKind::Result));
        assert_eq!(classify("tool_output"), Some(MsgKind::Result));
        assert_eq!(classify("turn_started"), None);
    }

    /// Attacca titles a session from its first turn; this is what makes the new name reach the
    /// sidebar without anyone asking for it.
    #[test]
    fn a_title_in_an_event_payload_is_picked_up() {
        assert_eq!(
            title_in(&event(1, "session_titled", json!({"title": "Rollout plan"}))).as_deref(),
            Some("Rollout plan")
        );
        // Any event carrying a title counts — the kind vocabulary is the server's.
        assert_eq!(
            title_in(&event(2, "session_updated", json!({"title": "Renamed"}))).as_deref(),
            Some("Renamed")
        );
    }

    #[test]
    fn a_frame_with_no_title_is_left_alone() {
        assert_eq!(title_in(&event(1, "assistant_message", json!({"text": "hi"}))), None);
        assert_eq!(title_in(&event(2, "session_titled", json!({"title": "  "}))), None);
        assert_eq!(
            title_in(&delta(ZDeltaKind::Assistant, "not an event")),
            None
        );
        assert_eq!(title_in(&ZTurnFrame::Status { running: false }), None);
    }

    #[test]
    fn display_title_shows_the_real_title_and_hides_the_legacy_placeholder() {
        assert_eq!(display_title(Some("Rollout".into())), "Rollout");
        // A title this client used to send at creation, which older sessions still carry.
        assert_eq!(display_title(Some("attacca-cli".into())), "untitled");
        assert_eq!(display_title(Some("   ".into())), "untitled");
        assert_eq!(display_title(None), "untitled");
    }

    fn projects() -> HashMap<String, String> {
        HashMap::from([(
            "5f2b1c40-1111-4222-8333-444455556666".to_string(),
            "Rollout".to_string(),
        )])
    }

    #[test]
    fn an_unset_project_falls_to_the_account_default() {
        assert_eq!(resolve_project("", &projects()), ProjectChoice::AccountDefault);
        assert_eq!(
            resolve_project("   ", &projects()),
            ProjectChoice::AccountDefault
        );
    }

    #[test]
    fn a_project_name_resolves_case_insensitively() {
        assert_eq!(
            resolve_project("rollout", &projects()),
            ProjectChoice::Resolved("5f2b1c40-1111-4222-8333-444455556666".into())
        );
    }

    /// The race this closes: `known` is populated by `list_projects` on connect, so during the round
    /// trip after launch it is empty. A project id must still resolve then, or the first session of
    /// the run silently lands in the account default.
    #[test]
    fn a_uuid_resolves_without_the_project_cache() {
        let empty = HashMap::new();
        assert_eq!(
            resolve_project("5f2b1c40-1111-4222-8333-444455556666", &empty),
            ProjectChoice::Resolved("5f2b1c40-1111-4222-8333-444455556666".into())
        );
        // A name cannot: it is only meaningful against the cache.
        assert_eq!(
            resolve_project("Rollout", &empty),
            ProjectChoice::Unresolved("Rollout".into())
        );
    }

    /// A typo must be reported, not silently answered with the default project.
    #[test]
    fn an_unmatched_project_is_reported_rather_than_swallowed() {
        assert_eq!(
            resolve_project("Rollowt", &projects()),
            ProjectChoice::Unresolved("Rollowt".into())
        );
    }

    #[test]
    fn looks_like_uuid_accepts_only_the_canonical_form() {
        assert!(looks_like_uuid("5f2b1c40-1111-4222-8333-444455556666"));
        assert!(looks_like_uuid("FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF"));
        assert!(!looks_like_uuid("5f2b1c40111142228333444455556666"));
        assert!(!looks_like_uuid("5f2b1c40-1111-4222-8333-44445555666"));
        assert!(!looks_like_uuid("5f2b1c40-1111-4222-8333-444455556666-7"));
        assert!(!looks_like_uuid("zzzzzzzz-1111-4222-8333-444455556666"));
        assert!(!looks_like_uuid("Rollout"));
        assert!(!looks_like_uuid(""));
    }

    #[test]
    fn text_of_tries_each_spelling_and_flattens_content_blocks() {
        assert_eq!(text_of(&json!({"text": "a"})).as_deref(), Some("a"));
        assert_eq!(text_of(&json!({"message": "b"})).as_deref(), Some("b"));
        assert_eq!(text_of(&json!({"content": "c"})).as_deref(), Some("c"));
        assert_eq!(
            text_of(&json!({"content": [{"type": "text", "text": "x"}, {"type": "text", "text": "y"}]}))
                .as_deref(),
            Some("xy")
        );
        assert_eq!(text_of(&json!("bare")).as_deref(), Some("bare"));
        assert_eq!(text_of(&json!({"text": "   "})), None);
        assert_eq!(text_of(&json!({"other": 1})), None);
    }
}
