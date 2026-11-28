/**
 * Native Rust authoring compiler bridge (`norn` CLI).
 *
 * Delegates all TypeScript profile checking, IR lowering, flow lowering,
 * question/dashboard compilation, and formatting to the native Rust `norn`
 * binary (`crates/norn-cli` / `crates/norn-authoring`), with zero `tsc` or
 * TypeScript Compiler API dependency.
 */
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { basename, dirname, join, relative, resolve, sep } from "node:path"
import type { SourceLocation } from "./builder/diagnostics.ts"
import type { Json } from "./canonical.ts"
import type { CompiledExample } from "./examples.ts"
import type { FlowSourceMapDraft, FlowSourceMapFlow } from "./flow-source-map.ts"
import { TOOL_ROOT, loadProfile, type Profile } from "./profile.ts"

export const PENDING_SOURCE_SHA256 = "pending:same-project-source-schema"

export interface Position {
  readonly line: number
  readonly column: number
}

export interface Diagnostic {
  readonly code: string
  readonly start: Position
  readonly end: Position
  readonly construct: string
  readonly message: string
}

export interface SymbolUse {
  readonly at: string
  readonly symbol: string
  readonly row: string
}

export interface FileReport {
  readonly file: string
  readonly profile: string
  readonly diagnostics: ReadonlyArray<Diagnostic>
  readonly uses: ReadonlyArray<SymbolUse>
}

export interface DefinitionSource {
  readonly artifactId: string
  readonly pointer: string
  readonly at: SourceLocation
}

export interface RetainedDefinitionDoc {
  readonly kind: "approval"
  readonly id: string
  readonly payload: Json
}

export interface EnvelopeInput {
  readonly scope: Readonly<Record<string, string>>
  readonly sourcePath: string
  readonly registryPackagePath?: string
}

export interface CompilerLimitsInput {
  readonly maxExpansionWork?: number
}

export interface DefinitionsCompileOptionsInput {
  readonly allowEmpty?: boolean
  readonly forbidUnsafeZen?: boolean
  readonly onModuleWork?: (path: string, workUsed: number) => void
}

export type DefinitionsCompileResult =
  | {
      readonly ok: true
      readonly artifacts: ReadonlyArray<Json>
      readonly flows: number
      readonly sourceMap: ReadonlyArray<DefinitionSource>
      readonly warnings: ReadonlyArray<Diagnostic>
      readonly examples: ReadonlyArray<CompiledExample>
      readonly retained: ReadonlyArray<RetainedDefinitionDoc>
    }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

export type FlowCompileResult =
  | {
      readonly ok: true
      readonly artifacts: ReadonlyArray<Json>
      readonly sourceMap: Json
      readonly warnings: ReadonlyArray<Diagnostic>
      readonly flowSourceMap?: FlowSourceMapDraft
    }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

export type QuestionsCompileResult =
  | { readonly ok: true; readonly questions: ReadonlyArray<Json> }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

export type DashboardsCompileResult =
  | { readonly ok: true; readonly dashboards: ReadonlyArray<Json> }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> }

export interface NativePackFlow {
  readonly file: string
  readonly artifacts: ReadonlyArray<Json>
  readonly flowSourceMap: FlowSourceMapFlow
  readonly builderSourceMap?: Json
}

export interface NativePackModuleOk {
  readonly file: string
  readonly ok: true
  readonly artifacts: ReadonlyArray<Json>
  readonly flows: number
  readonly sourceMap: ReadonlyArray<DefinitionSource>
  readonly warnings: ReadonlyArray<Diagnostic>
  readonly examples: ReadonlyArray<CompiledExample>
  readonly retained: ReadonlyArray<RetainedDefinitionDoc>
  readonly workUsed: number
  readonly flowSourceMap?: FlowSourceMapFlow | null
  readonly builderSourceMap?: Json | null
}

export interface NativePackModuleErr {
  readonly file: string
  readonly ok: false
  readonly diagnostics: ReadonlyArray<Diagnostic>
}

export type NativePackModule = NativePackModuleOk | NativePackModuleErr

export interface NativePackOutputOk {
  readonly ok: true
  readonly profile: string
  readonly artifacts: ReadonlyArray<Json>
  readonly sources: Readonly<Record<string, string>>
  readonly sourceMap: ReadonlyArray<DefinitionSource>
  readonly warnings: ReadonlyArray<Diagnostic & { readonly file: string }>
  readonly examples: ReadonlyArray<CompiledExample>
  readonly retained: ReadonlyArray<RetainedDefinitionDoc>
  readonly flows: ReadonlyArray<NativePackFlow>
  readonly flowSourceMap?: FlowSourceMapDraft
  readonly modules: ReadonlyArray<NativePackModule>
}

export interface NativePackOutputErr {
  readonly ok: false
  readonly profile: string
  readonly failures: ReadonlyArray<{ readonly file: string; readonly diagnostics: ReadonlyArray<Diagnostic> }>
  readonly modules: ReadonlyArray<NativePackModule>
}

export type NativePackOutput = NativePackOutputOk | NativePackOutputErr

const posix = (path: string): string => path.split(sep).join("/")
const REPO_ROOT = resolve(TOOL_ROOT, "..", "..")

/** Locate the native Rust `norn` CLI binary. */
export function resolveNornBin(): string {
  const candidates: Array<string | undefined> = [
    process.env.NORN_BIN,
    process.env.CARGO_TARGET_DIR ? join(process.env.CARGO_TARGET_DIR, "debug", "norn") : undefined,
    process.env.CARGO_TARGET_DIR ? join(process.env.CARGO_TARGET_DIR, "release", "norn") : undefined,
    join(REPO_ROOT, "target", "debug", "norn"),
    join(REPO_ROOT, "target", "release", "norn"),
  ]
  for (const candidate of candidates) {
    if (candidate && existsSync(candidate) && statSync(candidate).isFile()) return candidate
  }
  const onPath = Bun.which("norn")
  if (onPath) return onPath
  throw new Error(
    "native `norn` binary not found; set NORN_BIN or build with `cargo build -p norn-cli --bin norn`",
  )
}

function runNorn(args: ReadonlyArray<string>, cwd?: string): { readonly exitCode: number; readonly stdout: string; readonly stderr: string } {
  const bin = resolveNornBin()
  const result = Bun.spawnSync([bin, ...args], {
    cwd: cwd ?? process.cwd(),
    stdout: "pipe",
    stderr: "pipe",
  })
  return {
    exitCode: result.exitCode,
    stdout: result.stdout.toString("utf8"),
    stderr: result.stderr.toString("utf8"),
  }
}

function commonParentDir(paths: ReadonlyArray<string>): string {
  if (paths.length === 0) return process.cwd()
  const dirs = paths.map((file) => resolve(dirname(file)).split(sep))
  let prefix = dirs[0]!
  for (const parts of dirs.slice(1)) {
    let i = 0
    while (i < prefix.length && i < parts.length && prefix[i] === parts[i]) i++
    prefix = prefix.slice(0, i)
  }
  const joined = prefix.join(sep)
  return joined.length > 0 ? joined : sep
}

export interface NativeCompilePackArgs {
  readonly target: string
  readonly root?: string
  readonly scope?: Readonly<Record<string, string>>
  readonly registry?: string
  readonly sourcePath?: string
  readonly forbidUnsafeZen?: boolean
  readonly allowEmpty?: boolean
  readonly maxExpansionWork?: number
  readonly modules?: ReadonlyArray<{
    readonly path: string
    readonly scope?: Readonly<Record<string, string>>
    readonly sourcePath?: string
    readonly registryPackagePath?: string
  }>
}

/** Invoke `norn compile --pack-json` natively and return the parsed pack payload. */
export function runNativeCompilePackJson(options: NativeCompilePackArgs): NativePackOutput {
  let manifestDir: string | undefined
  try {
    const args: string[] = ["compile", options.target, "--pack-json", "--tool-root", TOOL_ROOT]
    if (options.root) args.push("--root", resolve(options.root))
    if (options.registry) {
      args.push("--registry", options.registry)
    } else if (options.scope?.orgId && options.scope?.environmentId) {
      args.push("--org", options.scope.orgId, "--env", options.scope.environmentId)
    }
    if (options.sourcePath) args.push("--source-path", options.sourcePath)
    if (options.forbidUnsafeZen) args.push("--forbid-unsafe-zen")
    if (options.allowEmpty) args.push("--allow-empty")
    if (options.maxExpansionWork !== undefined) args.push("--max-expansion-work", String(options.maxExpansionWork))
    if (options.modules) {
      manifestDir = mkdtempSync(join(tmpdir(), "norn-modules-"))
      const manifestPath = join(manifestDir, "modules.json")
      writeFileSync(manifestPath, JSON.stringify(options.modules), { mode: 0o600 })
      args.push("--modules", manifestPath)
    }
    const run = runNorn(args)
    if (run.exitCode !== 0 && run.exitCode !== 1) {
      throw new Error(`norn compile failed (exit ${run.exitCode}): ${run.stderr || run.stdout}`)
    }
    return JSON.parse(run.stdout) as NativePackOutput
  } finally {
    if (manifestDir) rmSync(manifestDir, { recursive: true, force: true })
  }
}

/** Compile one or more TypeScript definition modules in a single native `AuthoringProgram`. */
export function compileDefinitionFiles(
  files: ReadonlyArray<string>,
  envelope: EnvelopeInput | ((path: string) => EnvelopeInput),
  limits: CompilerLimitsInput = {},
  options: DefinitionsCompileOptionsInput = {},
): DefinitionsCompileResult[] {
  if (files.length === 0) return []
  const resolved = files.map((file) => resolve(file))
  const baseDir = commonParentDir(resolved)
  const specs = resolved.map((file) => {
    const env = typeof envelope === "function" ? envelope(file) : envelope
    return {
      path: file,
      scope: env.scope,
      sourcePath: env.sourcePath,
      ...(env.registryPackagePath ? { registryPackagePath: env.registryPackagePath } : {}),
    }
  })
  const output = runNativeCompilePackJson({
    target: baseDir,
    root: TOOL_ROOT,
    ...(options.forbidUnsafeZen !== undefined ? { forbidUnsafeZen: options.forbidUnsafeZen } : {}),
    allowEmpty: options.allowEmpty ?? false,
    ...(limits.maxExpansionWork !== undefined ? { maxExpansionWork: limits.maxExpansionWork } : {}),
    modules: specs,
  })
  return output.modules.map((mod, index) => {
    if (!mod.ok) return { ok: false, diagnostics: mod.diagnostics }
    options.onModuleWork?.(resolved[index]!, mod.workUsed)
    return {
      ok: true,
      artifacts: mod.artifacts,
      flows: mod.flows,
      sourceMap: mod.sourceMap,
      warnings: mod.warnings,
      examples: mod.examples,
      retained: mod.retained,
    }
  })
}

/** Compile a single TypeScript definition module natively. */
export function compileDefinitions(
  file: string,
  envelope: EnvelopeInput,
  limits: CompilerLimitsInput = {},
  options: DefinitionsCompileOptionsInput = {},
): DefinitionsCompileResult {
  const [first] = compileDefinitionFiles([file], envelope, limits, options)
  if (!first) {
    return {
      ok: false,
      diagnostics: [{
        code: "NORN-FLOW-RESULT",
        start: { line: 1, column: 1 },
        end: { line: 1, column: 1 },
        construct: file,
        message: "compiler produced no result",
      }],
    }
  }
  return first
}

export interface FlowCompileOptionsInput {
  readonly scope: Readonly<Record<string, string>>
  readonly sourcePath: string
  readonly registryPackagePath?: string
  readonly sourceRoot?: string
  readonly includeFlowSourceMap?: boolean
  readonly forbidUnsafeZen?: boolean
}

/** Compile one or more `Norn.flow` modules natively. */
export function compileFiles(
  files: ReadonlyArray<string>,
  options: FlowCompileOptionsInput,
): FlowCompileResult[] {
  if (files.length === 0) return []
  const resolved = files.map((file) => resolve(file))
  const baseDir = commonParentDir(resolved)
  const root = options.sourceRoot ?? TOOL_ROOT
  const specs = resolved.map((file) => ({
    path: file,
    scope: options.scope,
    sourcePath: options.sourcePath,
    ...(options.registryPackagePath ? { registryPackagePath: options.registryPackagePath } : {}),
  }))
  const output = runNativeCompilePackJson({
    target: baseDir,
    root,
    ...(options.forbidUnsafeZen !== undefined ? { forbidUnsafeZen: options.forbidUnsafeZen } : {}),
    allowEmpty: false,
    modules: specs,
  })
  return output.modules.map((mod) => {
    if (!mod.ok) return { ok: false, diagnostics: mod.diagnostics }
    const includeMap = options.includeFlowSourceMap !== false
    const flowSourceMap = includeMap && mod.flowSourceMap
      ? { format: "norn-flow-source-map-draft/1" as const, flows: [mod.flowSourceMap] }
      : undefined
    return {
      ok: true,
      artifacts: mod.artifacts,
      sourceMap: mod.builderSourceMap ?? ({} as Json),
      warnings: mod.warnings,
      ...(flowSourceMap ? { flowSourceMap } : {}),
    }
  })
}

/** Compile a single `Norn.flow` module natively. */
export function compileFile(file: string, options: FlowCompileOptionsInput): FlowCompileResult {
  const [first] = compileFiles([file], options)
  if (!first) {
    return {
      ok: false,
      diagnostics: [{
        code: "NORN-FLOW-RESULT",
        start: { line: 1, column: 1 },
        end: { line: 1, column: 1 },
        construct: file,
        message: "compiler produced no result",
      }],
    }
  }
  return first
}

/** Compile standalone `Norn.question` declarations via native `norn question`. */
export function compileQuestions(file: string): QuestionsCompileResult {
  const run = runNorn(["question", resolve(file), "--tool-root", TOOL_ROOT])
  if (run.exitCode === 0) {
    return { ok: true, questions: JSON.parse(run.stdout) as ReadonlyArray<Json> }
  }
  if (run.exitCode === 1) {
    const parsed = JSON.parse(run.stdout) as { readonly diagnostics?: ReadonlyArray<Diagnostic> }
    return { ok: false, diagnostics: parsed.diagnostics ?? [] }
  }
  throw new Error(`norn question failed (exit ${run.exitCode}): ${run.stderr || run.stdout}`)
}

/** Compile standalone `Norn.dashboard` declarations via native `norn dashboard`. */
export function compileDashboards(file: string): DashboardsCompileResult {
  const run = runNorn(["dashboard", resolve(file), "--tool-root", TOOL_ROOT])
  if (run.exitCode === 0) {
    return { ok: true, dashboards: JSON.parse(run.stdout) as ReadonlyArray<Json> }
  }
  if (run.exitCode === 1) {
    const parsed = JSON.parse(run.stdout) as { readonly diagnostics?: ReadonlyArray<Diagnostic> }
    return { ok: false, diagnostics: parsed.diagnostics ?? [] }
  }
  throw new Error(`norn dashboard failed (exit ${run.exitCode}): ${run.stderr || run.stdout}`)
}

/** Native `norn profile` wrapper preserving `FileReport` shape. */
export class ProfileChecker {
  readonly profile: Profile
  constructor(profile: Profile = loadProfile()) {
    this.profile = profile
  }

  check(files: ReadonlyArray<string>): FileReport[] {
    return files.map((file) => {
      const abs = resolve(file)
      const run = runNorn(["profile", abs, "--tool-root", TOOL_ROOT])
      if (run.exitCode !== 0 && run.exitCode !== 1) {
        throw new Error(`norn profile failed (exit ${run.exitCode}): ${run.stderr || run.stdout}`)
      }
      const reports = JSON.parse(run.stdout) as ReadonlyArray<FileReport>
      const first = reports[0]
      if (!first) throw new Error(`norn profile returned no report for ${file}`)
      return {
        file: posix(relative(TOOL_ROOT, abs)),
        profile: first.profile,
        diagnostics: first.diagnostics.map((d) => ({
          code: d.code,
          start: { line: d.start.line, column: d.start.column },
          end: { line: d.end.line, column: d.end.column },
          construct: d.construct,
          message: d.message,
        })),
        uses: first.uses.map((u) => ({ at: u.at, symbol: u.symbol, row: u.row })),
      }
    })
  }
}

/** Format a single TypeScript source string via native `norn fmt`. */
export function formatSourceNative(file: string, text: string): string {
  const dir = mkdtempSync(join(tmpdir(), "norn-fmt-"))
  try {
    const name = basename(file)
    const targetName = name.endsWith(".ts") || name.endsWith(".tsx") ? name : `${name}.ts`
    const target = join(dir, targetName)
    writeFileSync(target, text, { mode: 0o600 })
    const run = runNorn(["fmt", target])
    if (run.exitCode !== 0) {
      throw new Error(`norn fmt failed (exit ${run.exitCode}): ${run.stderr || run.stdout}`)
    }
    return readFileSync(target, "utf8")
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

const MAX_INLINE_MODULE_BYTES = 1_048_576
const UTF8_ENCODER = new TextEncoder()

function hasDisallowedInlineImports(source: string): boolean {
  if (/^\s*\/\/\/\s*<\s*(?:reference|amd-dependency|amd-module)\b/m.test(source)) return true
  const stripped = source
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/\/\/[^\n]*/g, " ")
  const specifiers = [
    ...stripped.matchAll(/\b(?:import|export)\s+(?:type\s+)?(?:[\s\S]*?\s+from\s+)?["']([^"']+)["']/g),
  ].map((m) => m[1]!)
  return specifiers.some(
    (spec) => spec !== "@norn/authoring" && !/^effect(?:\/[A-Za-z0-9_-]+)*$/.test(spec),
  )
}

/** Compile an inline TypeScript authoring module natively (zero `tsc`). */
export function compileModule(
  source: string,
  scope: Readonly<Record<string, string>>,
):
  | {
      readonly ok: true
      readonly artifacts: ReadonlyArray<Json>
      readonly sourceMap: ReadonlyArray<DefinitionSource>
      readonly warnings: ReadonlyArray<Diagnostic>
    }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<Diagnostic> } {
  if (UTF8_ENCODER.encode(source).length > MAX_INLINE_MODULE_BYTES) {
    return {
      ok: false,
      diagnostics: [{
        code: "NORN-FLOW-UNBOUNDED",
        start: { line: 1, column: 1 },
        end: { line: 1, column: 1 },
        construct: "module.ts",
        message: "module exceeds 1 MiB",
      }],
    }
  }
  if (hasDisallowedInlineImports(source)) {
    return {
      ok: false,
      diagnostics: [{
        code: "NORN-FLOW-MODULE",
        start: { line: 1, column: 1 },
        end: { line: 1, column: 1 },
        construct: "import",
        message: "MCP modules import only effect and @norn/authoring; include local definitions in this module",
      }],
    }
  }
  const dir = mkdtempSync(join(tmpdir(), "norn-authoring-"))
  try {
    const file = join(dir, "module.ts")
    writeFileSync(file, source, { mode: 0o600 })
    const output = runNativeCompilePackJson({
      target: file,
      root: dir,
      scope,
      sourcePath: "module.ts",
      allowEmpty: true,
    })
    const mod = output.modules[0]
    if (!mod) {
      return {
        ok: false,
        diagnostics: [{
          code: "NORN-FLOW-RESULT",
          start: { line: 1, column: 1 },
          end: { line: 1, column: 1 },
          construct: "module.ts",
          message: "module declares no definitions",
        }],
      }
    }
    if (!mod.ok) return { ok: false, diagnostics: mod.diagnostics }
    if (mod.artifacts.length === 0) {
      return {
        ok: false,
        diagnostics: [{
          code: "NORN-FLOW-RESULT",
          start: { line: 1, column: 1 },
          end: { line: 1, column: 1 },
          construct: "module.ts",
          message: "module declares no definitions",
        }],
      }
    }
    return {
      ok: true,
      artifacts: mod.artifacts,
      sourceMap: mod.sourceMap.map((s) => ({ ...s, at: { ...s.at, file: "module.ts" } })),
      warnings: output.ok ? output.warnings : mod.warnings,
    }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}
