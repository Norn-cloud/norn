import { expect, test } from "bun:test"
import { CellUnavailable, fetchWithTimeout, lastReachableAt, resetCellReachable } from "../src/lib/definitions.ts"
import { CellRefusal, isRetryableModeError, readMode } from "../src/lib/live-transport.ts"
import { explainError, plainMetricLabel, plainRestatement } from "../src/lib/plain-words.ts"
import { formatLastReachable, isWorkspaceUnavailableError } from "../src/components/load-state.tsx"

test("a non-retryable cell failure (503 cell.unopenable) is never auto-retried and preserves incident metadata", async () => {
  const fetcher = async (input: string) => {
    if (input === "/api/me") return Response.json({ user: { id: "u" }, organization: { id: "org_1", name: "Org", role: "admin" }, environmentId: "env_1" })
    return Response.json({
      format: "norn/refusal@2",
      code: "cell.unopenable",
      category: "internal",
      retryable: false,
      incidentId: "inc-unopenable-42",
      message: "The workspace cell could not be opened.",
      explanation: "Storage migration is paused for this environment.",
      decidedAt: "2027-01-15T08:00:00Z",
    }, { status: 503 })
  }
  let caught: unknown
  try {
    await readMode(fetcher)
  } catch (error) {
    caught = error
  }
  expect(caught).toBeInstanceOf(CellUnavailable)
  const err = caught as CellUnavailable
  expect(err.status).toBe(503)
  expect(err.code).toBe("cell.unopenable")
  expect(err.retryable).toBe(false)
  expect(err.incidentId).toBe("inc-unopenable-42")
  expect(err.explanation).toBe("Storage migration is paused for this environment.")
  expect(err.decidedAt).toBe("2027-01-15T08:00:00Z")
  expect(isRetryableModeError(err)).toBe(false)
  expect(isWorkspaceUnavailableError(err)).toBe(true)
})

test("transient 503 and network failures remain eligible for brief automatic retry", () => {
  expect(isRetryableModeError(new CellUnavailable(503, "temporarily busy", { retryable: true }))).toBe(true)
  expect(isRetryableModeError(new CellUnavailable(503))).toBe(true)
  expect(isRetryableModeError(new CellUnavailable(0, "network down", { timedOut: true }))).toBe(true)
  expect(isRetryableModeError(new CellUnavailable(503, "broken", { retryable: false }))).toBe(false)
  expect(isRetryableModeError(new CellRefusal(403, "authz.denied", "denied", { retryable: false }))).toBe(false)
})

test("plainMetricLabel and plainRestatement remove raw sum/conservation jargon for Business mode", () => {
  const raw = "sum(completed_count) / sum(application_count); sum(completion_rate); conservation []"
  expect(plainMetricLabel(raw)).toBe("Completion rate (completed count ÷ application count)")
  expect(plainMetricLabel("sum(quantity); conservation [law.a@1]")).toBe("Total quantity")
  expect(plainMetricLabel("sum(output_fine_g) / sum(input_fine_g); law melt@1")).toBe("Output fine g ÷ input fine g")
  expect(plainMetricLabel("quantity, as of observed_at; last value per canon.Holding at each period's end"))
    .toBe("Quantity at period end (as of observed at, per Holding)")

  const restatement = "sum(completed_count) / sum(application_count); sum(completion_rate); conservation [] (completed_count ÷ application_count) of canon.Application records, by week of submitted_at"
  const plain = plainRestatement(restatement)
  expect(plain).toBe("Completion rate (completed_count ÷ application_count) of canon.Application records, by week of submitted_at")
  expect(plain).not.toContain("sum(")
  expect(plain).not.toContain("conservation []")
})

test("explainError hides raw internal worker strings in Business mode while preserving server explanation and Technical details", () => {
  const err = new CellUnavailable(500, "Worker failed: handler is waiting on nothing", {
    code: "host.internal",
    explanation: "The query planner could not open the table.",
    incidentId: "inc-77",
    retryable: true,
  })
  const business = explainError(err, "en", "business")
  expect(business.headline).toBe("The workspace hit an internal problem and couldn't finish this request.")
  expect(business.detail).toBe("The query planner could not open the table.")
  expect(business.incidentId).toBe("inc-77")
  expect(business.raw).toContain("Worker failed: handler is waiting on nothing")

  const technical = explainError(err, "en", "technical")
  expect(technical.headline).toBe("Worker failed: handler is waiting on nothing")
  expect(technical.detail).toBe("The query planner could not open the table.")
  expect(technical.code).toBe("host.internal")

  const uncodedNoise = explainError(new Error("Worker failed: handler is waiting on nothing"), "en", "business")
  expect(uncodedNoise.headline).not.toContain("waiting on nothing")
  expect(uncodedNoise.raw).toContain("Worker failed: handler is waiting on nothing")
})

test("fetchWithTimeout aborts stalled requests cleanly as a timed-out CellUnavailable", async () => {
  const stalled = (_input: string, init?: RequestInit) => new Promise<Response>((_resolve, reject) => {
    init?.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")))
  })
  let caught: unknown
  try {
    await fetchWithTimeout("/api/host/v1/definitions", {}, stalled, 25)
  } catch (error) {
    caught = error
  }
  expect(caught).toBeInstanceOf(CellUnavailable)
  const err = caught as CellUnavailable
  expect(err.timedOut).toBe(true)
  expect(err.code).toBe("cell.timeout")
  expect(isWorkspaceUnavailableError(err)).toBe(true)
})

test("formatLastReachable reports null when unseen and human words once reachable", () => {
  resetCellReachable()
  expect(lastReachableAt()).toBeNull()
  expect(formatLastReachable(null)).toBeNull()
  const now = Date.UTC(2026, 10, 20, 12, 0, 0)
  expect(formatLastReachable(now - 10_000, now)).toBe("just now")
  expect(formatLastReachable(now - 5 * 60_000, now)).toBe("5 min ago")
})
