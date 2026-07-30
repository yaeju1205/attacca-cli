//! The live connection and the `attacca_api` client riding on it.
//!
//! This is the layer the old `transport`/`api` pair occupied: everything between "there is a
//! websocket" and "a background task can make a call". It does not spawn work of its own — that
//! is [`bg`](crate::bg)'s job — and it holds no application state.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use zyris::Connection;
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZSessionFilter};

use crate::app::{BgEvent, BgTx};

/// The server announces `attacca_api` immediately after the handshake; this is generous headroom.
pub const CONSUME_WAIT: Duration = Duration::from_secs(5);

/// How many sessions the sidebar asks for. Shared by the fetch on connect and every later refresh,
/// so the two can never disagree about how much of the list they are looking at.
pub const SESSION_LIMIT: u32 = 200;

/// What this node asks for at enrollment. `events:read` is the one that matters most and is the
/// easiest to miss: without it `turn_events` is refused and the chat produces nothing at all.
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

/// Run once per established connection, concurrently with the connection itself.
///
/// Idempotent by construction: everything it emits *replaces* UI state rather than appending to it,
/// which is what a reconnect needs since the runner re-runs this hook for every connection.
///
/// `session_usage` is deliberately absent from the fanout — see [`bg::session_usage`].
///
/// [`bg::session_usage`]: crate::bg::session_usage
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
    // than waiting on four round-trips they do not depend on.
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

/// The scopes in [`DEFAULT_SCOPES`] a grant is missing.
///
/// A credential enrolled by another node is reused as-is, and a refresh never re-requests scopes.
/// Without `events:read` there is no `turn_events` and therefore no output at all, so the gap is
/// worth naming rather than leaving to be discovered.
pub fn missing_scopes(me: &zyris_attacca::ZMe) -> Vec<&'static str> {
    DEFAULT_SCOPES
        .into_iter()
        .filter(|want| !me.scopes.iter().any(|got| got == want))
        .collect()
}

/// A deployment shaped like the real one, over an in-memory duplex: no server, no credential, no
/// network. Shared with [`bg`](crate::bg)'s tests.
#[cfg(test)]
pub(crate) mod stub {
    use super::*;
    use std::sync::Mutex;
    use zyris::{Datum, Node, NodeKind, Streaming};
    use zyris_attacca::{
        AttaccaApi, AttaccaApiServer, ZAgent, ZDeltaKind, ZHistoryQuery, ZMe, ZNewAgent,
        ZNewSession, ZProject, ZSession, ZSessionEvent, ZSessionFilter, ZTurnFrame,
        ZTurnStatus, ZUsage,
    };

    /// A durable timeline `session_history` reads back, and a `turn_events` carrying only what
    /// happens from now on — the difference the two-call open exists to bridge.
    pub(crate) struct StubApi {
        history_queries: Arc<Mutex<Vec<Option<i64>>>>,
        created: Arc<Mutex<Vec<ZNewSession>>>,
        /// Whether `turn_events` produces a turn, or just a head on an idle session.
        live_turn: bool,
        /// Whether this deployment implements `session_usage` at all. A deployment that predates
        /// the tool answers `capability_not_announced`, which a node must survive.
        meters: bool,
    }

    /// What the stub recorded, readable after the fact without holding the stub itself.
    #[derive(Clone, Default)]
    pub(crate) struct Spy {
        pub(crate) history_queries: Arc<Mutex<Vec<Option<i64>>>>,
        pub(crate) created: Arc<Mutex<Vec<ZNewSession>>>,
    }

    impl Spy {
        pub(crate) fn history_queries(&self) -> Vec<Option<i64>> {
            self.history_queries.lock().unwrap().clone()
        }

        pub(crate) fn created(&self) -> Vec<ZNewSession> {
            self.created.lock().unwrap().clone()
        }
    }

    impl StubApi {
        fn new(live_turn: bool, meters: bool) -> (StubApi, Spy) {
            let spy = Spy::default();
            (
                StubApi {
                    history_queries: spy.history_queries.clone(),
                    created: spy.created.clone(),
                    live_turn,
                    meters,
                },
                spy,
            )
        }
    }

    pub(crate) fn durable(cursor: i64, kind: &str, text: &str) -> ZSessionEvent {
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
                scopes: DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect(),
                plan: Some("pro".into()),
                credits: Some("42.50".into()),
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
            unimplemented!("the CLI never creates agents")
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

        async fn session_usage(&self, _session_id: String) -> zyris::Result<ZUsage> {
            if !self.meters {
                return Err(zyris::Error::new(
                    zyris::ErrorCode::CapabilityNotAnnounced,
                    "this deployment has no session_usage",
                ));
            }
            Ok(ZUsage {
                model: Some("claude-opus-5".into()),
                context_tokens: Some(12_400),
                input_tokens: Some(9_100),
                output_tokens: Some(3_300),
                total_tokens: Some(12_400),
                credits_used: None,
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

    /// A live slot over an in-memory duplex.
    pub(crate) async fn connected_slot() -> (ApiSlot, Connection) {
        connected_slot_with(true, true).await.0
    }

    pub(crate) async fn connected_slot_with(
        live_turn: bool,
        meters: bool,
    ) -> ((ApiSlot, Connection), Spy) {
        let (stub, spy) = StubApi::new(live_turn, meters);
        let server = Node::builder()
            .name("attacca")
            .kind(NodeKind::Service)
            .capability(AttaccaApiServer(stub))
            .build()
            .unwrap();
        let node = Node::builder().name("cli").kind(NodeKind::Cli).build().unwrap();

        let (_server_side, node_side) = zyris::testing::duplex(&server, &node).await.unwrap();
        let api: AttaccaApiClient = node_side.wait_capability(CONSUME_WAIT).await.unwrap();

        let slot = ApiSlot::new();
        slot.set(Live {
            conn_id: node_side.info().conn_id.clone(),
            conn: node_side.clone(),
            api: Arc::new(api),
        });
        ((slot, node_side), spy)
    }
}

#[cfg(test)]
mod tests {
    use super::stub::*;
    use super::*;
    use zyris_attacca::AttaccaApi;

    /// The race `clear_if` exists for: the outgoing connection's close handler and the runner's
    /// dial loop wake on the same event, so a blind clear can erase what a reconnect just
    /// published.
    #[tokio::test]
    async fn clear_if_ignores_a_stale_connection_id() {
        let ((slot, conn), _spy) = connected_slot_with(false, true).await;
        let live_id = conn.info().conn_id.clone();

        slot.clear_if("some-older-connection");
        assert!(slot.get().is_some(), "a stale id must not clear the slot");

        slot.clear_if(&live_id);
        assert!(slot.get().is_none(), "the live id must clear the slot");
    }

    /// The harness itself: the duplex resolves `attacca_api` and the client on the far end answers.
    /// Everything else in the test suite rests on this working.
    #[tokio::test]
    async fn the_stub_deployment_answers_over_the_duplex() {
        let (slot, _conn) = connected_slot().await;
        let api = slot.get().expect("slot is live").api;

        let me = api.me().await.unwrap();
        assert_eq!(me.display_name, "Ada");
        for scope in DEFAULT_SCOPES {
            assert!(me.scopes.iter().any(|s| s == scope), "missing {scope}");
        }
    }

    /// A deployment that predates `session_usage` refuses the call. It must stay a per-call error,
    /// not something that takes the connection with it.
    #[tokio::test]
    async fn a_deployment_without_session_usage_refuses_only_that_call() {
        let ((slot, _conn), _spy) = connected_slot_with(false, false).await;
        let api = slot.get().expect("slot is live").api;

        assert!(api.session_usage("s1".into()).await.is_err());
        // The connection is still good.
        assert_eq!(api.me().await.unwrap().display_name, "Ada");
    }
}
