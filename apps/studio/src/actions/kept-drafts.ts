import type { Draft } from "../lib/actions.ts"

/**
 * Drafts kept for later in this tab, by the session they were written in, the action and the
 * record it was opened on. Memory only: nothing is written to the cell or the browser's storage,
 * a reload forgets them, and so does a sign-out, so the next reader in this tab never sees
 * another's unsent work.
 */
const kept = new Map<string, Draft>()

/**
 * One session: its user, organization, the role the server reports there, and the active cell
 * environment. A role change is a new reader: what the previous role was served is not this one's.
 */
export function sessionKey(account: {
  readonly user: { readonly id: string }
  readonly organization: { readonly id: string; readonly role?: string } | null
  readonly environmentId?: string | null
} | null | undefined): string {
  return JSON.stringify([account?.user.id ?? null, account?.organization?.id ?? null, account?.organization?.role ?? null, account?.environmentId ?? null])
}

const key = (session: string, draft: string) => `${session}\u0000${draft}`

export const keptDrafts = {
  get: (session: string, draft: string): Draft | undefined => kept.get(key(session, draft)),
  set: (session: string, draft: string, value: Draft): void => { kept.set(key(session, draft), value) },
  delete: (session: string, draft: string): void => { kept.delete(key(session, draft)) },
}

/** Forget every kept draft: the session that wrote them has ended. */
export function forgetDrafts(): void { kept.clear() }
