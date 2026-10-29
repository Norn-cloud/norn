/**
 * A definition pack (T7-TS-4, #2692): every top-level `*.ts` module of a
 * directory compiled by the syntax compiler (ontology definitions, and
 * `Norn.flow` lowered to its Run type and decide rule), plus imported JSON IR artifacts
 * (such as a source schema imported from a source system) taken verbatim,
 * checked as one set by the Rust checker through its WASM ABI.
 *
 * Nothing here executes authored code: modules are compiled from syntax
 * (`src/compiler/definitions.ts`), and admission is the checker's.
 */
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs"
import { join, relative, resolve, sep } from "node:path"
import type { Diagnostic } from "./check.ts"
import type { Json } from "./canonical.ts"
import type { CheckResult, CheckerAbi } from "./checker-abi.ts"
import { FLOW_SOURCE_MAP_DRAFT_FORMAT, FLOW_SOURCE_MAP_FORMAT, type FlowSourceMap, type FlowSourceMapDraft, type FlowSourceMapFlow } from "./flow-source-map.ts"
import { type DefinitionSource, compileDefinitionFiles } from "./compiler/definitions.ts"
import { compileFile } from "./compiler/index.ts"
import type { FlowDef } from "./builder/flow.ts"
import { TOOL_ROOT, loadProfile } from "./profile.ts"

export interface PackOptions {
  /** Explicit module roots for single-file checks; packs otherwise use every top-level module. */
  readonly modules?: ReadonlyArray<string>
  /** Envelope scope of every compiled artifact (`orgId`, `environmentId`, …). */
  readonly scope: Readonly<Record<string, string>>
  /** Registry package path enables active/system envelopes and is used as the checker's read-only base. */
  readonly registry?: string
  /** Read-only bundled package paths for scoped candidates; this does not change their envelope scope. */
  readonly baseRegistries?: ReadonlyArray<string>
  /** Imported JSON IR artifacts (files, or directories of `*.json`) added to the set verbatim. */
  readonly imports?: ReadonlyArray<string>
  /** Provenance paths are written relative to this directory (default: the working directory). */
  readonly root?: string
}

export interface PackFlow {
  readonly file: string
  readonly definition: FlowDef
  readonly artifacts: ReadonlyArray<Json>
  readonly flowSourceMap: FlowSourceMapFlow
}

export interface PackModuleFailure {
  readonly file: string
  readonly diagnostics: ReadonlyArray<Diagnostic>
}

export type PackResult =
  | {
      readonly ok: true
      /** Compiled artifacts (module order, then declaration order), then imported ones. */
      readonly artifacts: ReadonlyArray<Json>
      /** artifactId → the module (or imported file) it came from. */
      readonly sources: ReadonlyMap<string, string>
      readonly sourceMap: ReadonlyArray<DefinitionSource>
      readonly warnings: ReadonlyArray<Diagnostic & { readonly file: string }>
      /** The compiled flows: definition and lowered artifacts (for `norn plan`'s flow limits). */
      readonly flows: ReadonlyArray<PackFlow>
      /** Compiler graph drafts; hashes are added only from the Rust check receipt. */
      readonly flowSourceMap?: FlowSourceMapDraft
      /** Registry package artifacts, excluding ids authored by this pack. */
      readonly baseArtifacts: ReadonlyArray<Json>
      readonly profile: string
    }
  | { readonly ok: false; readonly failures: ReadonlyArray<PackModuleFailure>; readonly profile: string }

const posix = (path: string): string => path.split(sep).join("/")

/** The pack's modules: top-level `*.ts` files (not `*.d.ts`), sorted; subdirectories hold shared support modules. */
export function packModules(dir: string): string[] {
  return readdirSync(dir)
    .filter((f) => f.endsWith(".ts") && !f.endsWith(".d.ts"))
    .sort()
    .map((f) => join(dir, f))
}

/** JSON documents that are artifact envelopes (other JSON, such as `principals.json`, is not IR). */
export function readArtifacts(path: string): Array<{ readonly file: string; readonly artifact: Json }> {
  const files = statSync(path).isDirectory()
    ? readdirSync(path).filter((f) => f.endsWith(".json")).sort().map((f) => join(path, f))
    : [path]
  const out: Array<{ file: string; artifact: Json }> = []
  for (const file of files) {
    const value = JSON.parse(readFileSync(file, "utf8")) as unknown
    if (isArtifact(value)) out.push({ file, artifact: value as Json })
  }
  return out
}

export function isArtifact(value: unknown): value is { readonly artifactId: string; readonly kind: string } {
  return typeof value === "object" && value !== null && !Array.isArray(value) && typeof (value as { artifactId?: unknown }).artifactId === "string" && typeof (value as { irVersion?: unknown }).irVersion === "string"
}

export function artifactId(artifact: Json): string {
  if (!isArtifact(artifact)) throw new Error("not an artifact envelope")
  return artifact.artifactId
}

function packageArtifacts(packagePath: string): Json[] {
  const manifestPath = join(packagePath, "norn-package.json")
  if (!existsSync(manifestPath)) throw new Error(`base registry package has no norn-package.json: ${packagePath}`)
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as {
    readonly format?: unknown
    readonly packageId?: unknown
    readonly version?: unknown
    readonly artifacts?: unknown
  }
  if (manifest.format !== "norn.registry.package/1" || typeof manifest.packageId !== "string" || typeof manifest.version !== "string" || !Array.isArray(manifest.artifacts)) {
    throw new Error(`base registry package manifest is invalid: ${manifestPath}`)
  }
  return manifest.artifacts.map((entry) => {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) throw new Error(`base registry package has an invalid artifact entry: ${manifestPath}`)
    const { path, artifactId: expectedId, version: expectedVersion } = entry as { readonly path?: unknown; readonly artifactId?: unknown; readonly version?: unknown }
    if (typeof path !== "string" || typeof expectedId !== "string" || typeof expectedVersion !== "string") throw new Error(`base registry package has an invalid artifact entry: ${manifestPath}`)
    const artifactPath = resolve(packagePath, path)
    const artifactRelative = relative(packagePath, artifactPath)
    if (!artifactRelative || artifactRelative === ".." || artifactRelative.startsWith(`..${sep}`)) throw new Error(`base registry artifact path escapes package: ${path}`)
    const documents = readArtifacts(artifactPath)
    if (documents.length !== 1) throw new Error(`base registry artifact is missing or invalid: ${artifactPath}`)
    const artifact = documents[0]!.artifact as unknown as { readonly artifactId?: unknown; readonly version?: unknown }
    if (artifact.artifactId !== expectedId || artifact.version !== expectedVersion) throw new Error(`base registry artifact does not match its package manifest: ${artifactPath}`)
    return documents[0]!.artifact
  })
}

/** Compile a pack directory and gather its imports. */
export function compilePack(dir: string, options: PackOptions): PackResult {
  const profile = loadProfile().profile
  const root = resolve(options.root ?? process.cwd())
  const registryPath = options.registry === undefined ? undefined : resolve(root, options.registry)
  const registryPackagePath = registryPath ? posix(relative(root, registryPath)) : undefined
  if (options.registry !== undefined && (!registryPackagePath || registryPackagePath === "." || registryPackagePath.startsWith("../") || registryPackagePath === "..")) {
    throw new RangeError("registry package path must be a non-root path inside the provenance root")
  }
  const baseRegistryPaths = (options.baseRegistries ?? []).map((registry) => resolve(root, registry))
  for (const path of baseRegistryPaths) {
    const packagePath = posix(relative(root, path))
    if (!packagePath || packagePath === "." || packagePath.startsWith("../") || packagePath === "..") {
      throw new RangeError("base registry package path must be a non-root path inside the provenance root")
    }
  }
  const modules = options.modules ? options.modules.map((file) => resolve(file)) : packModules(dir)
  const envelope = (path: string) => ({
    scope: registryPackagePath ? {} : options.scope,
    sourcePath: posix(relative(root, path)),
    ...(registryPackagePath ? { registryPackagePath } : {}),
  })
  const results = compileDefinitionFiles(modules, envelope, {}, { allowEmpty: true })
  const failures: PackModuleFailure[] = []
  const artifacts: Json[] = []
  const sources = new Map<string, string>()
  const flows: PackFlow[] = []
  const sourceMap: DefinitionSource[] = []
  const warnings: Array<Diagnostic & { readonly file: string }> = []
  const add = (artifact: Json, from: string): void => {
    const id = artifactId(artifact)
    if (sources.has(id)) {
      failures.push({ file: from, diagnostics: [{ code: "NORN-FLOW-LITERAL", start: { line: 1, column: 1 }, end: { line: 1, column: 1 }, construct: id, message: `artifact ${id} is also declared by ${sources.get(id)}` }] })
      return
    }
    sources.set(id, from)
    artifacts.push(artifact)
  }
  results.forEach((result, i) => {
    const path = modules[i]!, file = posix(relative(root, path))
    if (!result.ok) { failures.push({ file, diagnostics: result.diagnostics }); return }
    sourceMap.push(...result.sourceMap.map((s) => ({ ...s, at: { ...s.at, file: posix(relative(root, resolve(TOOL_ROOT, s.at.file))) } })))
    warnings.push(...result.warnings.map((warning) => ({ ...warning, file })))
    for (const artifact of result.artifacts) add(artifact, file)
    if (result.flows > 0) {
      // A flow lowers through the syntax compiler to its Run type and decide rule (T7-EFX-2).
      const flow = compileFile(path, { ...envelope(path), sourceRoot: root })
      if (!flow.ok) { failures.push({ file, diagnostics: flow.diagnostics }); return }
      warnings.push(...flow.warnings.map((warning) => ({ ...warning, file })))
      for (const artifact of flow.artifacts) add(artifact, file)
      const flowSourceMap = flow.flowSourceMap?.flows[0]
      if (!flowSourceMap) {
        failures.push({ file, diagnostics: [{ code: "NORN-FLOW-RESULT", start: { line: 1, column: 1 }, end: { line: 1, column: 1 }, construct: file, message: "the flow compiler produced no authoring source graph" }] })
        return
      }
      flows.push({ file, definition: flow.definition, artifacts: flow.artifacts, flowSourceMap })
    } else if (result.definitions.length === 0) {
      failures.push({ file, diagnostics: [{ code: "NORN-FLOW-RESULT", start: { line: 1, column: 1 }, end: { line: 1, column: 1 }, construct: file, message: "a pack module declares no definition and no Norn.flow; move shared code under a subdirectory" }] })
    }
    return
  })
  for (const path of options.imports ?? []) {
    for (const { file, artifact } of readArtifacts(path)) add(artifact, posix(relative(root, file)))
  }
  if (failures.length > 0) return { ok: false, failures, profile }
  const authoredIds = new Set(artifacts.map(artifactId))
  const registryBaseArtifacts = registryPath && existsSync(join(registryPath, "artifacts"))
    ? readArtifacts(join(registryPath, "artifacts")).map(({ artifact }) => artifact).filter((artifact) => !authoredIds.has(artifactId(artifact)))
    : []
  const baseArtifacts = [...registryBaseArtifacts, ...baseRegistryPaths.flatMap(packageArtifacts)]
  const flowSourceMap = flows.length > 0
    ? { format: FLOW_SOURCE_MAP_DRAFT_FORMAT, flows: flows.map((flow) => flow.flowSourceMap) }
    : undefined
  return { ok: true, artifacts, sources, sourceMap, warnings, flows, baseArtifacts, profile, ...(flowSourceMap ? { flowSourceMap } : {}) }
}

/** Check a compiled set with the Rust checker (the CheckReceipt hash comes from the checker). */
export function checkPack(
  checker: CheckerAbi,
  artifacts: ReadonlyArray<Json>,
  baseArtifacts: ReadonlyArray<Json> = [],
  flowSourceMap?: FlowSourceMap,
): CheckResult {
  const context = {
    ...(baseArtifacts.length > 0 ? { base: baseArtifacts } : {}),
    ...(flowSourceMap ? { flowSourceMap } : {}),
  }
  return checker.checkArtifactSet(artifacts, Object.keys(context).length > 0 ? context : null)
}

/** Bind draft flow projections only to hash refs the Rust checker already issued. */
export function finalizeFlowSourceMap(
  draft: FlowSourceMapDraft,
  flows: ReadonlyArray<PackFlow>,
  receipt: unknown,
): FlowSourceMap {
  if (draft.format !== FLOW_SOURCE_MAP_DRAFT_FORMAT) throw new Error("flow source map is not an unbound compiler draft")
  const artifactSet = typeof receipt === "object" && receipt !== null && !Array.isArray(receipt)
    ? (receipt as { readonly artifactSet?: unknown }).artifactSet
    : undefined
  const candidates = typeof artifactSet === "object" && artifactSet !== null && !Array.isArray(artifactSet)
    ? (artifactSet as { readonly artifacts?: unknown }).artifacts
    : undefined
  if (!Array.isArray(candidates)) throw new Error("Rust check receipt does not contain candidate artifact hashes")
  const refs = new Map<string, { artifactId: string; version: string; sha256: string }>()
  for (const candidate of candidates) {
    if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) throw new Error("Rust check receipt contains an invalid candidate artifact hash")
    const ref = candidate as { readonly artifactId?: unknown; readonly version?: unknown; readonly sha256?: unknown }
    if (typeof ref.artifactId !== "string" || typeof ref.version !== "string" || typeof ref.sha256 !== "string") throw new Error("Rust check receipt contains an invalid candidate artifact hash")
    const key = `${ref.artifactId}\u0000${ref.version}`
    if (refs.has(key)) throw new Error("Rust check receipt repeats a candidate artifact hash")
    refs.set(key, { artifactId: ref.artifactId, version: ref.version, sha256: ref.sha256 })
  }
  const flowById = new Map(flows.map((flow) => [`${flow.flowSourceMap.flowId}\u0000${flow.flowSourceMap.flowVersion}`, flow]))
  const boundFlows = draft.flows.map((flow) => {
    const packFlow = flowById.get(`${flow.flowId}\u0000${flow.flowVersion}`)
    if (!packFlow) throw new Error(`flow source map has no compiled flow artifacts for ${flow.flowId}@${flow.flowVersion}`)
    const artifactIds = [...new Set(packFlow.artifacts.map(artifactId))].sort()
    const artifactRefs = artifactIds.map((artifactId) => {
      const version = flow.flowVersion
      const ref = refs.get(`${artifactId}\u0000${version}`)
      if (!ref) throw new Error(`Rust check receipt has no candidate hash for flow artifact ${artifactId}@${version}`)
      return ref
    })
    const bound = new Set(artifactRefs.map((ref) => ref.artifactId))
    for (const step of flow.steps) {
      for (const artifactId of step.artifactIds) {
        if (!bound.has(artifactId)) throw new Error(`flow source map step ${step.id} refers to artifact outside ${flow.flowId}@${flow.flowVersion}`)
      }
    }
    return { ...flow, artifactRefs }
  })
  return { format: FLOW_SOURCE_MAP_FORMAT, flows: boundFlows }
}
