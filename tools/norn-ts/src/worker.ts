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
 * `ok: true` carries `artifacts`, `sourceMap` and `warnings`; `ok: false`
 * carries `diagnostics`, each with a project-relative `source`. `gen` grows
 * with every file-system batch; `sourceDigest` is the SHA-256 of the sorted
 * (project-relative path, bytes) pairs under `sources`, so a caller can tell
 * which bytes a result belongs to. A setup failure prints `{ event: "fatal" }`.
 */
import { createHash } from "node:crypto"
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync, statSync, watch } from "node:fs"
import { join, relative, resolve, sep } from "node:path"
import { compilePack, packModules } from "./pack.ts"
import { ProjectError, readProject, type Project } from "./project.ts"

export interface CompiledEvent {
  readonly event: "compiled"
  readonly gen: number
  readonly sourceDigest: string
  readonly ok: boolean
  readonly artifacts?: ReadonlyArray<unknown>
  readonly sourceMap?: ReadonlyArray<unknown>
  readonly warnings?: ReadonlyArray<unknown>
  readonly diagnostics?: ReadonlyArray<unknown>
}

const posix = (path: string) => path.split(sep).join("/")

/** Whether `path`, with every symlink resolved, is the project root or inside it. */
function insideProject(realRoot: string, path: string): boolean {
  const real = realpathSync(path)
  return real === realRoot || real.startsWith(realRoot + sep)
}

function outside(what: string): ProjectError {
  return new ProjectError("cli.project.invalid_manifest", `${what} resolves outside the project; sources must stay inside it.`)
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
function projectModules(root: string, manifest: Project): string[] {
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
      else if (kind.isFile() && entry.endsWith(".ts")) files.add(path)
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

/** Compile every top-level module of every source directory as one set. */
export function compileProject(root: string, manifest: Project, gen: number): CompiledEvent {
  const digest = sourceDigest(root, manifest)
  const modules = projectModules(root, manifest)
  const pack = compilePack(root, { modules, scope: { orgId: manifest.scope.orgId, environmentId: manifest.scope.environmentId }, root })
  if (!pack.ok) {
    const diagnostics = pack.failures.flatMap((failure) => failure.diagnostics.map((diagnostic) => ({
      ...diagnostic,
      source: { file: failure.file, line: diagnostic.start.line, column: diagnostic.start.column },
    })))
    return { event: "compiled", gen, sourceDigest: digest, ok: false, diagnostics }
  }
  return {
    event: "compiled", gen, sourceDigest: digest, ok: true,
    artifacts: [...pack.baseArtifacts, ...pack.artifacts], sourceMap: pack.sourceMap, warnings: pack.warnings,
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
