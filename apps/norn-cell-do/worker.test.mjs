// TENANT-ISOLATION-1 (S1, S3, S8): the Worker's side of tenant isolation, through the router
// and the Durable Object class (worker.mjs) over a counting fake storage. An outsider at tenant
// B's busy, provisioned cell gets the raw response (status, headers, body) a vacant scope gives,
// cold and warm, and the object does no storage work for them at all: no SQL, no get/put/list,
// no alarm read or write, no sync, no continuation. The Rust gate behind `NornGate.check` is
// pinned to the cell's own answers in crates/norn-cell-do/tests/tenant_isolation.rs; here it is a
// fake that admits `Bearer member-of-<org>` for its own org only.
// Run: bun test ./worker.test.mjs (from apps/norn-cell-do).
import { plugin } from "bun";
import { afterAll, describe, expect, test } from "bun:test";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const OUTSIDER = "Bearer member-of-org_a";
/** A former member of B: its token no longer verifies (the cell answers 401, the gate refuses it). */
const LAPSED = "Bearer expired-member-of-org_b";
const OUTSIDE_BODY = JSON.stringify({ format: "norn/refusal@2", code: "authz.denied", category: "permission" });

/** What the fake Wasm saw, per test. */
/** `wired` records what the Worker passes across the Wasm boundary: each call and its arguments. */
const world = { opened: 0, served: 0, gates: 0, failOpen: false, wired: [] };

class FakeCell {
  constructor(storage, org, env) {
    world.opened += 1;
    // A cell whose storage or alarms fail to open.
    if (world.failOpen) throw new Error("storage unavailable");
    this.storage = storage;
    this.scope = [org, env];
    // The real cell reads its definitions and binding when it opens.
    this.provisioned = storage.sql.exec("SELECT provisioned").one?.() ?? false;
  }
  static fixturesEnabled() {
    return false;
  }
  configureConnect(sources, vars) {
    world.wired.push({ call: "configureConnect", vars });
  }
  enableAnalytics(signingKey) {
    world.wired.push({ call: "enableAnalytics", signingKey });
  }
  serving() {
    return this.provisioned;
  }
  jwksDue() {
    return false;
  }
  replaceJwks() {}
  nextDue(now) {
    // A busy cell: a deadline is pending.
    return this.provisioned ? now + 5_000 : undefined;
  }
  syncDue() {
    this.storage.sql.exec("SELECT due sync");
    return undefined;
  }
  fetch(_method, _path, _query, authorization) {
    world.served += 1;
    this.storage.sql.exec("SELECT answer");
    if (authorization === LAPSED) return { status: 401, body: JSON.stringify({ code: "authn.required" }) };
    return { status: 200, body: JSON.stringify({ cell: this.scope }) };
  }
}

class FakeGate {
  constructor(org, _env, _issuer, _jwks, _lineageKey, _sources, vars) {
    world.gates += 1;
    world.wired.push({ call: "NornGate", vars });
    this.org = org;
  }
  check(_method, _path, _query, authorization) {
    if (authorization === `Bearer member-of-${this.org}`) return undefined;
    return { status: 403, body: OUTSIDE_BODY, refreshJwks: false };
  }
  jwksDue() {
    return false;
  }
  replaceJwks() {}
}

// The Worker is loaded as it ships (every module beside it), next to a stand-in for its
// wasm-bindgen package (as apps/studio/test/norn-cell-worker.test.ts does).
globalThis.__nornFakeWasm = { NornCell: FakeCell, NornGate: FakeGate };
const dir = mkdtempSync(join(tmpdir(), "norn-cell-isolation-"));
// The bundle's modules (apps/studio/scripts/celld-deploy-cell.ts BUNDLE_FILES).
for (const file of ["worker.mjs", "subscribe.mjs", "wake.mjs", "connect_vars.mjs", "telemetry.mjs", "otlp.mjs"]) copyFileSync(join(import.meta.dir, file), join(dir, file));
mkdirSync(join(dir, "pkg"));
writeFileSync(join(dir, "pkg", "norn_cell_do_bg.wasm"), "");
writeFileSync(
  join(dir, "pkg", "norn_cell_do.js"),
  "export function initSync() {}\nexport const NornCell = globalThis.__nornFakeWasm.NornCell;\nexport const NornGate = globalThis.__nornFakeWasm.NornGate;\n",
);
plugin({
  name: "norn-cell-wasm-stand-in",
  setup(build) {
    build.onLoad({ filter: /norn_cell_do_bg\.wasm$/ }, () => ({ contents: "export default null", loader: "js" }));
  },
});
afterAll(() => rmSync(dir, { recursive: true, force: true }));

// The runtime gives a WebSocket upgrade status 101, which a plain Response refuses.
class UpgradeResponse extends Response {
  constructor(body, init) {
    if (init?.status === 101) {
      super(body, { ...init, status: 200 });
      this.upgraded = true;
      this.webSocket = init.webSocket;
    } else {
      super(body, init);
    }
  }
  get status() {
    return this.upgraded ? 101 : super.status;
  }
}
globalThis.Response = UpgradeResponse;

class FakeSocket {
  constructor(attachment) {
    this.attachment = structuredClone(attachment);
    this.sent = [];
  }
  serializeAttachment(value) {
    this.attachment = structuredClone(value);
  }
  deserializeAttachment() {
    return structuredClone(this.attachment);
  }
  send(data) {
    this.sent.push(JSON.parse(data));
  }
  close(code, reason) {
    this.closed = [code, reason];
  }
}
globalThis.WebSocketPair = class {
  constructor() {
    this[0] = new FakeSocket(null);
    this[1] = new FakeSocket(null);
  }
};

const { default: router, Cell } = await import(join(dir, "worker.mjs"));

/** One Durable Object's storage and context, counting every storage operation. */
function object({ provisioned = false, scope = null, alarm = null, sockets = 0 } = {}) {
  const ops = [];
  const kv = new Map(scope === null ? [] : [["norn.cell.scope", scope]]);
  const socketList = [];
  for (let n = 0; n < sockets; n += 1) socketList.push({ ws: new FakeSocket({ scope, authorization: "Bearer member-of-org_b", subs: {}, role: "member" }), tags: ["member"] });
  const storage = {
    alarm,
    async get(key) {
      ops.push(`get ${key}`);
      return kv.get(key);
    },
    async put(key, value) {
      ops.push(`put ${key}`);
      kv.set(key, value);
    },
    async delete(key) {
      ops.push(`delete ${key}`);
      kv.delete(key);
    },
    async list() {
      ops.push("list");
      return new Map(kv);
    },
    async getAlarm() {
      ops.push("getAlarm");
      return this.alarm;
    },
    async setAlarm(at) {
      ops.push("setAlarm");
      this.alarm = at;
    },
    async deleteAlarm() {
      ops.push("deleteAlarm");
      this.alarm = null;
    },
    transactionSync(f) {
      ops.push("transactionSync");
      return f();
    },
    sql: {
      exec(statement) {
        ops.push(`sql ${statement}`);
        return { one: () => provisioned, toArray: () => [] };
      },
    },
  };
  const waited = [];
  const ctx = {
    storage,
    waitUntil: (promise) => waited.push(promise),
    getWebSockets: (tag) => socketList.filter((s) => tag === undefined || s.tags.includes(tag)).map((s) => s.ws),
    acceptWebSocket: (ws, tags = []) => socketList.push({ ws, tags }),
  };
  return { ctx, ops, waited, sockets: socketList };
}

const B = ["org_b", "prod"];
const NOWHERE = ["org_c", "prod"];

/** A deployment whose cell objects are `objects` (by scope key), each served by `cellFor` (cold: a fresh instance per request). */
function deploy(objects, { warm, bindings = {} }) {
  const instances = new Map();
  const env = { ALLOWED_ORIGINS: "https://studio.example", ...bindings };
  env.CELLS = {
    idFromName: (name) => name,
    get: (name) => ({
      fetch: (request) => {
        let cell = warm ? instances.get(name) : undefined;
        if (cell === undefined) {
          cell = new Cell(objects.get(name).ctx, env);
          instances.set(name, cell);
        }
        return cell.fetch(request);
      },
    }),
  };
  return env;
}

const keyOf = ([org, env]) => `${org}/${env}`;

/** B's busy, provisioned cell (saved scope, pending alarm, 256 members' sockets) and a vacant scope's object. */
function objects() {
  return new Map([
    [keyOf(B), object({ provisioned: true, scope: B, alarm: Date.now() + 60_000, sockets: 256 })],
    [keyOf(NOWHERE), object()],
  ]);
}

const request = ([org, env], { method = "GET", path = "/v1/objects/canon.Item", authorization = OUTSIDER, headers = {}, body } = {}) =>
  new Request(`https://cell.example${path}?orgId=${org}&environmentId=${env}`, { method, body, headers: { authorization, origin: "https://studio.example", ...headers } });

/** The raw response at the Worker boundary. */
async function raw(response) {
  return { status: response.status, headers: [...response.headers].sort(), body: response.status === 101 ? null : await response.text() };
}

/** Reset every counter of `objs`. */
function quiet(objs) {
  for (const o of objs.values()) {
    o.ops.length = 0;
    o.waited.length = 0;
  }
  world.opened = 0;
  world.served = 0;
  world.failOpen = false;
}

describe("an outsider at another tenant's cell", () => {
  for (const warm of [false, true]) {
    const temperature = warm ? "warm" : "cold";
    for (const [method, path, body] of [
      ["GET", "/v1/objects/canon.Item", undefined],
      ["POST", "/v1/runs", JSON.stringify({ flow: "ops.approve_refund" })],
      ["POST", "/v1/uploads:begin", JSON.stringify({ fileName: "rows.csv" })],
      ["GET", "/v1/access", undefined],
    ]) {
      test(`${temperature} ${method} ${path}: a vacant scope's raw response, and no storage work at all`, async () => {
        const objs = objects();
        const env = deploy(objs, { warm });
        if (warm) {
          // Both objects are open: B by its member, the vacant scope by an earlier caller.
          await router.fetch(request(B, { authorization: "Bearer member-of-org_b" }), env);
          await router.fetch(request(NOWHERE, { authorization: "Bearer member-of-org_c" }), env);
        }
        quiet(objs);
        const atB = await raw(await router.fetch(request(B, { method, path, body }), env));
        const atB_ops = [...objs.get(keyOf(B)).ops];
        const atNowhere = await raw(await router.fetch(request(NOWHERE, { method, path, body }), env));
        expect(atB).toEqual(atNowhere);
        expect(atB.status).toBe(403);
        expect(atB.body).toBe(OUTSIDE_BODY);
        // No SQL, no get/put/list, no alarm read or write, at the busy cell or the vacant one.
        expect(atB_ops).toEqual([]);
        expect(objs.get(keyOf(NOWHERE)).ops).toEqual([]);
        // The cell was neither opened nor asked; no continuation or sync was scheduled.
        expect(world.opened).toBe(0);
        expect(world.served).toBe(0);
        for (const o of objs.values()) expect(o.waited).toEqual([]);
        expect(objs.get(keyOf(B)).ctx.storage.alarm).not.toBeNull();
      });
    }
  }

  test("positive control: B's member opens the cell, reads its storage and is served by it", async () => {
    const objs = objects();
    const env = deploy(objs, { warm: false });
    quiet(objs);
    const served = await raw(await router.fetch(request(B, { authorization: "Bearer member-of-org_b" }), env));
    expect(served.status).toBe(200);
    expect(JSON.parse(served.body)).toEqual({ cell: B });
    expect(world.opened).toBe(1);
    expect(objs.get(keyOf(B)).ops.some((op) => op.startsWith("sql "))).toBe(true);
    expect(objs.get(keyOf(B)).ops).toContain("getAlarm");
  });
});

describe("a failed cell open (S1)", () => {
  test("never changes an outsider's answer: the gate's own failure state is apart from the cell's", async () => {
    const objs = objects();
    const env = deploy(objs, { warm: true });
    quiet(objs);
    const vacant = await raw(await router.fetch(request(NOWHERE), env));
    // B's member makes the object open its cell, and the open fails (its storage or alarms).
    world.failOpen = true;
    await expect(router.fetch(request(B, { authorization: "Bearer member-of-org_b" }), env)).rejects.toThrow("storage unavailable");
    // Inside the failed open's retry interval the outsider still gets the normal refusal.
    world.failOpen = false;
    const atB = await raw(await router.fetch(request(B), env));
    expect(atB).toEqual(vacant);
    expect(atB.status).toBe(403);
    // The member still sees the cached open failure until it may retry.
    await expect(router.fetch(request(B, { authorization: "Bearer member-of-org_b" }), env)).rejects.toThrow("storage unavailable");
  });
});

describe("an unclassified or lapsed socket (S3)", () => {
  // A socket a Worker from before the gate accepted: its attachment has no membership decision.
  const legacy = () => new FakeSocket({ scope: B, authorization: "Bearer member-of-org_b", subs: { a: { pathname: "/v1/objects/canon.Item", query: "", digest: "" } } });

  test("a message on a socket without classification closes it; storage is never opened for it", async () => {
    const objs = objects();
    const env = deploy(objs, { warm: false });
    quiet(objs);
    const ws = legacy();
    await new Cell(objs.get(keyOf(B)).ctx, env).webSocketMessage(ws, JSON.stringify({ type: "subscribe", id: "b", path: "/v1/objects/canon.Item" }));
    expect(ws.closed).toEqual([1012, "reconnect"]);
    expect(ws.sent).toEqual([]);
    expect(world.opened).toBe(0);
    expect(objs.get(keyOf(B)).ops).toEqual([]);
  });

  test("an unclassified socket is never counted, published to or answered as a member, and no one else's activity closes it", async () => {
    const objs = new Map([[keyOf(B), object({ provisioned: true, scope: B, alarm: Date.now() + 60_000 })]]);
    const { ctx, sockets } = objs.get(keyOf(B));
    // 256 unclassified sockets would fill the member quota if they counted as members.
    const old = [];
    for (let n = 0; n < 256; n += 1) {
      const ws = legacy();
      old.push(ws);
      sockets.push({ ws, tags: [] });
    }
    const env = deploy(objs, { warm: true });
    const member = await router.fetch(request(B, { path: "/v1/subscribe", authorization: "Bearer member-of-org_b", headers: { upgrade: "websocket" } }), env);
    expect(member.status).toBe(101);
    // A commit's publish asks the cell only for classified members.
    quiet(objs);
    const cell = new Cell(ctx, env);
    await cell.ready(B);
    world.served = 0;
    await cell.publish();
    for (const ws of old) {
      expect(ws.closed).toBeUndefined();
      expect(ws.sent).toEqual([]);
    }
    expect(world.served).toBe(0);
  });

  /**
   * One run at B: a legacy (unclassified) socket, a lapsed member's socket, a former member's
   * socket (saved role member, its token now an outsider's) and an outsider's socket sit idle;
   * a member commits (or does not), then each idle socket sends its own message.
   */
  async function run({ commit }) {
    const objs = new Map([[keyOf(B), object({ provisioned: true, scope: B, alarm: Date.now() + 60_000 })]]);
    const { ctx, sockets } = objs.get(keyOf(B));
    const sub = { a: { pathname: "/v1/objects/canon.Item", query: "orgId=org_b&environmentId=prod", digest: "" } };
    const idle = {
      legacy: new FakeSocket({ scope: B, authorization: OUTSIDER, subs: sub }),
      lapsed: new FakeSocket({ scope: B, authorization: LAPSED, subs: sub, role: "member" }),
      former: new FakeSocket({ scope: B, authorization: OUTSIDER, subs: sub, role: "member" }),
      outsider: new FakeSocket({ scope: B, authorization: OUTSIDER, subs: sub, role: "outside" }),
    };
    const watching = new FakeSocket({ scope: B, authorization: "Bearer member-of-org_b", subs: sub, role: "member" });
    sockets.push({ ws: idle.legacy, tags: [] }, { ws: idle.lapsed, tags: ["member"] }, { ws: idle.former, tags: ["member"] }, { ws: idle.outsider, tags: ["outside"] }, { ws: watching, tags: ["member"] });
    const env = deploy(objs, { warm: true });
    if (commit) {
      const committed = await router.fetch(request(B, { method: "POST", path: "/v1/runs", authorization: "Bearer member-of-org_b", body: JSON.stringify({ flow: "ops.approve_refund" }) }), env);
      expect(committed.status).toBe(200);
      // The publish is not awaited by the response; let it finish.
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    const afterCommit = Object.fromEntries(Object.entries(idle).map(([name, ws]) => [name, { closed: ws.closed, sent: structuredClone(ws.sent) }]));
    const cell = new Cell(ctx, env);
    for (const ws of Object.values(idle)) await cell.webSocketMessage(ws, JSON.stringify({ type: "subscribe", id: "b", path: "/v1/objects/canon.Item?orgId=org_b&environmentId=prod" }));
    const afterMessage = Object.fromEntries(Object.entries(idle).map(([name, ws]) => [name, { closed: ws.closed, sent: ws.sent.map(({ digest: _, ...rest }) => rest) }]));
    return { afterCommit, afterMessage, watching: watching.sent.length };
  }

  test("an outsider, a legacy socket and a lapsed or former member see no difference whether or not a commit happens", async () => {
    const without = await run({ commit: false });
    const withCommit = await run({ commit: true });
    // Positive control: the commit did publish to the member who is one now.
    expect(without.watching).toBe(0);
    expect(withCommit.watching).toBe(1);
    // During the publish nothing reached or closed any of the others.
    expect(withCommit.afterCommit).toEqual(without.afterCommit);
    for (const seen of Object.values(withCommit.afterCommit)) expect(seen).toEqual({ closed: undefined, sent: [] });
    // Each is ended or answered by its own message, identically with or without the commit.
    expect(withCommit.afterMessage).toEqual(without.afterMessage);
    expect(withCommit.afterMessage.legacy.closed).toEqual([1012, "reconnect"]);
    expect(withCommit.afterMessage.lapsed.closed).toEqual([4401, "not a member"]);
    expect(withCommit.afterMessage.former.closed).toEqual([4401, "not a member"]);
    expect(withCommit.afterMessage.outsider.sent.map((m) => m.status)).toEqual([403]);
  });

  test("a publish never closes a socket whose token the cell refuses (a lapse after the gate check)", async () => {
    const objs = new Map([[keyOf(B), object({ provisioned: true, scope: B })]]);
    const env = deploy(objs, { warm: true });
    const cell = new Cell(objs.get(keyOf(B)).ctx, env);
    await cell.ready(B);
    // A socket the gate still admits but whose token the cell refuses at publish time (a lapse in between).
    const ws = new FakeSocket({ scope: B, authorization: LAPSED, subs: { a: { pathname: "/v1/objects/canon.Item", query: "", digest: "" } }, role: "member" });
    const live = await import(join(dir, "subscribe.mjs"));
    expect(await live.publish({ getWebSockets: () => [ws] }, (path, query, authorization) => cell.ask(path, query, authorization))).toBe(0);
    expect(ws.closed).toBeUndefined();
    expect(ws.sent).toEqual([]);
  });

  test("a member socket whose token is no longer a member is closed before storage is reopened", async () => {
    const objs = objects();
    const env = deploy(objs, { warm: false });
    quiet(objs);
    const ws = new FakeSocket({ scope: B, authorization: OUTSIDER, subs: {}, role: "member" });
    await new Cell(objs.get(keyOf(B)).ctx, env).webSocketMessage(ws, JSON.stringify({ type: "subscribe", id: "b", path: "/v1/objects/canon.Item" }));
    expect(ws.closed).toEqual([4401, "not a member"]);
    expect(world.opened).toBe(0);
    expect(objs.get(keyOf(B)).ops).toEqual([]);
    // Positive control: a member's socket reopens the cell and is answered by it.
    const ok = new FakeSocket({ scope: B, authorization: "Bearer member-of-org_b", subs: {}, role: "member" });
    await new Cell(objs.get(keyOf(B)).ctx, env).webSocketMessage(ok, JSON.stringify({ type: "subscribe", id: "b", path: "/v1/objects/canon.Item" }));
    expect(ok.closed).toBeUndefined();
    expect(world.opened).toBe(1);
    expect(ok.sent.map((m) => m.status)).toEqual([200]);
  });
});

describe("an outsider's live socket", () => {
  for (const warm of [false, true]) {
    test(`${warm ? "warm" : "cold"}: the upgrade at a busy foreign cell answers as at a vacant scope, with no storage work`, async () => {
      const objs = objects();
      const env = deploy(objs, { warm });
      if (warm) await router.fetch(request(B, { authorization: "Bearer member-of-org_b" }), env);
      quiet(objs);
      const upgrade = (scope) => request(scope, { path: "/v1/subscribe", headers: { upgrade: "websocket", "sec-websocket-protocol": "norn.live.v1" } });
      const atB = await raw(await router.fetch(upgrade(B), env));
      const atNowhere = await raw(await router.fetch(upgrade(NOWHERE), env));
      // B holds 256 members' sockets (its member quota is full); the outsider never sees it.
      expect(atB).toEqual(atNowhere);
      expect(atB.status).toBe(101);
      expect(objs.get(keyOf(B)).ops).toEqual([]);
      expect(objs.get(keyOf(NOWHERE)).ops).toEqual([]);
      expect(world.opened).toBe(0);

      // Its subscription is answered by the gate: the cell is not opened for it either.
      const socket = objs.get(keyOf(B)).sockets.at(-1);
      expect(socket.tags).toEqual(["outside"]);
      const cell = new Cell(objs.get(keyOf(B)).ctx, env);
      await cell.webSocketMessage(socket.ws, JSON.stringify({ type: "subscribe", id: "b", path: "/v1/objects/canon.Item?orgId=org_b&environmentId=prod" }));
      expect(socket.ws.sent).toEqual([{ type: "snapshot", id: "b", status: 403, digest: expect.any(String), body: JSON.parse(OUTSIDE_BODY) }]);
      expect(objs.get(keyOf(B)).ops).toEqual([]);
      expect(world.opened).toBe(0);
    });
  }

  test("positive control: B's member at its full cell is refused for the quota", async () => {
    const objs = objects();
    const env = deploy(objs, { warm: false });
    const member = await router.fetch(request(B, { path: "/v1/subscribe", authorization: "Bearer member-of-org_b", headers: { upgrade: "websocket" } }), env);
    expect(member.status).toBe(429);
  });
});

describe("the analytics signing key's wiring (OLAP-01)", () => {
  const SIGNING = "k1:c2lnbmluZy1zZWVkLW5ldmVyLWZvci1jb25uZWN0b3Jz";
  // A deployment that still carries the binding the signing key had before OLAP-01.
  const LEGACY = "k0:bGVnYWN5LXNpZ25pbmcta2V5LWluLWNvbm5lY3Rpb24tbmFtZXNwYWNl";
  const bindings = {
    NORN_OLAP_URL: "http://olapd.internal:8790",
    NORN_OLAP_SIGNING_KEY: SIGNING,
    NORN_SECRET_OLAP_SIGNING_KEY: LEGACY,
    NORN_LINEAGE_KEY: "lineage-key-of-at-least-thirty-two-bytes",
    NORN_SOURCES: "[]",
    NORN_SECRET_ERPNEXT_REST_TOKEN: "connection-token",
    NORN_S3_SECRET_ACCESS_KEY: "storage-secret",
  };

  test("a member's open hands the signing key to enableAnalytics only, never to a connection's variables", async () => {
    const objs = objects();
    const env = deploy(objs, { warm: false, bindings });
    quiet(objs);
    world.wired.length = 0;
    const served = await router.fetch(request(B, { authorization: "Bearer member-of-org_b" }), env);
    expect(served.status).toBe(200);
    // The gate and the cell each got the connection variables once; analytics got its key once.
    expect(world.wired.map((w) => w.call).sort()).toEqual(["NornGate", "configureConnect", "enableAnalytics"]);
    const analytics = world.wired.filter((w) => w.call === "enableAnalytics");
    expect(analytics).toEqual([{ call: "enableAnalytics", signingKey: SIGNING }]);
    for (const { call, vars } of world.wired.filter((w) => w.call !== "enableAnalytics")) {
      expect(vars, call).not.toContain(SIGNING);
      expect(vars, call).not.toContain(SIGNING.split(":")[1]);
      // The connections still get their own secrets and storage settings.
      expect(JSON.parse(vars), call).toMatchObject({ NORN_SECRET_ERPNEXT_REST_TOKEN: "connection-token", NORN_S3_SECRET_ACCESS_KEY: "storage-secret" });
    }
  });
});
