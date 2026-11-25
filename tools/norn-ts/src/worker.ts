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
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync, statSync, watch } from "node:fs"
import { join, relative, resolve, sep } from "node:path"
import type { Json } from "./canonical.ts"
import type { RetainedDefinitionDoc } from "./compiler/definitions.ts"
import { artifactId, compilePack, packModules, readArtifacts, type PackResult } from "./pack.ts"
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

/** Whether `path`, with every symlink resolved, is the project root or inside it. */
function insideProject(realRoot: string, path: string): boolean {
  let real: string
  try { real = realpathSync(path) }
  catch { return false } // a dangling link resolves nowhere, so not inside the project
  return real === realRoot || real.startsWith(realRoot + sep)
}

function outside(what: string): ProjectError {
  return new ProjectError("cli.project.invalid_manifest", `${what} does not resolve inside the project; sources must stay inside it.`)
}

/** The manifest's source directories that exist; one that resolves outside the project refuses. */
function sourceDirs(root: string, manifest: Project): string[] {
  const realRoot = realpathSync(root)
  return manifest.sources.map((source) => resolve(root, source)).filter((dir) => {
    if (!existsSync(dir)) return false
    if (!insideProject(realRoot, dir)) throw outside(`source ${posix(relative(root, dir))}`)
    return statSync(dir).isDirectory()
  })
}

/**
 * The modules compiled as one set: each source directory's top-level `*.ts`.
 * A module that is a symlink is compiled only if it resolves inside the project.
 */
export function projectModules(root: string, manifest: Project): string[] {
  const realRoot = realpathSync(root)
  return sourceDirs(root, manifest).flatMap((dir) => packModules(dir)).map((module) => {
    if (!insideProject(realRoot, module)) throw outside(`module ${posix(relative(root, module))}`)
    return module
  })
}

function sourceFiles(root: string, manifest: Project): string[] {
  const files = new Set<string>(projectModules(root, manifest))
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
      else if (kind.isFile() && (entry.endsWith(".ts") || entry.endsWith(".json"))) files.add(path)
    }
  }
  for (const dir of sourceDirs(root, manifest)) walk(dir)
  return [...files]
}

/**
 * SHA-256 over the sorted (project-relative path, bytes) pairs of every `*.ts`
 * file under `sources` and of every compiled module, a symlinked one included
 * (its bytes are its target's).
 */
export function sourceDigest(root: string, manifest: Project): string {
  const hash = createHash("sha256")
  for (const file of sourceFiles(root, manifest).map((path) => posix(relative(root, path))).sort()) {
    const bytes = readFileSync(resolve(root, file))
    hash.update(`${file}\0${bytes.length}\0`).update(bytes)
  }
  return hash.digest("hex")
}

/**
 * Pre-compiled artifact envelopes (`*.json`) at the top level of each source
 * directory: generated registry artifacts a project carries as files, for
 * example the three-systems packs, whose TypeScript compiles only as the
 * registry release. The same checker admits them with the compiled modules;
 * nothing here trusts them.
 */
export function projectArtifacts(root: string, manifest: Project): unknown[] {
  const realRoot = realpathSync(root)
  return sourceDirs(root, manifest).flatMap((dir) => readArtifacts(dir)).map(({ file, artifact }) => {
    if (!insideProject(realRoot, file)) throw outside(`artifact ${posix(relative(root, file))}`)
    return artifact
  })
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
export class DefinitionSourceError extends Error {
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
 * Retained definitions (`{ kind, id, payload }`, kind `approval`, `question`
 * or `dashboard`) carried as
 * JSON files at the top level of each source directory, read exactly as
 * `projectArtifacts` reads artifact envelopes. A document whose `kind` is a
 * retained kind but whose `id` or `payload` is malformed, or whose `id`
 * another JSON source declares, throws `DefinitionSourceError`: a gate the
 * author declared is never silently dropped. Other JSON documents are not
 * definitions and are skipped.
 */
export function projectDefinitions(root: string, manifest: Project): SourceDefinitionDoc[] {
  const realRoot = realpathSync(root)
  const seen = new Set<string>()
  return sourceDirs(root, manifest).flatMap((dir) => readdirSync(dir).filter((f) => f.endsWith(".json")).sort().map((f) => join(dir, f))).flatMap((file) => {
    const path = posix(relative(root, file))
    if (!insideProject(realRoot, file)) throw outside(`definition ${path}`)
    const inexact: string[] = []
    const value = JSON.parse(readFileSync(file, "utf8"), exactNumbers(inexact)) as { readonly kind?: unknown; readonly id?: unknown; readonly payload?: unknown }
    const named = typeof value === "object" && value !== null && !Array.isArray(value) && typeof value.kind === "string" && RETAINED_KINDS.has(value.kind)
    if (!named) return []
    // #1469 S10: a number is checked as written, before any rounding.
    if (inexact.length > 0) {
      throw new DefinitionSourceError(path, `${path} writes the number ${inexact[0]}, which JSON cannot carry exactly: write an integer such as 1000, or the exact decimal as text, such as "1000.10"`)
    }
    if (typeof value.id !== "string" || value.id === "" || typeof value.payload !== "object" || value.payload === null || Array.isArray(value.payload)) {
      throw new DefinitionSourceError(path, `${path} declares ${value.kind === "approval" ? "an" : "a"} ${String(value.kind)} that is not { kind, id, payload } with a non-empty id and an object payload`)
    }
    if (seen.has(value.id)) throw new DefinitionSourceError(value.id, `definition ${value.id} is declared by more than one JSON source`)
    seen.add(value.id)
    return [{ kind: value.kind as SourceDefinitionDoc["kind"], id: value.id, payload: value.payload as Json }]
  })
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
  const pack = compilePack(root, {
    modules: projectModules(root, manifest),
    scope: { orgId: manifest.scope.orgId, environmentId: manifest.scope.environmentId },
    root,
    ...(options.forbidUnsafeZen !== undefined ? { forbidUnsafeZen: options.forbidUnsafeZen } : {}),
  })
  if (!pack.ok) return pack
  const files = projectArtifacts(root, manifest) as Json[]
  const owned = cellOwned([...files, ...pack.artifacts])
  const sources = new Map(pack.sources)
  const refused = (construct: string, message: string): PackResult =>
    ({ ok: false, profile: pack.profile, failures: [{ file: ".", diagnostics: [{ code: "NORN-FLOW-LITERAL", start: { line: 1, column: 1 }, end: { line: 1, column: 1 }, construct, message }] }] })
  for (const artifact of files) {
    const id = artifactId(artifact)
    if (sources.has(id)) return refused(id, `artifact ${id} is declared by a JSON source and by ${sources.get(id)}`)
    sources.set(id, "json")
  }
  for (const artifact of owned) sources.set(artifactId(artifact), "registry/core")
  let definitions: SourceDefinitionDoc[]
  try { definitions = projectDefinitions(root, manifest) }
  catch (error) {
    if (error instanceof DefinitionSourceError) return refused(error.construct, error.message)
    throw error
  }
  const ids = new Set(pack.retained.map((definition) => definition.id))
  const duplicate = definitions.find((definition) => ids.has(definition.id))
  if (duplicate) return refused(duplicate.id, `definition ${duplicate.id} is declared by a JSON source and by a module`)
  // Approvals ride the set wherever it is applied; questions and dashboards are
  // the dev host's to install once it serves (`readDefinitions`).
  const approvals = definitions.filter((definition): definition is RetainedDefinitionDoc => definition.kind === "approval")
  return { ...pack, artifacts: [...owned, ...files, ...pack.artifacts], sources, retained: [...pack.retained, ...approvals] }
}

/**
 * The sources' questions and dashboards (DEV-READ-DEFINITIONS-1): the dev
 * host installs them once a revision serves. `compileProjectPack` has already
 * refused a malformed or duplicated one.
 */
function readDefinitions(root: string, manifest: Project): SourceDefinitionDoc[] {
  return projectDefinitions(root, manifest).filter((definition) => definition.kind !== "approval")
}

/** Compile every top-level module of every source directory as one set. */
export function compileProject(root: string, manifest: Project, gen: number): CompiledEvent {
  const digest = sourceDigest(root, manifest)
  const pack = compileProjectPack(root, manifest)
  if (!pack.ok) {
    const diagnostics = pack.failures.flatMap((failure) => failure.diagnostics.map((diagnostic) => ({
      ...diagnostic,
      source: { file: failure.file, line: diagnostic.start.line, column: diagnostic.start.column },
    })))
    return { event: "compiled", gen, sourceDigest: digest, ok: false, diagnostics }
  }
  return {
    event: "compiled", gen, sourceDigest: digest, ok: true,
    artifacts: [...pack.baseArtifacts, ...pack.artifacts], definitions: [...pack.retained, ...readDefinitions(root, manifest)], sourceMap: pack.sourceMap, warnings: pack.warnings,
  }
}

function emit(event: unknown): void {
  process.stdout.write(`${JSON.stringify(event)}\n`)
}

function option(args: readonly string[], name: string): string | undefined {
  const index = args.indexOf(name)
  return index < 0 ? undefined : args[index + 1]
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
  // The host holds our stdin open for its lifetime: end of input means it is gone.
  process.stdin.on("end", () => process.exit(0))
  process.stdin.resume()
  // One batch per burst of saves: editors write several events per save.
  let timer: ReturnType<typeof setTimeout> | undefined
  const changed = () => {
    if (timer) clearTimeout(timer)
    timer = setTimeout(compile, 50)
  }
  // A source that resolves outside the project is reported by each compile, never watched.
  let dirs: string[] = []
  try { dirs = sourceDirs(root, manifest) } catch { /* reported by compile() above */ }
  for (const dir of dirs) watch(dir, { recursive: true }, changed)
}

if (import.meta.main) main(process.argv.slice(2))
