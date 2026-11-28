/**
 * The `norn dev` compile worker (#207): compiles the project's sources and
 * prints one NDJSON event per generation on stdout (logs go to stderr).
 *
 * The worker only compiles. Its artifacts are untrusted input that the dev
 * host re-admits with the native checker before anything is installed
 * (DEV-ONE-AUTHORITY-1). It never imports authored code.
 *
 *   bun src/worker.ts --project <dir> [--once]
 *
 * Each event is `{ event: "compiled", gen, sourceDigest, ok, ... }`:
 * `ok: true` carries `artifacts`, `definitions` (retained definitions such as
 * approvals, `{ kind, id, payload }`, which the host installs through the
 * cell's own admit and sign-off path), `sourceMap` and `warnings`; `ok: false`
 * carries `diagnostics`, each with a project-relative `source`. `gen` grows
 * with every file-system batch; `sourceDigest` is the SHA-256 of the sorted
 * (project-relative path, bytes) pairs under `sources`, so a caller can tell
 * which bytes a result belongs to. A setup failure prints `{ event: "fatal" }`.
 *
 * Limit: the watcher covers the source directories. A top-level module that
 * is a symlink to a file elsewhere in the project is compiled and digested,
 * but an edit to its target triggers no recompile until a source is saved.
 */
import { createHash } from "node:crypto"
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync, statSync, watch, type FSWatcher } from "node:fs"
import { join, relative, resolve, sep } from "node:path"
import type { Json } from "./canonical.ts"
import type { RetainedDefinitionDoc } from "./native-compiler.ts"
import { artifactId, compilePack, isArtifact, packModules, readArtifacts, type PackResult } from "./pack.ts"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError, readProject, type Project } from "./project.ts"

export interface CompiledEvent {
  readonly event: "compiled"
  readonly gen: number
  readonly sourceDigest: string
  readonly ok: boolean
  readonly artifacts?: ReadonlyArray<unknown>
  readonly definitions?: ReadonlyArray<unknown>
  readonly sourceMap?: ReadonlyArray<unknown>
  readonly warnings?: ReadonlyArray<unknown>
  readonly diagnostics?: ReadonlyArray<unknown>
}

const posix = (path: string) => path.split(sep).join("/")

/** Resolve `path` with every symlink followed, or return `undefined` if dangling or outside `realRoot`. */
function resolveInsideProject(realRoot: string, path: string): string | undefined {
  let real: string
  try { real = realpathSync(path) }
  catch { return undefined } // a dangling link resolves nowhere, so not inside the project
  return real === realRoot || real.startsWith(realRoot + sep) ? real : undefined
}

function outside(what: string): ProjectError {
  return new ProjectError("cli.project.invalid_manifest", `${what} does not resolve inside the project; sources must stay inside it.`)
}

/** The manifest's source directories that exist; one that resolves outside the project refuses. */
function sourceDirs(root: string, manifest: Project, realRoot: string = realpathSync(root)): string[] {
  return manifest.sources.map((source) => resolve(root, source)).filter((dir) => {
    if (!existsSync(dir)) return false
    const real = resolveInsideProject(realRoot, dir)
    if (real === undefined) throw outside(`source ${posix(relative(root, dir))}`)
    return statSync(real).isDirectory()
  })
}

interface ResolvedProjectModule {
  readonly logical: string
  readonly logicalRel: string
  readonly real: string
}

/**
 * The modules compiled as one set: each source directory's top-level `*.ts`.
 * A module that is a symlink is compiled only if it resolves once inside the
 * project, and every resolved target must be unique so two logical modules
 * never alias the same file.
 */
function projectModules(root: string, dirs: readonly string[], realRoot: string): ResolvedProjectModule[] {
  const seen = new Map<string, string>()
  return dirs.flatMap((dir) => packModules(dir)).map((logical) => {
    const logicalRel = posix(relative(root, logical))
    const real = resolveInsideProject(realRoot, logical)
    if (real === undefined) throw outside(`module ${logicalRel}`)
    const prior = seen.get(real)
    if (prior !== undefined) {
      throw new ProjectError(
        "cli.project.invalid_manifest",
        `modules ${prior} and ${logicalRel} resolve to the same file (${posix(relative(realRoot, real))}); remove the duplicate alias.`,
      )
    }
    seen.set(real, logicalRel)
    return { logical, logicalRel, real }
  })
}

function sourceFiles(root: string, dirs: readonly string[], realRoot: string): Map<string, string> {
  const files = new Map<string, string>()
  for (const module of projectModules(root, dirs, realRoot)) {
    files.set(module.logicalRel, module.real)
  }
  // Below the top level, symlinks are never followed (no cycle, nothing
  // outside the project), and each real directory is read once.
  const visited = new Set<string>()
  const walk = (dir: string) => {
    const real = realpathSync(dir)
    if (visited.has(real)) return
    visited.add(real)
    for (const entry of readdirSync(dir).sort()) {
      const path = join(dir, entry)
      const kind = lstatSync(path)
      if (kind.isDirectory()) walk(path)
      else if (kind.isFile() && (entry.endsWith(".ts") || entry.endsWith(".json"))) {
        const rel = posix(relative(root, path))
        if (!files.has(rel)) files.set(rel, path)
      }
    }
  }
  for (const dir of dirs) walk(dir)
  return files
}

/**
 * SHA-256 over the sorted (project-relative path, bytes) pairs of every `*.ts`
 * file under `sources` and of every compiled module, a symlinked one included
 * (its bytes are its target's).
 */
export function sourceDigest(root: string, manifest: Project): string {
  const realRoot = realpathSync(root)
  const dirs = sourceDirs(root, manifest, realRoot)
  const files = sourceFiles(root, dirs, realRoot)
  const hash = createHash("sha256")
  for (const rel of [...files.keys()].sort()) {
    const bytes = readFileSync(files.get(rel)!)
    hash.update(`${rel}\0${bytes.length}\0`).update(bytes)
  }
  return hash.digest("hex")
}

/**
 * The retained kinds a project's JSON sources may declare: approvals, which
 * ride the set, and questions and dashboards, which the dev host installs once
 * the set serves (DEV-READ-DEFINITIONS-1).
 */
const RETAINED_KINDS: ReadonlySet<string> = new Set(["approval", "question", "dashboard"])

/** A retained definition a JSON source declares. */
export interface SourceDefinitionDoc {
  readonly kind: "approval" | "question" | "dashboard"
  readonly id: string
  readonly payload: Json
}

/** A JSON source that names a retained kind but is not a definition of it. */
class DefinitionSourceError extends Error {
  constructor(readonly construct: string, message: string) { super(message) }
}

/**
 * A `JSON.parse` reviver that keeps each number token's exact source text and
 * records, in `inexact`, every one that is not a plain safe integer (a
 * fraction, an exponent, or an integer past 2^53): its parsed double may
 * already be rounded (`1000.00000000000001` parses as 1000). Without the
 * source text (a runtime that does not pass it) every number is inexact.
 */
function exactNumbers(inexact: string[]): (key: string, value: unknown, context?: { readonly source?: string }) => unknown {
  return (_key, value, context) => {
    if (typeof value !== "number") return value
    const source = context?.source
    if (source === undefined || !/^-?(0|[1-9][0-9]*)$/.test(source) || !Number.isSafeInteger(value)) inexact.push(source ?? String(value))
    return value
  }
}

/**
 * Top-level `*.json` files in each source directory, checked for project
 * confinement before reading and parsed once for both pre-compiled artifact
 * envelopes and retained definitions (`{ kind, id, payload }`, kind
 * `approval`, `question` or `dashboard`). A document whose `kind` is a
 * retained kind but whose `id` or `payload` is malformed, or whose `id`
 * another JSON source declares, throws `DefinitionSourceError`: a gate the
 * author declared is never silently dropped.
 */
function projectJsonSources(
  root: string,
  dirs: readonly string[],
  realRoot: string,
): { readonly artifacts: Json[]; readonly definitions: SourceDefinitionDoc[] } {
  const artifacts: Json[] = []
  const definitions: SourceDefinitionDoc[] = []
  const seen = new Set<string>()
  for (const dir of dirs) {
    for (const entry of readdirSync(dir).filter((f) => f.endsWith(".json")).sort()) {
      const file = join(dir, entry)
      const path = posix(relative(root, file))
      const realFile = resolveInsideProject(realRoot, file)
      if (realFile === undefined) throw outside(`artifact ${path}`)
      if (!statSync(realFile).isFile()) continue
      const inexact: string[] = []
      const value: unknown = JSON.parse(readFileSync(realFile, "utf8"), exactNumbers(inexact))
      if (isArtifact(value)) artifacts.push(value)
      if (typeof value !== "object" || value === null || Array.isArray(value)) continue
      const doc = value as { readonly kind?: unknown; readonly id?: unknown; readonly payload?: unknown }
      if (typeof doc.kind !== "string" || !RETAINED_KINDS.has(doc.kind)) continue
      // #1469 S10: a number is checked as written, before any rounding.
      if (inexact.length > 0) {
        throw new DefinitionSourceError(path, `${path} writes the number ${inexact[0]}, which JSON cannot carry exactly: write an integer such as 1000, or the exact decimal as text, such as "1000.10"`)
      }
      if (typeof doc.id !== "string" || doc.id === "" || typeof doc.payload !== "object" || doc.payload === null || Array.isArray(doc.payload)) {
        throw new DefinitionSourceError(path, `${path} declares ${doc.kind === "approval" ? "an" : "a"} ${doc.kind} that is not { kind, id, payload } with a non-empty id and an object payload`)
      }
      if (seen.has(doc.id)) throw new DefinitionSourceError(doc.id, `definition ${doc.id} is declared by more than one JSON source`)
      seen.add(doc.id)
      definitions.push({ kind: doc.kind as SourceDefinitionDoc["kind"], id: doc.id, payload: doc.payload as Json })
    }
  }
  return { artifacts, definitions }
}

/**
 * The cell-owned `norn.` types a mapping needs (the link claims and source
 * evidence the cell writes for every mapped row). A hosted tenant's base
 * provides them; a project that declares a mapping and none of them gets the
 * registry's copies, as `norn init --template three-systems` copies them.
 */
const CELL_OWNED = ["norn.ObservedLinkClaim.entity", "norn.SourceDocument.interface", "norn.SourceEvidence.interface"] as const
const CELL_OWNED_DIR = resolve(TOOL_ROOT, "..", "..", "registry", "core", "artifacts")

function cellOwned(artifacts: ReadonlyArray<Json>): Json[] {
  const kinds = artifacts.map((artifact) => (artifact as { readonly kind?: unknown }).kind)
  if (!kinds.includes("mapping")) return []
  const declared = new Set(artifacts.map(artifactId))
  if (CELL_OWNED.some((id) => declared.has(id))) return []
  return CELL_OWNED.flatMap((id) => readArtifacts(join(CELL_OWNED_DIR, `${id}.json`)).map(({ artifact }) => artifact))
}

/**
 * The project compiled as one pack: `norn dev`, `norn check <project>` and
 * `norn explain` all compile through here, so each reads the same sources:
 * the top-level modules, the top-level JSON artifacts and retained
 * definitions, and the cell-owned types a mapping needs.
 */
export function compileProjectPack(root: string, manifest: Project, options: { readonly forbidUnsafeZen?: boolean } = {}): PackResult {
  return projectPack(root, manifest, options).pack
}

/** Why a hosted command (`norn plan`, `apply`, `check`) leaves a source question or dashboard out. */
export const HOSTED_READS_REASON = "hosted cells install questions and dashboards only through the definitions API (POST /v1/definitions:install), not from sources"

/** A source question or dashboard a hosted command leaves out, and why. */
export interface OmittedDefinition {
  readonly kind: string
  readonly id: string
  readonly reason: string
}

/**
 * `compileProjectPack` for a hosted command, with the source questions and
 * dashboards it leaves out (DEV-READ-DEFINITIONS-1): only `norn dev` installs
 * them from sources, so a hosted plan, apply or check names each one it does
 * not send, never dropping it silently.
 */
export function loadProjectPack(root: string, manifest: Project, options: { readonly forbidUnsafeZen?: boolean } = {}): { readonly pack: PackResult; readonly omitted: OmittedDefinition[] } {
  const { pack, reads } = projectPack(root, manifest, options)
  return { pack, omitted: reads.map((definition) => ({ kind: definition.kind, id: definition.id, reason: HOSTED_READS_REASON })) }
}

/** One line per omitted definition, for a hosted command's text output. */
export function omittedLines(omitted: ReadonlyArray<OmittedDefinition>): string[] {
  return omitted.map((definition) => `note: ${definition.kind} ${definition.id} is not sent: ${definition.reason}`)
}

/**
 * `compileProjectPack`, with the sources' questions and dashboards
 * (DEV-READ-DEFINITIONS-1) from the same one read of the JSON sources: the
 * dev host installs them once a revision serves, and approvals ride the set.
 */
function projectPack(root: string, manifest: Project, options: { readonly forbidUnsafeZen?: boolean }): { readonly pack: PackResult; readonly reads: SourceDefinitionDoc[] } {
  const realRoot = realpathSync(root)
  const dirs = sourceDirs(root, manifest, realRoot)
  const modules = projectModules(root, dirs, realRoot)
  // Every module was resolved once and verified inside `realRoot` with unique
  // canonical targets; pass those exact canonical paths to native `norn`
  // (which refuses symlinks with O_NOFOLLOW) while preserving each logical
  // project-relative path for provenance.
  const moduleSourcePaths = new Map<string, string>(
    modules.map(({ logicalRel, real }) => [real, logicalRel]),
  )
  const pack = compilePack(realRoot, {
    modules: modules.map(({ real }) => real),
    moduleSourcePaths,
    scope: { orgId: manifest.scope.orgId, environmentId: manifest.scope.environmentId },
    root: realRoot,
    ...(options.forbidUnsafeZen !== undefined ? { forbidUnsafeZen: options.forbidUnsafeZen } : {}),
  })
  if (!pack.ok) return { pack, reads: [] }
  const refused = (construct: string, message: string): { readonly pack: PackResult; readonly reads: SourceDefinitionDoc[] } =>
    ({ pack: { ok: false, profile: pack.profile, failures: [{ file: ".", diagnostics: [{ code: "NORN-FLOW-LITERAL", start: { line: 1, column: 1 }, end: { line: 1, column: 1 }, construct, message }] }] }, reads: [] })
  let files: Json[]
  let definitions: SourceDefinitionDoc[]
  try {
    const jsonSources = projectJsonSources(root, dirs, realRoot)
    files = jsonSources.artifacts
    definitions = jsonSources.definitions
  } catch (error) {
    if (error instanceof DefinitionSourceError) return refused(error.construct, error.message)
    throw error
  }
  const owned = cellOwned([...files, ...pack.artifacts])
  const sources = new Map(pack.sources)
  for (const artifact of files) {
    const id = artifactId(artifact)
    if (sources.has(id)) return refused(id, `artifact ${id} is declared by a JSON source and by ${sources.get(id)}`)
    sources.set(id, "json")
  }
  for (const artifact of owned) sources.set(artifactId(artifact), "registry/core")
  const ids = new Set(pack.retained.map((definition) => definition.id))
  const duplicate = definitions.find((definition) => ids.has(definition.id))
  if (duplicate) return refused(duplicate.id, `definition ${duplicate.id} is declared by a JSON source and by a module`)
  // Approvals ride the set wherever it is applied; questions and dashboards are
  // the dev host's to install once it serves (`reads`).
  const approvals = definitions.filter((definition): definition is RetainedDefinitionDoc => definition.kind === "approval")
  return {
    pack: { ...pack, artifacts: [...owned, ...files, ...pack.artifacts], sources, retained: [...pack.retained, ...approvals] },
    reads: definitions.filter((definition) => definition.kind !== "approval"),
  }
}

/** Shared project-vs-pack loader used by `norn check`, `norn compile`, `norn codegen`, and `norn plan|apply`. */
export function compileInputPack(input: string, options: InputPackOptions = {}): PackResult {
  return loadInputPack(input, options).pack
}

/** Where `compileInputPack` and `loadInputPack` read a pack from, and as which scope. */
export interface InputPackOptions {
  readonly scope?: { readonly orgId: string; readonly environmentId: string }
  readonly overrideProjectScope?: boolean
  readonly forbidUnsafeZen?: boolean
}

/**
 * `compileInputPack` for a hosted command (`norn plan`, `apply`, `check`), with
 * a project's source questions and dashboards it leaves out (`loadProjectPack`).
 * A pack that is not a project has none.
 */
export function loadInputPack(input: string, options: InputPackOptions = {}): { readonly pack: PackResult; readonly omitted: OmittedDefinition[] } {
  const full = resolve(input)
  const stat = statSync(full)
  const unsafe = options.forbidUnsafeZen !== undefined ? { forbidUnsafeZen: options.forbidUnsafeZen } : {}
  if (stat.isDirectory() && existsSync(join(full, "norn.json"))) {
    const { root, manifest } = readProject(full)
    const effective = options.overrideProjectScope && options.scope ? { ...manifest, scope: options.scope } : manifest
    return loadProjectPack(root, effective, unsafe)
  }
  if (!options.scope) throw new Error("--org and --env are required")
  return {
    pack: compilePack(full, { scope: options.scope, root: process.cwd(), ...unsafe, ...(stat.isFile() ? { modules: [full] } : {}) }),
    omitted: [],
  }
}

/** Compile every top-level module of every source directory as one set. */
export function compileProject(root: string, manifest: Project, gen: number): CompiledEvent {
  const digest = sourceDigest(root, manifest)
  const { pack, reads } = projectPack(root, manifest, {})
  if (!pack.ok) {
    const diagnostics = pack.failures.flatMap((failure) => failure.diagnostics.map((diagnostic) => ({
      ...diagnostic,
      source: { file: failure.file, line: diagnostic.start.line, column: diagnostic.start.column },
    })))
    return { event: "compiled", gen, sourceDigest: digest, ok: false, diagnostics }
  }
  return {
    event: "compiled", gen, sourceDigest: digest, ok: true,
    artifacts: [...pack.baseArtifacts, ...pack.artifacts], definitions: [...pack.retained, ...reads], sourceMap: pack.sourceMap, warnings: pack.warnings,
  }
}

function emit(event: unknown): void {
  process.stdout.write(`${JSON.stringify(event)}\n`)
}

function option(args: readonly string[], name: string): string | undefined {
  const index = args.indexOf(name)
  const value = index < 0 ? undefined : args[index + 1]
  return value === undefined || value.startsWith("--") ? undefined : value
}

export function main(args: readonly string[]): void {
  let project: { root: string; manifest: Project }
  try { project = readProject(option(args, "--project") ?? process.cwd()) }
  catch (error) {
    emit({ event: "fatal", diagnostic: error instanceof ProjectError ? error.diagnostic : { code: "cli.project.io", message: String(error) } })
    process.exit(3)
  }
  const { root, manifest } = project
  let gen = 0
  const compile = () => {
    gen += 1
    try { emit(compileProject(root, manifest, gen)) }
    catch (error) {
      // The digest can fail for the same reason the compile did; the worker survives either.
      let digest = ""
      try { digest = sourceDigest(root, manifest) } catch { /* reported below */ }
      emit({ event: "compiled", gen, sourceDigest: digest, ok: false,
        diagnostics: [error instanceof ProjectError ? error.diagnostic
          : { code: "cli.compile.internal", message: `The compiler failed on this source: ${String(error)}` }] })
    }
  }
  compile()
  if (args.includes("--once")) return
  // One batch per burst of saves: editors write several events per save.
  let timer: ReturnType<typeof setTimeout> | undefined
  const changed = () => {
    if (timer) clearTimeout(timer)
    timer = setTimeout(compile, 50)
  }
  // A source that resolves outside the project is reported by each compile, never watched.
  let dirs: string[] = []
  try { dirs = sourceDirs(root, manifest) } catch { /* reported by compile() above */ }
  const watchers: FSWatcher[] = []
  for (const dir of dirs) {
    const watcher = watch(dir, { recursive: true }, changed)
    watcher.on("error", () => { /* watched dir disappeared; next compile reports it */ })
    watchers.push(watcher)
  }
  // The host holds our stdin open for its lifetime: end of input means it is gone.
  process.stdin.on("end", () => {
    if (timer) clearTimeout(timer)
    for (const watcher of watchers) watcher.close()
    process.exit(0)
  })
  process.stdin.resume()
}

if (import.meta.main) main(process.argv.slice(2))
