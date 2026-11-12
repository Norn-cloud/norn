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
import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { pathToFileURL } from "node:url"

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

export interface CheckerAbi {
  /** `checkArtifactSet(artifacts, context)`; the context defaults to an empty base layer. */
  readonly checkArtifactSet: (artifacts: ReadonlyArray<unknown>, context?: unknown) => CheckResult
  /** `explainArtifactSet` (#517), absent from a package built before it existed. */
  readonly explainArtifactSet?: (artifacts: ReadonlyArray<unknown>, context: unknown, kind: string, id: string, options: unknown) => ExplainResult
  readonly version: () => Readonly<Record<string, string>>
}

interface WasmModule {
  readonly initSync: (options: { module: Uint8Array }) => unknown
  readonly checkArtifactSet: (artifactsJson: string, contextJson: string) => string
  readonly explainArtifactSet?: (artifactsJson: string, contextJson: string, kind: string, id: string, optionsJson: string) => string
  readonly version: () => string
}

export const CHECKER_PKG_ENV = "NORN_CHECKER_WASM_PKG"

/** Load the checker package from `dir` (default `$NORN_CHECKER_WASM_PKG`); undefined when absent. */
export async function loadChecker(dir: string | undefined = process.env[CHECKER_PKG_ENV]): Promise<CheckerAbi | undefined> {
  if (!dir) return undefined
  const js = join(dir, "norn_checker_wasm.js")
  const wasm = join(dir, "norn_checker_wasm_bg.wasm")
  if (!existsSync(js) || !existsSync(wasm)) throw new Error(`${CHECKER_PKG_ENV}=${dir} holds no norn-checker-wasm package`)
  const abi = (await import(pathToFileURL(js).href)) as WasmModule
  abi.initSync({ module: readFileSync(wasm) })
  const explain = abi.explainArtifactSet?.bind(abi)
  return {
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
  return diagnostics.map((d) => {
    const pointer = typeof d.span === "object" && d.span !== null && "pointer" in d.span ? String(d.span.pointer) : ""
    const match = sourceMap.filter((s) => s.artifactId === d.artifactId && (pointer === s.pointer || pointer.startsWith(s.pointer + "/")))
      .sort((a, b) => b.pointer.length - a.pointer.length)[0]
    return match ? { ...d, source: match.at } : d
  })
}
