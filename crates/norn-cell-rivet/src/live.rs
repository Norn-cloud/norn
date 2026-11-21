//! Live reads over one WebSocket per client (UI-LIVE-1, protocol
//! `norn.live.v1`): the native port of `apps/norn-cell-do/subscribe.mjs`.
//!
//! A subscription names a GET route of this cell. The cell answers it the way
//! it answers that GET, as the subscriber, after every commit this actor
//! makes; only a changed answer is sent. Nothing here reads data or decides
//! access. A token the cell no longer admits ends the socket (4401).
//!
//! The router ([`crate::router`]) accepts the client's socket and opens one
//! to this actor through the engine's gateway. Its first frame is the
//! router's `norn.hello`, carrying the caller's `Authorization` and the scope
//! (a gateway socket carries no request headers of its own); every later
//! frame is the client's. Sockets do not hibernate: an open socket keeps the
//! actor awake.

use std::collections::BTreeMap;
use std::sync::Arc;

use norn_api::scope::CellScope;
use rivetkit::{Ctx, WebSocket, WsMessage};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::{Mutex, mpsc};

use crate::actor::{CellActor, now_ms, scope_of};
use crate::core::Call;

/// The subprotocol a client asks for.
pub const PROTOCOL: &str = "norn.live.v1";
/// Subscriptions per socket.
pub const MAX_SUBSCRIPTIONS: usize = 8;
/// Sockets per cell actor.
pub const MAX_SOCKETS: usize = 256;
const MAX_MESSAGE: usize = 4096;
/// The router's first frame.
pub const HELLO: &str = "norn.hello";

struct Sub {
    pathname: String,
    query: String,
    digest: String,
}

#[derive(Default)]
struct Attachment {
    hello: Option<(CellScope, String)>,
    subs: BTreeMap<String, Sub>,
}

struct LiveSocket {
    ws: WebSocket,
    state: Mutex<Attachment>,
    id: u64,
}

/// The live sockets of one actor.
#[derive(Default)]
pub struct Sockets {
    open: std::sync::Mutex<Vec<Arc<LiveSocket>>>,
    next: std::sync::atomic::AtomicU64,
}

impl Sockets {
    /// Whether no socket is open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open.lock().map_or(true, |open| open.is_empty())
    }

    fn all(&self) -> Vec<Arc<LiveSocket>> {
        self.open
            .lock()
            .map(|open| open.clone())
            .unwrap_or_default()
    }

    fn remove(&self, id: u64) {
        if let Ok(mut open) = self.open.lock() {
            open.retain(|socket| socket.id != id);
        }
    }
}

fn send(ws: &WebSocket, message: &Value) {
    ws.send(WsMessage::Text(message.to_string()));
}

fn digest(text: &str) -> String {
    let mut out = String::with_capacity(24);
    for byte in Sha256::digest(text.as_bytes()).iter().take(12) {
        for nibble in [byte >> 4, byte & 0x0f] {
            out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    out
}

/// A path a subscription may name: one of this cell's GET routes, never the
/// live route itself.
fn subscribable(path: &str) -> Option<(String, String)> {
    if path.len() > 512 || !path.starts_with("/v1/") || path.starts_with("/v1/subscribe") {
        return None;
    }
    let (pathname, query) = path.split_once('?').unwrap_or((path, ""));
    let pathname = pathname
        .split_once('#')
        .map_or(pathname, |(before, _)| before);
    if pathname.contains("..") {
        return None;
    }
    Some((
        pathname.to_owned(),
        query
            .split_once('#')
            .map_or(query, |(before, _)| before)
            .to_owned(),
    ))
}

/// Accept one socket from the router.
pub fn accept(actor: Arc<CellActor>, ctx: Ctx<CellActor>, ws: &WebSocket) {
    let sockets = actor.sockets();
    let id = sockets
        .next
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let socket = Arc::new(LiveSocket {
        ws: ws.clone(),
        state: Mutex::new(Attachment::default()),
        id,
    });
    let full = match sockets.open.lock() {
        Ok(mut open) if open.len() < MAX_SOCKETS => {
            open.push(Arc::clone(&socket));
            false
        }
        Ok(_) | Err(_) => true,
    };
    if full {
        let ws = ws.clone();
        tokio::spawn(async move {
            ws.close(Some(1013), Some("too many live readers".to_owned()))
                .await;
        });
        return;
    }
    let (frames, mut queue) = mpsc::unbounded_channel::<WsMessage>();
    ws.configure_message_event_callback(Some(Arc::new(move |message, _index| {
        // A frame after the reader stopped has nowhere to go.
        let _sent = frames.send(message);
        Ok(())
    })));
    let closing = Arc::clone(&actor);
    ws.configure_close_event_callback(Some(Arc::new(move |_code, _reason, _clean| {
        closing.sockets().remove(id);
        Box::pin(async { Ok::<(), anyhow::Error>(()) })
    })));
    tokio::spawn(async move {
        while let Some(message) = queue.recv().await {
            if !handle(&actor, &ctx, &socket, message).await {
                break;
            }
        }
        actor.sockets().remove(socket.id);
    });
}

/// One frame; whether the socket stays open.
async fn handle(
    actor: &Arc<CellActor>,
    ctx: &Ctx<CellActor>,
    socket: &Arc<LiveSocket>,
    message: WsMessage,
) -> bool {
    let WsMessage::Text(raw) = message else {
        socket
            .ws
            .close(Some(1003), Some("text frames only".to_owned()))
            .await;
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        socket
            .ws
            .close(Some(1007), Some("not JSON".to_owned()))
            .await;
        return false;
    };
    let hello = socket.state.lock().await.hello.clone();
    let Some((scope, _authorization)) = hello else {
        return greet(actor, ctx, socket, &parsed).await;
    };
    if raw.len() > MAX_MESSAGE {
        socket
            .ws
            .close(Some(1009), Some("message too large".to_owned()))
            .await;
        return false;
    }
    if actor.ready(ctx, &scope).await.is_err() {
        socket
            .ws
            .close(Some(1011), Some("the cell is unavailable".to_owned()))
            .await;
        return false;
    }
    let id = parsed
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
        })
        .map(str::to_owned);
    let Some(id) = id else {
        send(
            &socket.ws,
            &json!({"type": "refused", "id": null, "code": "live.invalid_id", "message": "a subscription id is 1 to 64 letters, digits or _.:-"}),
        );
        return true;
    };
    match parsed.get("type").and_then(Value::as_str) {
        Some("unsubscribe") => {
            socket.state.lock().await.subs.remove(&id);
            true
        }
        Some("subscribe") => {
            let Some((pathname, query)) = parsed
                .get("path")
                .and_then(Value::as_str)
                .and_then(subscribable)
            else {
                send(
                    &socket.ws,
                    &json!({"type": "refused", "id": id, "code": "live.not_subscribable", "message": "a subscription names one of this cell's GET routes"}),
                );
                return true;
            };
            {
                let mut state = socket.state.lock().await;
                if !state.subs.contains_key(&id) && state.subs.len() >= MAX_SUBSCRIPTIONS {
                    send(
                        &socket.ws,
                        &json!({"type": "refused", "id": id, "code": "live.too_many_subscriptions", "message": format!("at most {MAX_SUBSCRIPTIONS} subscriptions per socket")}),
                    );
                    return true;
                }
                state.subs.insert(
                    id.clone(),
                    Sub {
                        pathname,
                        query,
                        digest: String::new(),
                    },
                );
            }
            evaluate(actor, socket, Some(&id)).await
        }
        Some(_) | None => {
            send(
                &socket.ws,
                &json!({"type": "refused", "id": id, "code": "live.unknown_message", "message": "messages are subscribe or unsubscribe"}),
            );
            true
        }
    }
}

/// The router's first frame: the caller's authorization and the scope.
async fn greet(
    actor: &Arc<CellActor>,
    ctx: &Ctx<CellActor>,
    socket: &Arc<LiveSocket>,
    hello: &Value,
) -> bool {
    let authorization = hello.get("authorization").and_then(Value::as_str);
    let scope = hello.get("scope").map(Value::to_string);
    let scope = scope_of(scope.as_deref()).ok();
    let (Some(HELLO), Some(authorization), Some(scope)) = (
        hello.get("type").and_then(Value::as_str),
        authorization,
        scope,
    ) else {
        socket
            .ws
            .close(Some(1008), Some("no live state".to_owned()))
            .await;
        return false;
    };
    if crate::actor::scope_refused(ctx, &scope, actor.config().conformance)
        || actor.ready(ctx, &scope).await.is_err()
    {
        socket
            .ws
            .close(Some(1011), Some("the cell is unavailable".to_owned()))
            .await;
        return false;
    }
    socket.state.lock().await.hello = Some((scope, authorization.to_owned()));
    true
}

/// Answer the socket's subscriptions again and send the changed ones;
/// whether the socket stays open. A refused token closes it (4401).
async fn evaluate(actor: &Arc<CellActor>, socket: &Arc<LiveSocket>, only: Option<&str>) -> bool {
    let mut state = socket.state.lock().await;
    let Some((_, authorization)) = state.hello.clone() else {
        return false;
    };
    let ids: Vec<String> = state
        .subs
        .keys()
        .filter(|id| only.is_none_or(|only| only == id.as_str()))
        .cloned()
        .collect();
    for id in ids {
        let Some(sub) = state.subs.get(&id) else {
            continue;
        };
        let call = Call {
            method: "GET".to_owned(),
            path: sub.pathname.clone(),
            query: sub.query.clone(),
            authorization: Some(authorization.clone()),
            now_ms: now_ms(),
            body: None,
        };
        let Ok(reply) = actor.ask(call).await else {
            socket
                .ws
                .close(Some(1011), Some("the cell is unavailable".to_owned()))
                .await;
            return false;
        };
        if reply.status == 401 {
            socket
                .ws
                .close(Some(4401), Some("token expired".to_owned()))
                .await;
            return false;
        }
        let value = digest(&format!("{}\n{}", reply.status, reply.body));
        let Some(sub) = state.subs.get_mut(&id) else {
            continue;
        };
        if sub.digest == value {
            continue;
        }
        sub.digest.clone_from(&value);
        let body = serde_json::from_str::<Value>(&reply.body).unwrap_or(Value::Null);
        send(
            &socket.ws,
            &json!({"type": "snapshot", "id": id, "status": reply.status, "digest": value, "body": body}),
        );
    }
    true
}

/// After a commit: answer every live subscription on this actor again.
pub async fn publish(actor: &Arc<CellActor>) {
    for socket in actor.sockets().all() {
        if !evaluate(actor, &socket, None).await {
            actor.sockets().remove(socket.id);
        }
    }
}
