import { cellCall, defaultFetch, humanize, type CellResult, type Fetch } from "./cell-read.ts"

/**
 * Runs of installed flows (RUN-ROUTES-1, #1047), with the step timeline RUNS adds for Studio:
 *
 *   POST /v1/runs        { flow, runId }      start (a retry with the same id returns the run)
 *   GET  /v1/runs/{id}   RunDocument; 404 run.not_found for an absent or unreadable run.
 *
 * The step timeline (`steps`), the parked `decision` and ETag/304 arrive in a RUNS follow-up. Until
 * then a run has neither, and every read is a plain 200: Studio shows the run's state, flow and
 * generation, and reads it again on a fixed beat.
 */

export type StepKind = "perform" | "decide" | "route" | "wait" | "succeed" | "fail"
/** `skipped` appears only on a run that has ended. */
export type StepStatus = "pending" | "running" | "waiting" | "done" | "failed" | "skipped"

export interface StepReceipt {
  readonly kind: "function" | "decision" | "commit"
  readonly receiptHash: string
  readonly commitSeq: number
  readonly recordedAtMs: number
  /** Display words ("you", a name, "another decider"), never an id. */
  readonly executor?: string | null
}

export interface RunStep {
  readonly id: string
  readonly label: string | null
  readonly kind: StepKind
  readonly status: StepStatus
  readonly startedAtMs: number | null
  readonly endedAtMs: number | null
  readonly receipts: readonly StepReceipt[]
  readonly error: { readonly code: string; readonly message: string } | null
}

export interface Run {
  readonly id: string
  readonly generation: number
  readonly flow: { readonly ruleId: string; readonly version: string | number; readonly title?: string | null }
  readonly objectType: string
  readonly state: string | null
  readonly version: number
  readonly createdAtMs: number
  readonly createdCommitSeq: number
  readonly updatedAtMs: number
  readonly updatedCommitSeq: number
  /** Absent until RUNS's follow-up serves the timeline. */
  readonly steps?: readonly RunStep[]
  /** Absent until the follow-up; null when the run waits on no decision this actor can read. */
  readonly decision?: { readonly id: string; readonly expiresAtMs: number } | null
}

export const RUNS_UNAVAILABLE = "This cell doesn't serve runs yet."

export const getRun = (id: string, etag: string | null = null, fetcher: Fetch = defaultFetch): Promise<CellResult<Run>> =>
  cellCall<Run>(`/v1/runs/${encodeURIComponent(id)}`, etag === null ? {} : { headers: { "if-none-match": etag } }, fetcher)

export const startRun = (flow: string, runId: string, fetcher: Fetch = defaultFetch): Promise<CellResult<Run>> =>
  cellCall<Run>("/v1/runs", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ flow, runId }) }, fetcher)

/** A fresh run id for the Run button: URL-safe, inside RUNS's `[A-Za-z0-9._:-]{1,128}`. */
export const newRunId = () => `run_${crypto.randomUUID()}`

export const stepTitle = (step: Pick<RunStep, "id" | "label">) => step.label ?? humanize(step.id)
export const runTitle = (run: Pick<Run, "flow">) => run.flow.title ?? humanize(run.flow.ruleId.replace(/\.flow\.rule$/, ""))

/** `active`: a run without a timeline yet, whose state alone says it has not ended. */
export type Phase = "running" | "waiting" | "succeeded" | "failed" | "ended" | "active"

/** Where the run stands: from its steps when the cell sends them, else from its state alone. */
export function phaseOf(run: Pick<Run, "steps" | "decision" | "state">): Phase {
  const steps = run.steps
  if (steps === undefined) {
    // No timeline yet: a deleted run object (state null) has ended; any other state may still move.
    return run.state === null ? "ended" : "active"
  }
  if (steps.some((s) => s.status === "running")) return "running"
  if ((run.decision ?? null) !== null || steps.some((s) => s.status === "waiting")) return "waiting"
  if (steps.some((s) => s.status === "failed") || steps.some((s) => s.kind === "fail" && s.status === "done")) return "failed"
  if (steps.some((s) => s.kind === "succeed" && s.status === "done")) return "succeeded"
  if (steps.some((s) => s.status === "pending")) return "running"
  return "ended"
}

/** How often to read the run again: every 2 s while it works, 15 s while a person decides, never once it ended. */
export function refreshEvery(phase: Phase): number | false {
  if (phase === "running") return 2_000
  if (phase === "active") return 5_000
  if (phase === "waiting") return 15_000
  return false
}
