import { plugin } from "bun"
import { afterAll, beforeAll, expect, test } from "bun:test"
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"

// The cell Worker (apps/norn-cell-do/worker.mjs) refuses to open a cell when its Wasm is a
// fixtures build and NORN_STAGE is not a smoke or test stage. The Worker is loaded as shipped,
// beside a stand-in for its wasm-bindgen package that reports the build kind and records opens.

type Opened = { org: string; env: string }
type Stub = { fixtures: boolean; opened: Opened[] }
type CellClass = new (ctx: unknown, env: Record<string, string | undefined>) => { open(scope: [string, string]): Promise<void> }

const stub: Stub = { fixtures: true, opened: [] }
const dir = mkdtempSync(join(tmpdir(), "norn-cell-worker-"))
let Cell: CellClass

beforeAll(async () => {
  const source = resolve(import.meta.dir, "../../norn-cell-do")
  copyFileSync(join(source, "worker.mjs"), join(dir, "worker.mjs"))
  copyFileSync(join(source, "subscribe.mjs"), join(dir, "subscribe.mjs"))
  copyFileSync(join(source, "wake.mjs"), join(dir, "wake.mjs"))
  mkdirSync(join(dir, "pkg"))
  writeFileSync(join(dir, "pkg", "norn_cell_do_bg.wasm"), "")
  writeFileSync(
    join(dir, "pkg", "norn_cell_do.js"),
    `const stub = globalThis.__nornCellStub;
export function initSync() {}
export class NornCell {
  static fixturesEnabled() { return stub.fixtures; }
  constructor(_storage, org, env) { stub.opened.push({ org, env }); }
  configureConnect() {}
  enableAnalytics() {}
  // A vacant scope: no alarms to arm, no scope to keep.
  serving() { return false; }
}
`,
  )
  ;(globalThis as { __nornCellStub?: Stub }).__nornCellStub = stub
  // The Worker imports its Wasm as a module (a Workers bundler rule); the stand-in package ignores it.
  plugin({
    name: "norn-cell-wasm-stand-in",
    setup(build) {
      build.onLoad({ filter: /norn_cell_do_bg\.wasm$/ }, () => ({ contents: "export default null", loader: "js" }))
    },
  })
  Cell = ((await import(join(dir, "worker.mjs"))) as { Cell: CellClass }).Cell
})

afterAll(() => rmSync(dir, { recursive: true, force: true }))

const open = (fixtures: boolean, stage: string | undefined) => {
  stub.fixtures = fixtures
  stub.opened = []
  const env: Record<string, string | undefined> = stage === undefined ? {} : { NORN_STAGE: stage }
  return new Cell({ storage: {} }, env).open(["org_demo", "dev"])
}

test("a fixtures build opens no cell when NORN_STAGE is unset or a deployed stage", async () => {
  for (const stage of [undefined, "next", "demo", ""]) {
    await expect(open(true, stage)).rejects.toThrow("fixture principals")
    expect(stub.opened).toEqual([])
  }
})

test("a fixtures build opens on a smoke or test stage", async () => {
  for (const stage of ["smoke", "test"]) {
    await open(true, stage)
    expect(stub.opened).toEqual([{ org: "org_demo", env: "dev" }])
  }
})

test("a build without fixtures opens on any stage", async () => {
  for (const stage of [undefined, "next"]) {
    await open(false, stage)
    expect(stub.opened).toEqual([{ org: "org_demo", env: "dev" }])
  }
})
