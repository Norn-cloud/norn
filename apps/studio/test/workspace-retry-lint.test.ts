import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"
import ts from "typescript"

/**
 * One workspace Retry. While a page shows its own Workspace unavailable state the shell's one-line
 * status steps aside (it watches for `data-workspace-unavailable`), so that state's Retry is the only
 * one and must do what the shell's would: decide the mode again, then read again what errored.
 *
 * - The `data-workspace-unavailable` marker is set only by `WorkspaceUnavailable` (`components/load-state.tsx`),
 *   whose Retry is `retryWorkspace`; no page draws its own unavailable state with some other Retry. No other
 *   file names the marker (as a JSX attribute, in a string or template, or as `dataset.workspaceUnavailable`)
 *   except in an attribute-presence selector, `[data-workspace-unavailable]`, which can only find it.
 * - `WorkspaceUnavailable` takes no Retry from its page: it is used only as a JSX element (by its own name,
 *   an alias or a namespace), with only `error` and `compact`, never passed as a value (`createElement`,
 *   a wrapper) where its props cannot be read. Its own props are `error` and `compact`, and its Retry
 *   calls `retryWorkspace`.
 * - `retryMode` is named only in `lib/live-transport.ts` (as a name or a string key): every other user
 *   Retry of the mode goes through `retryWorkspace`, which reads errored reads again only once the mode has settled.
 */
const SOURCE = join(import.meta.dir, "..", "src")
const LOAD_STATE = join(SOURCE, "components", "load-state.tsx")
const TRANSPORT = join(SOURCE, "lib", "live-transport.ts")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SOURCE, dir)))
const MARKER = "data-workspace-unavailable"
const COMPONENT = "WorkspaceUnavailable"
const PROPS = new Set(["error", "compact"])

function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (GENERATED.has(path)) return []
    if (statSync(path).isDirectory()) return sources(path)
    return /\.tsx?$/.test(name) && !name.endsWith(".d.ts") ? [path] : []
  })
}

interface Violation { readonly file: string; readonly line: number; readonly text: string }

function parse(file: string, text: string): ts.SourceFile {
  return ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
}

function reporter(file: string, source: ts.SourceFile, found: Violation[]) {
  return (node: ts.Node, why: string) => found.push({
    file: relative(SOURCE, file),
    line: source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1,
    text: `${why}: ${node.getText(source).slice(0, 120)}`,
  })
}

/** The literal text of a string or template (its static parts), or `undefined` for any other node. */
function literalText(node: ts.Node): string | undefined {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return node.text
  if (ts.isTemplateExpression(node)) return [node.head.text, ...node.templateSpans.map((span) => span.literal.text)].join("")
  return undefined
}

/** Whether `text` names the marker other than in an attribute-presence selector (which can only find it). */
function namesMarker(text: string): boolean {
  return text.replaceAll(`[${MARKER}]`, "").includes(MARKER)
}

function violations(file: string, text: string): Violation[] {
  const source = parse(file, text)
  const found: Violation[] = []
  const report = reporter(file, source, found)
  // The local names `WorkspaceUnavailable` goes by in this file: its own, an import alias, a const alias.
  const names = new Set([COMPONENT])
  const isComponent = (node: ts.Node): boolean =>
    (ts.isIdentifier(node) && names.has(node.text))
    || (ts.isPropertyAccessExpression(node) && node.name.text === COMPONENT)
    || (ts.isJsxNamespacedName(node) && node.name.text === COMPONENT)
  const collect = (node: ts.Node) => {
    if (ts.isImportSpecifier(node) && (node.propertyName ?? node.name).getText(source) === COMPONENT) names.add(node.name.text)
    if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer !== undefined && isComponent(node.initializer)) names.add(node.name.text)
    ts.forEachChild(node, collect)
  }
  collect(source)
  const visit = (node: ts.Node) => {
    const parent = node.parent
    if (file !== TRANSPORT) {
      if (ts.isIdentifier(node) && node.text === "retryMode") report(node, "retryMode outside lib/live-transport.ts (use retryWorkspace)")
      else if (literalText(node)?.includes("retryMode") === true) report(node, "retryMode outside lib/live-transport.ts (use retryWorkspace)")
    }
    if (file !== LOAD_STATE) {
      if (ts.isJsxAttribute(node) && node.name.getText(source) === MARKER) {
        report(node, "a page draws its own Workspace unavailable state (use WorkspaceUnavailable)")
      } else if (!ts.isJsxAttribute(parent) && namesMarker(literalText(node) ?? "")) {
        report(node, "a page sets the Workspace unavailable marker (use WorkspaceUnavailable; only [data-workspace-unavailable] selectors may name it)")
      }
      const datasetKey =
        (ts.isIdentifier(node) && node.text === "workspaceUnavailable" && (ts.isPropertyAccessExpression(parent) || ts.isPropertyAssignment(parent) || ts.isShorthandPropertyAssignment(parent)))
        || (literalText(node)?.includes("workspaceUnavailable") === true)
      if (datasetKey) report(node, "a page sets the Workspace unavailable marker through dataset (use WorkspaceUnavailable)")
      if (isComponent(node) && !ts.isPropertyAccessExpression(parent)) {
        const tag = (ts.isJsxSelfClosingElement(parent) || ts.isJsxOpeningElement(parent)) && parent.tagName === node
        // An import or a `const` alias: the alias's own JSX uses are read like the component's.
        const declared = ts.isImportSpecifier(parent) || ts.isVariableDeclaration(parent)
        if (tag) {
          for (const attribute of parent.attributes.properties) {
            const name = ts.isJsxAttribute(attribute) ? attribute.name.getText(source) : "{...spread}"
            if (!PROPS.has(name)) report(attribute, "WorkspaceUnavailable owns its Retry; it takes only error and compact")
          }
        } else if (!declared && !ts.isJsxClosingElement(parent)) {
          report(node, "WorkspaceUnavailable is used only as a JSX element, whose props this rule reads")
        }
      }
    }
    ts.forEachChild(node, visit)
  }
  ts.forEachChild(source, visit)
  return found
}

/** `WorkspaceUnavailable` itself: its props are `error` and `compact`, and its Retry calls `retryWorkspace`. */
function ownerViolations(file: string, text: string): Violation[] {
  const source = parse(file, text)
  const found: Violation[] = []
  const report = reporter(file, source, found)
  const owner = source.statements.find((statement): statement is ts.FunctionDeclaration => ts.isFunctionDeclaration(statement) && statement.name?.text === COMPONENT)
  if (owner === undefined) {
    report(source, "WorkspaceUnavailable is declared as a function in components/load-state.tsx")
    return found
  }
  const [props, ...rest] = owner.parameters
  for (const extra of rest) report(extra, "WorkspaceUnavailable takes one props parameter")
  if (props === undefined || !ts.isObjectBindingPattern(props.name) || props.type === undefined || !ts.isTypeLiteralNode(props.type)) {
    report(props ?? owner, "WorkspaceUnavailable's props are a destructured type literal of error and compact")
  } else {
    for (const element of props.name.elements) {
      const name = (element.propertyName ?? element.name).getText(source)
      if (element.dotDotDotToken !== undefined || !PROPS.has(name)) report(element, "WorkspaceUnavailable takes only error and compact")
    }
    for (const member of props.type.members) {
      if (member.name === undefined || !PROPS.has(member.name.getText(source))) report(member, "WorkspaceUnavailable takes only error and compact")
    }
  }
  let callsRetryWorkspace = false
  const visit = (node: ts.Node) => {
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === "retryWorkspace") callsRetryWorkspace = true
    ts.forEachChild(node, visit)
  }
  if (owner.body !== undefined) visit(owner.body)
  if (!callsRetryWorkspace) report(owner.name ?? owner, "WorkspaceUnavailable's Retry calls retryWorkspace")
  return found
}

const lines = (found: readonly Violation[]) => found.map(({ line, text }) => `${line} ${text.split(":")[0]}`)

test("every page-level Workspace unavailable state's Retry is the workspace Retry", () => {
  const files = sources(SOURCE)
  // The walk reads Studio's sources, not an empty or moved directory.
  expect(files.length).toBeGreaterThan(200)
  expect(files).toContain(LOAD_STATE)
  expect(files).toContain(TRANSPORT)
  const found = [...files.flatMap((file) => violations(file, readFileSync(file, "utf8"))), ...ownerViolations(LOAD_STATE, readFileSync(LOAD_STATE, "utf8"))]
  expect(found).toEqual([])
})

test("the rule catches a page's own unavailable state, a Retry passed in, and a bare retryMode, however written", () => {
  const page = join(SOURCE, "page.tsx")
  const found = violations(page, [
    'import { retryMode } from "./lib/live-transport.ts"',
    "export const A = () => <div data-workspace-unavailable=\"\"><button onClick={() => void retryMode(q)}>Retry</button></div>",
    "export const B = () => <WorkspaceUnavailable error={e} onRetry={() => void read.refetch()} />",
    "export const C = () => <WorkspaceUnavailable error={e} compact />",
    'export const D = () => <div {...{ "data-workspace-unavailable": "" }} />',
    "export const E = (el: HTMLElement) => el.setAttribute(`data-workspace-${\"\"}unavailable`, \"\")",
    'export const F = (el: HTMLElement) => { el.dataset.workspaceUnavailable = ""; el.dataset["workspaceUnavailable"] = "" }',
    'import { WorkspaceUnavailable as Unavailable } from "@/components/load-state"',
    "export const G = () => <Unavailable error={e} onRetry={retry} />",
    'import * as LoadStates from "@/components/load-state"',
    "export const H = () => <LoadStates.WorkspaceUnavailable error={e} onRetry={retry} />",
    "export const I = () => createElement(WorkspaceUnavailable, { error: e, onRetry: retry })",
    'export const J = (t: Record<string, () => Promise<void>>) => t["retryMode"]()',
    "const Aliased = WorkspaceUnavailable",
    "export const K = () => <Aliased error={e} {...rest} />",
    "export const L = (el: HTMLElement) => el.querySelector(\"[data-workspace-unavailable]\")",
    "export const M = (el: HTMLElement) => el.querySelector(\"[data-workspace-unavailable] button\")",
    "export const N = (el: HTMLElement) => el.querySelector(\"[data-workspace-unavailable=x]\")",
  ].join("\n"))
  expect(lines(found)).toEqual([
    "1 retryMode outside lib/live-transport.ts (use retryWorkspace)",
    "2 a page draws its own Workspace unavailable state (use WorkspaceUnavailable)",
    "2 retryMode outside lib/live-transport.ts (use retryWorkspace)",
    "3 WorkspaceUnavailable owns its Retry; it takes only error and compact",
    "5 a page sets the Workspace unavailable marker (use WorkspaceUnavailable; only [data-workspace-unavailable] selectors may name it)",
    "6 a page sets the Workspace unavailable marker (use WorkspaceUnavailable; only [data-workspace-unavailable] selectors may name it)",
    "7 a page sets the Workspace unavailable marker through dataset (use WorkspaceUnavailable)",
    "7 a page sets the Workspace unavailable marker through dataset (use WorkspaceUnavailable)",
    "9 WorkspaceUnavailable owns its Retry; it takes only error and compact",
    "11 WorkspaceUnavailable owns its Retry; it takes only error and compact",
    "12 WorkspaceUnavailable is used only as a JSX element, whose props this rule reads",
    "13 retryMode outside lib/live-transport.ts (use retryWorkspace)",
    "15 WorkspaceUnavailable owns its Retry; it takes only error and compact",
    "18 a page sets the Workspace unavailable marker (use WorkspaceUnavailable; only [data-workspace-unavailable] selectors may name it)",
  ])
})

test("the rule catches a WorkspaceUnavailable that takes a Retry or does not call retryWorkspace", () => {
  const found = ownerViolations(LOAD_STATE, [
    "export function WorkspaceUnavailable({ error, onRetry, ...rest }: { readonly error: unknown; readonly onRetry: () => void }) {",
    "  return <button onClick={onRetry}>Retry</button>",
    "}",
  ].join("\n"))
  expect(lines(found)).toEqual([
    "1 WorkspaceUnavailable takes only error and compact",
    "1 WorkspaceUnavailable takes only error and compact",
    "1 WorkspaceUnavailable takes only error and compact",
    "1 WorkspaceUnavailable's Retry calls retryWorkspace",
  ])
  expect(ownerViolations(LOAD_STATE, "export const WorkspaceUnavailable = () => null").map(({ text }) => text.split(":")[0])).toEqual([
    "WorkspaceUnavailable is declared as a function in components/load-state.tsx",
  ])
})
