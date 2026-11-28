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
import type { Position } from "./native-compiler.ts"
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

/** Format a structured counterexample witness (steps, table overlap/gap, chase, lifecycle, red-team) for CLI output. */
export function formatWitnessLines(witness: unknown, indent = "  "): string[] {
  if (witness === undefined || witness === null) return []
  if (typeof witness !== "object" || Array.isArray(witness)) {
    return [`${indent}witness: ${JSON.stringify(witness)}`]
  }
  const record = witness as Readonly<Record<string, unknown>>
  if (Array.isArray(record.steps)) {
    if (record.steps.length === 0) return [`${indent}witness: (initial state, 0 steps)`]
    const trace = record.steps.map((step) => {
      if (typeof step !== "object" || step === null) return String(step)
      const s = step as Readonly<Record<string, unknown>>
      const moves = Array.isArray(s.moves)
        ? s.moves.map((mv) => {
            if (typeof mv !== "object" || mv === null) return String(mv)
            const m = mv as Readonly<Record<string, unknown>>
            return `${String(m.side ?? "?")}:${String(m.label ?? "?")}`
          }).join(", ")
        : ""
      return `edge ${String(s.edge ?? "?")} [${moves}]`
    }).join(" -> ")
    return [`${indent}witness: ${trace}`]
  }
  const parts: string[] = []
  if (record.input !== undefined) parts.push(`input=${JSON.stringify(record.input)}`)
  if (Array.isArray(record.matchingRows)) parts.push(`matchingRows=[${record.matchingRows.map(String).join(", ")}]`)
  if (Array.isArray(record.selectedRows)) parts.push(`selectedRows=[${record.selectedRows.map(String).join(", ")}]`)
  if (typeof record.state === "string") parts.push(`state=${record.state}`)
  if (Array.isArray(record.run)) parts.push(`run=${record.run.map(String).join(" -> ")}`)
  else if (Array.isArray(record.trace)) parts.push(`trace=${record.trace.map(String).join(" -> ")}`)
  if (record.blocked !== undefined) parts.push(`blocked=${JSON.stringify(record.blocked)}`)
  if (Array.isArray(record.rules)) parts.push(`rules=[${record.rules.map(String).join(", ")}]`)
  if (Array.isArray(record.facts)) parts.push(`facts=${JSON.stringify(record.facts)}`)
  if (typeof record.role === "string") parts.push(`role=${record.role}`)
  if (typeof record.ruleId === "string") parts.push(`ruleId=${record.ruleId}`)
  if (typeof record.actionId === "string") parts.push(`actionId=${record.actionId}`)
  if (typeof record.objectType === "string") parts.push(`objectType=${record.objectType}`)
  if (typeof record.transitionId === "string") parts.push(`transitionId=${record.transitionId}`)
  if (typeof record.property === "string") parts.push(`property=${record.property}`)
  if (typeof record.missingAttribute === "string") parts.push(`missingAttribute=${record.missingAttribute}`)
  if (typeof record.reason === "string") parts.push(`reason=${record.reason}`)
  if (parts.length > 0) return [`${indent}witness: ${parts.join("; ")}`]
  return [`${indent}witness: ${JSON.stringify(witness)}`]
}

/** Render a CLI diagnostic with its registered code, message, witness (when present) and guidance. */
export function formatCliDiagnostic(finding: Finding): string {
  const lines = [`${finding.code}: ${finding.message}`, ...formatWitnessLines(finding.witness)]
  if (finding.fix) lines.push(`  fix: ${finding.fix}`)
  else if (finding.no_fix) lines.push(`  no automatic fix: ${finding.no_fix}`)
  return lines.join("\n")
}

/** A `norn-checker/diagnostics@1` finding (what `norn check` reports) as `norn/diagnostic@2`. */
export function checkerDiagnostic(finding: CheckerDiagnostic): Finding {
  const entry = registered(finding.code)
  const { source, witness, ...rest } = finding as CheckerDiagnostic & { readonly tier?: string | null; readonly remediation?: unknown }
  return {
    ...rest,
    code: entry.code, tier: entry.tier, severity: finding.severity, message: finding.message,
    span: finding.span ?? null, artifactId: finding.artifactId ?? null, remediation: rest.remediation ?? null,
    format: "norn/diagnostic@2", phase: entry.tier, retryable: false, fix: entry.fix, no_fix: entry.no_fix,
    ...(witness !== undefined ? { witness } : {}),
    ...(source ? { source } : {}),
  }
}
