import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"
import ts from "typescript"

/**
 * One workspace Retry. While a page shows its own Workspace unavailable state the shell's one-line
 * status steps aside (it watches for `data-workspace-unavailable`), so that state's Retry is the only
 * one and must do what the shell's would: decide the mode again, then read again what errored.
 *
 * - `data-workspace-unavailable` is drawn only by `WorkspaceUnavailable` (`components/load-state.tsx`),
 *   whose Retry is `retryWorkspace`; no page draws its own unavailable state with some other Retry.
 * - `WorkspaceUnavailable` takes no Retry from its page (no `onRetry`, no `isFetching`): a page's own
 *   read would leave the mode unavailable and the top status hidden.
 * - `retryMode` is named only in `lib/live-transport.ts`: every other user Retry of the mode goes
 *   through `retryWorkspace`, which reads errored reads again only once the mode has settled.
 */
const SOURCE = join(import.meta.dir, "..", "src")
const LOAD_STATE = join(SOURCE, "components", "load-state.tsx")
const TRANSPORT = join(SOURCE, "lib", "live-transport.ts")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SOURCE, dir)))

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (GENERATED.has(path)) return []
    if (statSync(path).isDirectory()) return sources(path)
    return /\.tsx?$/.test(name) && !name.endsWith(".d.ts") ? [path] : []
  })
}

interface Violation { readonly file: string; readonly line: number; readonly text: string }

function violations(file: string, text: string): Violation[] {
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  const found: Violation[] = []
  const report = (node: ts.Node, why: string) => found.push({
    file: relative(SOURCE, file),
    line: source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1,
    text: `${why}: ${node.getText(source).slice(0, 120)}`,
  })
  const visit = (node: ts.Node) => {
    if (ts.isIdentifier(node) && node.text === "retryMode" && file !== TRANSPORT) report(node, "retryMode outside lib/live-transport.ts (use retryWorkspace)")
    if (ts.isJsxAttribute(node) && node.name.getText(source) === "data-workspace-unavailable" && file !== LOAD_STATE) {
      report(node, "a page draws its own Workspace unavailable state (use WorkspaceUnavailable)")
    }
    if ((ts.isJsxSelfClosingElement(node) || ts.isJsxOpeningElement(node)) && node.tagName.getText(source) === "WorkspaceUnavailable") {
      for (const attribute of node.attributes.properties) {
        const name = ts.isJsxAttribute(attribute) ? attribute.name.getText(source) : "{...spread}"
        if (name !== "error" && name !== "compact") report(attribute, "WorkspaceUnavailable owns its Retry; it takes only error and compact")
      }
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
  return found
}

test("every page-level Workspace unavailable state's Retry is the workspace Retry", () => {
  const found = sources(SOURCE).flatMap((file) => violations(file, readFileSync(file, "utf8")))
  expect(found).toEqual([])
})

test("the rule catches a page's own unavailable state, a Retry passed in, and a bare retryMode", () => {
  const page = join(SOURCE, "page.tsx")
  const found = violations(page, [
    'import { retryMode } from "./lib/live-transport.ts"',
    "export const A = () => <div data-workspace-unavailable=\"\"><button onClick={() => void retryMode(q)}>Retry</button></div>",
    "export const B = () => <WorkspaceUnavailable error={e} onRetry={() => void read.refetch()} />",
    "export const C = () => <WorkspaceUnavailable error={e} compact />",
  ].join("\n"))
  expect(found.map(({ line, text }) => `${line} ${text.split(":")[0]}`)).toEqual([
    "1 retryMode outside lib/live-transport.ts (use retryWorkspace)",
    "2 a page draws its own Workspace unavailable state (use WorkspaceUnavailable)",
    "2 retryMode outside lib/live-transport.ts (use retryWorkspace)",
    "3 WorkspaceUnavailable owns its Retry; it takes only error and compact",
  ])
})
