//! Background task spawning.
//!
//! Each public function spawns a `tokio::spawn` task that talks to the live connection and reports
//! back through the [`BgEvent`] channel. These functions take only the data they need (no `&App`)
//! so they stay testable and independent of the main thread's state.

use crate::app::{BgEvent, BgTx};
use crate::zyris_client::{Api, ApiSlot, Live, SESSION_LIMIT};

use futures_util::StreamExt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use zyris::Streaming;
use zyris_attacca::{
    AttaccaApi, ZHistoryQuery, ZNewSession, ZSessionFilter, ZTurnFrame, ZTurnStatus,
};

/// How long to wait before re-subscribing after a `turn_events` stream that delivered something.
const RESUBSCRIBE_MIN: Duration = Duration::from_millis(500);
/// Ceiling for the same wait when subscriptions keep coming back empty. A deployment that closes
/// the stream immediately on an idle session would otherwise have this re-dialling twice a second
/// forever, which is the polling this client exists to be rid of.
const RESUBSCRIBE_MAX: Duration = Duration::from_secs(15);

// ── The request helper ─────────────────────────────────────────

/// Run a request against the live connection, guaranteeing exactly one [`BgEvent::Done`].
///
/// The single `Done` is what keeps the caller's busy count honest across every early return —
/// including the not-connected path, which has no task to run at all.
fn rpc<F, Fut>(slot: &ApiSlot, tx: &BgTx, f: F)
where
    F: FnOnce(Api, BgTx) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let tx = tx.clone();
    let Some(live) = slot.get() else {
        let _ = tx.send(BgEvent::Notice("not connected yet".into()));
        let _ = tx.send(BgEvent::Done);
        return;
    };
    tokio::spawn(async move {
        f(live.api, tx.clone()).await;
        let _ = tx.send(BgEvent::Done);
    });
}

/// Like [`rpc`] for work that needs no connection.
fn task<F, Fut>(tx: &BgTx, f: F)
where
    F: FnOnce(BgTx) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let tx = tx.clone();
    tokio::spawn(async move {
        f(tx.clone()).await;
        let _ = tx.send(BgEvent::Done);
    });
}

// ── High-level spawners ────────────────────────────────────────

/// Send a message, creating the session first when there is not one yet.
pub fn send(
    slot: &ApiSlot,
    tx: &BgTx,
    text: String,
    sid: Option<String>,
    spec: Option<ZNewSession>,
) {
    rpc(slot, tx, move |api, tx| async move {
        let sid = match sid {
            Some(sid) => sid,
            None => {
                let Some(spec) = spec else {
                    let _ = tx.send(BgEvent::Notice(
                        "no agent available — set ATTACCA_AGENT or create one in Attacca".into(),
                    ));
                    return;
                };
                match api.create_session_with(spec).await {
                    Ok(session) => {
                        let id = session.id.clone();
                        let _ = tx.send(BgEvent::SessionCreated(Box::new(session)));
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

pub fn create_session(slot: &ApiSlot, tx: &BgTx, spec: Option<ZNewSession>) {
    rpc(slot, tx, move |api, tx| async move {
        let Some(spec) = spec else {
            let _ = tx.send(BgEvent::Notice(
                "no agent available — set ATTACCA_AGENT or create one in Attacca".into(),
            ));
            return;
        };
        match api.create_session_with(spec).await {
            Ok(session) => {
                let _ = tx.send(BgEvent::SessionCreated(Box::new(session)));
            }
            Err(e) => {
                let _ = tx.send(BgEvent::Notice(format!("create_session: {e}")));
            }
        }
    });
}

pub fn cancel_turn(slot: &ApiSlot, tx: &BgTx, sid: String) {
    rpc(slot, tx, move |api, tx| async move {
        let msg = match api.cancel_turn(sid).await {
            Ok(()) => "turn cancelled".to_string(),
            Err(e) => format!("cancel_turn: {e}"),
        };
        let _ = tx.send(BgEvent::Notice(msg));
    });
}

/// Re-read the session list. Event-driven, never on a timer — see the call site in
/// [`event`](crate::event).
///
/// Quiet on failure: this runs off a turn ending, and a failure costs a slightly stale sidebar,
/// which is not worth a line in the transcript every turn.
pub fn refresh_sessions(slot: &ApiSlot, tx: &BgTx) {
    rpc(slot, tx, move |api, tx| async move {
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
            Err(e) => tracing::warn!(error = %e, "list_sessions refresh failed"),
        }
    });
}

/// Fetch what a session has cost.
///
/// Deliberately not part of the fanout on connect: `session_usage` was added to `attacca_api`
/// within version 1, so a deployment that predates it answers `capability_not_announced`. Asking
/// only when the answer is wanted keeps that a one-line notice instead of an error on every launch.
///
/// `quiet` separates the two callers. `/usage` asked, so it deserves an answer either way; the
/// refresh at the end of every turn did not, and on a deployment without the tool it would
/// otherwise print the same apology after every single reply.
pub fn session_usage(slot: &ApiSlot, tx: &BgTx, sid: String, quiet: bool) {
    rpc(slot, tx, move |api, tx| async move {
        match api.session_usage(sid).await {
            Ok(usage) => {
                let _ = tx.send(BgEvent::Usage(Box::new(usage)));
            }
            Err(e) => {
                tracing::debug!(error = %e, "session_usage unavailable");
                if !quiet {
                    let _ = tx.send(BgEvent::Notice(
                        "usage is not available on this deployment yet".into(),
                    ));
                }
            }
        }
    });
}

pub fn logout(tx: &BgTx, auth: Arc<crate::auth::Authenticator>) {
    task(tx, move |tx| async move {
        let msg = match auth.logout().await {
            Ok(()) => "credential cleared — /login now, or restart".to_string(),
            Err(e) => format!("logout: {e}"),
        };
        let _ = tx.send(BgEvent::Notice(msg));
    });
}

// ── The session feed ───────────────────────────────────────────

/// Follow a session: read what has already happened, then stay subscribed to what happens next.
///
/// The two calls divide cleanly, and the difference in how they read `after` is the whole reason to
/// use both. `session_history` with no `after` is the entire timeline, which is how a session opens;
/// `turn_events` with no `after` is live frames only. So history catches up and the stream takes
/// over, and each reconnect repeats the pair — the catch-up fetch closes whatever gap the
/// disconnection left, because [`wait_for_api`] parks the task rather than letting it die with its
/// connection.
pub fn spawn_session_stream(session_id: String, slot: ApiSlot, tx: BgTx) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = slot.subscribe();
        let mut last_cursor = 0i64;
        // A failure that will not clear — no `events:read` in the grant, say — would otherwise post
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

            match api.turn_events(session_id.clone(), Some(last_cursor)).await {
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
    use crate::zyris_client::stub::*;
    use tokio::sync::mpsc;

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

    /// Apply whatever is already queued, without waiting for more.
    fn drain_into(rx: &mut mpsc::UnboundedReceiver<BgEvent>, chat: &mut Transcript) {
        while let Ok(ev) = rx.try_recv() {
            if let BgEvent::Frame { frame, .. } = ev {
                chat.apply_frame(frame, false);
            }
        }
    }

    /// No transcript content arrives for `window`.
    ///
    /// Deliberately not "no events at all": a subscription that was already in flight still reports
    /// its head, and a head changes nothing on screen. What must not happen is a frame.
    async fn assert_no_frames(rx: &mut mpsc::UnboundedReceiver<BgEvent>, window: Duration) {
        let deadline = tokio::time::Instant::now() + window;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if let BgEvent::Frame { frame, .. } = ev {
                panic!("expected silence, got a frame: {frame:?}");
            }
        }
    }

    /// Opening a session loads its history. This is what `session_history` is for: the durable log
    /// is a separate read, because `turn_events` carries only what happens from now on.
    #[tokio::test]
    async fn opening_a_session_loads_its_history() {
        let ((slot, _conn), _spy) = connected_slot_with(false, true).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), slot, tx);

        let mut chat = Transcript::new();
        collect(&mut rx, &mut chat, |c| c.msgs.len() >= 2).await;
        handle.abort();

        assert_eq!(chat.msgs[0].kind, MsgKind::User);
        assert_eq!(chat.msgs[0].text, "what is in main.rs?");
        assert_eq!(chat.msgs[1].kind, MsgKind::Agent);
        assert_eq!(chat.msgs[1].text, "a main function");
    }

    /// History first, then the live turn — and the deltas settle into one card rather than two.
    #[tokio::test]
    async fn history_is_followed_by_the_live_stream() {
        let (slot, _conn) = connected_slot().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), slot, tx);

        let mut chat = Transcript::new();
        // Wait for the card to *settle*, not merely to open: the first delta already makes three
        // messages, and stopping there would assert on a half-streamed "Hel".
        collect(&mut rx, &mut chat, |c| {
            c.msgs.len() >= 3 && !c.msgs[2].streaming
        })
        .await;
        handle.abort();

        assert_eq!(chat.msgs.len(), 3, "{:?}", chat.msgs);
        assert_eq!(chat.msgs[2].kind, MsgKind::Agent);
        assert_eq!(chat.msgs[2].text, "Hello!");
        assert_eq!(chat.cur, 3);
    }

    /// A re-subscribe must not re-read the whole timeline, or every turn boundary would replay it.
    #[tokio::test]
    async fn a_resubscribe_asks_for_history_after_the_last_cursor() {
        let ((slot, _conn), spy) = connected_slot_with(true, true).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), slot, tx);

        let mut chat = Transcript::new();
        collect(&mut rx, &mut chat, |c| c.msgs.len() >= 3).await;
        // The first subscription delivered a turn, so the backoff is the 500 ms floor.
        tokio::time::sleep(Duration::from_millis(900)).await;
        handle.abort();

        let queries = spy.history_queries();
        assert_eq!(queries.first(), Some(&None), "the open reads everything");
        assert!(queries.len() > 1, "expected a re-subscribe, saw {queries:?}");
        assert_eq!(
            queries[1],
            Some(3),
            "a re-subscribe resumes from the last cursor, saw {queries:?}"
        );
    }

    /// Sessions are created untitled and with a preamble.
    ///
    /// The title matters: Attacca names a session from its first message, and a title supplied here
    /// is permanent and opts the session out of that for good — which is what the old `attacca-cli`
    /// placeholder cost every session this client made.
    #[tokio::test]
    async fn a_session_is_created_with_a_preamble_and_no_title() {
        let ((slot, _conn), spy) = connected_slot_with(false, true).await;
        let (tx, mut rx) = mpsc::unbounded_channel();

        create_session(
            &slot,
            &tx,
            Some(ZNewSession {
                agent_id: "agent-1".into(),
                title: None,
                project_id: None,
                preamble: Some("you are driving build-box".into()),
            }),
        );

        // Wait for the request to settle.
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out")
                .expect("channel closed")
            {
                BgEvent::Done => break,
                BgEvent::Notice(text) => panic!("unexpected notice: {text}"),
                _ => {}
            }
        }

        let created = spy.created();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].title, None, "a title here would be permanent");
        assert_eq!(
            created[0].preamble.as_deref(),
            Some("you are driving build-box")
        );
    }

    /// Not connected is a normal state, not an error state: the `Done` still has to arrive or the
    /// caller's busy count never comes back down and the action queue wedges forever.
    #[tokio::test]
    async fn a_request_with_no_connection_still_reports_done() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        cancel_turn(&ApiSlot::new(), &tx, "s1".into());

        assert!(matches!(rx.try_recv(), Ok(BgEvent::Notice(_))));
        assert!(matches!(rx.try_recv(), Ok(BgEvent::Done)));
    }

    /// A deployment that predates `session_usage` must cost one notice, not a broken client.
    #[tokio::test]
    async fn usage_is_absent_rather_than_a_startup_failure() {
        let ((slot, _conn), _spy) = connected_slot_with(false, false).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        session_usage(&slot, &tx, "s1".into(), false);

        let mut notices = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out")
                .expect("channel closed")
            {
                BgEvent::Done => break,
                BgEvent::Notice(text) => notices.push(text),
                BgEvent::Usage(_) => panic!("a deployment without the tool must not report usage"),
                _ => {}
            }
        }
        assert_eq!(notices.len(), 1, "say it once: {notices:?}");
        assert!(notices[0].contains("not available"), "{}", notices[0]);
    }

    /// And when it does meter, the numbers reach the info bar.
    #[tokio::test]
    async fn usage_reaches_the_app_when_the_deployment_meters() {
        let ((slot, _conn), _spy) = connected_slot_with(false, true).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        session_usage(&slot, &tx, "s1".into(), false);

        let usage = loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timed out")
                .expect("channel closed")
            {
                BgEvent::Usage(u) => break u,
                BgEvent::Done => panic!("finished without reporting usage"),
                _ => {}
            }
        };
        assert_eq!(usage.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(usage.total_tokens, Some(12_400));
        // Unmetered fields stay absent rather than rendering as a zero.
        assert_eq!(usage.credits_used, None);
    }

    /// A stream started before the connection exists must park, not die.
    #[tokio::test]
    async fn a_stream_started_while_disconnected_waits_for_the_connection() {
        let empty = ApiSlot::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), empty.clone(), tx);

        // Nothing at all while the slot is empty.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "a parked stream must stay quiet"
        );

        // Publish a live connection into the very slot the task is watching.
        let ((live_slot, _conn), _spy) = connected_slot_with(false, true).await;
        empty.set(live_slot.get().expect("live"));

        let mut chat = Transcript::new();
        collect(&mut rx, &mut chat, |c| c.msgs.len() >= 2).await;
        handle.abort();
        assert_eq!(chat.msgs[0].text, "what is in main.rs?");
    }

    /// The whole reason `ApiSlot` is a `watch` and not a mutex: a stream outlives the connection it
    /// started on, and resumes on the next one from the cursor it had reached.
    #[tokio::test]
    async fn a_reconnect_republishes_the_slot_and_the_parked_stream_resumes() {
        let ((first, conn), spy) = connected_slot_with(false, true).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = spawn_session_stream("s1".to_string(), first.clone(), tx);

        let mut chat = Transcript::new();
        collect(&mut rx, &mut chat, |c| c.msgs.len() >= 2).await;
        assert_eq!(chat.cur, 2);

        // The connection goes away; the task must park rather than exit.
        first.clear_if(&conn.info().conn_id);
        assert_no_frames(&mut rx, Duration::from_millis(400)).await;

        // A second connection is published into the same slot. The wait covers the re-subscribe
        // backoff, which has doubled to a second after the first subscription came back empty.
        let ((second, _conn2), spy2) = connected_slot_with(false, true).await;
        first.set(second.get().expect("live"));
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        handle.abort();
        drain_into(&mut rx, &mut chat);

        // Nothing was duplicated, and the catch-up asked only for what came after cursor 2.
        assert_eq!(chat.msgs.len(), 2, "{:?}", chat.msgs);
        assert_eq!(spy.history_queries().first(), Some(&None));
        assert_eq!(
            spy2.history_queries().first(),
            Some(&Some(2)),
            "the reconnect must resume from the last cursor"
        );
    }
}
