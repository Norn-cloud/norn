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
import { dirname, join, relative, resolve, sep } from "node:path"
import type { Json } from "./canonical.ts"
import type { CheckResult, CheckerAbi } from "./checker-abi.ts"
import { FLOW_SOURCE_MAP_DRAFT_FORMAT, FLOW_SOURCE_MAP_FORMAT, type FlowSourceMap, type FlowSourceMapDraft, type FlowSourceMapFlow } from "./flow-source-map.ts"
import type { CompiledExample } from "./examples.ts"
import {
  type DefinitionSource,
  type Diagnostic,
  PENDING_SOURCE_SHA256,
  type RetainedDefinitionDoc,
  runNativeCompilePackJson,
} from "./native-compiler.ts"
import { loadProfile } from "./profile.ts"

export interface PackOptions {
  /** Explicit module roots for single-file checks; packs otherwise use every top-level module. */
  readonly modules?: ReadonlyArray<string>
  /** Optional logical provenance `sourcePath` overrides keyed by resolved module path (for validated in-project symlinks). */
  readonly moduleSourcePaths?: ReadonlyMap<string, string>
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
  /** Promote `NORN-AUTHORING-UNSAFE-ZEN` from warning to hard error; defaults to true when `registry` is set or any pack/module path lies under a `registry/` directory segment. */
  readonly forbidUnsafeZen?: boolean
}

export interface PackFlow {
  readonly file: string
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
      /** LAW-EXAMPLE-1 cases of every module; `norn check` runs them. */
      readonly examples: ReadonlyArray<CompiledExample>
      /** Retained definitions (`Norn.approval`), installed beside the set. */
      readonly retained: ReadonlyArray<RetainedDefinitionDoc>
    }
  | { readonly ok: false; readonly failures: ReadonlyArray<PackModuleFailure>; readonly profile: string }

const posix = (path: string): string => path.split(sep).join("/")
const hasRegistrySegment = (path: string): boolean => /(?:^|\/)registry(?:\/|$)/.test(posix(path))

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

function commonDir(paths: ReadonlyArray<string>): string {
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

/** Compile a pack directory and gather its imports natively via `norn compile --pack-json`. */
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
  const absDir = resolve(dir)
  const isRegistryPath =
    registryPackagePath !== undefined ||
    hasRegistrySegment(absDir) ||
    modules.some((m) => hasRegistrySegment(m))
  const forbidUnsafeZen = options.forbidUnsafeZen ?? isRegistryPath
  const failures: PackModuleFailure[] = []
  const artifacts: Json[] = []
  const sources = new Map<string, string>()
  const flows: PackFlow[] = []
  const sourceMap: DefinitionSource[] = []
  const examples: CompiledExample[] = []
  const retained: RetainedDefinitionDoc[] = []
  const warnings: Array<Diagnostic & { readonly file: string }> = []
  let flowSourceMap: FlowSourceMapDraft | undefined

  const addImported = (artifact: Json, from: string): void => {
    const id = artifactId(artifact)
    if (sources.has(id)) {
      failures.push({ file: from, diagnostics: [{ code: "NORN-FLOW-LITERAL", start: { line: 1, column: 1 }, end: { line: 1, column: 1 }, construct: id, message: `artifact ${id} is also declared by ${sources.get(id)}` }] })
      return
    }
    sources.set(id, from)
    artifacts.push(artifact)
  }

  if (modules.length > 0) {
    const specs = modules.map((file) => ({
      path: file,
      scope: registryPackagePath ? {} : options.scope,
      sourcePath: options.moduleSourcePaths?.get(file) ?? posix(relative(root, file)),
      ...(registryPackagePath ? { registryPackagePath } : {}),
    }))
    const compiled = runNativeCompilePackJson({
      target: commonDir(modules),
      root,
      ...(registryPackagePath ? { registry: registryPackagePath } : { scope: options.scope }),
      forbidUnsafeZen,
      allowEmpty: false,
      modules: specs,
    })
    if (!compiled.ok) {
      failures.push(...compiled.failures)
    } else {
      for (const artifact of compiled.artifacts) {
        const id = artifactId(artifact)
        sources.set(id, compiled.sources[id] ?? "")
        artifacts.push(artifact)
      }
      sourceMap.push(...compiled.sourceMap)
      warnings.push(...compiled.warnings)
      examples.push(...compiled.examples)
      retained.push(...compiled.retained)
      for (const flow of compiled.flows) {
        flows.push({ file: flow.file, artifacts: flow.artifacts, flowSourceMap: flow.flowSourceMap })
      }
      flowSourceMap = compiled.flowSourceMap
    }
  }

  for (const path of options.imports ?? []) {
    for (const { file, artifact } of readArtifacts(path)) addImported(artifact, posix(relative(root, file)))
  }
  if (failures.length > 0) return { ok: false, failures, profile }
  const authoredIds = new Set(artifacts.map(artifactId))
  const registryBaseArtifacts = registryPath && existsSync(join(registryPath, "artifacts"))
    ? readArtifacts(join(registryPath, "artifacts")).map(({ artifact }) => artifact).filter((artifact) => !authoredIds.has(artifactId(artifact)))
    : []
  const baseArtifacts = [...registryBaseArtifacts, ...baseRegistryPaths.flatMap(packageArtifacts)]
  return { ok: true, artifacts, sources, sourceMap, warnings, flows, baseArtifacts, profile, examples, retained, ...(flowSourceMap ? { flowSourceMap } : {}) }
}

/** Check a compiled set with the Rust checker (the CheckReceipt hash comes from the checker). */
export function checkPack(
  checker: CheckerAbi,
  artifacts: ReadonlyArray<Json>,
  baseArtifacts: ReadonlyArray<Json> = [],
  flowSourceMap?: FlowSourceMap,
): CheckResult {
  const input = admissionInput(artifacts, baseArtifacts, flowSourceMap)
  return checker.checkArtifactSet(input.artifacts, input.context)
}

/**
 * Admit a compiled pack the way `norn check` does: check it, bind the flow
 * source map to that check's receipt, and when there is one check again with
 * it. Returns the final report and the exact input of that final check, which
 * is also the input `norn explain` gives the checker.
 */
export function admit(
  checker: CheckerAbi,
  pack: Extract<PackResult, { readonly ok: true }>,
): { readonly report: CheckResult; readonly input: ReturnType<typeof admissionInput> } {
  const first = checkPack(checker, pack.artifacts, pack.baseArtifacts)
  // MAPPING-SOURCE-REF-1: pins the checker hashed, then the set again.
  const artifacts = bindSourceSchemas(pack.artifacts, first.receipt)
  const checked = artifacts === pack.artifacts ? first : checkPack(checker, artifacts, pack.baseArtifacts)
  const flowSourceMap = pack.flowSourceMap ? finalizeFlowSourceMap(pack.flowSourceMap, pack.flows, checked.receipt) : undefined
  const input = admissionInput(artifacts, pack.baseArtifacts, flowSourceMap)
  return { report: flowSourceMap ? checker.checkArtifactSet(input.artifacts, input.context) : checked, input }
}

/** The candidate hashes a Rust check receipt issued, by `artifactId\0version`. */
function receiptRefs(receipt: unknown): Map<string, string> {
  const refs = new Map<string, string>()
  const set = typeof receipt === "object" && receipt !== null ? (receipt as { readonly artifactSet?: { readonly artifacts?: unknown } }).artifactSet : undefined
  for (const ref of Array.isArray(set?.artifacts) ? set.artifacts : []) {
    const { artifactId, version, sha256 } = (ref ?? {}) as { readonly artifactId?: unknown; readonly version?: unknown; readonly sha256?: unknown }
    if (typeof artifactId === "string" && typeof version === "string" && typeof sha256 === "string") refs.set(`${artifactId}\u0000${version}`, sha256)
  }
  return refs
}

function pending(artifact: Json): boolean {
  const value = artifact as { readonly kind?: unknown; readonly payload?: { readonly source?: { readonly sha256?: unknown } } }
  return value.kind === "mapping" && value.payload?.source?.sha256 === PENDING_SOURCE_SHA256
}

/** Whether a mapping of `artifacts` still waits for its source schema's hash. */
export function pendingSourceSchemas(artifacts: ReadonlyArray<Json>): boolean {
  return artifacts.some(pending)
}

/**
 * MAPPING-SOURCE-REF-1: each mapping bound by reference to a source schema of
 * the same set gets the hash the Rust checker computed for that schema (its
 * receipt's candidate ref). Nothing here hashes: a schema the receipt does not
 * name leaves the pending pin, which the checker refuses. Returns `artifacts`
 * itself when nothing was pending.
 */
export function bindSourceSchemas(artifacts: ReadonlyArray<Json>, receipt: unknown): ReadonlyArray<Json> {
  if (!artifacts.some(pending)) return artifacts
  const refs = receiptRefs(receipt)
  return artifacts.map((artifact) => {
    if (!pending(artifact)) return artifact
    const value = artifact as { readonly payload: { readonly source: { readonly schemaId: string; readonly version: string } } & Record<string, unknown> } & Record<string, unknown>
    const sha256 = refs.get(`${value.payload.source.schemaId}\u0000${value.payload.source.version}`)
    return sha256 === undefined ? artifact : ({ ...value, payload: { ...value.payload, source: { ...value.payload.source, sha256 } } } as unknown as Json)
  })
}

/**
 * What the checker is given for a compiled pack: the candidates, and the
 * context that carries the installed base (and the flow source map). `norn
 * check` and `norn explain` both build their input here, so a base artifact is
 * never a candidate in one and installed in the other.
 */
export function admissionInput(
  artifacts: ReadonlyArray<Json>,
  baseArtifacts: ReadonlyArray<Json> = [],
  flowSourceMap?: FlowSourceMap,
): { readonly artifacts: ReadonlyArray<Json>; readonly context: Readonly<Record<string, unknown>> | null } {
  const context = {
    ...(baseArtifacts.length > 0 ? { base: baseArtifacts } : {}),
    ...(flowSourceMap ? { flowSourceMap } : {}),
  }
  return { artifacts, context: Object.keys(context).length > 0 ? context : null }
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
