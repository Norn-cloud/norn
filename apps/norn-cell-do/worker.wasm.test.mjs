// TENANT-ISOLATION-1 (S8): the Worker boundary over the real Wasm gate. worker.test.mjs pins the
// Worker's own routing and storage discipline with a stand-in gate; here the shipped wasm-bindgen
// package (NORN_CELL_DO_PKG: a directory holding norn_cell_do.js and norn_cell_do_bg.wasm, built
// as apps/norn-cell-do/README.md says) answers, over a SQLite-backed Durable Object storage.
// B's owner provisions B's cell through the Worker, and B's member reads it there; then a member
// of org A, with a real host token, asks B's provisioned cell and a scope nobody provisioned: the
// raw responses (status, every header, body) are byte for byte the same, and neither object's
// storage is touched.
// Run: NORN_CELL_DO_PKG=<pkg> bun test ./worker.wasm.test.mjs (from apps/norn-cell-do).
import { plugin } from "bun";
import { Database } from "bun:sqlite";
import { afterAll, describe, expect, test } from "bun:test";
import { copyFileSync, cpSync, existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PKG = process.env.NORN_CELL_DO_PKG;
// A missing package is a broken setup, never a skip.
if (PKG === undefined || !existsSync(join(PKG, "norn_cell_do.js")) || !existsSync(join(PKG, "norn_cell_do_bg.wasm"))) {
  throw new Error("NORN_CELL_DO_PKG must name the built norn-cell-do wasm-bindgen package (norn_cell_do.js, norn_cell_do_bg.wasm)");
}

const ISSUER = "https://issuer.example";
const JWKS_URL = "https://issuer.example/.well-known/jwks.json";
const KID = "boundary-1";

// The host's signing key and its pinned JWKS (EdDSA, as the cell verifies).
const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]);
const publicJwk = await crypto.subtle.exportKey("jwk", keys.publicKey);
const JWKS = JSON.stringify({ keys: [{ kty: "OKP", crv: "Ed25519", x: publicJwk.x, kid: KID, alg: "EdDSA", use: "sig" }] });
const b64 = (bytes) => Buffer.from(bytes).toString("base64url");
/** A host token for `sub`, a `role` member of `org`. */
async function token(org, sub = "ana", role = "admin") {
  const now = Math.floor(Date.now() / 1000);
  const head = b64(JSON.stringify({ alg: "EdDSA", kid: KID, typ: "JWT" }));
  const body = b64(JSON.stringify({ iss: ISSUER, aud: "norn-host", sub, iat: now, exp: now + 240, norn: { kind: "user", org, role, amr: "password" } }));
  const signature = await crypto.subtle.sign("Ed25519", keys.privateKey, new TextEncoder().encode(`${head}.${body}`));
  return `Bearer ${head}.${body}.${b64(new Uint8Array(signature))}`;
}

// The issuer answers only its pinned JWKS URL; nothing else leaves the Worker in this test.
const fetched = [];
globalThis.fetch = async (input) => {
  const url = typeof input === "string" ? input : input.url;
  fetched.push(url);
  if (url === JWKS_URL) return new Response(JWKS, { status: 200, headers: { "content-type": "application/json" } });
  throw new Error(`unexpected fetch ${url}`);
};

// The Worker as it ships (every module beside it), with the built package as its pkg/.
const dir = mkdtempSync(join(tmpdir(), "norn-cell-wasm-boundary-"));
for (const file of ["worker.mjs", "subscribe.mjs", "wake.mjs", "connect_vars.mjs", "telemetry.mjs", "otlp.mjs"]) copyFileSync(join(import.meta.dir, file), join(dir, file));
// The whole package: wasm-bindgen's inline JS snippets live beside the module.
cpSync(PKG, join(dir, "pkg"), { recursive: true });
// The Workers bundler imports the .wasm as a compiled module; so does this loader.
plugin({
  name: "norn-cell-wasm-module",
  setup(build) {
    build.onLoad({ filter: /norn_cell_do_bg\.wasm$/ }, (args) => ({
      contents: `import { readFileSync } from "node:fs";\nexport default new WebAssembly.Module(readFileSync(${JSON.stringify(args.path)}));`,
      loader: "js",
    }));
  },
});
afterAll(() => rmSync(dir, { recursive: true, force: true }));

// The runtime gives a WebSocket upgrade status 101, which a plain Response refuses.
class UpgradeResponse extends Response {
  constructor(body, init) {
    if (init?.status === 101) {
      super(body, { ...init, status: 200 });
      this.upgraded = true;
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
  serializeAttachment(value) {
    this.attachment = structuredClone(value);
  }
  deserializeAttachment() {
    return structuredClone(this.attachment);
  }
  send() {}
  close() {}
}
globalThis.WebSocketPair = class {
  constructor() {
    this[0] = new FakeSocket();
    this[1] = new FakeSocket();
  }
};

const { default: router, Cell } = await import(join(dir, "worker.mjs"));

/**
 * A Durable Object over a real SQLite database (bun:sqlite) with the DO storage surface the cell and
 * the Worker use (sql.exec(...).raw(), transactionSync, get/put/delete, alarms). Every access is
 * recorded in `touched`, so a test sees whether an outsider's request reached it at all.
 */
function object() {
  const touched = [];
  const db = new Database(":memory:");
  const kv = new Map();
  let alarm = null;
  const cursor = (rows) => ({ raw: () => rows[Symbol.iterator](), toArray: () => rows, one: () => rows[0] });
  const surface = {
    sql: {
      exec(statement, ...params) {
        // A parameterless multi-statement script (a migration) runs whole; anything else is one statement.
        const body = statement.trim().replace(/;\s*$/, "");
        if (params.length === 0 && body.includes(";")) {
          db.run(statement);
          return cursor([]);
        }
        // bun:sqlite answers null for a statement that returns no rows; the DO cursor is empty.
        return cursor(db.prepare(statement).values(...params) ?? []);
      },
    },
    transactionSync: (callback) => db.transaction(callback)(),
    get: async (key) => structuredClone(kv.get(key)),
    put: async (key, value) => void kv.set(key, structuredClone(value)),
    delete: async (key) => kv.delete(key),
    list: async () => new Map(kv),
    getAlarm: async () => alarm,
    setAlarm: async (at) => void (alarm = at),
    deleteAlarm: async () => void (alarm = null),
  };
  const storage = new Proxy(surface, {
    get: (target, name) => {
      touched.push(String(name));
      return target[name];
    },
  });
  const sockets = [];
  const ctx = {
    storage,
    waitUntil: (promise) => {
      touched.push("waitUntil");
      return promise;
    },
    getWebSockets: (tag) => sockets.filter((s) => tag === undefined || s.tags.includes(tag)).map((s) => s.ws),
    acceptWebSocket: (ws, tags = []) => sockets.push({ ws, tags }),
  };
  return { ctx, touched };
}

const ENV = {
  ALLOWED_ORIGINS: "https://studio.example",
  NORN_HOST_ISSUER: ISSUER,
  NORN_HOST_JWKS_URL: JWKS_URL,
  NORN_LINEAGE_KEY: "boundary-test-lineage-key-0123456789abcdef",
  NORN_TENANT_ENVIRONMENTS: "dev,prod",
};

/** A deployment of cell objects (created on first use), each a cold Cell per request. */
function deploy() {
  const objects = new Map();
  const env = { ...ENV };
  env.CELLS = {
    idFromName: (name) => name,
    get: (name) => ({
      fetch: (request) => {
        if (!objects.has(name)) objects.set(name, object());
        return new Cell(objects.get(name).ctx, env).fetch(request);
      },
    }),
  };
  return { env, objects };
}

const request = ([org, environment], authorization, { method = "GET", path = "/v1/objects/canon.Item", headers = {}, body } = {}) =>
  new Request(`https://cell.example${path}?orgId=${org}&environmentId=${environment}`, { method, body, headers: { authorization, origin: "https://studio.example", ...headers } });

/** The raw response at the Worker boundary. */
async function raw(response) {
  return { status: response.status, headers: [...response.headers].sort(), body: response.status === 101 ? null : await response.text() };
}

/**
 * Two sequential router.fetch calls can straddle a UTC second boundary, shifting the body's
 * `"decidedAt":"<UTC second>"` field by 1 s (e.g. 03:40:21Z vs 03:40:22Z) while every other byte
 * is identical. Validate both timestamps as calendar-valid ISO UTC seconds within a 2 s window,
 * replace ONLY that matched value with a fixed placeholder in both bodies, then assert exact
 * equality on { status, headers, body } — preserving byte-for-byte comparison on every other field.
 */
function expectSameRefusal(actual, expected) {
  const pattern = /"decidedAt":"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z"/;
  const stamp = (body, label) => {
    const match = body.match(pattern);
    expect(match, `${label} body must contain a "decidedAt":"<UTC second>" field`).not.toBeNull();
    const value = match[0].slice('"decidedAt":"'.length, -1);
    const ms = Date.parse(value);
    expect(Number.isNaN(ms), `${label} decidedAt "${value}" must parse as a UTC timestamp`).toBe(false);
    expect(`${new Date(ms).toISOString().slice(0, 19)}Z`, `${label} decidedAt "${value}" must be a calendar-valid UTC timestamp`).toBe(value);
    return ms;
  };
  const tsActual = stamp(actual.body, "actual");
  const tsExpected = stamp(expected.body, "expected");
  expect(Math.abs(tsActual - tsExpected)).toBeLessThanOrEqual(2_000);
  const placeholder = '"decidedAt":"<utc-second>"';
  const normalise = (r) => ({ ...r, body: r.body.replace(pattern, placeholder) });
  expect(normalise(actual)).toEqual(normalise(expected));
}

const B = ["org_b", "prod"];
const NOWHERE = ["org_c", "prod"];
const keyOf = ([org, environment]) => `${org}/${environment}`;

/** A tenant's artifacts from the recorded three-systems set (everything outside the reserved base namespaces). */
const TENANT_ARTIFACTS = JSON.parse(readFileSync(join(import.meta.dir, "../../fixtures/demo/explorer/three-systems.definitions.json"), "utf8")).filter(
  (artifact) => !artifact.artifactId.startsWith("core.") && !artifact.artifactId.startsWith("std."),
);

/**
 * A deployment where B's owner has provisioned B's cell through the Worker (the cell's real
 * storage now holds B's definitions) and the vacant scope's object exists but nobody provisioned
 * it. Every storage touch so far is forgotten.
 */
async function provisioned() {
  const deployment = deploy();
  const owner = await token("org_b", "olga", "owner");
  // The host states its trusted base when a submission names another one.
  const probe = await router.fetch(request(B, owner, { method: "POST", path: "/v1/cell:provision", body: JSON.stringify({ baseSha256: "0".repeat(64), artifacts: [] }) }), deployment.env);
  const probed = JSON.parse(await probe.text());
  expect(probed.code).toBe("cell.base_mismatch");
  const baseSha256 = /sha256 ([0-9a-f]{64})/.exec(probed.explanation)[1];
  const done = await router.fetch(request(B, owner, { method: "POST", path: "/v1/cell:provision", body: JSON.stringify({ baseSha256, artifacts: TENANT_ARTIFACTS }) }), deployment.env);
  expect(done.status, await done.clone().text()).toBe(200);
  // The vacant scope's object is created, never provisioned.
  await router.fetch(request(NOWHERE, await token("org_c"), { path: "/v1/definitions" }), deployment.env);
  for (const o of deployment.objects.values()) o.touched.length = 0;
  return deployment;
}

describe("the real Wasm gate at the Worker boundary", () => {
  for (const [method, path, body] of [
    ["GET", "/v1/objects/canon.Item", undefined],
    ["GET", "/v1/access", undefined],
    ["POST", "/v1/runs", JSON.stringify({ flow: "ops.approve_refund" })],
    ["POST", "/v1/uploads:begin", JSON.stringify({ fileName: "rows.csv", contentType: "text/csv", size: 3 })],
    ["POST", "/v1/cell:provision", JSON.stringify({ artifacts: [] })],
  ]) {
    test(`${method} ${path}: an outsider at B's provisioned cell gets a vacant scope's exact refusal and headers, and no storage is touched`, async () => {
      const outsider = await token("org_a");
      const { env, objects } = await provisioned();
      expect([...objects.keys()].sort()).toEqual([keyOf(B), keyOf(NOWHERE)]);
      const atB = await raw(await router.fetch(request(B, outsider, { method, path, body }), env));
      const atNowhere = await raw(await router.fetch(request(NOWHERE, outsider, { method, path, body }), env));
      expectSameRefusal(atB, atNowhere);
      expect(atB.status).toBeGreaterThanOrEqual(400);
      expect(JSON.parse(atB.body).format).toBe("norn/refusal@2");
      for (const o of objects.values()) expect(o.touched).toEqual([]);
    });
  }

  test("a live upgrade: an outsider's socket at B's provisioned cell is accepted as at a vacant scope, with no storage touched", async () => {
    const outsider = await token("org_a");
    const { env, objects } = await provisioned();
    const upgrade = (scope) => request(scope, outsider, { path: "/v1/subscribe", headers: { upgrade: "websocket", "sec-websocket-protocol": "norn.live.v1" } });
    const atB = await raw(await router.fetch(upgrade(B), env));
    const atNowhere = await raw(await router.fetch(upgrade(NOWHERE), env));
    expect(atB).toEqual(atNowhere);
    expect(atB.status).toBe(101);
    for (const o of objects.values()) expect(o.touched).toEqual([]);
  });

  test("positive control: B's member reads B's provisioned definitions through the Worker, from B's storage", async () => {
    const { env, objects } = await provisioned();
    const read = await router.fetch(request(B, await token("org_b", "ana", "member"), { path: "/v1/definitions" }), env);
    const body = await read.text();
    expect(read.status, body).toBe(200);
    // The tenant's own artifacts are what B serves; the cell read them from B's storage.
    for (const artifact of TENANT_ARTIFACTS.slice(0, 3)) expect(body).toContain(artifact.artifactId);
    expect(objects.get(keyOf(B)).touched).toContain("sql");
    // The vacant scope's own member sees no such definitions there.
    const vacant = await router.fetch(request(NOWHERE, await token("org_c", "ana", "member"), { path: "/v1/definitions" }), env);
    expect(await vacant.text()).not.toContain(TENANT_ARTIFACTS[0].artifactId);
  });

  test("positive control: a forged member token at B is answered exactly as an outsider", async () => {
    const { env, objects } = await provisioned();
    const forged = (await token("org_b")).slice(0, -4) + "AAAA";
    const atB = await raw(await router.fetch(request(B, forged), env));
    const outsider = await raw(await router.fetch(request(B, await token("org_a")), env));
    expectSameRefusal(atB, outsider);
    for (const o of objects.values()) expect(o.touched).toEqual([]);
  });
});
