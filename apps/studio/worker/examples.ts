import corpus from "../../../fixtures/demo/jewelry-proposals.json"
import { ProposalError, applyProposal, verdictOf, type CheckResult, type Envelope, type Proposal } from "./proposal.ts"
import { CONTEXT, WORLD } from "./world.ts"

/**
 * The example proposals: the exact typed changes recorded in
 * fixtures/demo/jewelry-proposals.json, sent to the server's checker as they
 * are, without the model. Studio labels them "Example proposal".
 */
export const EXAMPLES: readonly { readonly chip: string; readonly proposal: Proposal }[] =
  corpus.cases.map(({ chip, artifact, pointer, value }) => ({ chip, proposal: { artifact, pointer, value } }))

/** The checker's verdict on one recorded example; `undefined` for a name that is not one. */
export function exampleVerdict(chip: string, check: (artifacts: readonly Envelope[], context: string) => CheckResult, world: readonly Envelope[] = WORLD) {
  const example = EXAMPLES.find((entry) => entry.chip === chip)
  if (example === undefined) return undefined
  try {
    return verdictOf(world, example.proposal, check(applyProposal(world, example.proposal), CONTEXT))
  } catch (error) {
    if (error instanceof ProposalError) return { proposal: example.proposal, error: error.message }
    throw error
  }
}
