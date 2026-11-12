// Live reads over one hibernating WebSocket per client (UI-LIVE-1, protocol `norn.live/1`).
//
// A subscription names a GET route of this cell. The cell answers it the way it answers that GET:
// the same Rust handler, the same principal, the same authorization, run again as the subscriber
// after every commit this object makes. Only a changed answer is sent. Nothing here reads data,
// filters rows or decides access; it only re-asks the cell and forwards what the cell said. A
// token that expires (or a principal the cell no longer admits) ends the socket; the client opens a
// new one with a fresh token, so revocation reaches live reads within one token lifetime.
//
// Client → cell: {"type":"subscribe","id":"<id>","path":"/v1/…?…"} | {"type":"unsubscribe","id":"<id>"}
// Cell → client: {"type":"snapshot","id","status","digest","body"} on subscribe and on every change,
//                {"type":"refused","id","code","message"} for a subscription the protocol refuses,
//                then close 4401 when the token no longer verifies.

export const PROTOCOL = "norn.live/1";
/** Subscriptions per socket. The attachment (scope, token, subscriptions) must fit 2 KiB. */
export const MAX_SUBSCRIPTIONS = 8;
/** Sockets per cell object. */
export const MAX_SOCKETS = 256;
const MAX_ATTACHMENT = 2000;
const MAX_MESSAGE = 4096;

async function digest(text) {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  return Array.from(bytes.slice(0, 12), (b) => b.toString(16).padStart(2, "0")).join("");
}

/** A path a subscription may name: one of this cell's GET routes, never the live route itself. */
export function subscribable(path) {
  if (typeof path !== "string" || path.length > 512 || !path.startsWith("/v1/") || path.startsWith("/v1/subscribe")) return null;
  const url = new URL(path, "https://cell");
  if (url.pathname.includes("..")) return null;
  return { pathname: url.pathname, query: url.search.slice(1) };
}

const send = (ws, message) => {
  try {
    ws.send(JSON.stringify(message));
  } catch {
    // A socket closing under us: its close handler cleans up.
  }
};

/**
 * Accept one live socket on `ctx` for `scope` as `authorization`. The upgrade itself carries no
 * data; every answer comes later from `evaluate`.
 */
export function accept(ctx, scope, authorization, requested = "") {
  if (ctx.getWebSockets().length >= MAX_SOCKETS) {
    return Response.json({ format: "norn/refusal@2", retryable: true, reason: null, code: "live.too_many_sockets", category: "limit", message: "this cell has too many live readers", explanation: `at most ${MAX_SOCKETS} sockets per cell` }, { status: 429 });
  }
  const [client, server] = Object.values(new WebSocketPair());
  ctx.acceptWebSocket(server);
  server.serializeAttachment({ scope, authorization, subs: {} });
  // Echo the protocol only when the client asked for it: a strict client refuses an unasked one.
  const asked = requested.split(",").map((name) => name.trim()).includes(PROTOCOL);
  return new Response(null, { status: 101, webSocket: client, ...(asked ? { headers: { "sec-websocket-protocol": PROTOCOL } } : {}) });
}

/**
 * Answer every subscription of `ws` again and send the ones that changed. `ask(path, query,
 * authorization)` is the cell's own GET. Returns false when the socket was closed for an expired
 * or refused token.
 */
export async function evaluate(ws, ask, only) {
  const attachment = ws.deserializeAttachment();
  if (!attachment) return false;
  let changed = false;
  for (const [id, sub] of Object.entries(attachment.subs)) {
    if (only !== undefined && id !== only) continue;
    const reply = ask(sub.pathname, sub.query, attachment.authorization);
    if (reply.status === 401) {
      ws.close(4401, "token expired");
      return false;
    }
    const value = await digest(`${reply.status}\n${reply.body}`);
    if (value === sub.digest) continue;
    sub.digest = value;
    changed = true;
    let body = null;
    try {
      body = JSON.parse(reply.body);
    } catch {
      body = null;
    }
    send(ws, { type: "snapshot", id, status: reply.status, digest: value, body });
  }
  if (changed) ws.serializeAttachment(attachment);
  return true;
}

/** Handle one client message. */
export async function message(ws, raw, ask) {
  if (typeof raw !== "string" || raw.length > MAX_MESSAGE) return ws.close(1009, "message too large");
  let msg;
  try {
    msg = JSON.parse(raw);
  } catch {
    return ws.close(1007, "not JSON");
  }
  const attachment = ws.deserializeAttachment();
  const id = typeof msg?.id === "string" && /^[A-Za-z0-9_.:-]{1,64}$/.test(msg.id) ? msg.id : null;
  if (id === null) return send(ws, { type: "refused", id: null, code: "live.invalid_id", message: "a subscription id is 1 to 64 letters, digits or _.:-" });
  if (msg.type === "unsubscribe") {
    delete attachment.subs[id];
    ws.serializeAttachment(attachment);
    return;
  }
  if (msg.type !== "subscribe") return send(ws, { type: "refused", id, code: "live.unknown_message", message: "messages are subscribe or unsubscribe" });
  const target = subscribable(msg.path);
  if (target === null) return send(ws, { type: "refused", id, code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" });
  if (!(id in attachment.subs) && Object.keys(attachment.subs).length >= MAX_SUBSCRIPTIONS) {
    return send(ws, { type: "refused", id, code: "live.too_many_subscriptions", message: `at most ${MAX_SUBSCRIPTIONS} subscriptions per socket` });
  }
  attachment.subs[id] = { ...target, digest: "" };
  if (JSON.stringify(attachment).length > MAX_ATTACHMENT) {
    delete attachment.subs[id];
    return send(ws, { type: "refused", id, code: "live.too_many_subscriptions", message: "this socket's subscriptions are full" });
  }
  ws.serializeAttachment(attachment);
  await evaluate(ws, ask, id);
}

/** After a commit: answer every live subscription on this object again. */
export async function publish(ctx, ask) {
  for (const ws of ctx.getWebSockets()) await evaluate(ws, ask);
}
