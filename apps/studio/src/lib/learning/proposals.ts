import type { Clause, Predicate, RecordsQuery, Served } from "../records-query.ts"
import type { ObjectRecord } from "../transport.ts"
import {
  DecodeError, bool, defaultFetch, int, list, listLearningObjects, obj, oneOf, readLearningObject, receipt, str, text,
  type DecodedRecord, type Fetch, type ObjectItemRead, type ObjectListRead, type ReceiptRef,
} from "./objects.ts"

/**
 * Gap proposals and replay tables from the learning pack (#1208, #1220 L-5).
 *
 * A gap proposal proposes a definition diff citing episodes. Submitting it runs the checker and
 * replays curated cases under the proposed definitions, listing every case whose outcome flips
 * (for example, past cases a tightened numeric limit would have refused). A flipped protected case
 * refuses admission unless a Ruling (#62) supersedes it by name.
 *
 * Served through `GET /v1/objects/learning.GapProposal[/{id}]`; every mutation goes through the
 * cell's governed action catalog (`learning.draft_gap_proposal`, `learning.edit_gap_proposal`,
 * `learning.withdraw_gap_proposal`, `learning.submit_gap_proposal`, `learning.ratify_gap_proposal`).
 */

export const GAP_PROPOSAL_TYPE = "learning.GapProposal"
export const PROPOSALS_UNAVAILABLE = "This cell doesn't serve gap proposals yet."

export type ProposalStatus = "draft" | "submitted" | "admitted" | "refused" | "ratified" | "withdrawn"
export const PROPOSAL_STATUSES: readonly ProposalStatus[] = ["draft", "submitted", "admitted", "refused", "ratified", "withdrawn"]

export type ReplayOutcome = "allowed" | "refused" | "would_block" | "waived"
const REPLAY_OUTCOMES: readonly ReplayOutcome[] = ["allowed", "refused", "would_block", "waived"]

export interface ReplayCase {
  readonly caseId: string
  readonly episodeId: string
  readonly label: string
  readonly protected: boolean
  readonly supersededByRuling: boolean
  readonly beforeOutcome: ReplayOutcome
  readonly afterOutcome: ReplayOutcome
  readonly flipped: boolean
  readonly detail: string | null
}

export interface ProposalReplay {
  readonly evalSetId: string
  readonly evalSetName: string
  readonly evalSetVersion: number
  readonly definitionPin: string
  readonly replayedAtMs: number
  readonly totalCases: number
  readonly flippedCount: number
  readonly protectedFlippedCount: number
  readonly admissionStatus: "admitted" | "refused"
  readonly refusalReason: string | null
  readonly cases: readonly ReplayCase[]
}

export interface ProposalRuling extends ReceiptRef {
  readonly ratifiedBy: string
  readonly ratifiedAtMs: number
  readonly supersededCaseIds: readonly string[]
}

export interface GapProposal {
  readonly id: string
  readonly version: number
  readonly title: string
  readonly summary: string
  readonly status: ProposalStatus
  readonly target: {
    readonly artifactId: string
    readonly kind: string
    readonly label: string | null
  }
  readonly diff: {
    readonly summary: string
    readonly before: string
    readonly after: string
    readonly patch: string
  }
  readonly episodeIds: readonly string[]
  readonly author: string
  readonly createdAtMs: number
  readonly updatedAtMs: number
  readonly checker: {
    readonly status: "not_run" | "admitted" | "refused"
    readonly diagnostics: readonly { readonly code: string; readonly message: string }[]
  }
  readonly replay: ProposalReplay | null
  readonly ruling: ProposalRuling | null
  readonly supersedesProposalId: string | null
}

/** Fail-closed decode of a served `learning.GapProposal` record, checking internal consistency. */
export function decodeGapProposal(record: ObjectRecord): DecodedRecord<GapProposal> {
  try {
    if (record.objectType !== GAP_PROPOSAL_TYPE) throw new DecodeError(`/objectType is ${record.objectType}, not ${GAP_PROPOSAL_TYPE}`)
    const id = str(record.id, "/id")
    const version = int(record.version, "/version")
    const v = obj(record.values, "/values")
    const status = oneOf(v.status, "/status", PROPOSAL_STATUSES)
    const targetObj = obj(v.target, "/target")
    const diffObj = obj(v.diff, "/diff")
    const checkerObj = obj(v.checker, "/checker")
    const checkerStatus = oneOf(checkerObj.status, "/checker/status", ["not_run", "admitted", "refused"] as const)
    const checkerDiagnostics = list(checkerObj.diagnostics, "/checker/diagnostics", (x, at) => {
      const d = obj(x, at)
      return { code: str(d.code, `${at}/code`), message: str(d.message, `${at}/message`) }
    })

    const rawReplay = v.replay === null || v.replay === undefined ? null : obj(v.replay, "/replay")
    let replay: ProposalReplay | null = null
    if (rawReplay !== null) {
      const evalSetVersion = int(rawReplay.evalSetVersion, "/replay/evalSetVersion")
      if (evalSetVersion < 1) throw new DecodeError("/replay/evalSetVersion must be at least 1")
      const totalCases = int(rawReplay.totalCases, "/replay/totalCases")
      const flippedCount = int(rawReplay.flippedCount, "/replay/flippedCount")
      const protectedFlippedCount = int(rawReplay.protectedFlippedCount, "/replay/protectedFlippedCount")
      const admissionStatus = oneOf(rawReplay.admissionStatus, "/replay/admissionStatus", ["admitted", "refused"] as const)
      const cases = list(rawReplay.cases, "/replay/cases", (x, at) => {
        const c = obj(x, at)
        const beforeOutcome = oneOf(c.beforeOutcome, `${at}/beforeOutcome`, REPLAY_OUTCOMES)
        const afterOutcome = oneOf(c.afterOutcome, `${at}/afterOutcome`, REPLAY_OUTCOMES)
        const flipped = bool(c.flipped, `${at}/flipped`)
        if (flipped !== (beforeOutcome !== afterOutcome)) {
          throw new DecodeError(`${at}/flipped contradicts beforeOutcome and afterOutcome`)
        }
        return {
          caseId: str(c.caseId, `${at}/caseId`),
          episodeId: str(c.episodeId, `${at}/episodeId`),
          label: str(c.label, `${at}/label`),
          protected: bool(c.protected, `${at}/protected`),
          supersededByRuling: bool(c.supersededByRuling, `${at}/supersededByRuling`),
          beforeOutcome,
          afterOutcome,
          flipped,
          detail: text(c.detail, `${at}/detail`),
        }
      })
      if (totalCases < cases.length) throw new DecodeError("/replay/totalCases is smaller than /replay/cases length")
      if (flippedCount > totalCases) throw new DecodeError("/replay/flippedCount exceeds /replay/totalCases")
      if (protectedFlippedCount > flippedCount) throw new DecodeError("/replay/protectedFlippedCount exceeds /replay/flippedCount")
      if (totalCases === cases.length) {
        const countedFlips = cases.filter((c) => c.flipped).length
        const countedProtectedFlips = cases.filter((c) => c.flipped && c.protected).length
        if (countedFlips !== flippedCount) throw new DecodeError("/replay/flippedCount does not match /replay/cases")
        if (countedProtectedFlips !== protectedFlippedCount) throw new DecodeError("/replay/protectedFlippedCount does not match /replay/cases")
      }
      // Protected-case admission law (#1208): an unsuperseded flipped protected case never reads as admitted.
      const blockingProtected = cases.some((c) => c.flipped && c.protected && !c.supersededByRuling)
      if (blockingProtected && admissionStatus === "admitted") {
        throw new DecodeError("/replay/admissionStatus cannot be admitted when a protected case flips without ruling supersession")
      }
      replay = {
        evalSetId: str(rawReplay.evalSetId, "/replay/evalSetId"),
        evalSetName: str(rawReplay.evalSetName, "/replay/evalSetName"),
        evalSetVersion,
        definitionPin: str(rawReplay.definitionPin, "/replay/definitionPin"),
        replayedAtMs: int(rawReplay.replayedAtMs, "/replay/replayedAtMs"),
        totalCases,
        flippedCount,
        protectedFlippedCount,
        admissionStatus,
        refusalReason: text(rawReplay.refusalReason, "/replay/refusalReason"),
        cases,
      }
    }

    const rawRuling = v.ruling === null || v.ruling === undefined ? null : obj(v.ruling, "/ruling")
    if ((status === "ratified") !== (rawRuling !== null)) {
      throw new DecodeError("/ruling must be present exactly when status is ratified")
    }
    const ruling: ProposalRuling | null = rawRuling === null ? null : {
      ...receipt(rawRuling, "/ruling"),
      ratifiedBy: str(rawRuling.ratifiedBy, "/ruling/ratifiedBy"),
      ratifiedAtMs: int(rawRuling.ratifiedAtMs, "/ruling/ratifiedAtMs"),
      supersededCaseIds: list(rawRuling.supersededCaseIds, "/ruling/supersededCaseIds", (x, at) => str(x, at)),
    }

    const item: GapProposal = {
      id,
      version,
      title: str(v.title, "/title"),
      summary: str(v.summary, "/summary"),
      status,
      target: {
        artifactId: str(targetObj.artifactId, "/target/artifactId"),
        kind: str(targetObj.kind, "/target/kind"),
        label: text(targetObj.label, "/target/label"),
      },
      diff: {
        summary: str(diffObj.summary, "/diff/summary"),
        before: str(diffObj.before, "/diff/before"),
        after: str(diffObj.after, "/diff/after"),
        patch: str(diffObj.patch, "/diff/patch"),
      },
      episodeIds: list(v.episodeIds, "/episodeIds", (x, at) => str(x, at)),
      author: str(v.author, "/author"),
      createdAtMs: int(v.createdAtMs, "/createdAtMs"),
      updatedAtMs: int(v.updatedAtMs, "/updatedAtMs"),
      checker: { status: checkerStatus, diagnostics: checkerDiagnostics },
      replay,
      ruling,
      supersedesProposalId: text(v.supersedesProposalId, "/supersedesProposalId"),
    }
    return { kind: "ok", item, record }
  } catch (error) {
    if (error instanceof DecodeError) return { kind: "malformed", problem: error.message }
    throw error
  }
}

export const listGapProposals = (query: RecordsQuery, fetcher: Fetch = defaultFetch): Promise<ObjectListRead<GapProposal>> =>
  listLearningObjects(GAP_PROPOSAL_TYPE, query, decodeGapProposal, fetcher)

export const readGapProposal = (id: string, fetcher: Fetch = defaultFetch): Promise<ObjectItemRead<GapProposal>> =>
  readLearningObject(GAP_PROPOSAL_TYPE, id, decodeGapProposal, fetcher)

/** Encode a status filter for `GET /v1/objects/learning.GapProposal`. */
export function proposalFilterPredicate(status: ProposalStatus | undefined): Predicate | null {
  if (status === undefined) return null
  const all: Clause[] = [{ property: "status", op: "eq", value: status }]
  return { all }
}

/** Read back the proposal status filter the server echoed. */
export function proposalStatusFromServed(served: Served | null): ProposalStatus | undefined {
  const clause = served?.filter?.all.find((c) => c.property === "status" && c.op === "eq")
  return typeof clause?.value === "string" && PROPOSAL_STATUSES.includes(clause.value as ProposalStatus)
    ? clause.value as ProposalStatus
    : undefined
}
