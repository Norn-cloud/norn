/**
 * The one diagnostic shape the CLI prints for its own `cli.*` codes:
 * `norn/diagnostic@2`, the projection the checker makes of its findings
 * (`Diagnostic::to_json_v2`). The registry is the authority for a code's tier,
 * severity and guidance; `generated/cli-diagnostics.json` is its `cli.*` part, kept
 * identical to `crates/norn-checker/conformance` by `cli-code-registry.test.ts`,
 * so the CLI can say what to do even when the checker package cannot load.
 *
 * A code that is not registered cannot be printed: `cliDiagnostic` throws.
 */
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { SourceLocation } from "./builder/diagnostics.ts"
import type { DiagnosticV2 } from "./checker-abi.ts"
import { TOOL_ROOT } from "./profile.ts"
import type { ProjectError } from "./project.ts"

export interface RegisteredCliCode {
  readonly code: string
  readonly tier: string
  readonly severity: string
  readonly message: string
  readonly fix: string | null
  readonly no_fix: string | null
}

let registry: ReadonlyMap<string, RegisteredCliCode> | undefined

/** The registered `cli.*` codes, by code. */
export function cliCodes(): ReadonlyMap<string, RegisteredCliCode> {
  if (!registry) {
    const snapshot = JSON.parse(readFileSync(join(TOOL_ROOT, "generated/cli-diagnostics.json"), "utf8")) as { codes: RegisteredCliCode[] }
    registry = new Map(snapshot.codes.map((entry) => [entry.code, entry]))
  }
  return registry
}

/** A CLI refusal as `norn/diagnostic@2`, with the registry's guidance for its code. */
export function cliDiagnostic(error: ProjectError, source?: SourceLocation): DiagnosticV2 & { readonly source?: SourceLocation } {
  const entry = cliCodes().get(error.code)
  if (!entry) throw new Error(`${error.code} is not a registered CLI diagnostic code`)
  return {
    code: entry.code, tier: entry.tier, severity: entry.severity, message: error.message,
    span: null, artifactId: null, remediation: null,
    format: "norn/diagnostic@2", phase: entry.tier, retryable: false, fix: entry.fix, no_fix: entry.no_fix,
    ...(source ? { source } : {}),
  }
}
