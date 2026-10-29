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

export interface CheckerAbi {
  /** `checkArtifactSet(artifacts, context)`; the context defaults to an empty base layer. */
  readonly checkArtifactSet: (artifacts: ReadonlyArray<unknown>, context?: unknown) => CheckResult
  readonly version: () => Readonly<Record<string, string>>
}

interface WasmModule {
  readonly initSync: (options: { module: Uint8Array }) => unknown
  readonly checkArtifactSet: (artifactsJson: string, contextJson: string) => string
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
  return {
    checkArtifactSet: (artifacts, context = null) =>
      JSON.parse(abi.checkArtifactSet(JSON.stringify(artifacts), JSON.stringify(context))) as CheckResult,
    version: () => JSON.parse(abi.version()) as Record<string, string>,
  }
}

/** Map a checker JSON pointer to its most specific compiler-owned TS location. */
export function sourceMappedDiagnostics(
  diagnostics: ReadonlyArray<CheckerDiagnostic>,
  sourceMap: ReadonlyArray<import("./compiler/definitions.ts").DefinitionSource>,
): ReadonlyArray<CheckerDiagnostic & { readonly source?: import("./builder/diagnostics.ts").SourceLocation }> {
  return diagnostics.map((d) => {
    const pointer = typeof d.span === "object" && d.span !== null && "pointer" in d.span ? String(d.span.pointer) : ""
    const match = sourceMap.filter((s) => s.artifactId === d.artifactId && (pointer === s.pointer || pointer.startsWith(s.pointer + "/")))
      .sort((a, b) => b.pointer.length - a.pointer.length)[0]
    return match ? { ...d, source: match.at } : d
  })
}
