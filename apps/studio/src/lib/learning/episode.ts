import { CellUnavailable, readStudioScope, scopedHostFetch } from "../definitions.ts"
import type { ObjectRecord } from "../transport.ts"

/**
 * A decision episode from the learning pack (#1208): a read-only ledger projection that joins a
 * `norn.decision@1` receipt, the Function receipt that produced its suggestion (same run), the rule
 * outcomes (#1108), any waiver (#1192) and the recorded Outcome. Studio reads it, it never builds
 * one: agreement, standing and outcome state all arrive from the cell.
 *
 * Served as an ordinary object, `GET /v1/objects/learning.Episode/{id}` → NornObjectRecord, with
 * the fields below in `values` (the shape requested on #1208). Decoding is fail-closed: a record
 * missing a field the timeline reads is not an episode, and the page says so instead of guessing.
 */

export const EPISODE_TYPE = "learning.Episode"

export type Agreement = "accepted" | "edited" | "overridden" | "none"
export type Lifecycle = "candidate" | "shadow" | "active" | "retired"
export type RuleOutcome = "allowed" | "would_block" | "blocked"
export type OutcomeState = "recorded" | "open" | "overdue"
export type Verdict = "exception" | "noise" | "pattern" | "gap"

export interface ReceiptRef { readonly receiptHash: string; readonly commitSeq: number }

export interface Episode {
  readonly id: string
  readonly version: number
  readonly decision: ReceiptRef & {
    readonly id: string
    readonly recordedAtMs: number
    readonly question: string
    readonly risk: "low" | "medium" | "high"
    readonly options: readonly { readonly id: string; readonly label: string }[]
    readonly choice: string
    readonly note: string | null
    /** Display words, never an id. */
    readonly decidedBy: string
  }
  readonly run: { readonly id: string; readonly generation: number; readonly flow: { readonly ruleId: string; readonly version: string; readonly title: string | null }; readonly step: { readonly id: string; readonly label: string | null } }
  readonly recommendation: {
    readonly pick: string
    readonly rationale: string | null
    readonly producer: ReceiptRef & { readonly functionId: string; readonly manifestPin: string; readonly lifecycle: Lifecycle }
  } | null
  readonly agreement: Agreement
  readonly context: { readonly definitionGeneration: number; readonly asOfCommitSeq: number; readonly subjects: readonly { readonly objectType: string; readonly id: string; readonly label: string | null }[] }
  readonly ruleOutcomes: readonly {
    readonly lawId: string
    readonly title: string
    readonly outcome: RuleOutcome
    readonly waiver: { readonly by: string; readonly reason: string; readonly receiptHash: string } | null
  }[]
  readonly outcome: {
    readonly state: OutcomeState
    readonly spec: { readonly title: string; readonly within: string | null }
    readonly record: ReceiptRef & { readonly objectType: string; readonly id: string; readonly recordedAtMs: number; readonly values: readonly { readonly name: string; readonly display: string }[] } | null
  }
  readonly standing: "context" | "measured"
  readonly triage: { readonly verdict: Verdict; readonly reason: string; readonly curator: string; readonly atMs: number } | null
}

// ---- Fail-closed decoding -------------------------------------------------------------------

class Bad extends Error {}
type Obj = Record<string, unknown>
const obj = (v: unknown, at: string): Obj => { if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Bad(`${at} is not an object`); return v as Obj }
const str = (v: unknown, at: string): string => { if (typeof v !== "string" || v === "") throw new Bad(`${at} is not text`); return v }
const text = (v: unknown, at: string): string | null => v === null || v === undefined ? null : str(v, at)
const int = (v: unknown, at: string): number => { if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 0) throw new Bad(`${at} is not a whole number`); return v }
const list = <T,>(v: unknown, at: string, each: (x: unknown, at: string) => T): readonly T[] => { if (!Array.isArray(v)) throw new Bad(`${at} is not a list`); return v.map((x, i) => each(x, `${at}/${i}`)) }
const oneOf = <T extends string>(v: unknown, at: string, values: readonly T[]): T => { if (!values.includes(v as T)) throw new Bad(`${at} is not one of ${values.join(", ")}`); return v as T }
const receipt = (o: Obj, at: string): ReceiptRef => ({ receiptHash: str(o.receiptHash, `${at}/receiptHash`), commitSeq: int(o.commitSeq, `${at}/commitSeq`) })

function decodeValues(v: Obj, id: string, version: number): Episode {
  const d = obj(v.decision, "/decision")
  const options = list(d.options, "/decision/options", (x, at) => { const o = obj(x, at); return { id: str(o.id, `${at}/id`), label: str(o.label, `${at}/label`) } })
  const choice = str(d.choice, "/decision/choice")
  if (!options.some((o) => o.id === choice)) throw new Bad("/decision/choice is not one of the decision's options")
  const run = obj(v.run, "/run"), flow = obj(run.flow, "/run/flow"), step = obj(run.step, "/run/step")
  const rec = v.recommendation === null ? null : obj(v.recommendation, "/recommendation")
  const producer = rec === null ? null : obj(rec.producer, "/recommendation/producer")
  const ctx = obj(v.context, "/context")
  const out = obj(v.outcome, "/outcome"), spec = obj(out.spec, "/outcome/spec")
  const outRec = out.record === null || out.record === undefined ? null : obj(out.record, "/outcome/record")
  const state = oneOf(out.state, "/outcome/state", ["recorded", "open", "overdue"] as const)
  if ((state === "recorded") !== (outRec !== null)) throw new Bad("/outcome/record must be present exactly when the outcome is recorded")
  const triage = v.triage === null || v.triage === undefined ? null : obj(v.triage, "/triage")
  // The cell derives these; one that contradicts its own parts is not shown as if it held.
  const agreement = oneOf(v.agreement, "/agreement", ["accepted", "edited", "overridden", "none"] as const)
  const pick = rec === null ? null : str(rec.pick, "/recommendation/pick")
  if ((agreement === "none") !== (pick === null)) throw new Bad("/agreement is none exactly when nothing was suggested")
  if (agreement === "accepted" && pick !== choice) throw new Bad("/agreement is accepted but the choice is not the suggestion")
  if (agreement === "overridden" && pick === choice) throw new Bad("/agreement is overridden but the choice is the suggestion")
  const standing = oneOf(v.standing, "/standing", ["context", "measured"] as const)
  if ((standing === "measured") !== (state === "recorded")) throw new Bad("/standing is measured exactly when the outcome is recorded")
  return {
    id, version,
    decision: {
      ...receipt(d, "/decision"), id: str(d.id, "/decision/id"), recordedAtMs: int(d.recordedAtMs, "/decision/recordedAtMs"), question: str(d.question, "/decision/question"),
      risk: oneOf(d.risk, "/decision/risk", ["low", "medium", "high"] as const), options, choice, note: text(d.note, "/decision/note"), decidedBy: str(d.decidedBy, "/decision/decidedBy"),
    },
    run: {
      id: str(run.id, "/run/id"), generation: int(run.generation, "/run/generation"),
      flow: { ruleId: str(flow.ruleId, "/run/flow/ruleId"), version: String(typeof flow.version === "number" ? int(flow.version, "/run/flow/version") : str(flow.version, "/run/flow/version")), title: text(flow.title, "/run/flow/title") },
      step: { id: str(step.id, "/run/step/id"), label: text(step.label, "/run/step/label") },
    },
    recommendation: rec === null || producer === null ? null : {
      pick: pick!, rationale: text(rec.rationale, "/recommendation/rationale"),
      producer: { ...receipt(producer, "/recommendation/producer"), functionId: str(producer.functionId, "/recommendation/producer/functionId"), manifestPin: str(producer.manifestPin, "/recommendation/producer/manifestPin"), lifecycle: oneOf(producer.lifecycle, "/recommendation/producer/lifecycle", ["candidate", "shadow", "active", "retired"] as const) },
    },
    agreement,
    context: {
      definitionGeneration: int(ctx.definitionGeneration, "/context/definitionGeneration"), asOfCommitSeq: int(ctx.asOfCommitSeq, "/context/asOfCommitSeq"),
      subjects: list(ctx.subjects, "/context/subjects", (x, at) => { const s = obj(x, at); return { objectType: str(s.objectType, `${at}/objectType`), id: str(s.id, `${at}/id`), label: text(s.label, `${at}/label`) } }),
    },
    ruleOutcomes: list(v.ruleOutcomes, "/ruleOutcomes", (x, at) => {
      const r = obj(x, at)
      const w = r.waiver === null || r.waiver === undefined ? null : obj(r.waiver, `${at}/waiver`)
      return {
        lawId: str(r.lawId, `${at}/lawId`), title: str(r.title, `${at}/title`), outcome: oneOf(r.outcome, `${at}/outcome`, ["allowed", "would_block", "blocked"] as const),
        waiver: w === null ? null : { by: str(w.by, `${at}/waiver/by`), reason: str(w.reason, `${at}/waiver/reason`), receiptHash: str(w.receiptHash, `${at}/waiver/receiptHash`) },
      }
    }),
    outcome: {
      state, spec: { title: str(spec.title, "/outcome/spec/title"), within: text(spec.within, "/outcome/spec/within") },
      record: outRec === null ? null : {
        ...receipt(outRec, "/outcome/record"), objectType: str(outRec.objectType, "/outcome/record/objectType"), id: str(outRec.id, "/outcome/record/id"), recordedAtMs: int(outRec.recordedAtMs, "/outcome/record/recordedAtMs"),
        values: list(outRec.values, "/outcome/record/values", (x, at) => { const o = obj(x, at); return { name: str(o.name, `${at}/name`), display: str(o.display, `${at}/display`) } }),
      },
    },
    standing,
    triage: triage === null ? null : { verdict: oneOf(triage.verdict, "/triage/verdict", ["exception", "noise", "pattern", "gap"] as const), reason: str(triage.reason, "/triage/reason"), curator: str(triage.curator, "/triage/curator"), atMs: int(triage.atMs, "/triage/atMs") },
  }
}

/** The episode in a served record, or why it is not one (the pointer names the first field that failed). */
export function decodeEpisode(record: ObjectRecord): { readonly kind: "episode"; readonly episode: Episode } | { readonly kind: "malformed"; readonly problem: string } {
  try {
    if (record.objectType !== EPISODE_TYPE) throw new Bad(`/objectType is ${record.objectType}, not ${EPISODE_TYPE}`)
    return { kind: "episode", episode: decodeValues(obj(record.values, "/values"), str(record.id, "/id"), int(record.version, "/version")) }
  } catch (error) {
    if (error instanceof Bad) return { kind: "malformed", problem: error.message }
    throw error
  }
}

// ---- Reading one --------------------------------------------------------------------------------

export type EpisodeRead =
  | { readonly kind: "episode"; readonly episode: Episode }
  | { readonly kind: "malformed"; readonly problem: string }
  /** 404 with a Norn code: absent, or not readable by this actor (the cell does not say which). */
  | { readonly kind: "not-found"; readonly message: string }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string }
  /** The cell does not serve the learning pack's episodes (a 404 without a code, or 503). */
  | { readonly kind: "unavailable" }
  | { readonly kind: "no-cell" }

export const EPISODES_UNAVAILABLE = "This cell doesn't serve decision episodes yet."

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

export async function readEpisode(id: string, fetcher: Fetch = (input, init) => fetch(input, init)): Promise<EpisodeRead> {
  let scope
  try { scope = await readStudioScope(fetcher) } catch (error) { if (error instanceof CellUnavailable) return { kind: "unavailable" }; throw error }
  if (scope === null) return { kind: "no-cell" }
  const active = scope
  const response = await scopedHostFetch(fetcher, "/api/host", async () => active)(`/v1/objects/${encodeURIComponent(EPISODE_TYPE)}/${encodeURIComponent(id)}`, { headers: { accept: "application/json" } })
  const raw = await response.text()
  let body: unknown = null
  try { body = JSON.parse(raw) } catch { /* not JSON: decided by the status below */ }
  const code = (body as { code?: unknown } | null)?.code
  const message = (body as { message?: unknown } | null)?.message
  if (response.ok) {
    if (body === null) throw new CellUnavailable(response.status, "The cell's episode was not JSON.")
    return decodeEpisode(body as ObjectRecord)
  }
  if (typeof code === "string" && response.status !== 503) {
    const words = typeof message === "string" ? message : code
    return response.status === 404 ? { kind: "not-found", message: words } : { kind: "refused", status: response.status, code, message: words }
  }
  if (response.status === 404 || response.status === 503) return { kind: "unavailable" }
  throw new CellUnavailable(response.status)
}
