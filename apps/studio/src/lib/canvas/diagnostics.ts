import type { Origin } from "./lifecycle.ts"

/** One finding of the checker's `norn-checker/diagnostics@1` document. */
export interface Finding {
  readonly code: string
  readonly severity: "error" | "warning" | "info" | string
  readonly message: string
  readonly span: { readonly pointer: string } | null
  readonly artifactId: string | null
}

/** The `norn-checker-abi/check@1` result of `checkArtifactSet`. */
export interface CheckResult {
  readonly admitted: boolean
  readonly diagnostics: { readonly diagnostics: readonly Finding[] }
}

/** A finding placed on what the user drew; neither `node` nor `edge` means it belongs to the whole graph. */
export interface Placed {
  readonly finding: Finding
  readonly node?: string
  readonly edge?: string
}

// Reachability findings point at the whole lifecycle and name the state in their message.
const STATE_FINDINGS = new Set(["lifecycle.validation.unreachable_state", "lifecycle.validation.dead_end_state"])
const NAMED_STATE = /^state "([^"]+)"/

const within = (pointer: string, prefix: string) => pointer === prefix || pointer.startsWith(`${prefix}/`)

/**
 * Place each finding about `artifactId` on the node or edge whose origin is
 * the longest prefix of its pointer. Findings about other artifacts in the
 * set are kept, unplaced, so none is hidden.
 */
export function place(findings: readonly Finding[], artifactId: string, origins: readonly Origin[], nodes: ReadonlySet<string>): Placed[] {
  return findings.map((finding) => {
    if (finding.artifactId !== null && finding.artifactId !== artifactId) return { finding }
    const named = STATE_FINDINGS.has(finding.code) ? NAMED_STATE.exec(finding.message)?.[1] : undefined
    if (named !== undefined && nodes.has(named)) return { finding, node: named }
    const pointer = finding.span?.pointer ?? ""
    const best = origins.filter((origin) => within(pointer, origin.pointer))
      .reduce<Origin | undefined>((longest, origin) => !longest || origin.pointer.length > longest.pointer.length ? origin : longest, undefined)
    return { finding, ...(best?.node ? { node: best.node } : {}), ...(best?.edge ? { edge: best.edge } : {}) }
  })
}
