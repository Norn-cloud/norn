import type { RecordsQuery } from "../records-query.ts"
import type { ObjectRecord } from "../transport.ts"
import { AGREEMENTS, type Agreement } from "./episode.ts"
import {
  DecodeError, bool, defaultFetch, int, list, listLearningObjects, obj, oneOf, readLearningObject, receipt, str, text,
  type DecodedRecord, type Fetch, type ObjectItemRead, type ObjectListRead, type ReceiptRef, type ScopedReadOptions,
} from "./objects.ts"

/**
 * Curated cases and eval sets from the learning pack (#1208, #1220 L-3).
 *
 * Curated cases pin ledger references only (no copies), and only episodes whose Outcome is
 * recorded may be curated (enforced by the cell). Eval sets group pinned case references under a
 * split key; once sealed they are append-only/immutable and evolve via a new version.
 *
 * Studio reads them through the generic `GET /v1/objects/{type}` routes and invokes every mutation
 * only through actions offered by `GET /v1/actions` (`learning.curate_case`,
 * `learning.set_case_protected`, `learning.uncurate_case`, `learning.create_eval_set`,
 * `learning.rename_eval_set`, `learning.delete_eval_set`, `learning.add_case`,
 * `learning.remove_case`, `learning.seal_eval_set`, `learning.new_eval_set_version`).
 *
 * PROVISIONAL CONTRACT: The `values` payload shapes for `learning.CuratedCase` and
 * `learning.EvalSet` are provisional and gated behind S1 capability discovery until the backend
 * learning-pack lane pins them with route-produced fixtures.
 */

import { CURATED_CASE_TYPE, EVAL_SET_TYPE } from "./resource-names.ts"
export { CURATED_CASE_TYPE, EVAL_SET_TYPE }

export const CASES_UNAVAILABLE = "This cell doesn't serve curated cases yet."
export const EVAL_SETS_UNAVAILABLE = "This cell doesn't serve eval sets yet."

export interface CuratedCase {
  readonly id: string
  readonly version: number
  readonly episodeId: string
  readonly decisionReceipt: ReceiptRef
  readonly outcomeReceipt: ReceiptRef
  readonly question: string
  readonly expectedChoice: string
  readonly agreement: Agreement
  readonly protected: boolean
  readonly note: string | null
  readonly curatedBy: string
  readonly curatedAtMs: number
  readonly evalSetIds: readonly string[]
}

export type EvalSetStatus = "unsealed" | "sealed"

export interface EvalSetCaseRef {
  readonly caseId: string
  readonly episodeId: string
  readonly receiptHash: string
  readonly protected: boolean
  readonly label: string | null
}

export interface EvalSet {
  readonly id: string
  readonly version: number
  readonly name: string
  readonly splitKey: string
  readonly setVersion: number
  readonly status: EvalSetStatus
  readonly sealedAtMs: number | null
  readonly sealedBy: string | null
  readonly parentSetId: string | null
  readonly caseRefs: readonly EvalSetCaseRef[]
  readonly createdBy: string
  readonly createdAtMs: number
}

/** Fail-closed decode of a served `learning.CuratedCase` record. */
export function decodeCuratedCase(record: ObjectRecord): DecodedRecord<CuratedCase> {
  try {
    if (record.objectType !== CURATED_CASE_TYPE) throw new DecodeError(`/objectType is ${record.objectType}, not ${CURATED_CASE_TYPE}`)
    const id = str(record.id, "/id")
    const version = int(record.version, "/version")
    const v = obj(record.values, "/values")
    const item: CuratedCase = {
      id,
      version,
      episodeId: str(v.episodeId, "/episodeId"),
      decisionReceipt: receipt(obj(v.decisionReceipt, "/decisionReceipt"), "/decisionReceipt"),
      outcomeReceipt: receipt(obj(v.outcomeReceipt, "/outcomeReceipt"), "/outcomeReceipt"),
      question: str(v.question, "/question"),
      expectedChoice: str(v.expectedChoice, "/expectedChoice"),
      agreement: oneOf(v.agreement, "/agreement", AGREEMENTS),
      protected: bool(v.protected, "/protected"),
      note: text(v.note, "/note"),
      curatedBy: str(v.curatedBy, "/curatedBy"),
      curatedAtMs: int(v.curatedAtMs, "/curatedAtMs"),
      evalSetIds: list(v.evalSetIds, "/evalSetIds", (x, at) => str(x, at)),
    }
    return { kind: "ok", item, record }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

/** Fail-closed decode of a served `learning.EvalSet` record. */
export function decodeEvalSet(record: ObjectRecord): DecodedRecord<EvalSet> {
  try {
    if (record.objectType !== EVAL_SET_TYPE) throw new DecodeError(`/objectType is ${record.objectType}, not ${EVAL_SET_TYPE}`)
    const id = str(record.id, "/id")
    const version = int(record.version, "/version")
    const v = obj(record.values, "/values")
    const status = oneOf(v.status, "/status", ["unsealed", "sealed"] as const)
    const sealedAtMs = v.sealedAtMs === null || v.sealedAtMs === undefined ? null : int(v.sealedAtMs, "/sealedAtMs")
    const sealedBy = text(v.sealedBy, "/sealedBy")
    // Sealed state and seal provenance must agree: never render a contradictory set.
    if ((status === "sealed") !== (sealedAtMs !== null)) throw new DecodeError("/sealedAtMs must be present exactly when status is sealed")
    if ((status === "sealed") !== (sealedBy !== null)) throw new DecodeError("/sealedBy must be present exactly when status is sealed")
    const setVersion = int(v.setVersion, "/setVersion")
    if (setVersion < 1) throw new DecodeError("/setVersion must be at least 1")
    const item: EvalSet = {
      id,
      version,
      name: str(v.name, "/name"),
      splitKey: str(v.splitKey, "/splitKey"),
      setVersion,
      status,
      sealedAtMs,
      sealedBy,
      parentSetId: text(v.parentSetId, "/parentSetId"),
      caseRefs: list(v.caseRefs, "/caseRefs", (x, at) => {
        const ref = obj(x, at)
        return {
          caseId: str(ref.caseId, `${at}/caseId`),
          episodeId: str(ref.episodeId, `${at}/episodeId`),
          receiptHash: str(ref.receiptHash, `${at}/receiptHash`),
          protected: bool(ref.protected, `${at}/protected`),
          label: text(ref.label, `${at}/label`),
        }
      }),
      createdBy: str(v.createdBy, "/createdBy"),
      createdAtMs: int(v.createdAtMs, "/createdAtMs"),
    }
    return { kind: "ok", item, record }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

export const listCuratedCases = (query: RecordsQuery, fetcher: Fetch = defaultFetch, options?: ScopedReadOptions): Promise<ObjectListRead<CuratedCase>> =>
  listLearningObjects(CURATED_CASE_TYPE, query, decodeCuratedCase, fetcher, options)

export const readCuratedCase = (id: string, fetcher: Fetch = defaultFetch, options?: ScopedReadOptions): Promise<ObjectItemRead<CuratedCase>> =>
  readLearningObject(CURATED_CASE_TYPE, id, decodeCuratedCase, fetcher, options)

export const listEvalSets = (query: RecordsQuery, fetcher: Fetch = defaultFetch, options?: ScopedReadOptions): Promise<ObjectListRead<EvalSet>> =>
  listLearningObjects(EVAL_SET_TYPE, query, decodeEvalSet, fetcher, options)

export const readEvalSet = (id: string, fetcher: Fetch = defaultFetch, options?: ScopedReadOptions): Promise<ObjectItemRead<EvalSet>> =>
  readLearningObject(EVAL_SET_TYPE, id, decodeEvalSet, fetcher, options)
