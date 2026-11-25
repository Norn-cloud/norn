/**
 * Canonical learning pack object type and published question names used by capability discovery.
 * Kept in a standalone leaf module so the shell's capability gate never pulls the full learning
 * decoders or misfit analytics into the initial entry bundle.
 */
export const EPISODE_TYPE = "learning.Episode"
export const CURATED_CASE_TYPE = "learning.CuratedCase"
export const EVAL_SET_TYPE = "learning.EvalSet"
export const MISFIT_BY_LAW_QUESTION = "learning.misfit_by_law"
export const TRIAGE_OVER_TIME_QUESTION = "learning.triage_over_time"
export const GAP_PROPOSAL_TYPE = "learning.GapProposal"
