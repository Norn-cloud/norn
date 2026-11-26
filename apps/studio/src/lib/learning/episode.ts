import type { Clause, Predicate, RecordsQuery, Served } from "../records-query.ts"
import type { ObjectRecord } from "../transport.ts"
import {
  DecodeError, defaultFetch, int, list, listLearningObjects, obj, oneOf, readLearningObject, receipt, str, text,
  type DecodedRecord, type Fetch, type JsonObj, type ObjectListRead, type ReceiptRef, type ScopedReadOptions,
} from "./objects.ts"

/**
 * A decision episode from the learning pack (#1208): a read-only ledger projection that joins a
 * `norn.decision@1` receipt, the Function receipt that produced its suggestion (same run), the rule
 * outcomes (#1108), any waiver (#1192) and the recorded Outcome. Studio reads it, it never builds
 * one: agreement, standing and outcome state all arrive from the cell.
 *
 * Served as an ordinary object, `GET /v1/objects/learning.Episode[/{id}]` → NornObjectPage /
 * NornObjectRecord, with the fields below in `values` (the shape requested on #1208). Decoding is
 * fail-closed: a record missing a field the timeline reads is not an episode, and the page says so
 * instead of guessing.
 *
 * PROVISIONAL CONTRACT: The `values` payload shape decoded below is provisional and gated behind
 * S1 capability discovery (`learning.Episode` advertised by the scoped cell's catalog) until the
 * backend learning-pack lane pins it with route-produced fixtures.
 */

import { EPISODE_TYPE } from "./resource-names.ts"
export { EPISODE_TYPE }

export type Agreement = "accepted" | "edited" | "overridden" | "none"
export type Lifecycle = "candidate" | "shadow" | "active" | "retired"
export type RuleOutcome = "allowed" | "would_block" | "blocked"
export type OutcomeState = "recorded" | "open" | "overdue"
export type Verdict = "exception" | "noise" | "pattern" | "gap"
export type { ReceiptRef }

export const AGREEMENTS: readonly Agreement[] = ["accepted", "edited", "overridden", "none"]
export const OUTCOME_STATES: readonly OutcomeState[] = ["recorded", "open", "overdue"]
export const VERDICTS: readonly Verdict[] = ["exception", "noise", "pattern", "gap"]

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

function decodeValues(v: JsonObj, id: string, version: number): Episode {
  const d = obj(v.decision, "/decision")
  const options = list(d.options, "/decision/options", (x, at) => { const o = obj(x, at); return { id: str(o.id, `${at}/id`), label: str(o.label, `${at}/label`) } })
  const choice = str(d.choice, "/decision/choice")
  if (!options.some((o) => o.id === choice)) throw new DecodeError("/decision/choice is not one of the decision's options")
  const run = obj(v.run, "/run"), flow = obj(run.flow, "/run/flow"), step = obj(run.step, "/run/step")
  const rec = v.recommendation === null ? null : obj(v.recommendation, "/recommendation")
  const producer = rec === null ? null : obj(rec.producer, "/recommendation/producer")
  const ctx = obj(v.context, "/context")
  const out = obj(v.outcome, "/outcome"), spec = obj(out.spec, "/outcome/spec")
  const outRec = out.record === null || out.record === undefined ? null : obj(out.record, "/outcome/record")
  const state = oneOf(out.state, "/outcome/state", OUTCOME_STATES)
  if ((state === "recorded") !== (outRec !== null)) throw new DecodeError("/outcome/record must be present exactly when the outcome is recorded")
  const triage = v.triage === null || v.triage === undefined ? null : obj(v.triage, "/triage")
  // The cell derives these; one that contradicts its own parts is not shown as if it held.
  const agreement = oneOf(v.agreement, "/agreement", AGREEMENTS)
  const pick = rec === null ? null : str(rec.pick, "/recommendation/pick")
  if ((agreement === "none") !== (pick === null)) throw new DecodeError("/agreement is none exactly when nothing was suggested")
  if (agreement === "accepted" && pick !== choice) throw new DecodeError("/agreement is accepted but the choice is not the suggestion")
  if (agreement === "overridden" && pick === choice) throw new DecodeError("/agreement is overridden but the choice is the suggestion")
  const standing = oneOf(v.standing, "/standing", ["context", "measured"] as const)
  if ((standing === "measured") !== (state === "recorded")) throw new DecodeError("/standing is measured exactly when the outcome is recorded")
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
    triage: triage === null ? null : { verdict: oneOf(triage.verdict, "/triage/verdict", VERDICTS), reason: str(triage.reason, "/triage/reason"), curator: str(triage.curator, "/triage/curator"), atMs: int(triage.atMs, "/triage/atMs") },
  }
}

/** The episode in a served record, or why it is not one (the pointer names the first field that failed). */
export function decodeEpisode(record: ObjectRecord): { readonly kind: "episode"; readonly episode: Episode; readonly record: ObjectRecord } | { readonly kind: "malformed"; readonly problem: string } {
  try {
    if (record.objectType !== EPISODE_TYPE) throw new DecodeError(`/objectType is ${record.objectType}, not ${EPISODE_TYPE}`)
    return { kind: "episode", episode: decodeValues(obj(record.values, "/values"), str(record.id, "/id"), int(record.version, "/version")), record }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

const decodeAsItem = (record: ObjectRecord): DecodedRecord<Episode> => {
  const decoded = decodeEpisode(record)
  return decoded.kind === "episode" ? { kind: "ok", item: decoded.episode, record: decoded.record } : decoded
}

// ---- Reading one and listing pages --------------------------------------------------------------

export type EpisodeRead =
  /** `record` is the served document as it came, for what the page reads off it as-is (the cell's offers). */
  | { readonly kind: "episode"; readonly episode: Episode; readonly record: ObjectRecord }
  | { readonly kind: "malformed"; readonly problem: string }
  /** 404 with a Norn code: absent, or not readable by this actor (the cell does not say which). */
  | { readonly kind: "not-found"; readonly message: string }
  | { readonly kind: "refused"; readonly status: number; readonly code: string; readonly message: string }
  /** The cell does not serve the learning pack's episodes (a 404 without a code, or 503). */
  | { readonly kind: "unavailable" }
  | { readonly kind: "no-cell" }
  | { readonly kind: "scope-changed" }

export type EpisodeListRead = ObjectListRead<Episode>

export const EPISODES_UNAVAILABLE = "This cell doesn't serve decision episodes yet."

export async function readEpisode(id: string, fetcher: Fetch = defaultFetch, options?: ScopedReadOptions): Promise<EpisodeRead> {
  const read = await readLearningObject(EPISODE_TYPE, id, decodeAsItem, fetcher, options)
  return read.kind === "ok" ? { kind: "episode", episode: read.item, record: read.record } : read
}

export const listEpisodes = (query: RecordsQuery, fetcher: Fetch = defaultFetch, options?: ScopedReadOptions): Promise<EpisodeListRead> =>
  listLearningObjects(EPISODE_TYPE, query, decodeAsItem, fetcher, options)

// ---- Server-side filter contract (#1208, PR #1631 S2) -------------------------------------------

/**
 * Server-side episode filters accepted by the real generic object query route today.
 *
 * Backend follow-up (#1220 / #1208): `crates/norn-api/src/object_query.rs` (`is_name`, lines 291
 * and 505) only accepts single flat declared properties matching `[a-z][a-z0-9_]*` and rejects
 * dotted paths. Until the backend learning-pack lane either exposes flat filterable properties on
 * `learning.Episode` or extends the generic object-query grammar (with route-produced fixtures),
 * Studio only sends `agreement`; the remaining four #1208 filters (outcome state, law id,
 * producer manifest pin, and triage verdict / untriaged) are hidden.
 */
export interface EpisodeFilters {
  readonly agreement?: Agreement
}

/** Encode active episode filters as the generic object-query predicate accepted by the cell today. */
export function episodeFilterPredicate(filters: EpisodeFilters): Predicate | null {
  const all: Clause[] = []
  if (filters.agreement !== undefined) all.push({ property: "agreement", op: "eq", value: filters.agreement })
  return all.length === 0 ? null : { all }
}

/** Read back the episode filters the cell echoed in `page.query` (Studio never assumes a filter applied). */
export function episodeFiltersFromServed(served: Served | null): EpisodeFilters {
  const clauses = served?.filter?.all ?? []
  const out: { agreement?: Agreement } = {}
  for (const clause of clauses) {
    if (clause.property === "agreement" && clause.op === "eq" && typeof clause.value === "string" && AGREEMENTS.includes(clause.value as Agreement)) {
      out.agreement = clause.value as Agreement
    }
  }
  return out
}
