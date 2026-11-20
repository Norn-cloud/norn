// TENANT-ISOLATION-1 (S8): the Worker boundary over the real Wasm gate. worker.test.mjs pins the
// Worker's own routing and storage discipline with a stand-in gate; here the shipped wasm-bindgen
// package (NORN_CELL_DO_PKG: a directory holding norn_cell_do.js and norn_cell_do_bg.wasm, built
// as apps/norn-cell-do/README.md says) answers. A member of org A, with a real host token, asks
// org B's cell and a scope nobody provisioned: the raw responses (status, every header, body)
// are byte for byte the same, and the object's storage is never touched.
// Run: NORN_CELL_DO_PKG=<pkg> bun test ./worker.wasm.test.mjs (from apps/norn-cell-do).
import { plugin } from "bun";
import { afterAll, describe, expect, test } from "bun:test";
import { copyFileSync, cpSync, existsSync, mkdtempSync, rmSync } from "node:fs";
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
for (const file of ["worker.mjs", "subscribe.mjs", "wake.mjs", "telemetry.mjs", "otlp.mjs"]) copyFileSync(join(import.meta.dir, file), join(dir, file));
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
const { NornGate } = await import(join(dir, "pkg", "norn_cell_do.js"));

/** A Durable Object whose storage records every touch (the gate must make none). */
function object() {
  const touched = [];
  const touch = (name) => (...args) => {
    touched.push(name);
    throw new Error(`storage ${name} touched for an outsider`);
  };
  const storage = new Proxy({}, { get: (_, name) => (name === "sql" ? new Proxy({}, { get: (_, op) => touch(`sql.${String(op)}`) }) : touch(String(name))) });
  const sockets = [];
  const ctx = {
    storage,
    waitUntil: touch("waitUntil"),
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

/** A deployment of two objects, B's and a vacant scope's, each a cold Cell per request. */
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

const B = ["org_b", "prod"];
const NOWHERE = ["org_c", "prod"];

describe("the real Wasm gate at the Worker boundary", () => {
  for (const [method, path, body] of [
    ["GET", "/v1/objects/canon.Item", undefined],
    ["GET", "/v1/access", undefined],
    ["POST", "/v1/runs", JSON.stringify({ flow: "ops.approve_refund" })],
    ["POST", "/v1/uploads:begin", JSON.stringify({ fileName: "rows.csv", contentType: "text/csv", size: 3 })],
    ["POST", "/v1/cell:provision", JSON.stringify({ artifacts: [] })],
  ]) {
    test(`${method} ${path}: an outsider at B gets a vacant scope's exact refusal and headers, and no storage is touched`, async () => {
      const outsider = await token("org_a");
      const { env, objects } = deploy();
      const atB = await raw(await router.fetch(request(B, outsider, { method, path, body }), env));
      const atNowhere = await raw(await router.fetch(request(NOWHERE, outsider, { method, path, body }), env));
      expect(atB).toEqual(atNowhere);
      expect(atB.status).toBeGreaterThanOrEqual(400);
      expect(JSON.parse(atB.body).format).toBe("norn/refusal@2");
      for (const o of objects.values()) expect(o.touched).toEqual([]);
    });
  }

  test("a live upgrade: an outsider's socket at B is accepted as at a vacant scope, with no storage touched", async () => {
    const outsider = await token("org_a");
    const { env, objects } = deploy();
    const upgrade = (scope) => request(scope, outsider, { path: "/v1/subscribe", headers: { upgrade: "websocket", "sec-websocket-protocol": "norn.live.v1" } });
    const atB = await raw(await router.fetch(upgrade(B), env));
    const atNowhere = await raw(await router.fetch(upgrade(NOWHERE), env));
    expect(atB).toEqual(atNowhere);
    expect(atB.status).toBe(101);
    for (const o of objects.values()) expect(o.touched).toEqual([]);
  });

  test("positive control: the same gate admits B's own member, and refuses a forged token as an outsider", async () => {
    const gate = new NornGate("org_b", "prod", ISSUER, JWKS, ENV.NORN_LINEAGE_KEY, "[]", "{}", Date.now());
    const query = "orgId=org_b&environmentId=prod";
    expect(gate.check("GET", "/v1/objects/canon.Item", query, await token("org_b"), Date.now(), undefined)).toBeUndefined();
    const outsider = gate.check("GET", "/v1/objects/canon.Item", query, await token("org_a"), Date.now(), undefined);
    expect(outsider?.status).toBeGreaterThanOrEqual(400);
    const forged = (await token("org_b")).slice(0, -4) + "AAAA";
    expect(gate.check("GET", "/v1/objects/canon.Item", query, forged, Date.now(), undefined)?.body).toBe(outsider.body);
  });
});
