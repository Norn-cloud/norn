import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { dirname, join, relative, resolve } from "node:path"
import ts from "typescript"

/**
 * One workspace decision (#1762): a page reads records only through the reader of the workspace the
 * server's mode answer decided (`src/lib/workspace.tsx`), never at the session's scope or at none.
 *
 * - The raw reads (`src/lib/raw/`: records, one record, its findings, the recording) are private to
 *   `src/lib/`: no module outside it imports them, statically, dynamically or by re-export. A type-only
 *   import is no read and is let through.
 * - A `DecidedScope` is made in one place, `src/lib/workspace.tsx`: no other module asserts one
 *   (`as DecidedScope`, `<DecidedScope>`), so every one is the mode's answer.
 * - The closed state unions (`src/lib/state/`) import no app code and no package: they stay ready to
 *   move into their own package as they are.
 *
 * Both rules are errors. There is no allowlist: the boundary is the `src/lib/` directory itself.
 */
const ROOT = join(import.meta.dir, "..")
const SOURCE = join(ROOT, "src")
const LIB = join(SOURCE, "lib")
const RAW = join(LIB, "raw")
const WORKSPACE = join(LIB, "workspace.tsx")
const STATE = join(LIB, "state")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SOURCE, dir)))

interface Violation { readonly file: string; readonly line: number; readonly text: string }

/** Where an import specifier points, as a path under `src/` (`@/` is `src/`); undefined for a package. */
function target(from: string, specifier: string): string | undefined {
  if (specifier.startsWith("@/")) return join(SOURCE, specifier.slice(2))
  if (specifier.startsWith(".")) return resolve(dirname(from), specifier)
  return undefined
}

const insideRaw = (path: string | undefined): boolean => path !== undefined && (path === RAW || path.startsWith(`${RAW}/`))
const insideState = (path: string | undefined): boolean => path !== undefined && (path === STATE || path.startsWith(`${STATE}/`))

/**
 * Every way `source` reaches `src/lib/raw/` (outside `src/lib/`), asserts a `DecidedScope` (outside
 * `workspace.tsx`), or, in `src/lib/state/`, imports anything from outside it.
 */
export function violations(file: string, source: string): readonly Violation[] {
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  const found: Violation[] = []
  const at = (node: ts.Node, text: string) => found.push({ file, line: parsed.getLineAndCharacterOfPosition(node.getStart(parsed)).line + 1, text })
  const outsideLib = !(file === LIB || file.startsWith(`${LIB}/`))
  const stateModule = insideState(file)
  const visit = (node: ts.Node) => {
    if (stateModule && (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier !== undefined && ts.isStringLiteral(node.moduleSpecifier)
      && !insideState(target(file, node.moduleSpecifier.text))) {
      at(node, `state imports ${node.moduleSpecifier.text}`)
    }
    if (outsideLib) {
      if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier) && insideRaw(target(file, node.moduleSpecifier.text))) {
        const typeOnly = node.importClause?.isTypeOnly === true
          || (node.importClause?.name === undefined && node.importClause?.namedBindings !== undefined && ts.isNamedImports(node.importClause.namedBindings)
            && node.importClause.namedBindings.elements.length > 0 && node.importClause.namedBindings.elements.every((element) => element.isTypeOnly))
        if (!typeOnly) at(node, `imports ${node.moduleSpecifier.text}`)
      }
      if (ts.isExportDeclaration(node) && node.moduleSpecifier !== undefined && ts.isStringLiteral(node.moduleSpecifier) && !node.isTypeOnly && insideRaw(target(file, node.moduleSpecifier.text))) {
        at(node, `re-exports ${node.moduleSpecifier.text}`)
      }
      if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) {
        const [specifier] = node.arguments
        if (specifier !== undefined && ts.isStringLiteralLike(specifier) && insideRaw(target(file, specifier.text))) at(node, `imports ${specifier.text}`)
      }
    }
    if (file !== WORKSPACE && (ts.isAsExpression(node) || ts.isTypeAssertionExpression(node) || ts.isSatisfiesExpression(node))
      && ts.isTypeReferenceNode(node.type) && node.type.typeName.getText(parsed) === "DecidedScope") {
      at(node, "asserts a DecidedScope")
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

test("the rule finds a page reaching the raw reads or minting a DecidedScope, and lets lib and type-only imports through", () => {
  // A `.ts` page: in `.tsx`, `<DecidedScope>session` is JSX, and `as` is the only assertion there is.
  const page = join(SOURCE, "grid", "sample.ts")
  const sample = [
    'import { readRecordsAtScope } from "../lib/raw/records.ts"',
    'import type { RecordsRead } from "../lib/records-source.ts"',
    'import type { ObjectReads } from "@/lib/raw/record"',
    'import { type ObjectReads as Reads } from "@/lib/raw/record"',
    'import { readFindings } from "@/lib/raw/record"',
    'export { recordedRecords } from "../lib/raw/recorded-transport.ts"',
    'const raw = await import("../lib/raw/records.ts")',
    "const scope = session as DecidedScope",
    "const other = <DecidedScope>session",
    'import { useWorkspace } from "../lib/workspace.tsx"',
  ].join("\n")
  expect(violations(page, sample).map(({ line, text }) => [line, text])).toEqual([
    [1, "imports ../lib/raw/records.ts"],
    [5, "imports @/lib/raw/record"],
    [6, "re-exports ../lib/raw/recorded-transport.ts"],
    [7, "imports ../lib/raw/records.ts"],
    [8, "asserts a DecidedScope"],
    [9, "asserts a DecidedScope"],
  ])
  // Inside lib the raw reads are imported freely; only workspace.tsx makes a DecidedScope.
  expect(violations(join(LIB, "views.ts"), sample).map(({ line }) => line)).toEqual([8, 9])
  expect(violations(WORKSPACE, sample)).toEqual([])
  // The state unions import only each other.
  const state = ['import type { Read } from "../exact.ts"', 'import { useQuery } from "@tanstack/react-query"', 'export type { ReadState } from "./read.ts"', 'import { x } from "@/lib/state/other"'].join("\n")
  expect(violations(join(STATE, "index.ts"), state).map(({ line, text }) => [line, text])).toEqual([[1, "state imports ../exact.ts"], [2, "state imports @tanstack/react-query"]])
})

test("pages read records only through a decided workspace's reader (#1762)", () => {
  const files = walk(SOURCE)
  expect(files.length).toBeGreaterThan(100)
  expect(files).toContain(WORKSPACE)
  expect(files.some((file) => file.startsWith(`${RAW}/`))).toBe(true)
  expect(files.some((file) => insideState(file))).toBe(true)
  const found = files.flatMap((file) => violations(file, readFileSync(file, "utf8")))
  const report = found.map(({ file, line, text }) => `  ${relative(ROOT, file)}:${line} ${text}`).join("\n")
  expect(found, `raw reads reached outside src/lib/, or a DecidedScope made outside src/lib/workspace.tsx:\n${report}`).toEqual([])
})
