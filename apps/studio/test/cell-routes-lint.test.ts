import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"
import ts from "typescript"

/**
 * One door to the cell (#1374): Studio reaches cell routes (`/v1/…`, through `/api/host`) only
 * through the typed client and the read primitive (`lib/cell-query.ts`, over `@norn/sdk`), so a
 * server field change is a compile error rather than a silent miss. Anywhere else, a string that
 * names a cell route is a hand-rolled call. The door's own calls name routes by design and are
 * not counted: a door call's route argument (`useCellQuery(route, …)`), and a route prefix's
 * `route` where the door takes its prefixes (`invalidateCellRoutes(client, scope, [{ route }])`,
 * or the list a `useCellMutation` `invalidates` function returns). Any other `{ route: … }` is a
 * hand-rolled call like any other string.
 *
 * Phase 1a lands the rule as a warning: it reports every hand-rolled call. Phase 1b (after #1643)
 * migrates them and makes it an error (`NORN_CELL_ROUTES_LINT=error` does so now). There is no
 * allowlist: the only exempt modules are the door's own.
 */
const ROOT = join(import.meta.dir, "..")
const SOURCE = join(ROOT, "src")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SOURCE, dir)))
/** The door: the modules that turn a route into a call. */
const DOOR = new Set(["src/lib/cell-query.ts"])
/** Calls whose first argument is a route by design. */
const DOOR_CALLS = new Set(["useCellQuery", "useCellMutation", "cellQueryOptions", "cellQueryKey", "cellPath"])
const CELL_ROUTE = /^\/(v1\/|api\/host(\/|$|\?))/

interface Violation { readonly file: string; readonly line: number; readonly text: string }

const calleeName = (call: ts.CallExpression): string | undefined =>
  ts.isIdentifier(call.expression) ? call.expression.text : ts.isPropertyAccessExpression(call.expression) ? call.expression.name.text : undefined

/** `node` without the parentheses around it. */
function unwrapped(node: ts.Node): ts.Node {
  let at = node
  while (ts.isParenthesizedExpression(at.parent)) at = at.parent
  return at
}

/** Whether `call` is a call of `name` with `node` as its argument at `index`. */
const isArgument = (call: ts.Node, name: string, index: number, node: ts.Node): call is ts.CallExpression =>
  ts.isCallExpression(call) && calleeName(call) === name && call.arguments[index] === node

/**
 * Whether `list` (an array literal) is where the door takes route prefixes: the third argument of
 * `invalidateCellRoutes`, or the list an `invalidates` arrow function of a `useCellMutation` call
 * returns (as its expression body, or from a `return` in its block body).
 */
function isPrefixList(list: ts.ArrayLiteralExpression): boolean {
  const at = unwrapped(list)
  if (isArgument(at.parent, "invalidateCellRoutes", 2, at)) return true
  let fn: ts.Node | undefined
  if (ts.isArrowFunction(at.parent) && at.parent.body === at) fn = at.parent
  else if (ts.isReturnStatement(at.parent)) fn = ts.findAncestor(at.parent, ts.isFunctionLike)
  if (fn === undefined || !ts.isArrowFunction(fn)) return false
  const property = unwrapped(fn).parent
  if (!ts.isPropertyAssignment(property) || !ts.isIdentifier(property.name) || property.name.text !== "invalidates") return false
  const config = property.parent
  return ts.isObjectLiteralExpression(config) && isArgument(config.parent, "useCellMutation", 1, config)
}

/** Whether `node` is where the door names a route: a door call's route argument, or a route prefix's `route` in a prefix list. */
function throughDoor(node: ts.Node): boolean {
  let at: ts.Node = node
  if (ts.isTemplateHead(at)) at = at.parent
  const parent = at.parent
  if (ts.isCallExpression(parent) && parent.arguments[0] === at) {
    const name = calleeName(parent)
    return name !== undefined && DOOR_CALLS.has(name)
  }
  if (!ts.isPropertyAssignment(parent) || parent.initializer !== at || !ts.isIdentifier(parent.name) || parent.name.text !== "route") return false
  const prefix = parent.parent
  return ts.isObjectLiteralExpression(prefix) && ts.isArrayLiteralExpression(prefix.parent) && isPrefixList(prefix.parent)
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
    'useCellMutation("/v1/actions/{actionId}", { scope, invalidates: () => [{ route: "/v1/inbox" }] })',
    'useCellMutation("/v1/actions/{actionId}", { scope, invalidates: (result) => { return [{ route: "/v1/decisions" }] } })',
    'const label = "/v1 is the API version"',
    'await fetch("/api/me")',
    // A `route` property anywhere else is a hand-rolled call.
    'await request({ route: "/v1/inbox" })',
    'const config = { route: "/api/host/v1/decisions" }',
    'const prefixes = [{ route: "/v1/runs" }]',
    'invalidateCellRoutes(client, [{ route: "/v1/flows" }], scope)',
    'useCellQuery("/v1/runs/{id}", { route: "/v1/inbox" }, { scope })',
  ].join("\n")
  expect(handRolledRoutes("sample.ts", sample).map(({ line, text }) => [line, text])).toEqual([
    [1, "/api/host/v1/objects/x?orgId=o"],
    [2, "/api/host"],
    [3, "/v1/runs/"],
    [4, "/v1/decisions?state=pending"],
    [12, "/v1/inbox"],
    [13, "/api/host/v1/decisions"],
    [14, "/v1/runs"],
    [15, "/v1/flows"],
    [16, "/v1/inbox"],
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
