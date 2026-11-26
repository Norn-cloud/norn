/**
 * The Rust checker, through its WASM ABI (`crates/norn-checker-wasm`,
 * `norn-checker-abi/1`).
 *
 * The authoring toolchain never re-implements a check: it hands canonical
 * JSON to the same checker `cargo test` runs and reports its findings
 * verbatim. The npm package is built by
 * `crates/norn-checker-wasm/scripts/build-pkg.sh` (CI job
 * `norn-ts-authoring`) and located through `NORN_CHECKER_WASM_PKG`; it is not
 * a dependency in `package.json`.
 */
import { existsSync, readFileSync, statSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { TOOL_ROOT } from "./profile.ts"
import { cachedCheckerWasmDir } from "./runtime-cache.ts"

export interface CheckerDiagnostic {
  readonly code: string
  readonly severity: string
  readonly message: string
  readonly artifactId?: string
  readonly span?: unknown
  readonly source?: import("./builder/diagnostics.ts").SourceLocation
}

export interface CheckResult {
  readonly format: string
  readonly admitted: boolean
  readonly diagnostics: { readonly diagnostics: ReadonlyArray<CheckerDiagnostic> }
  readonly receipt: unknown | null
  readonly receiptSha256: string | null
}

/** `norn/diagnostic@2`: the checker's diagnostic with its registry tier and guidance. */
export interface DiagnosticV2 {
  readonly code: string
  readonly tier: string | null
  readonly severity: string
  readonly message: string
  readonly span: unknown
  readonly artifactId: string | null
  readonly remediation: unknown
  readonly format: "norn/diagnostic@2"
  /** The registry tier that produced the finding; the same value as `tier`. */
  readonly phase: string | null
  /** Whether repeating the same request can succeed. A definition finding never is. */
  readonly retryable: boolean
  /** Exactly one of `fix` and `no_fix` is a string. */
  readonly fix: string | null
  readonly no_fix: string | null
}

export interface ExplainResult {
  readonly format: "norn/explain@2"
  readonly admitted: boolean
  readonly report: unknown | null
  readonly diagnostics: ReadonlyArray<DiagnosticV2>
}

/** LAW-EXAMPLE-1: the checker's verdict of one law on one subject value. */
export type LawVerdict =
  | { readonly ok: true; readonly verdict: "not_applicable" | "holds" | "violated" }
  | { readonly ok: true; readonly verdict: "excepted"; readonly applied: ReadonlyArray<string> }
  | { readonly ok: false; readonly code: string; readonly error: string }

export interface CheckerAbi {
  /** `evaluateLaw(law, subject)` (LAW-EXAMPLE-1), absent from a package built before it existed. */
  readonly evaluateLaw?: (law: unknown, subject: unknown) => LawVerdict
  /** `checkArtifactSet(artifacts, context)`; the context defaults to an empty base layer. */
  readonly checkArtifactSet: (artifacts: ReadonlyArray<unknown>, context?: unknown) => CheckResult
  /** `explainArtifactSet` (#517), absent from a package built before it existed. */
  readonly explainArtifactSet?: (artifacts: ReadonlyArray<unknown>, context: unknown, kind: string, id: string, options: unknown) => ExplainResult
  readonly version: () => Readonly<Record<string, string>>
}

interface WasmModule {
  readonly initSync: (options: { module: Uint8Array }) => unknown
  readonly checkArtifactSet: (artifactsJson: string, contextJson: string) => string
  readonly evaluateLaw?: (lawJson: string, subjectJson: string) => string
  readonly explainArtifactSet?: (artifactsJson: string, contextJson: string, kind: string, id: string, optionsJson: string) => string
  readonly version: () => string
}

export const CHECKER_PKG_ENV = "NORN_CHECKER_WASM_PKG"

/** Where a released package carries its checker: built with it, next to `src`. */
export const BUNDLED_CHECKER = join(TOOL_ROOT, "checker")

function isRegularFile(path: string): boolean {
  try {
    return statSync(path).isFile()
  } catch {
    return false
  }
}

function hasCheckerPackage(dir: string): boolean {
  return isRegularFile(join(dir, "norn_checker_wasm.js")) && isRegularFile(join(dir, "norn_checker_wasm_bg.wasm"))
}

/**
 * The checker package directory, in strict priority order:
 * 1. `$NORN_CHECKER_WASM_PKG` override — wins whenever set, even when it names
 *    a directory without a package (that is an error to report, not a reason
 *    to use another checker silently);
 * 2. the package's bundled checker (`<package>/checker`), so a released CLI's
 *    matching checker is never silently replaced by a global cache;
 * 3. the cached prebuilt runtime (`~/.cache/norn/runtime/.../checker-wasm` or
 *    `$XDG_CACHE_HOME/norn/runtime/.../checker-wasm`, installed by `norn setup`).
 * Otherwise none.
 */
export function checkerPackageDir(env: Readonly<Record<string, string | undefined>> = process.env, bundled: string = BUNDLED_CHECKER): string | undefined {
  const override = env[CHECKER_PKG_ENV]
  if (override) return override
  if (hasCheckerPackage(bundled)) return bundled
  const cached = cachedCheckerWasmDir(env)
  if (cached && hasCheckerPackage(cached)) return cached
  return undefined
}

/**
 * WASM freshness: in a source checkout (this tool beside `crates/norn-checker-wasm`),
 * a checker found only in the local runtime cache was built from some release
 * or earlier checkout, not from this source, so it can refuse definitions this
 * source admits (an IR field or write mode it predates) and report false
 * failures. Returns why that checker is not trusted here, or `undefined` when
 * the checker is an explicit override, the bundled one, or outside a checkout.
 */
export function staleCheckerWarning(
  env: Readonly<Record<string, string | undefined>> = process.env,
  bundled: string = BUNDLED_CHECKER,
  sourceCrate: string = join(TOOL_ROOT, "..", "..", "crates", "norn-checker-wasm", "Cargo.toml"),
): string | undefined {
  const dir = checkerPackageDir(env, bundled)
  if (dir === undefined || env[CHECKER_PKG_ENV] || dir === bundled || !isRegularFile(sourceCrate)) return undefined
  return `norn: the cached checker at ${dir} was not built from this source checkout and may predate it; build crates/norn-checker-wasm (wasm-pack build --target web) and set ${CHECKER_PKG_ENV} to its package`
}

/** Load the checker package from `dir` (default: `checkerPackageDir()`); undefined when there is none. */
export async function loadChecker(dir: string | undefined = checkerPackageDir()): Promise<CheckerAbi | undefined> {
  if (!dir) return undefined
  const js = join(dir, "norn_checker_wasm.js")
  const wasm = join(dir, "norn_checker_wasm_bg.wasm")
  if (!existsSync(js) || !existsSync(wasm)) throw new Error(`${CHECKER_PKG_ENV}=${dir} holds no norn-checker-wasm package`)
  const abi = (await import(pathToFileURL(js).href)) as WasmModule
  abi.initSync({ module: readFileSync(wasm) })
  const explain = abi.explainArtifactSet?.bind(abi)
  const evaluateLaw = abi.evaluateLaw?.bind(abi)
  return {
    ...(evaluateLaw ? {
      evaluateLaw: (law: unknown, subject: unknown) => JSON.parse(evaluateLaw(JSON.stringify(law), JSON.stringify(subject))) as LawVerdict,
    } : {}),
    checkArtifactSet: (artifacts, context = null) =>
      JSON.parse(abi.checkArtifactSet(JSON.stringify(artifacts), JSON.stringify(context))) as CheckResult,
    ...(explain ? {
      explainArtifactSet: (artifacts: ReadonlyArray<unknown>, context: unknown, kind: string, id: string, options: unknown) =>
        JSON.parse(explain(JSON.stringify(artifacts), JSON.stringify(context ?? {}), kind, id, JSON.stringify(options))) as ExplainResult,
    } : {}),
    version: () => JSON.parse(abi.version()) as Record<string, string>,
  }
}

/** Map a checker JSON pointer to its most specific compiler-owned TS location. */
export function sourceMappedDiagnostics<D extends { readonly span?: unknown; readonly artifactId?: string | null }>(
  diagnostics: ReadonlyArray<D>,
  sourceMap: ReadonlyArray<import("./compiler/definitions.ts").DefinitionSource>,
): ReadonlyArray<D & { readonly source?: import("./builder/diagnostics.ts").SourceLocation }> {
  const byArtifact = new Map<string, Array<import("./compiler/definitions.ts").DefinitionSource>>()
  for (const entry of sourceMap) {
    let bucket = byArtifact.get(entry.artifactId)
    if (!bucket) { bucket = []; byArtifact.set(entry.artifactId, bucket) }
    bucket.push(entry)
  }
  for (const bucket of byArtifact.values()) bucket.sort((a, b) => b.pointer.length - a.pointer.length)
  return diagnostics.map((d) => {
    if (!d.artifactId) return d
    const bucket = byArtifact.get(d.artifactId)
    if (!bucket) return d
    const pointer = typeof d.span === "object" && d.span !== null && "pointer" in d.span ? String(d.span.pointer) : ""
    const match = bucket.find((s) => pointer === s.pointer || pointer.startsWith(`${s.pointer}/`))
    return match ? { ...d, source: match.at } : d
  })
}
