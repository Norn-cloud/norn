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
const OUTSIDE_BODY = JSON.stringify({ format: "norn/refusal@2", code: "authz.denied", category: "permission" });

/** What the fake Wasm saw, per test. */
const world = { opened: 0, served: 0, gates: 0 };

class FakeCell {
  constructor(storage, org, env) {
    world.opened += 1;
    this.storage = storage;
    this.scope = [org, env];
    // The real cell reads its definitions and binding when it opens.
    this.provisioned = storage.sql.exec("SELECT provisioned").one?.() ?? false;
  }
  static fixturesEnabled() {
    return false;
  }
  configureConnect() {}
  enableAnalytics() {}
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
  fetch() {
    world.served += 1;
    this.storage.sql.exec("SELECT answer");
    return { status: 200, body: JSON.stringify({ cell: this.scope }) };
  }
}

class FakeGate {
  constructor(org) {
    world.gates += 1;
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
for (const file of ["worker.mjs", "subscribe.mjs", "wake.mjs", "telemetry.mjs", "otlp.mjs"]) copyFileSync(join(import.meta.dir, file), join(dir, file));
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
  close() {}
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
  for (let n = 0; n < sockets; n += 1) socketList.push({ ws: new FakeSocket({ scope, authorization: "Bearer member-of-org_b", subs: {} }), tags: [] });
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
function deploy(objects, { warm }) {
  const instances = new Map();
  const env = { ALLOWED_ORIGINS: "https://studio.example" };
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
