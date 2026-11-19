import { plugin } from "bun"
import { afterAll, beforeAll, expect, test } from "bun:test"
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"

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
  copyFileSync(join(source, "worker.mjs"), join(dir, "worker.mjs"))
  copyFileSync(join(source, "subscribe.mjs"), join(dir, "subscribe.mjs"))
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
  constructor() { throw new Error("no cell opens in this test"); }
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
