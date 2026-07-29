//! Background task spawning.
//!
//! Each public function spawns a `tokio::spawn` task that performs HTTP calls
//! via the API layer and sends results back through the [`BgEvent`] channel.
//! These functions take only the data they need (no `&App`) so they stay
//! testable and independent of the main thread's state.

use crate::api;
use crate::app::{BgEvent, Msg};
use crate::tools::{parse_tools, short, PROTOCOL};
use crate::transport::Transport;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

// ── High-level spawners ────────────────────────────────────────

/// Send a tool result and then poll until done.
pub fn poll_after_tool(
    transport: Transport,
    tx: mpsc::UnboundedSender<BgEvent>,
    sid: String,
    result: String,
) {
    tokio::spawn(async move {
        let _ = api::send_message(
            &transport,
            &sid,
            &format!("[tool result]\n{result}"),
        )
        .await;

        // Same poll loop as in poll_until_done but with the sid already set
        let mut last_cursor = 0i64;
        loop {
            let msgs = api::fetch_messages_raw(&transport, &sid).await;
            if !msgs.is_empty() {
                let mut new_msgs = Vec::new();
                let mut max_cursor = last_cursor;
                for m in &msgs {
                    if let Some(c) = m["cursor"].as_i64() {
                        max_cursor = max_cursor.max(c);
                    }
                    if m["role"].as_str() == Some("assistant") {
                        if let Some(text) = m["text"].as_str() {
                            new_msgs.push(Msg {
                                role: "assistant".into(),
                                text: text.into(),
                                raw: None,
                                done: false,
                                cursor: m["cursor"].as_i64().unwrap_or(0),
                            });
                        }
                    }
                }
                if !new_msgs.is_empty() {
                    let _ = tx.send(BgEvent::NewMsgs {
                        sid: sid.clone(),
                        msgs: new_msgs,
                        new_cur: max_cursor,
                    });
                }
                last_cursor = max_cursor;
            }

            if !api::session_is_running(&transport, &sid).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        // Final fetch
        let msgs = api::fetch_messages_raw(&transport, &sid).await;
        if !msgs.is_empty() {
            let mut new_cur = last_cursor;
            let mut msgs_out = Vec::new();
            for m in &msgs {
                if let Some(c) = m["cursor"].as_i64() {
                    new_cur = new_cur.max(c);
                }
                if m["role"].as_str() == Some("assistant") {
                    if let Some(text) = m["text"].as_str() {
                        msgs_out.push(Msg {
                            role: "assistant".into(),
                            text: text.into(),
                            raw: None,
                            done: false,
                            cursor: m["cursor"].as_i64().unwrap_or(0),
                        });
                    }
                }
            }
            if !msgs_out.is_empty() {
                let _ = tx.send(BgEvent::NewMsgs {
                    sid: sid.clone(),
                    msgs: msgs_out,
                    new_cur,
                });
            }
        }

        let _ = tx.send(BgEvent::Done { sid });
    });
}

// ── High-level spawners ────────────────────────────────────────

/// Spawn a send-message task.
///
/// Creates a session if needed, sends the user message (prepending PROTOCOL
/// on first message), then polls until done.
pub fn spawn_send(
    transport: Transport,
    tx: mpsc::UnboundedSender<BgEvent>,
    msg: String,
    existing_sid: Option<String>,
    is_first: bool,
) {
    tokio::spawn(async move {
        // 1. Ensure session
        let sid = if let Some(s) = existing_sid {
            s
        } else {
            match api::create_session(&transport, "attacca-cli").await {
                Some(id) => {
                    let _ = tx.send(BgEvent::SessionCreated(id.clone()));
                    id
                }
                None => {
                    let _ = tx.send(BgEvent::Done {
                        sid: String::new(),
                    });
                    return;
                }
            }
        };

        // 2. Send the message
        let payload = if is_first {
            format!("{PROTOCOL}\n\n---\n{}", msg)
        } else {
            msg.clone()
        };

        if !api::send_message(&transport, &sid, &payload).await {
            let _ = tx.send(BgEvent::Done {
                sid: sid.clone(),
            });
            return;
        }

        // 3. Poll — reuse the shared poll function
        // We inline the poll here because we need to keep 'sid' in scope.
        let mut last_cursor = 0i64;
        loop {
            let msgs = api::fetch_messages_raw(&transport, &sid).await;
            if !msgs.is_empty() {
                let mut new_msgs = Vec::new();
                let mut max_cursor = last_cursor;
                for m in &msgs {
                    if let Some(c) = m["cursor"].as_i64() {
                        max_cursor = max_cursor.max(c);
                    }
                    if m["role"].as_str() == Some("assistant") {
                        if let Some(text) = m["text"].as_str() {
                            let cursor = m["cursor"].as_i64().unwrap_or(0);
                            new_msgs.push(Msg {
                                role: "assistant".into(),
                                text: text.into(),
                                raw: None,
                                done: false,
                                cursor,
                            });
                        }
                    }
                }
                if !new_msgs.is_empty() {
                    let _ = tx.send(BgEvent::NewMsgs {
                        sid: sid.clone(),
                        msgs: new_msgs,
                        new_cur: max_cursor,
                    });
                }
                last_cursor = max_cursor;
            }

            if !api::session_is_running(&transport, &sid).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        // Final fetch
        let msgs = api::fetch_messages_raw(&transport, &sid).await;
        if !msgs.is_empty() {
            let mut new_cur = last_cursor;
            let mut msgs_out = Vec::new();
            for m in &msgs {
                if let Some(c) = m["cursor"].as_i64() {
                    new_cur = new_cur.max(c);
                }
                if m["role"].as_str() == Some("assistant") {
                    if let Some(text) = m["text"].as_str() {
                        msgs_out.push(Msg {
                            role: "assistant".into(),
                            text: text.into(),
                            raw: None,
                            done: false,
                            cursor: m["cursor"].as_i64().unwrap_or(0),
                        });
                    }
                }
            }
            if !msgs_out.is_empty() {
                let _ = tx.send(BgEvent::NewMsgs {
                    sid: sid.clone(),
                    msgs: msgs_out,
                    new_cur,
                });
            }
        }

        // Load usage
        let v = api::get_session_usage(&transport, &sid).await;
        if !v.is_null() {
            let _ = tx.send(BgEvent::Usage(v));
        }

        let _ = tx.send(BgEvent::Done { sid });
    });
}

/// Spawn an open-session task.
pub fn spawn_open(
    transport: Transport,
    tx: mpsc::UnboundedSender<BgEvent>,
    sid: String,
) {
    tokio::spawn(async move {
        let msgs = api::fetch_messages_raw(&transport, &sid).await;
        let mut new_cur = 0i64;
        let mut new_msgs = Vec::new();
        for m in &msgs {
            if let Some(c) = m["cursor"].as_i64() {
                if c > new_cur {
                    new_cur = c;
                }
            }
            let role = m["role"].as_str().unwrap_or("");
            let text = m["text"].as_str().unwrap_or("");
            let cursor = m["cursor"].as_i64().unwrap_or(0);
            if role == "assistant" || role == "user" {
                let (clean, _) = parse_tools(text);
                if !clean.is_empty() {
                    new_msgs.push(Msg {
                        role: role.into(),
                        text: clean,
                        raw: None,
                        done: false,
                        cursor,
                    });
                }
            }
        }
        if !new_msgs.is_empty() {
            let _ = tx.send(BgEvent::NewMsgs {
                sid: sid.clone(),
                msgs: new_msgs,
                new_cur,
            });
        }

        // Load usage
        let v = api::get_session_usage(&transport, &sid).await;
        if !v.is_null() {
            let _ = tx.send(BgEvent::Usage(v));
        }

        let _ = tx.send(BgEvent::Done { sid });
    });
}

/// Spawn a create-session task.
pub fn spawn_create(transport: Transport, tx: mpsc::UnboundedSender<BgEvent>) {
    tokio::spawn(async move {
        match api::create_session(&transport, "attacca-cli").await {
            Some(id) => {
                let _ = tx.send(BgEvent::SessionCreated(id.clone()));
                let _ = tx.send(BgEvent::NewMsgs {
                    sid: id.clone(),
                    msgs: vec![Msg {
                        role: "sys".into(),
                        text: format!("new session {}", short(&id)),
                        raw: None,
                        done: true,
                        cursor: 0,
                    }],
                    new_cur: 0,
                });
                let _ = tx.send(BgEvent::Done { sid: id });
            }
            None => {
                let _ = tx.send(BgEvent::Done {
                    sid: String::new(),
                });
            }
        }
    });
}

/// Spawn a login task (just updates the key and does a whoami check).
pub fn spawn_login(
    transport: Transport,
    tx: mpsc::UnboundedSender<BgEvent>,
    _key: String,
) {
    tokio::spawn(async move {
        let name = api::whoami(&transport).await;
        let _ = tx.send(BgEvent::NewMsgs {
            sid: String::new(),
            msgs: vec![Msg {
                role: "sys".into(),
                text: format!("logged in — {name}"),
                raw: None,
                done: true,
                cursor: 0,
            }],
            new_cur: 0,
        });
        let _ = tx.send(BgEvent::Done {
            sid: String::new(),
        });
    });
}

/// Spawn a show-info task (fetches user info + session usage).
pub fn spawn_show_info(
    transport: Transport,
    tx: mpsc::UnboundedSender<BgEvent>,
    sid: Option<String>,
) {
    tokio::spawn(async move {
        let (me_name, _email, me_plan, me_credits) = api::whoami_detailed(&transport).await;

        let usage = if let Some(ref s) = sid {
            api::get_session_usage(&transport, s).await
        } else {
            Value::Null
        };

        let mut msgs = Vec::new();
        msgs.push(Msg {
            role: "sys".into(),
            text: "── Account ───────────────────────────".into(),
            raw: None,
            done: true,
            cursor: 0,
        });
        msgs.push(Msg {
            role: "sys".into(),
            text: format!("  User:     {}", if me_name.is_empty() { "not logged in" } else { &me_name }),
            raw: None,
            done: true,
            cursor: 0,
        });
        if !me_plan.is_empty() {
            msgs.push(Msg {
                role: "sys".into(),
                text: format!("  Plan:     {me_plan}"),
                raw: None,
                done: true,
                cursor: 0,
            });
        }
        if !me_credits.is_empty() {
            msgs.push(Msg {
                role: "sys".into(),
                text: format!("  Credits:  {me_credits}"),
                raw: None,
                done: true,
                cursor: 0,
            });
        }
        if !usage.is_null() {
            let used = api::field_val(&usage["credits_used"]);
            if !used.is_empty() {
                msgs.push(Msg {
                    role: "sys".into(),
                    text: format!("  Used:     {used}"),
                    raw: None,
                    done: true,
                    cursor: 0,
                });
            }
        }

        if let Some(ref s) = sid {
            msgs.push(Msg {
                role: "sys".into(),
                text: format!("── Session ({}) ─────────────", short(s)),
                raw: None,
                done: true,
                cursor: 0,
            });
            if !usage.is_null() {
                let v = |k: &str| api::field_val(&usage[k]);
                let model = v("model");
                if !model.is_empty() {
                    msgs.push(Msg {
                        role: "sys".into(),
                        text: format!("  Model:    {model}"),
                        raw: None,
                        done: true,
                        cursor: 0,
                    });
                }
                let ctx = v("context_tokens");
                if !ctx.is_empty() {
                    msgs.push(Msg {
                        role: "sys".into(),
                        text: format!("  Context:  {ctx}"),
                        raw: None,
                        done: true,
                        cursor: 0,
                    });
                }
                let inp = v("input_tokens");
                if !inp.is_empty() {
                    msgs.push(Msg {
                        role: "sys".into(),
                        text: format!("  Input:    {inp}"),
                        raw: None,
                        done: true,
                        cursor: 0,
                    });
                }
                let out = v("output_tokens");
                if !out.is_empty() {
                    msgs.push(Msg {
                        role: "sys".into(),
                        text: format!("  Output:   {out}"),
                        raw: None,
                        done: true,
                        cursor: 0,
                    });
                }
                let tot = v("total_tokens");
                if !tot.is_empty() {
                    msgs.push(Msg {
                        role: "sys".into(),
                        text: format!("  Total:    {tot}"),
                        raw: None,
                        done: true,
                        cursor: 0,
                    });
                }
            }
        }

        let sid_str = sid.clone().unwrap_or_default();
        let _ = tx.send(BgEvent::NewMsgs {
            sid: sid_str.clone(),
            msgs,
            new_cur: 0,
        });
        if !usage.is_null() {
            let _ = tx.send(BgEvent::Usage(usage));
        }
        let _ = tx.send(BgEvent::Done { sid: sid_str });
    });
}
