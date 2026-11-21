import type { ParkedCall } from "../lib/screen/call.ts"

/**
 * Calls that parked for approval in this tab, by the session that sent them and the form they were
 * sent from (an action and the record it was opened on), so reopening the form shows the parked call
 * and not an empty draft. A convenience only: the cell holds the call (`GET /v1/decisions:requested`),
 * says when it may run, and runs only exactly the call it holds. Memory only, like kept drafts: a
 * reload or a sign-out forgets them, and the next reader in this tab never sees another's call.
 */
const parked = new Map<string, ParkedCall>()

const key = (session: string, form: string) => `${session}\u0000${form}`

export const parkedCalls = {
  get: (session: string, form: string): ParkedCall | undefined => parked.get(key(session, form)),
  /** Keep a parked call exactly as it was sent: its decision, its encoded input and its key. */
  set: (session: string, form: string, call: ParkedCall): void => { parked.set(key(session, form), { decision: { ...call.decision }, input: call.input, key: call.key }) },
  delete: (session: string, form: string): void => { parked.delete(key(session, form)) },
}

/** Forget every parked call: the session that sent them has ended. */
export function forgetParkedCalls(): void { parked.clear() }
