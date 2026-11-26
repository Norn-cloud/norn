// Live reads over one hibernating WebSocket per client (UI-LIVE-1, protocol `norn.live.v1`).
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

export const PROTOCOL = "norn.live.v1";
/** Subscriptions per socket. The attachment (scope, token, subscriptions) must fit 2 KiB. */
export const MAX_SUBSCRIPTIONS = 8;
/** Sockets per cell object. */
export const MAX_SOCKETS = 256;
/**
 * TENANT-ISOLATION-1 (S3): sockets per cell object of callers who are not its members. They are
 * counted apart (by this tag) and answered by the membership gate, never the cell, so a member's
 * socket never shapes what an outsider sees, and outsiders never crowd out members.
 */
export const MAX_OUTSIDE_SOCKETS = 32;
/**
 * S3: every socket's attachment carries the membership decision made when it was accepted
 * (`role`): MEMBER or OUTSIDE. A socket without one (accepted before the gate) is neither: it is
 * never counted, answered or published to as a member, and it is closed (`reclassify`) only on its
 * own message, never on another caller's activity (an upgrade or a commit), which it could observe.
 */
export const OUTSIDE = "outside";
export const MEMBER = "member";
const MAX_ATTACHMENT = 2000;
const MAX_MESSAGE = 4096;
const UTF8 = new TextEncoder();
const RESERVED_IDS = new Set(["__proto__", "constructor", "prototype"]);

const utf8Bytes = (text) => UTF8.encode(text).byteLength;

async function digest(text) {
  const bytes = new Uint8Array(await crypto.subtle.digest("SHA-256", UTF8.encode(text)));
  return Array.from(bytes.slice(0, 12), (b) => b.toString(16).padStart(2, "0")).join("");
}

/** A path a subscription may name: one of this cell's GET routes, never the live route itself. */
export function subscribable(path) {
  if (typeof path !== "string" || path.length > 512 || /[\x00-\x1f\x7f\\]/.test(path) || path.includes("..")) return null;
  let url;
  try {
    url = new URL(path, "https://cell");
  } catch {
    return null;
  }
  if (url.origin !== "https://cell" || !url.pathname.startsWith("/v1/") || url.pathname === "/v1/subscribe" || url.pathname.startsWith("/v1/subscribe/")) return null;
  return { pathname: url.pathname, query: url.search.slice(1) };
}

/** Send one message (or a frame already encoded); whether the socket took it. */
const send = (ws, message) => {
  try {
    ws.send(typeof message === "string" ? message : JSON.stringify(message));
    return true;
  } catch {
    // A socket closing under us: its close handler cleans up.
    return false;
  }
};

/**
 * Accept one live socket on `ctx` for `scope` as `authorization`. The upgrade itself carries no
 * data; every answer comes later from `evaluate`.
 */
export function accept(ctx, scope, authorization, requested = "", { outside = false } = {}) {
  // An outsider is counted only among outsiders; a member among members.
  const open = outside ? ctx.getWebSockets(OUTSIDE).length : members(ctx).length;
  const limit = outside ? MAX_OUTSIDE_SOCKETS : MAX_SOCKETS;
  if (open >= limit) {
    return Response.json({ format: "norn/refusal@2", retryable: true, reason: null, code: "live.too_many_sockets", category: "limit", message: "this cell has too many live readers", explanation: `at most ${limit} sockets per cell` }, { status: 429 });
  }
  const role = outside ? OUTSIDE : MEMBER;
  const attachment = { scope, authorization, subs: {}, role };
  if (utf8Bytes(JSON.stringify(attachment)) > MAX_ATTACHMENT) {
    return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code: "request.too_large", category: "limit", message: "the live authorization header is too large", explanation: `socket state must fit within ${MAX_ATTACHMENT} bytes` }, { status: 413 });
  }
  const [client, server] = Object.values(new WebSocketPair());
  ctx.acceptWebSocket(server, [role]);
  server.serializeAttachment(attachment);
  // Echo the protocol only when the client asked for it: a strict client refuses an unasked one.
  const asked = requested.split(",").map((name) => name.trim()).includes(PROTOCOL);
  return new Response(null, { status: 101, webSocket: client, ...(asked ? { headers: { "sec-websocket-protocol": PROTOCOL } } : {}) });
}

/** The membership decision an attachment carries: MEMBER, OUTSIDE, or null (unclassified). */
export function roleOf(attachment) {
  const role = attachment?.role;
  return role === MEMBER || role === OUTSIDE ? role : null;
}

/** The object's members' sockets: only those classified MEMBER when accepted. */
export function members(ctx) {
  return ctx.getWebSockets().filter((ws) => roleOf(ws.deserializeAttachment()) === MEMBER);
}

/** Close a socket no membership decision classified; the client reconnects through the gate. */
export function reclassify(ws) {
  try {
    ws.close(1012, "reconnect");
  } catch {
    // Already closed.
  }
}

/**
 * Answer every subscription of `ws` again and send the ones that changed (`tally.sent` counts the sends the socket took). `ask(path, query,
 * authorization)` is the cell's own GET. Returns false when the token no longer verifies: the
 * socket is closed, or, when `quiet` (a publish, S3), left untouched so another caller's commit is
 * never what ends it; its own next message does.
 */
export async function evaluate(ws, ask, only, tally, { quiet = false } = {}) {
  const attachment = ws.deserializeAttachment();
  if (!attachment || typeof attachment !== "object" || !attachment.subs || typeof attachment.subs !== "object") return false;
  const updates = new Map();
  for (const [id, sub] of Object.entries(attachment.subs)) {
    if (only !== undefined && id !== only) continue;
    const reply = ask(sub.pathname, sub.query, attachment.authorization);
    if (reply.status === 401) {
      if (!quiet) ws.close(4401, "token expired");
      return false;
    }
    const value = await digest(`${reply.status}\n${reply.body}`);
    if (value === sub.digest) continue;
    updates.set(id, { pathname: sub.pathname, query: sub.query, digest: value });
    // The body is the GET's own JSON text, byte for byte: re-encoding a parsed value would round a
    // decimal beyond double precision. Text that is not JSON is sent as null.
    let json = true;
    try {
      JSON.parse(reply.body);
    } catch {
      json = false;
    }
    const frame = `{"type":"snapshot","id":${JSON.stringify(id)},"status":${JSON.stringify(reply.status)},"digest":${JSON.stringify(value)},"body":${json ? reply.body : "null"}}`;
    if (send(ws, frame) && tally !== undefined) tally.sent += 1;
  }
  if (updates.size > 0) {
    // Re-read attachment after async digest() so concurrent subscribe/unsubscribe updates are not overwritten.
    const latest = ws.deserializeAttachment();
    if (latest && typeof latest === "object" && latest.subs && typeof latest.subs === "object") {
      let merged = false;
      for (const [id, updated] of updates) {
        if (!Object.hasOwn(latest.subs, id)) continue;
        const current = latest.subs[id];
        if (current && current.pathname === updated.pathname && current.query === updated.query) {
          current.digest = updated.digest;
          merged = true;
        }
      }
      if (merged) {
        try {
          ws.serializeAttachment(latest);
        } catch {
          // Socket closed or attachment limit reached during publish.
        }
      }
    }
  }
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
  if (!attachment || typeof attachment !== "object" || !attachment.subs || typeof attachment.subs !== "object") {
    return ws.close(1011, "no live state");
  }
  const id = typeof msg?.id === "string" && /^[A-Za-z0-9_.:-]{1,64}$/.test(msg.id) && !RESERVED_IDS.has(msg.id) ? msg.id : null;
  if (id === null) return send(ws, { type: "refused", id: null, code: "live.invalid_id", message: "a subscription id is 1 to 64 letters, digits or _.:-" });
  if (msg.type === "unsubscribe") {
    if (Object.hasOwn(attachment.subs, id)) {
      delete attachment.subs[id];
      ws.serializeAttachment(attachment);
    }
    return;
  }
  if (msg.type !== "subscribe") return send(ws, { type: "refused", id, code: "live.unknown_message", message: "messages are subscribe or unsubscribe" });
  const target = subscribable(msg.path);
  if (target === null) return send(ws, { type: "refused", id, code: "live.not_subscribable", message: "a subscription names one of this cell's GET routes" });
  const existed = Object.hasOwn(attachment.subs, id);
  if (!existed && Object.keys(attachment.subs).length >= MAX_SUBSCRIPTIONS) {
    return send(ws, { type: "refused", id, code: "live.too_many_subscriptions", message: `at most ${MAX_SUBSCRIPTIONS} subscriptions per socket` });
  }
  const previous = existed ? attachment.subs[id] : undefined;
  Object.defineProperty(attachment.subs, id, {
    value: { ...target, digest: "" },
    enumerable: true,
    configurable: true,
    writable: true,
  });
  const restore = () => {
    if (existed) {
      Object.defineProperty(attachment.subs, id, {
        value: previous,
        enumerable: true,
        configurable: true,
        writable: true,
      });
    } else {
      delete attachment.subs[id];
    }
  };
  if (utf8Bytes(JSON.stringify(attachment)) > MAX_ATTACHMENT) {
    restore();
    return send(ws, { type: "refused", id, code: "live.too_many_subscriptions", message: "this socket's subscriptions are full" });
  }
  try {
    ws.serializeAttachment(attachment);
  } catch {
    restore();
    return send(ws, { type: "refused", id, code: "live.too_many_subscriptions", message: "this socket's subscriptions are full" });
  }
  await evaluate(ws, ask, id);
}

/**
 * After a commit: answer every live subscription on `ctx`'s sockets again. Resolves to the number
 * of changed snapshots the sockets took. A publish never closes a socket (S3): a token that lapsed
 * is left for the socket's own next message to end.
 */
export async function publish(ctx, ask) {
  const tally = { sent: 0 };
  for (const ws of ctx.getWebSockets()) await evaluate(ws, ask, undefined, tally, { quiet: true });
  return tally.sent;
}
