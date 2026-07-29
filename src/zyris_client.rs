use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use zyris::{Connection, Streaming};
use zyris_attacca::{
    AttaccaApi, AttaccaApiClient, ZAgent, ZHistoryQuery, ZMe, ZProject, ZSession, ZSessionFilter,
    ZTurnFrame, ZTurnStatus,
};

/// The server announces `attacca_api` immediately after the handshake; this is generous headroom.
const CONSUME_WAIT: Duration = Duration::from_secs(5);
/// How long to wait before re-subscribing after a `turn_events` stream that delivered something.
const RESUBSCRIBE_MIN: Duration = Duration::from_millis(500);
/// Ceiling for the same wait when subscriptions keep coming back empty. A deployment that closes the
/// stream immediately on an idle session would otherwise have this re-dialling twice a second
/// forever, which is the polling this client exists to be rid of.
const RESUBSCRIBE_MAX: Duration = Duration::from_secs(15);
/// How many sessions the sidebar asks for. Shared by the fetch on connect and every later refresh,
/// so the two can never disagree about how much of the list they are looking at.
pub const SESSION_LIMIT: u32 = 200;

pub const DEFAULT_SCOPES: [&str; 5] = [
    "agents:read",
    "projects:read",
    "sessions:read",
    "sessions:write",
    "events:read",
];

pub type Api = Arc<AttaccaApiClient>;

/// One live connection and the `attacca_api` client riding on it.
#[derive(Clone)]
pub struct Live {
    pub conn_id: String,
    pub conn: Connection,
    pub api: Api,
}

/// The current connection, republished on every reconnect.
///
/// A `watch` rather than a mutex because the session stream supervisor needs to *await* a fresh
/// client after a disconnect rather than poll for one.
#[derive(Clone)]
pub struct ApiSlot(Arc<watch::Sender<Option<Live>>>);

impl ApiSlot {
    pub fn new() -> ApiSlot {
        let (tx, _rx) = watch::channel(None);
        ApiSlot(Arc::new(tx))
    }

    pub fn set(&self, live: Live) {
        self.0.send_replace(Some(live));
    }

    /// Clear only if the slot still holds *this* connection.
    ///
    /// A blind clear would race a reconnect: the outgoing connection's close handler and the
    /// runner's dial loop wake on the same event, so a late clear could erase the credential the
    /// new connection just published and leave every stream parked until the connection after it.
    pub fn clear_if(&self, conn_id: &str) {
        self.0.send_if_modified(|cur| {
            if cur.as_ref().is_some_and(|l| l.conn_id == conn_id) {
                *cur = None;
                true
            } else {
                false
            }
        });
    }

    pub fn get(&self) -> Option<Live> {
        self.0.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Live>> {
        self.0.subscribe()
    }
}

impl Default for ApiSlot {
    fn default() -> Self {
        ApiSlot::new()
    }
}

/// Everything the background half tells the UI.
pub enum BgEvent {
    Connected(Box<ZMe>),
    Disconnected(String),
    Projects(Vec<ZProject>),
    Sessions(Vec<ZSession>),
    Agents(Vec<ZAgent>),
    SessionCreated(ZSession),
    /// The `turn_events` head: the session's state at subscribe time. The head's `last_cursor` stays
    /// in the supervisor, which is the only thing that acts on it.
    StreamHead { session_id: String, running: bool },
    Frame { session_id: String, frame: ZTurnFrame },
    Notice(String),
    /// One in-flight request finished, whatever its outcome.
    Done,
}

pub type BgTx = mpsc::UnboundedSender<BgEvent>;

/// Run once per established connection, concurrently with the connection itself.
///
/// Idempotent by construction: everything it emits replaces UI state rather than appending to it,
/// which is what a reconnect needs since the runner re-runs this hook for every connection.
pub async fn on_connect(conn: Connection, tx: BgTx, slot: ApiSlot) {
    let conn_id = conn.info().conn_id.clone();

    let api = match conn.wait_capability::<AttaccaApiClient>(CONSUME_WAIT).await {
        Ok(api) => Arc::new(api),
        Err(e) => {
            let _ = tx.send(BgEvent::Notice(format!(
                "server did not announce attacca_api: {e}"
            )));
            return;
        }
    };

    // Published before the metadata fanout so parked session streams resume immediately rather
    // than waiting on three round-trips they do not depend on.
    slot.set(Live {
        conn_id: conn_id.clone(),
        conn: conn.clone(),
        api: api.clone(),
    });

    let (me, projects, sessions, agents) = tokio::join!(
        api.me(),
        api.list_projects(),
        api.list_sessions(ZSessionFilter {
            project_id: None,
            limit: Some(SESSION_LIMIT),
        }),
        api.list_agents(),
    );

    match me {
        Ok(me) => {
            let _ = tx.send(BgEvent::Connected(Box::new(me)));
        }
        Err(e) => {
            let _ = tx.send(BgEvent::Notice(format!("me: {e}")));
        }
    }
    match projects {
        Ok(projects) => {
            let _ = tx.send(BgEvent::Projects(projects));
        }
        Err(e) => {
            let _ = tx.send(BgEvent::Notice(format!("list_projects: {e}")));
        }
    }
    match sessions {
        Ok(sessions) => {
            let _ = tx.send(BgEvent::Sessions(sessions));
        }
        Err(e) => {
            let _ = tx.send(BgEvent::Notice(format!("list_sessions: {e}")));
        }
    }
    match agents {
        Ok(agents) => {
            let _ = tx.send(BgEvent::Agents(agents));
        }
        Err(e) => {
            let _ = tx.send(BgEvent::Notice(format!("list_agents: {e}")));
        }
    }

    let reason = conn.closed().await;
    slot.clear_if(&conn_id);
    let _ = tx.send(BgEvent::Disconnected(reason.to_string()));
}

/// Follow a session: read what has already happened, then stay subscribed to what happens next.
///
/// The two calls divide cleanly, and the difference in how they read `after` is the whole reason to
/// use both. `session_history` with no `after` is the entire timeline, which is how a session opens;
/// `turn_events` with no `after` is live frames only. So history catches up and the stream takes over,
/// and each reconnect repeats the pair — the catch-up fetch closes whatever gap the disconnection
/// left, because `wait_for_api` parks the task rather than letting it die with its connection.
pub fn spawn_session_stream(session_id: String, slot: ApiSlot, tx: BgTx) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = slot.subscribe();
        let mut last_cursor = 0i64;
        // A failure that will not clear - no `events:read` in the grant, say - would otherwise post
        // a chat line every retry forever. Say it once per run of failures.
        let mut reported = false;
        let mut backoff = RESUBSCRIBE_MIN;

        loop {
            let api = wait_for_api(&mut rx).await;

            // Everything up to now, or everything missed since the last cursor seen.
            let query = ZHistoryQuery {
                after: (last_cursor > 0).then_some(last_cursor),
                limit: None,
            };
            match api.session_history(session_id.clone(), query).await {
                Ok(events) => {
                    for event in events {
                        last_cursor = last_cursor.max(event.cursor);
                        let _ = tx.send(BgEvent::Frame {
                            session_id: session_id.clone(),
                            frame: ZTurnFrame::Event {
                                cursor: event.cursor,
                                event,
                            },
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, session = %session_id, "session_history failed");
                    if !reported {
                        reported = true;
                        let _ = tx.send(BgEvent::Notice(format!("history: {e}")));
                    }
                }
            }

            match api
                .turn_events(session_id.clone(), Some(last_cursor))
                .await
            {
                Ok(stream) => {
                    reported = false;
                    let _ = tx.send(BgEvent::StreamHead {
                        session_id: session_id.clone(),
                        running: stream.head.running,
                    });

                    let drained = drain(stream, &session_id, &tx).await;
                    last_cursor = last_cursor.max(drained.max_cursor);

                    if drained.events > 0 {
                        backoff = RESUBSCRIBE_MIN;
                    } else {
                        backoff = (backoff * 2).min(RESUBSCRIBE_MAX);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, session = %session_id, "turn_events subscribe failed");
                    if !reported {
                        reported = true;
                        let _ = tx.send(BgEvent::Notice(format!("stream: {e}")));
                    }
                    backoff = (backoff * 2).min(RESUBSCRIBE_MAX);
                }
            }

            tokio::time::sleep(backoff).await;
        }
    })
}

struct Drained {
    events: usize,
    max_cursor: i64,
}

/// Forward one subscription's items to the UI until it ends.
async fn drain(
    mut stream: Streaming<ZTurnStatus, ZTurnFrame>,
    session_id: &str,
    tx: &BgTx,
) -> Drained {
    let mut out = Drained {
        events: 0,
        max_cursor: 0,
    };
    while let Some(item) = stream.items.next().await {
        match item {
            Ok(frame) => {
                if let ZTurnFrame::Event { cursor, .. } = &frame {
                    out.events += 1;
                    out.max_cursor = out.max_cursor.max(*cursor);
                }
                let _ = tx.send(BgEvent::Frame {
                    session_id: session_id.to_string(),
                    frame,
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, session = %session_id, "turn_events failed");
                break;
            }
        }
    }
    out
}

/// Park until a connection is live, then hand back its client.
async fn wait_for_api(rx: &mut watch::Receiver<Option<Live>>) -> Api {
    loop {
        if let Some(live) = rx.borrow_and_update().clone() {
            return live.api;
        }
        if rx.changed().await.is_err() {
            // The slot is gone, so the process is shutting down. Park rather than spin.
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{MsgKind, Transcript};
    use std::sync::Mutex;
    use zyris::{Datum, Node, NodeKind, Streaming};
    use zyris_attacca::{
        AttaccaApiServer, ZDeltaKind, ZNewAgent, ZNewSession, ZScope, ZSessionEvent, ZSessionFilter,
        ZTurnStatus,
    };

    /// A stub deployment shaped like the real one: a durable timeline `session_history` reads back,
    /// and a `turn_events` that carries only what happens from now on.
    struct StubApi {
        /// Every `after` `session_history` was queried with, in order.
        history_queries: Arc<Mutex<Vec<Option<i64>>>>,
        /// Sessions created through `create_session_with`.
        created: Arc<Mutex<Vec<ZNewSession>>>,
        /// Whether `turn_events` produces a turn, or just a head on an idle session.
        live_turn: bool,
    }

    #[derive(Clone, Default)]
    struct Spy {
        history_queries: Arc<Mutex<Vec<Option<i64>>>>,
        created: Arc<Mutex<Vec<ZNewSession>>>,
    }

    impl StubApi {
        fn new(live_turn: bool) -> (StubApi, Spy) {
            let spy = Spy::default();
            (
                StubApi {
                    history_queries: spy.history_queries.clone(),
                    created: spy.created.clone(),
                    live_turn,
                },
                spy,
            )
        }
    }

    fn durable(cursor: i64, kind: &str, text: &str) -> ZSessionEvent {
        ZSessionEvent {
            seq: cursor,
            cursor,
            kind: kind.to_string(),
            payload: serde_json::json!({ "text": text }),
            created_at: None,
        }
    }

    #[zyris::async_trait]
    impl AttaccaApi for StubApi {
        async fn me(&self) -> zyris::Result<ZMe> {
            Ok(ZMe {
                user_id: "u1".into(),
                email: "ada@example.com".into(),
                display_name: "Ada".into(),
                scopes: ZScope::ALL.iter().map(|s| s.as_str().to_string()).collect(),
            })
        }
        async fn list_agents(&self) -> zyris::Result<Vec<ZAgent>> {
            Ok(vec![ZAgent {
                id: "agent-1".into(),
                name: "Researcher".into(),
                description: None,
                model: None,
            }])
        }
        async fn create_agent(&self, _agent: ZNewAgent) -> zyris::Result<ZAgent> {
            unimplemented!()
        }
        async fn list_projects(&self) -> zyris::Result<Vec<ZProject>> {
            Ok(vec![ZProject {
                id: "p1".into(),
                name: "Default".into(),
                description: None,
                is_default: true,
            }])
        }
        async fn list_sessions(&self, _filter: ZSessionFilter) -> zyris::Result<Vec<ZSession>> {
            Ok(vec![ZSession {
                id: "s1".into(),
                title: Some("Rollout".into()),
                agent_id: Some("agent-1".into()),
                project_id: Some("p1".into()),
                running: false,
                preamble: None,
            }])
        }
        async fn create_session(
            &self,
            agent_id: String,
            title: Option<String>,
            project_id: Option<String>,
        ) -> zyris::Result<ZSession> {
            Ok(ZSession {
                id: "s2".into(),
                title,
                agent_id: Some(agent_id),
                project_id,
                running: false,
                preamble: None,
            })
        }
        async fn create_session_with(&self, session: ZNewSession) -> zyris::Result<ZSession> {
            self.created.lock().unwrap().push(session.clone());
            Ok(ZSession {
                id: "s2".into(),
                title: session.title,
                agent_id: Some(session.agent_id),
                project_id: session.project_id,
                running: false,
                preamble: session.preamble,
            })
        }
        async fn session_history(
            &self,
            _session_id: String,
            query: ZHistoryQuery,
        ) -> zyris::Result<Vec<ZSessionEvent>> {
            self.history_queries.lock().unwrap().push(query.after);
            let all = vec![
                durable(1, "user_message", "what is in main.rs?"),
                durable(2, "assistant_message", "a main function"),
            ];
            Ok(match query.after {
                Some(after) => all.into_iter().filter(|e| e.cursor > after).collect(),
                None => all,
            })
        }
        async fn send_message(
            &self,
            _session_id: String,
            _message: String,
            _data: Vec<Datum>,
        ) -> zyris::Result<()> {
            Ok(())
        }
        async fn cancel_turn(&self, _session_id: String) -> zyris::Result<()> {
            Ok(())
        }

        async fn turn_events(
            &self,
            session_id: String,
            after: Option<i64>,
        ) -> zyris::Result<Streaming<ZTurnStatus, ZTurnFrame>> {
            let head = ZTurnStatus {
                session_id,
                running: self.live_turn,
                last_cursor: after,
            };
            if !self.live_turn {
                // An idle session: a head, and nothing further until something happens.
                return Ok(Streaming::new(head, futures_util::stream::empty()));
            }
            let frames = vec![
                Ok(ZTurnFrame::Delta {
                    kind: ZDeltaKind::Assistant,
                    text: "Hel".into(),
                }),
                Ok(ZTurnFrame::Delta {
                    kind: ZDeltaKind::Assistant,
                    text: "lo!".into(),
                }),
                Ok(ZTurnFrame::Event {
                    cursor: 3,
                    event: durable(3, "assistant_message", "Hello!"),
                }),
                Ok(ZTurnFrame::Status { running: false }),
            ];
            Ok(Streaming::new(head, futures_util::stream::iter(frames)))
        }
    }

    /// A live slot over an in-memory duplex - no server, no credential, no network.
    async fn connected_slot() -> (ApiSlot, Connection) {
        connected_slot_with(true).await.0
    }

    async fn connected_slot_with(live_turn: bool) -> ((ApiSlot, Connection), Spy) {
        let (stub, spy) = StubApi::new(live_turn);
        let server = Node::builder()
            .name("attacca")
            .kind(NodeKind::Service)
            .capability(AttaccaApiServer(stub))
            .build()
            .unwrap();
        let node = Node::builder()
            .name("cli")
            .kind(NodeKind::Cli)
            .build()
            .unwrap();

        let (_server_side, node_side) = zyris::testing::duplex(&server, &node).await.unwrap();
        let api: AttaccaApiClient = node_side
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();

        let slot = ApiSlot::new();
        slot.set(Live {
            conn_id: node_side.info().conn_id.clone(),
            conn: node_side.clone(),
            api: Arc::new(api),
        });
        ((slot, node_side), spy)
    }

    /// Collect background events until `stop` is satisfied, reducing frames into a transcript.
    async fn collect(
        rx: &mut mpsc::UnboundedReceiver<BgEvent>,
        chat: &mut Transcript,
        mut stop: impl FnMut(&Transcript) -> bool,
    ) {
        for _ in 0..64 {
            if stop(chat) {
                return;
            }
            let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out waiting for a background event")
                .expect("channel closed");
            if let BgEvent::Frame { frame, .. } = ev {
                chat.apply_frame(frame, false);
            }
        }
        panic!("never satisfied the stop condition: {:?}", chat.msgs);
    }

    /// Opening a session loads its history. This is the bug that `session_history` fixed: the durable
    /// log is a separate read, because `turn_events` carries only what happens from now on.
    #[tokio::test]
    async fn opening_a_session_loads_its_history() {
        let ((slot, _conn), spy) = connected_slot_with(false).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), slot, tx);

        let mut chat = Transcript::new();
        collect(&mut rx, &mut chat, |c| c.msgs.len() >= 2).await;
        handle.abort();

        assert_eq!(chat.msgs[0].kind, MsgKind::User);
        assert_eq!(chat.msgs[0].text, "what is in main.rs?");
        assert_eq!(chat.msgs[1].kind, MsgKind::Agent);
        assert_eq!(chat.msgs[1].text, "a main function");
        assert_eq!(chat.cur, 2);
        assert_eq!(
            spy.history_queries.lock().unwrap().first(),
            Some(&None),
            "the first read must ask for the whole timeline"
        );
    }

    /// History then live, in that order, with the stream's deltas landing after the durable past.
    #[tokio::test]
    async fn history_is_followed_by_the_live_stream() {
        let ((slot, _conn), _spy) = connected_slot_with(true).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), slot, tx);

        let mut chat = Transcript::new();
        // Settled, not merely present: a card opens on its first delta, so counting alone would look
        // at "Hel" mid-stream.
        collect(&mut rx, &mut chat, |c| {
            c.msgs.len() >= 3 && c.msgs.iter().all(|m| !m.streaming)
        })
        .await;
        handle.abort();

        assert_eq!(chat.msgs.len(), 3, "{:?}", chat.msgs);
        assert_eq!(chat.msgs[0].text, "what is in main.rs?");
        assert_eq!(chat.msgs[1].text, "a main function");
        // Streamed as deltas, then settled by its durable event.
        assert_eq!(chat.msgs[2].kind, MsgKind::Agent);
        assert_eq!(chat.msgs[2].text, "Hello!");
        assert!(!chat.msgs[2].streaming);
        assert_eq!(chat.cur, 3);
    }

    /// Each reconnect re-reads history from the last cursor seen, which is what closes the gap a
    /// disconnection leaves. Asking for the whole timeline again would re-render the conversation.
    #[tokio::test]
    async fn a_resubscribe_asks_for_history_after_the_last_cursor() {
        let ((slot, _conn), spy) = connected_slot_with(false).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), slot, tx);

        let mut chat = Transcript::new();
        collect(&mut rx, &mut chat, |c| c.msgs.len() >= 2).await;
        // An idle session ends its subscription at once, so the supervisor comes back around.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        handle.abort();

        let queries = spy.history_queries.lock().unwrap().clone();
        assert!(queries.len() >= 2, "expected a re-read, got {queries:?}");
        assert_eq!(queries[0], None, "the first read is the whole timeline");
        assert!(
            queries[1..].iter().all(|q| *q == Some(2)),
            "later reads resume from the last cursor: {queries:?}"
        );
        assert_eq!(chat.msgs.len(), 2, "a re-read must not duplicate history");
    }

    /// A stream task holds no client of its own; it waits for the slot to be filled. This is what
    /// makes a reconnect resume rather than dying with the connection it started on.
    #[tokio::test]
    async fn a_stream_started_while_disconnected_waits_for_the_connection() {
        let slot = ApiSlot::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), slot.clone(), tx);

        // Nothing can arrive yet: there is no connection to subscribe over.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "a parked stream must not emit anything"
        );

        let (connected, _conn) = connected_slot().await;
        slot.set(connected.get().unwrap());

        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the parked stream did not wake on connect")
            .unwrap();
        assert!(matches!(ev, BgEvent::Frame { .. } | BgEvent::StreamHead { .. }));
        handle.abort();
    }

    #[tokio::test]
    async fn on_connect_publishes_the_client_and_the_account_metadata() {
        let server = Node::builder()
            .name("attacca")
            .kind(NodeKind::Service)
            .capability(AttaccaApiServer(StubApi::new(true).0))
            .build()
            .unwrap();
        let node = Node::builder()
            .name("cli")
            .kind(NodeKind::Cli)
            .build()
            .unwrap();
        let (_server_side, node_side) = zyris::testing::duplex(&server, &node).await.unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let slot = ApiSlot::new();
        let hook = tokio::spawn(on_connect(node_side.clone(), tx, slot.clone()));

        let mut me = None;
        let mut projects = 0;
        let mut sessions = 0;
        let mut agents = 0;
        for _ in 0..4 {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out")
                .unwrap()
            {
                BgEvent::Connected(m) => me = Some(m),
                BgEvent::Projects(p) => projects = p.len(),
                BgEvent::Sessions(s) => sessions = s.len(),
                BgEvent::Agents(a) => agents = a.len(),
                other => panic!("unexpected event: {}", describe(&other)),
            }
        }

        assert_eq!(me.expect("no identity").display_name, "Ada");
        assert_eq!((projects, sessions, agents), (1, 1, 1));
        assert!(slot.get().is_some(), "the client must be published for RPCs");

        node_side.close("done");
        hook.abort();
    }

    /// What `create_session_with` is actually asked for: the brief as the session's own preamble, and
    /// no title, so Attacca's title agent names the session from its first message.
    #[tokio::test]
    async fn a_session_is_created_with_a_preamble_and_no_title() {
        let ((slot, _conn), spy) = connected_slot_with(false).await;
        let live = slot.get().unwrap();

        let session = live
            .api
            .create_session_with(ZNewSession {
                agent_id: "agent-1".into(),
                title: None,
                project_id: Some("p1".into()),
                preamble: Some("node brief".into()),
            })
            .await
            .unwrap();

        assert_eq!(session.preamble.as_deref(), Some("node brief"));
        let created = spy.created.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].title, None, "a title here is permanent and suppresses auto-titling");
        assert_eq!(created[0].preamble.as_deref(), Some("node brief"));
    }

    fn describe(ev: &BgEvent) -> &'static str {
        match ev {
            BgEvent::Connected(_) => "connected",
            BgEvent::Disconnected(_) => "disconnected",
            BgEvent::Projects(_) => "projects",
            BgEvent::Sessions(_) => "sessions",
            BgEvent::Agents(_) => "agents",
            BgEvent::SessionCreated(_) => "session_created",
            BgEvent::StreamHead { .. } => "stream_head",
            BgEvent::Frame { .. } => "frame",
            BgEvent::Notice(_) => "notice",
            BgEvent::Done => "done",
        }
    }
}
