import { plugin, spawnSync } from "bun"
import { afterAll, beforeAll, expect, spyOn, test } from "bun:test"
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { BUNDLE_FILES } from "../scripts/celld-deploy-cell.ts"

// The host conformance entry (apps/norn-cell-do/conformance/worker.mjs) serves nothing unless it is
// opted in: a fixtures build, NORN_STAGE=test and NORN_CONFORMANCE=synthetic. Opted in, it serves
// only the synthetic scope (org_demo/analytics) and its probe objects. The entry is loaded as
// shipped, beside the unchanged cell Worker and stand-ins for both wasm-bindgen packages.

type Stub = { fixtures: boolean }
type Env = Record<string, unknown>
type Handler = { fetch(request: Request, env?: Env): Promise<Response> }
type ObjectClass = new (ctx: unknown, env: Env) => Handler

const stub: Stub = { fixtures: true }
const dir = mkdtempSync(join(tmpdir(), "norn-cell-conformance-"))
let entry: Handler
let Cell: ObjectClass
let Probe: ObjectClass

beforeAll(async () => {
  const source = resolve(import.meta.dir, "../../norn-cell-do")
  // Every module the deployed bundle holds (BUNDLE_FILES), so the cell Worker loads as it ships.
  for (const file of BUNDLE_FILES.filter((name) => name.endsWith(".mjs"))) copyFileSync(join(source, file), join(dir, file))
  copyFileSync(join(source, "conformance", "worker.mjs"), join(dir, "conformance.mjs"))
  mkdirSync(join(dir, "pkg"))
  mkdirSync(join(dir, "probe-pkg"))
  writeFileSync(join(dir, "pkg", "norn_cell_do_bg.wasm"), "")
  writeFileSync(join(dir, "probe-pkg", "workers_sql_bg.wasm"), "")
  // A fixtures build has the ledger-head accessor and reports itself; a deployable build has neither.
  writeFileSync(
    join(dir, "pkg", "norn_cell_do.js"),
    `const stub = globalThis.__nornConformanceStub;
export function initSync() {}
export class NornCell {
  static fixturesEnabled() { return stub.fixtures; }
  static engineSteps() { return 64; }
  constructor() { throw new Error("no cell opens in this test"); }
}
// The tenant membership gate (TENANT-ISOLATION-1); the conformance scope is a demo scope, never gated.
export class NornGate {
  constructor() { throw new Error("no gate opens in this test"); }
}
Object.defineProperty(NornCell.prototype, "ledgerHead", { get() { return stub.fixtures ? function () { return "{}"; } : undefined; } });
`,
  )
  writeFileSync(join(dir, "probe-pkg", "workers_sql.js"), "export function initSync() {}\nexport function exercise() { return 'probe:pass'; }\n")
  ;(globalThis as { __nornConformanceStub?: Stub }).__nornConformanceStub = stub
  plugin({
    name: "norn-conformance-wasm-stand-in",
    setup(build) {
      build.onLoad({ filter: /(norn_cell_do|workers_sql)_bg\.wasm$/ }, () => ({ contents: "export default null", loader: "js" }))
    },
  })
  const module = (await import(join(dir, "conformance.mjs"))) as { default: Handler; Cell: ObjectClass; Probe: ObjectClass }
  entry = module.default
  Cell = module.Cell
  Probe = module.Probe
})

afterAll(() => rmSync(dir, { recursive: true, force: true }))

/** Bindings that record every object a request reached. */
function bindings() {
  const reached: string[] = []
  const namespace = (kind: string) => ({
    idFromName: (name: string) => `${kind}:${name}`,
    get: (id: string) => ({
      fetch: async () => {
        reached.push(id)
        return new Response("{}", { status: 200 })
      },
    }),
  })
  return { reached, CELLS: namespace("cell"), PROBES: namespace("probe") }
}

const OPTED_IN = { NORN_STAGE: "test", NORN_CONFORMANCE: "synthetic" }
const SCOPE = "orgId=org_demo&environmentId=analytics"
const ROUTES = [
  "/__probe/identity?object=probe",
  "/__probe/adapter",
  `/__conformance/head?${SCOPE}`,
  `/__conformance/stats?${SCOPE}`,
  `/v1/definitions?${SCOPE}&conformanceObject=kill-1`,
  `/v1/definitions?${SCOPE}`,
  `/v1/subscribe?${SCOPE}`,
  "/v1/definitions",
]

const ctx = () => ({ storage: { sql: { exec: () => ({ raw: () => [] }) } }, getWebSockets: () => [] })

test("without the opt-in, every route of the entry and its objects refuses", async () => {
  const refusing: Array<[boolean, Env]> = [
    [true, {}],
    [true, { NORN_STAGE: "test" }],
    [true, { NORN_CONFORMANCE: "synthetic" }],
    [true, { NORN_STAGE: "next", NORN_CONFORMANCE: "synthetic" }],
    [true, { NORN_STAGE: "smoke", NORN_CONFORMANCE: "synthetic" }],
    [true, { NORN_STAGE: "test", NORN_CONFORMANCE: "1" }],
    [false, OPTED_IN],
  ]
  for (const [fixtures, vars] of refusing) {
    stub.fixtures = fixtures
    for (const route of ROUTES) {
      const env = { ...bindings(), ...vars }
      const response = await entry.fetch(new Request(`http://cell${route}`), env)
      expect(response.status).toBe(403)
      expect(((await response.json()) as { code: string }).code).toBe("conformance.disabled")
      expect(env.reached).toEqual([])
      const headers = { "x-norn-cell": JSON.stringify(["org_demo", "analytics"]) }
      for (const Object of [Cell, Probe]) {
        const direct = await new Object(ctx(), vars).fetch(new Request(`http://cell${route}`, { headers }))
        expect(direct.status).toBe(403)
      }
    }
  }
})

test("opted in, only the synthetic scope is served", async () => {
  stub.fixtures = true
  for (const route of ["/v1/definitions?orgId=org_acme&environmentId=prod", "/__conformance/head?orgId=org_demo&environmentId=dev", "/v1/definitions", `/v1/definitions?${SCOPE}&orgId=org_acme`]) {
    const env = { ...bindings(), ...OPTED_IN }
    const response = await entry.fetch(new Request(`http://cell${route}`), env)
    expect(response.status).toBe(403)
    expect(((await response.json()) as { code: string }).code).toBe("conformance.scope")
    expect(env.reached).toEqual([])
  }
  for (const scope of [["org_demo", "dev"], ["org_acme", "analytics"], null]) {
    const headers = scope === null ? undefined : { "x-norn-cell": JSON.stringify(scope) }
    const direct = await new Cell(ctx(), OPTED_IN).fetch(new Request(`http://cell/__conformance/head?${SCOPE}`, { headers }))
    expect(direct.status).toBe(403)
  }
})

test("opted in, the synthetic scope and the probes reach their objects", async () => {
  stub.fixtures = true
  const env = { ...bindings(), ...OPTED_IN }
  for (const route of ["/__probe/identity?object=p1", `/v1/definitions?${SCOPE}&conformanceObject=kill-1`, `/v1/definitions?${SCOPE}`]) {
    expect((await entry.fetch(new Request(`http://cell${route}`), env)).status).toBe(200)
  }
  expect(env.reached).toEqual(["probe:p1", "cell:conformance/kill-1", "cell:org_demo/analytics"])
})

test("the checked-in conformance config enables nothing: its vars fail the entry guard and it has no public URL", async () => {
  const config = JSON.parse(readFileSync(resolve(import.meta.dir, "../../norn-cell-do/conformance/wrangler.json"), "utf8")) as Record<string, unknown>
  expect(config.workers_dev).toBe(false)
  expect(config.preview_urls).toBe(false)
  expect(config.routes).toEqual([])
  const vars = (config.vars ?? {}) as Env
  expect(vars.NORN_STAGE).toBeUndefined()
  expect(vars.NORN_CONFORMANCE).toBeUndefined()
  stub.fixtures = true
  for (const route of ROUTES) {
    const env = { ...bindings(), ...vars }
    const response = await entry.fetch(new Request(`http://cell${route}`), env)
    expect(response.status).toBe(403)
    expect(((await response.json()) as { code: string }).code).toBe("conformance.disabled")
    expect(env.reached).toEqual([])
  }
})

// The runner's kill marker is printed only from inside the request's write transaction, after a
// statement ran: never for a refusal that wrote nothing, never when the transaction's callback throws.
type Storage = { sql: { exec(...args: unknown[]): unknown }; transactionSync<T>(callback: () => T): T }
type Behaviour = "refuse-before-sql" | "refuse-in-transaction" | "commit"
test("a hold marks only inside a write transaction that ran a statement", async () => {
  stub.fixtures = true
  const production = Object.getPrototypeOf(Cell) as { prototype: { fetch(this: { ctx: { storage: Storage } }, request: Request): Promise<Response> } }
  const original = production.prototype.fetch
  production.prototype.fetch = async function (request) {
    const behaviour = request.headers.get("x-behaviour") as Behaviour
    const storage = this.ctx.storage
    if (behaviour === "refuse-before-sql") return Response.json({ code: "refused" }, { status: 422 })
    try {
      storage.transactionSync(() => {
        storage.sql.exec("INSERT INTO t VALUES (1)")
        if (behaviour === "refuse-in-transaction") throw "norn:rollback"
        storage.sql.exec("INSERT INTO t VALUES (2)")
      })
    } catch {
      return Response.json({ code: "refused" }, { status: 422 })
    }
    return Response.json({}, { status: 200 })
  }
  const errors = spyOn(console, "error").mockImplementation(() => {})
  try {
    const storage: Storage = { sql: { exec: () => ({}) }, transactionSync: (callback) => callback() }
    const cell = new Cell({ storage, getWebSockets: () => [] }, OPTED_IN)
    const cases: Array<[Behaviour, string, boolean]> = [
      ["refuse-before-sql", "stmt:1:1", false],
      ["refuse-before-sql", "turn-end:1", false],
      ["refuse-in-transaction", "turn-end:1", false],
      ["commit", "stmt:1:1", true],
      ["commit", "turn-end:1", true],
    ]
    for (const [behaviour, hold, marks] of cases) {
      errors.mockClear()
      const marker = crypto.randomUUID().replaceAll("-", "")
      const headers = { "x-norn-cell": JSON.stringify(["org_demo", "analytics"]), "x-behaviour": behaviour, "x-norn-conformance-hold": hold, "x-norn-conformance-marker": marker }
      const response = await cell.fetch(new Request(`http://cell/v1/definitions?${SCOPE}`, { method: "POST", headers }))
      const printed = errors.mock.calls.some((call) => String(call[0]).includes(`norn-conformance-hold-reached ${marker}`))
      expect([behaviour, hold, printed]).toEqual([behaviour, hold, marks])
      expect(response.headers.get("x-norn-conformance-hold-reached")).toBe(String(marks))
    }
  } finally {
    errors.mockRestore()
    production.prototype.fetch = original
  }
})

// The runner, against a stubbed host whose held requests are refused (422) and never print a
// marker: it must not kill the host, and the kill sweep is "not tested", not a pass.
test("a refused held request prints no marker: the kill sweep skips the kill and reports not tested", () => {
  const runner = resolve(import.meta.dir, "../../norn-cell-do/conformance/conformance.py")
  const script = String.raw`
import importlib.util, json, pathlib, sys, tempfile
spec = importlib.util.spec_from_file_location("conformance", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
log = pathlib.Path(tempfile.mkdtemp()) / "host.log"
log.write_text("")

class Host:
    name, controls, runners, kills, starts = "stub", True, 1, 0, 0
    def urls(self): return ["http://stub"]
    def logs(self): return [log]
    def kill(self): self.kills += 1
    def start(self):
        self.starts += 1
        return 0
    stop = kill

step = {"authorization": "a", "method": "POST", "path": "/v1/definitions", "query": m.ANALYTICS, "body": "{}"}
native = {"openedJson": "h0", "steps": [{"status": 200, "body": "{}", "headJson": f"h{i + 1}"} for i in range(10)]}
heads = {}

def head(base, query=m.ANALYTICS, timeout=120, obj=None):
    return heads.setdefault(obj, "h0"), {}, 0

def scripted(base, step, now, hold=None, timeout=120, obj=None, marker=None):
    if hold is not None:
        return 422, b'{"code":"refused"}', 0, {"x-norn-conformance-hold-reached": "false", "x-norn-conformance-statements": "0"}
    heads[obj] = f"h{int(heads.setdefault(obj, 'h0')[1:]) + 1}"
    return 200, b"{}", 0, {"x-norn-conformance-statements": "4"}

m.head, m.scripted = head, scripted
host = Host()
status, detail = m.Suite(host, {"nowMs": 0, "kill": {"script": [step] * 10, "native": native}}, quick=True).kill_points()
print(json.dumps({"status": status, "kills": host.kills, "starts": host.starts, "detail": detail}))
`
  const run = spawnSync(["python3", "-I", "-c", script, runner])
  expect(run.exitCode).toBe(0)
  const result = JSON.parse(run.stdout.toString()) as { status: string; kills: number; starts: number; detail: string }
  expect(result).toMatchObject({ status: "not tested", kills: 0, starts: 0 })
  expect(result.detail).toContain("stmt:1=not reached, not killed (reply 422)")
})
