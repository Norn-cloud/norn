import { useAccount } from "@/lib/session"

/**
 * Who is reading, from the signed-in account (/api/me). It only names them:
 * the server decides what they may see, and its documents arrive already
 * masked or refused.
 */
export interface Viewer {
  readonly name: string
  /** The role in the active organization; `null` without one. */
  readonly role: string | null
}

export function useViewer(): Viewer {
  const account = useAccount().data
  if (account == null) return { name: "Not signed in", role: null }
  return { name: account.user.name || account.user.email, role: account.organization?.role ?? null }
}
