import { useMemo } from "react"
import { sameStudioScope, useStudioScope, type StudioScope } from "../definitions.ts"
import { useMode, type Mode } from "../live-transport.ts"
import { CURATED_CASE_TYPE, EVAL_SET_TYPE } from "./cases.ts"
import { EPISODE_TYPE } from "./episode.ts"
import { MISFIT_BY_LAW_QUESTION, TRIAGE_OVER_TIME_QUESTION } from "./misfit.ts"
import { GAP_PROPOSAL_TYPE } from "./proposals.ts"

/**
 * Server-driven capability gating for Studio's Learning destinations (#1220, PR #1631 S1).
 *
 * All visibility and route entry decisions derive from the single scoped catalog read
 * (`GET /v1/definitions`, shared via `useMode()`). No client-side permission logic is used:
 * a destination is enabled strictly when the current scoped cell advertises every resource it
 * consumes:
 *
 * - Episodes (`/learning/episodes`, `/episodes/$id`) → object type `learning.Episode`
 * - Cases & eval sets (`/learning/cases`) → object types `learning.CuratedCase` + `learning.EvalSet`
 * - Misfit board (`/learning/misfit`) → published questions `learning.misfit_by_law` +
 *   `learning.triage_over_time` (read from provisional `publishedQuestions` catalog metadata)
 * - Proposals (`/learning/proposals`, `/learning/proposals/$id`) → object type `learning.GapProposal`
 *
 * While the capability read is pending, failed, missing, or in recorded mode, every destination
 * fails closed (`false`): no Learning nav entry appears and direct URLs mount an unavailable state
 * before issuing any page data fetches.
 *
 * PROVISIONAL CONTRACT: `publishedQuestions` on `GET /v1/definitions` is provisional capability
 * metadata until the backend learning pack lane pins published-question discovery on the catalog.
 */

export type LearningSurface = "episodes" | "cases" | "misfit" | "proposals"

export type CapabilityStatus = "ready" | "pending" | "error" | "unavailable"

export interface LearningCapabilities {
  readonly status: CapabilityStatus
  /** Server-reported scope (`orgId` + `environmentId`) whose catalog authorized these flags, or null when closed. */
  readonly scope: StudioScope | null
  readonly episodes: boolean
  readonly cases: boolean
  readonly misfit: boolean
  readonly proposals: boolean
  readonly objectTypes: ReadonlySet<string>
  readonly publishedQuestions: ReadonlySet<string>
}

export const REQUIRED_RESOURCES: Readonly<Record<LearningSurface, readonly string[]>> = {
  episodes: [EPISODE_TYPE],
  cases: [CURATED_CASE_TYPE, EVAL_SET_TYPE],
  misfit: [MISFIT_BY_LAW_QUESTION, TRIAGE_OVER_TIME_QUESTION],
  proposals: [GAP_PROPOSAL_TYPE],
}

export const LEARNING_SECTION_SURFACE: Readonly<Record<string, LearningSurface>> = {
  "/learning/episodes": "episodes",
  "/learning/cases": "cases",
  "/learning/misfit": "misfit",
  "/learning/proposals": "proposals",
}

const EMPTY_SET: ReadonlySet<string> = new Set()

const CLOSED = (status: CapabilityStatus): LearningCapabilities => ({
  status,
  scope: null,
  episodes: false,
  cases: false,
  misfit: false,
  proposals: false,
  objectTypes: EMPTY_SET,
  publishedQuestions: EMPTY_SET,
})

/** Extract active admitted object type names from a live cell's `GET /v1/definitions` artifacts. */
function activeObjectTypes(mode: Extract<Mode, { readonly kind: "live" }>): ReadonlySet<string> {
  const names = new Set<string>()
  for (const artifact of mode.catalog.document) {
    if (artifact === null || typeof artifact !== "object") continue
    if (artifact.kind !== "entity") continue
    if ((artifact as { lifecycle?: unknown }).lifecycle === "retired") continue
    const payload = artifact.payload as { objectType?: { name?: unknown } } | undefined
    const name = payload?.objectType?.name
    if (typeof name === "string" && name !== "") names.add(name)
  }
  return names
}

/**
 * Pure capability evaluation from the single scoped `Mode` read (`GET /v1/definitions`).
 * Always fails closed when `mode` is undefined, failed, not a live scoped cell, or belongs to a
 * different scope than `state.expectedScope` (for example after a same-org environment switch).
 */
export function learningCapabilitiesOf(
  mode: Mode | undefined,
  state: {
    readonly isPending?: boolean
    readonly isError?: boolean
    readonly expectedScope?: StudioScope | null
  } = {},
): LearningCapabilities {
  if (state.isError) return CLOSED("error")
  if (state.isPending || mode === undefined) return CLOSED("pending")
  if (mode.kind !== "live") return CLOSED("unavailable")
  if (state.expectedScope !== undefined && !sameStudioScope(mode.scope, state.expectedScope)) {
    return CLOSED("pending")
  }

  const objectTypes = activeObjectTypes(mode)
  const publishedQuestions = mode.publishedQuestions

  return {
    status: "ready",
    scope: mode.scope,
    episodes: objectTypes.has(EPISODE_TYPE),
    cases: objectTypes.has(CURATED_CASE_TYPE) && objectTypes.has(EVAL_SET_TYPE),
    misfit: publishedQuestions.has(MISFIT_BY_LAW_QUESTION) && publishedQuestions.has(TRIAGE_OVER_TIME_QUESTION),
    proposals: objectTypes.has(GAP_PROPOSAL_TYPE),
    objectTypes,
    publishedQuestions,
  }
}

/** Whether a sidebar section path should be shown given the current cell's learning capabilities. */
export function isSectionVisible(path: string, capabilities: LearningCapabilities): boolean {
  const surface = LEARNING_SECTION_SURFACE[path]
  return surface === undefined ? true : capabilities[surface]
}

/**
 * Single shared capability read for both sidebar visibility and route entry, backed by `useMode()`
 * (`GET /v1/definitions` keyed by the current server-reported scope).
 */
export function useLearningCapabilities(): LearningCapabilities & { readonly refetch: () => void } {
  const scope = useStudioScope()
  const mode = useMode()
  const expectedScope = scope.isSuccess ? scope.data : undefined
  const capabilities = useMemo(
    () => learningCapabilitiesOf(mode.data, {
      isPending: scope.isPending || mode.isPending,
      isError: scope.isError || mode.isError,
      ...(expectedScope === undefined ? {} : { expectedScope }),
    }),
    [mode.data, mode.isPending, mode.isError, scope.isPending, scope.isError, expectedScope],
  )
  return { ...capabilities, refetch: () => { void mode.refetch() } }
}
