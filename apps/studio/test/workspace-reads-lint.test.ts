import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { dirname, join, relative, resolve } from "node:path"
import ts from "typescript"

/**
 * One workspace decision (#1762): a page reads records only through the reader of the workspace the
 * server's mode answer decided (`src/lib/workspace.tsx`), never at the session's scope or at none.
 *
 * - The raw reads (`src/lib/raw/`: records, one record, its findings, the recording) are private to
 *   `src/lib/`: no module outside it imports them, statically, dynamically (a computed specifier
 *   included) or by `import.meta.glob`, and no module outside `src/lib/raw/` re-exports them
 *   (`export … from`, or a binding it imported from there), so no `lib/` module or barrel can hand a
 *   page the raw read in one hop. A type-only import or re-export is no read and is let through.
 * - A `DecidedScope` is made in one place, `src/lib/workspace.tsx`. The TypeScript compiler, not the
 *   type's name, decides what is one: no other module asserts a value into a type that carries the brand
 *   from one that does not (an alias, `ScopedReader["scope"]`, a namespace's name, `as unknown as`, or
 *   `as any` where a `DecidedScope` is asked), and no object literal spreads one into a new one.
 * - The closed state unions live in `@norn/react` (`packages/norn-react/src/state.ts`) and import
 *   nothing: no app code and no package, so any renderer can take them as they are.
 *
 * Both rules are errors. There is no allowlist: the boundary is the `src/lib/` directory itself.
 */
const ROOT = join(import.meta.dir, "..")
const SOURCE = join(ROOT, "src")
const LIB = join(SOURCE, "lib")
const RAW = join(LIB, "raw")
const WORKSPACE = join(LIB, "workspace.tsx")
const STATE = join(ROOT, "..", "..", "packages", "norn-react", "src", "state.ts")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SOURCE, dir)))

interface Violation { readonly file: string; readonly line: number; readonly text: string }

/** Where an import specifier points, as a path under `src/` (`@/` is `src/`); undefined for a package. */
function target(from: string, specifier: string): string | undefined {
  if (specifier.startsWith("@/")) return join(SOURCE, specifier.slice(2))
  if (specifier.startsWith(".")) return resolve(dirname(from), specifier)
  return undefined
}

const insideLib = (path: string): boolean => path === LIB || path.startsWith(`${LIB}/`)
const insideRaw = (path: string | undefined): boolean => path !== undefined && (path === RAW || path.startsWith(`${RAW}/`))
const insideState = (path: string | undefined): boolean => path === STATE

/** Whether an import or re-export names only types: no value crosses it. */
function typeOnly(node: ts.ImportDeclaration | ts.ExportDeclaration): boolean {
  if (ts.isImportDeclaration(node)) {
    const clause = node.importClause
    return clause?.isTypeOnly === true
      || (clause?.name === undefined && clause?.namedBindings !== undefined && ts.isNamedImports(clause.namedBindings)
        && clause.namedBindings.elements.length > 0 && clause.namedBindings.elements.every((element) => element.isTypeOnly))
  }
  return node.isTypeOnly
    || (node.exportClause !== undefined && ts.isNamedExports(node.exportClause) && node.exportClause.elements.length > 0 && node.exportClause.elements.every((element) => element.isTypeOnly))
}

/**
 * Every way `source` reaches `src/lib/raw/` (an import outside `src/lib/`, a re-export outside
 * `src/lib/raw/`) or, in the state unions' module, imports anything at all. Read from the syntax alone;
 * {@link mintedScopes} asks the compiler about `DecidedScope`.
 */
export function violations(file: string, source: string): readonly Violation[] {
  const parsed = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, file.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  const found: Violation[] = []
  const at = (node: ts.Node, text: string) => found.push({ file, line: parsed.getLineAndCharacterOfPosition(node.getStart(parsed)).line + 1, text })
  const outsideLib = !insideLib(file)
  const outsideRaw = !insideRaw(file)
  const stateModule = insideState(file)
  // The value bindings this module imported from `src/lib/raw/`: exporting one again is a re-export too.
  const rawBindings = new Set<string>()
  for (const statement of parsed.statements) {
    if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier) || !insideRaw(target(file, statement.moduleSpecifier.text)) || typeOnly(statement)) continue
    const clause = statement.importClause
    if (clause?.name !== undefined) rawBindings.add(clause.name.text)
    if (clause?.namedBindings !== undefined) {
      if (ts.isNamespaceImport(clause.namedBindings)) rawBindings.add(clause.namedBindings.name.text)
      else for (const element of clause.namedBindings.elements) if (!element.isTypeOnly) rawBindings.add(element.name.text)
    }
  }
  const visit = (node: ts.Node) => {
    if (stateModule && (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier !== undefined && ts.isStringLiteral(node.moduleSpecifier)
      && !insideState(target(file, node.moduleSpecifier.text))) {
      at(node, `state imports ${node.moduleSpecifier.text}`)
    }
    if (outsideLib) {
      if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier) && insideRaw(target(file, node.moduleSpecifier.text)) && !typeOnly(node)) {
        at(node, `imports ${node.moduleSpecifier.text}`)
      }
      if (ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword) {
        const [specifier] = node.arguments
        // A computed specifier can name lib/raw/ at run time: no page imports one.
        if (specifier !== undefined && !ts.isStringLiteralLike(specifier)) at(node, "imports a computed specifier")
        else if (specifier !== undefined && insideRaw(target(file, specifier.text))) at(node, `imports ${specifier.text}`)
      }
      if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) && node.expression.name.text === "glob"
        && ts.isMetaProperty(node.expression.expression) && node.expression.expression.keywordToken === ts.SyntaxKind.ImportKeyword) {
        const [patterns] = node.arguments
        const each = patterns === undefined ? [] : ts.isArrayLiteralExpression(patterns) ? [...patterns.elements] : [patterns]
        for (const pattern of each) {
          if (!ts.isStringLiteralLike(pattern)) { at(node, "globs a computed pattern"); continue }
          // The pattern's fixed directory: a glob that could reach lib/raw/ (below it, or above it) imports it.
          const fixed = pattern.text.replace(/^!/, "").split(/[*?[{]/)[0] ?? ""
          const base = target(file, fixed.endsWith("/") || fixed === "" ? fixed : dirname(fixed))
          if (base !== undefined && (insideRaw(base) || RAW.startsWith(`${base}/`))) at(node, `globs ${pattern.text}`)
        }
      }
    }
    if (outsideRaw && ts.isExportDeclaration(node) && !typeOnly(node)) {
      if (node.moduleSpecifier !== undefined && ts.isStringLiteral(node.moduleSpecifier) && insideRaw(target(file, node.moduleSpecifier.text))) {
        at(node, `re-exports ${node.moduleSpecifier.text}`)
      }
      if (node.moduleSpecifier === undefined && node.exportClause !== undefined && ts.isNamedExports(node.exportClause)) {
        for (const element of node.exportClause.elements) {
          const local = (element.propertyName ?? element.name).getText(parsed)
          if (!element.isTypeOnly && rawBindings.has(local)) at(element, `re-exports ${local}, imported from src/lib/raw/`)
        }
      }
    }
    if (outsideRaw && ts.isExportAssignment(node) && ts.isIdentifier(node.expression) && rawBindings.has(node.expression.text)) {
      at(node, `re-exports ${node.expression.text}, imported from src/lib/raw/`)
    }
    ts.forEachChild(node, visit)
  }
  visit(parsed)
  return found
}

/** Studio's compiler options (`tsconfig.json`: its `paths`, JSX and module resolution). */
const OPTIONS = ((): ts.CompilerOptions => {
  const parsed = ts.getParsedCommandLineOfConfigFile(join(ROOT, "tsconfig.json"), { noEmit: true }, { ...ts.sys, onUnRecoverableConfigFileDiagnostic: () => {} })
  if (parsed === undefined) throw new Error("tsconfig.json could not be read")
  return parsed.options
})()

/** A program over `files`, where `overlay` stands in for files on disk (a fixture that is not on disk). */
export function programOf(files: readonly string[], overlay: ReadonlyMap<string, string> = new Map()): ts.Program {
  const host = ts.createCompilerHost(OPTIONS, true)
  const read = host.getSourceFile.bind(host)
  host.getSourceFile = (name, language, onError, fresh) => {
    const text = overlay.get(resolve(name))
    return text === undefined ? read(name, language, onError, fresh) : ts.createSourceFile(name, text, language, true)
  }
  const exists = host.fileExists.bind(host)
  host.fileExists = (name) => overlay.has(resolve(name)) || exists(name)
  return ts.createProgram({ rootNames: [...files], options: OPTIONS, host })
}

/**
 * Every place in `files` outside `workspace.tsx` that makes a `DecidedScope` the mode did not answer, as
 * the compiler types it: an assertion (`as`, `<…>`) into a type with a constituent that carries the brand
 * from an operand none of whose constituents does, `as any` (or `never`) where the expression's context
 * asks for one, and an object literal that spreads one (it keeps the brand under any scope it is given).
 */
export function mintedScopes(program: ts.Program, files: readonly string[]): readonly Violation[] {
  const checker = program.getTypeChecker()
  const workspace = program.getSourceFile(WORKSPACE)
  const alias = workspace?.statements.find((statement): statement is ts.TypeAliasDeclaration => ts.isTypeAliasDeclaration(statement) && statement.name.text === "DecidedScope")
  if (alias === undefined) throw new Error("src/lib/workspace.tsx declares no DecidedScope")
  const decided = checker.getTypeAtLocation(alias.name)
  const loose = ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never
  const constituents = (type: ts.Type): readonly ts.Type[] => type.isUnion() ? type.types : [type]
  const branded = (type: ts.Type | undefined): boolean =>
    type !== undefined && constituents(type).some((part) => (part.flags & loose) === 0 && checker.isTypeAssignableTo(part, decided))
  const found: Violation[] = []
  for (const file of files) {
    if (file === WORKSPACE) continue
    const source = program.getSourceFile(file)
    if (source === undefined) throw new Error(`${relative(ROOT, file)} is not in the program`)
    const at = (node: ts.Node, text: string) => found.push({ file, line: source.getLineAndCharacterOfPosition(node.getStart(source)).line + 1, text })
    const visit = (node: ts.Node) => {
      if (ts.isAsExpression(node) || ts.isTypeAssertionExpression(node)) {
        const asserted = checker.getTypeFromTypeNode(node.type)
        const operand = branded(checker.getTypeAtLocation(node.expression))
        if (!operand && branded(asserted)) at(node, "asserts a DecidedScope")
        else if (!operand && (asserted.flags & (ts.TypeFlags.Any | ts.TypeFlags.Never)) !== 0 && branded(checker.getContextualType(node))) at(node, "asserts a DecidedScope (as any)")
      }
      if (ts.isObjectLiteralExpression(node) && node.properties.some((property) => ts.isSpreadAssignment(property) && branded(checker.getTypeAtLocation(property.expression)))) {
        at(node, "spreads a DecidedScope into a new one")
      }
      ts.forEachChild(node, visit)
    }
    visit(source)
  }
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

test("the rule finds a page reaching the raw reads, and lets lib and type-only imports through", () => {
  const page = join(SOURCE, "grid", "sample.ts")
  const sample = [
    'import { readRecordsAtScope } from "../lib/raw/records.ts"',
    'import type { RecordsRead } from "../lib/records-source.ts"',
    'import type { ObjectReads } from "@/lib/raw/record"',
    'import { type ObjectReads as Reads } from "@/lib/raw/record"',
    'import { readFindings } from "@/lib/raw/record"',
    'export { recordedRecords } from "../lib/raw/recorded-transport.ts"',
    'const raw = await import("../lib/raw/records.ts")',
    'export type { RecordsRead } from "../lib/raw/records.ts"',
    'export { type ObjectReads } from "../lib/raw/record.ts"',
    'import { useWorkspace } from "../lib/workspace.tsx"',
    'const named = "records"; const computed = await import(`../lib/raw/${named}.ts`)',
    'const raws = import.meta.glob("../lib/raw/*.ts")',
    'const libs = import.meta.glob(["../lib/**/*.ts", "!../lib/state/*"])',
    'const registry = import.meta.glob("../../../../registry/core/artifacts/*.json", { eager: true })',
  ].join("\n")
  expect(violations(page, sample).map(({ line, text }) => [line, text])).toEqual([
    [1, "imports ../lib/raw/records.ts"],
    [5, "imports @/lib/raw/record"],
    [6, "re-exports ../lib/raw/recorded-transport.ts"],
    [7, "imports ../lib/raw/records.ts"],
    [11, "imports a computed specifier"],
    [12, "globs ../lib/raw/*.ts"],
    [13, "globs ../lib/**/*.ts"],
  ])
  // The state unions import nothing.
  const state = ['import type { Read } from "../exact.ts"', 'import { useQuery } from "@tanstack/react-query"', 'export type { ReadState } from "./read.ts"'].join("\n")
  expect(violations(STATE, state).map(({ line, text }) => [line, text])).toEqual([[1, "state imports ../exact.ts"], [2, "state imports @tanstack/react-query"], [3, "state imports ./read.ts"]])
})

// #1763 round 1: a `lib/` module (or a barrel) re-exporting a raw read handed it to every page in one hop.
test("inside lib the raw reads are imported freely, and re-exported only from lib/raw itself", () => {
  const barrel = [
    'import { readRecordsAtScope } from "./raw/records.ts"',
    'import { readFindings as findings, type ObjectReads } from "./raw/record.ts"',
    'import * as raw from "./raw/records.ts"',
    'export { readRecordsAtScope } from "./raw/records.ts"',
    'export * from "./raw/record.ts"',
    'export * as records from "@/lib/raw/records"',
    "export { findings, readRecordsAtScope as read }",
    "export { type ObjectReads }",
    "export default raw",
    'export type { RecordsRead } from "./raw/records.ts"',
    'export { type ObjectReads as Reads } from "./raw/record.ts"',
    'import { workspaceOf } from "./workspace.tsx"',
    "export { workspaceOf }",
  ].join("\n")
  expect(violations(join(LIB, "barrel.ts"), barrel).map(({ line, text }) => [line, text])).toEqual([
    [4, "re-exports ./raw/records.ts"],
    [5, "re-exports ./raw/record.ts"],
    [6, "re-exports @/lib/raw/records"],
    [7, "re-exports findings, imported from src/lib/raw/"],
    [7, "re-exports readRecordsAtScope, imported from src/lib/raw/"],
    [9, "re-exports raw, imported from src/lib/raw/"],
  ])
  // A raw module may re-export another: the boundary is lib/raw itself.
  expect(violations(join(RAW, "records.ts"), barrel)).toEqual([])
})

// #1763 round 1: the check matched the literal name `DecidedScope`, so an alias, an indexed type, a
// namespace's name or `as any` made one anywhere. The compiler decides what carries the brand.
test("the compiler finds every DecidedScope made outside workspace.tsx, under any name, and lets the brand's own values through", () => {
  const fixture = join(LIB, "minted-sample.ts")
  const source = [
    'import type { StudioScope } from "./definitions.ts"',
    'import type { DecidedScope, DecidedScope as D, ScopedReader } from "./workspace.tsx"',
    'import type * as W from "./workspace.tsx"',
    'import { readFindings } from "./raw/record.ts"',
    "declare const session: StudioScope",
    "declare const scope: DecidedScope",
    "declare const maybe: DecidedScope | null",
    "export const named = session as DecidedScope",
    "export const aliased = session as D",
    'export const indexed = session as ScopedReader["scope"]',
    "export const qualified = session as W.DecidedScope",
    "export const twice = session as unknown as DecidedScope",
    "export const nullable = session as DecidedScope | null",
    "export const angled = <DecidedScope>session",
    'export const loose = readFindings("t", "id", session as any)',
    "export const spread = { ...scope, orgId: \"other\" }",
    "export const narrowed = maybe as DecidedScope",
    "export const widened = scope as StudioScope",
    "export const kept = scope as DecidedScope",
    "export const unrelated = session as any",
    "export const copied = { ...session }",
  ].join("\n")
  const program = programOf([WORKSPACE, fixture], new Map([[fixture, source]]))
  expect(mintedScopes(program, [fixture]).map(({ line, text }) => [line, text])).toEqual([
    [8, "asserts a DecidedScope"],
    [9, "asserts a DecidedScope"],
    [10, "asserts a DecidedScope"],
    [11, "asserts a DecidedScope"],
    [12, "asserts a DecidedScope"],
    [13, "asserts a DecidedScope"],
    [14, "asserts a DecidedScope"],
    [15, "asserts a DecidedScope (as any)"],
    [16, "spreads a DecidedScope into a new one"],
  ])
  // workspace.tsx is where a DecidedScope is made.
  expect(mintedScopes(program, [WORKSPACE])).toEqual([])
}, 60_000)

test("pages read records only through a decided workspace's reader (#1762)", () => {
  const files = [...walk(SOURCE), STATE]
  expect(files.length).toBeGreaterThan(100)
  expect(files).toContain(WORKSPACE)
  expect(files.some((file) => file.startsWith(`${RAW}/`))).toBe(true)
  const found = [...files.flatMap((file) => violations(file, readFileSync(file, "utf8"))), ...mintedScopes(programOf(files), files)]
  const report = found.map(({ file, line, text }) => `  ${relative(ROOT, file)}:${line} ${text}`).join("\n")
  expect(found, `raw reads reached outside src/lib/, or a DecidedScope made outside src/lib/workspace.tsx:\n${report}`).toEqual([])
}, 120_000)
