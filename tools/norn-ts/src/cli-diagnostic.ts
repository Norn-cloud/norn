/**
 * The one diagnostic shape the CLI prints: `norn/diagnostic@2`, the projection
 * the checker makes of its findings (`Diagnostic::to_json_v2`). The registry
 * is the authority for a code's tier, severity and guidance.
 * `generated/diagnostics.json` is the registry as the CLI reads it, kept
 * identical to `crates/norn-checker/conformance` by
 * `cli-code-registry.test.ts`, so the CLI can say what to do even when the
 * checker package cannot load.
 *
 * A code that is not registered cannot be printed: both functions throw.
 */
import { readFileSync } from "node:fs"
import { join } from "node:path"
import type { SourceLocation } from "./builder/diagnostics.ts"
import type { Position } from "./check.ts"
import type { CheckerDiagnostic, DiagnosticV2 } from "./checker-abi.ts"
import { TOOL_ROOT } from "./profile.ts"
import type { ProjectError } from "./project.ts"

export interface RegisteredCode {
  readonly code: string
  readonly tier: string
  readonly severity: string
  readonly message: string
  readonly fix: string | null
  readonly no_fix: string | null
}

/** What the authoring compiler reported, beside the registered code that carries it. */
export interface CompilerFinding {
  /** The compiler's own code (`NORN-FLOW-TYPE`); not a registry code. */
  readonly code: string
  readonly start: Position
  readonly end: Position
}

/** A finding as the CLI prints it: v2, with the project's source line when it has one. */
export type Finding = DiagnosticV2 & { readonly source?: SourceLocation; readonly compiler?: CompilerFinding }

let registry: ReadonlyMap<string, RegisteredCode> | undefined

/** Every registered diagnostic code, by code. */
export function registeredCodes(): ReadonlyMap<string, RegisteredCode> {
  if (!registry) {
    const snapshot = JSON.parse(readFileSync(join(TOOL_ROOT, "generated/diagnostics.json"), "utf8")) as { codes: RegisteredCode[] }
    registry = new Map(snapshot.codes.map((entry) => [entry.code, entry]))
  }
  return registry
}

function registered(code: string): RegisteredCode {
  const entry = registeredCodes().get(code)
  if (!entry) throw new Error(`${code} is not a registered diagnostic code`)
  return entry
}

/** A CLI refusal as `norn/diagnostic@2`, with the registry's guidance for its code. */
export function cliDiagnostic(error: ProjectError, extra: { readonly source?: SourceLocation; readonly compiler?: CompilerFinding } = {}): Finding {
  const entry = registered(error.code)
  return {
    code: entry.code, tier: entry.tier, severity: entry.severity, message: error.message,
    span: null, artifactId: null, remediation: null,
    format: "norn/diagnostic@2", phase: entry.tier, retryable: false, fix: entry.fix, no_fix: entry.no_fix,
    ...(extra.source ? { source: extra.source } : {}),
    ...(extra.compiler ? { compiler: extra.compiler } : {}),
  }
}

/** A `norn-checker/diagnostics@1` finding (what `norn check` reports) as `norn/diagnostic@2`. */
export function checkerDiagnostic(finding: CheckerDiagnostic): Finding {
  const entry = registered(finding.code)
  const { source, ...rest } = finding as CheckerDiagnostic & { readonly tier?: string | null; readonly remediation?: unknown }
  return {
    ...rest,
    code: entry.code, tier: entry.tier, severity: finding.severity, message: finding.message,
    span: finding.span ?? null, artifactId: finding.artifactId ?? null, remediation: rest.remediation ?? null,
    format: "norn/diagnostic@2", phase: entry.tier, retryable: false, fix: entry.fix, no_fix: entry.no_fix,
    ...(source ? { source } : {}),
  }
}
