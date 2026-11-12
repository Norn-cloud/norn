// Payloads that open every guard of the decision sheet's machine, shared by the document-level
// rows here and Studio's browser walk rows: both kinds of choice (one that starts an action and
// asks for confirmation, one that records), and the refusal codes the guards route on. Option ids
// match the stand-in cell's refund decision (`approve` starts an action, `reject` records).

import type { Fields, MachineEvent, MachineTransition } from "../../src/machine/machine.ts"

export function sheetPayloads(transition: MachineTransition, event: MachineEvent): readonly Fields[] | undefined {
  if (event.source.kind === "user") {
    if (event.eventId === "edit-note") return [{ value: "Checked with finance." }]
    return [{ value: "approve", confirm: true }, { value: "reject", confirm: false }]
  }
  if (event.source.kind !== "outcome" || event.source.outcome !== "refused") return undefined
  if (transition.target === "resolved") return [{ code: "decision.resolved", retryable: false }]
  if (transition.target === "expired") return [{ code: "decision.expired", retryable: false }]
  if (transition.target === "gone") return [{ code: "decision.not_found", retryable: false }]
  return [{ code: "decision.law_not_satisfied", retryable: false }]
}
