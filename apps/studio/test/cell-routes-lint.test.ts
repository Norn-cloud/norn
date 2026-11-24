import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"
import ts from "typescript"

/**
 * One door to the cell (#1374): Studio reaches cell routes (`/v1/…`, through `/api/host`) only
 * through the typed client and the read primitive (`lib/cell-query.ts`, over `@norn/sdk`), so a
 * server field change is a compile error rather than a silent miss. Anywhere else, a string that
 * names a cell route is a hand-rolled call. The door's own calls (`useCellQuery(route, …)`, a
 * `CellRoutePrefix`'s `route`) name routes by design and are not counted.
 *
 * Phase 1a lands the rule as a warning: it reports every hand-rolled call. Phase 1b (after #1643)
 * migrates them and makes it an error (`NORN_CELL_ROUTES_LINT=error` does so now). There is no
 * allowlist: the only exempt modules are the door's own.
 */
const ROOT = join(import.meta.dir, "..")
const SOURCE = join(ROOT, "src")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SOURCE, dir)))
/** The door: the modules that turn a route into a call. */
const DOOR = new Set(["src/lib/cell-query.ts", "src/lib/cell-cache.ts", "src/lib/cell-cache-store.ts"])
/** Calls whose first argument is a route by design. */
const DOOR_CALLS = new Set(["useCellQuery", "useCellMutation", "cellQueryOptions", "cellQueryKey", "cellPath"])
const CELL_ROUTE = /^\/(v1\/|api\/host(\/|$|\?))/

interface Violation { readonly file: string; readonly line: number; readonly text: string }

const calleeName = (call: ts.CallExpression): string | undefined =>
  ts.isIdentifier(call.expression) ? call.expression.text : ts.isPropertyAccessExpression(call.expression) ? call.expression.name.text : undefined

/** Whether `node` is where the door names a route: its call's route argument, or a route prefix's `route`. */
function throughDoor(node: ts.Node): boolean {
  let at: ts.Node = node
  if (ts.isTemplateHead(at)) at = at.parent
  const parent = at.parent
  if (ts.isCallExpression(parent) && parent.arguments[0] === at) {
    const name = calleeName(parent)
    return name !== undefined && DOOR_CALLS.has(name)
  }
  return ts.isPropertyAssignment(parent) && parent.initializer === at && ts.isIdentifier(parent.name) && parent.name.text === "route"
}

/** Every string in `source` that names a cell route outside the door. */
function handRolledRoutes(file: string, source: string): readonly Violation[] {
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  const found: Violation[] = []
  const visit = (node: ts.Node) => {
    const text = ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node) || ts.isTemplateHead(node) ? node.text : undefined
    if (text !== undefined && CELL_ROUTE.test(text) && !throughDoor(node)) {
      found.push({ file, line: parsed.getLineAndCharacterOfPosition(node.getStart(parsed)).line + 1, text })
    }
    ts.forEachChild(node, visit)
  }
  visit(parsed)
  return found
}

function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry)
    if (GENERATED.has(full)) return []
    if (statSync(full).isDirectory()) return walk(full)
    return /\.tsx?$/.test(entry) ? [full] : []
  })
}

test("the rule finds a hand-rolled cell call and lets the door's own calls through", () => {
  const sample = [
    'await fetch("/api/host/v1/objects/x?orgId=o")',
    "const call = scopedHostFetch(fetcher, \"/api/host\")",
    "await call(`/v1/runs/${id}`)",
    'useLiveRefresh("/v1/decisions?state=pending", key)',
    'useCellQuery("/v1/runs/{id}", { path: { id } }, { scope })',
    'useCellMutation("/v1/actions/{actionId}", { scope })',
    'invalidateCellRoutes(client, scope, [{ route: "/v1/inbox" }])',
    'const label = "/v1 is the API version"',
    'await fetch("/api/me")',
  ].join("\n")
  expect(handRolledRoutes("sample.ts", sample).map(({ line, text }) => [line, text])).toEqual([
    [1, "/api/host/v1/objects/x?orgId=o"],
    [2, "/api/host"],
    [3, "/v1/runs/"],
    [4, "/v1/decisions?state=pending"],
  ])
})

test("Studio reaches cell routes only through the typed client (warning until phase 1b)", () => {
  const files = walk(SOURCE)
  expect(files.length).toBeGreaterThan(100)
  const violations = files
    .map((file) => ({ file, at: relative(ROOT, file) }))
    .filter(({ at }) => !DOOR.has(at))
    .flatMap(({ file, at }) => handRolledRoutes(at, readFileSync(file, "utf8")))
  const report = violations.map(({ file, line, text }) => `  ${file}:${line} ${text}`).join("\n")
  if (process.env["NORN_CELL_ROUTES_LINT"] === "error") {
    expect(violations, `cell routes named outside @norn/sdk and lib/cell-query.ts:\n${report}`).toEqual([])
  } else if (violations.length > 0) {
    console.warn(`cell-routes lint (warning): ${violations.length} hand-rolled cell routes in ${new Set(violations.map(({ file }) => file)).size} files; phase 1b migrates them to useCellQuery/useCellMutation.\n${report}`)
  }
})
