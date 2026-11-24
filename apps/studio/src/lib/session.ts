import { type QueryClient, queryOptions, useQuery, useQueryClient } from "@tanstack/react-query"
import { forgetDrafts, sessionKey } from "../actions/kept-drafts.ts"
import { forgetParkedCalls } from "../actions/parked-calls.ts"
import { sameStudioScope, type StudioScope } from "./definitions.ts"
import { refreshMode } from "./live-transport.ts"

/** Who is signed in, as the Worker reports it (/api/me). Display only: the server decides every permission. */
export interface Account {
  readonly user: { readonly id: string; readonly name: string; readonly email: string }
  readonly organization: { readonly id: string; readonly name: string; readonly role: string } | null
  readonly environmentId?: string | null
}

const SESSION = ["session"] as const

/** A refusal the auth server explained, shown to the user as is. */
export class AuthRefusal extends Error {}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

async function readAccount(fetcher: Fetch): Promise<Account | null> {
  const response = await fetcher("/api/me", { headers: { accept: "application/json" } })
  if (response.status === 401) return null
  if (!response.ok) throw new Error(`The session check failed (${response.status}).`)
  return (await response.json()) as Account
}

/** Forget every read made as the reader who made it (records, picker choices, the inbox): all but the session itself. */
function forgetReads(client: QueryClient): void {
  // Drop the cached live/recorded mode so the next scoped cell's catalog and capabilities are read fresh.
  refreshMode()
  const isOther = (query: { readonly queryKey: readonly unknown[] }) => query.queryKey[0] !== SESSION[0]
  void client.cancelQueries({ predicate: isOther })
  client.removeQueries({ predicate: isOther })
}

function accountScopeOf(account: Account | null): StudioScope | null | undefined {
  if (account === null) return null
  if (account.environmentId === undefined) return undefined
  return account.organization === null || account.environmentId === null
    ? null
    : { orgId: account.organization.id, environmentId: account.environmentId }
}

/**
 * The session query. Whenever a read of it names a different reader or scope than the last one
 * (the session expired and `/api/me` answered 401, someone else signed in, the organization
 * changed, or the organization's cell environment changed), the previous reader/scope's reads and
 * parked calls are forgotten before the new answer is shown, the way a sign-out forgets them.
 * Hygiene only: every read is the server's decision for whoever asks.
 */
export function accountQuery(client: QueryClient, fetcher: Fetch = (input, init) => fetch(input, init)) {
  return queryOptions({
    queryKey: SESSION,
    queryFn: async () => {
      const account = await readAccount(fetcher)
      const previous = client.getQueryData<Account | null>(SESSION)
      if (previous !== undefined && sessionKey(previous) !== sessionKey(account)) {
        // A parked call's exact input and key are the previous reader/scope's: never kept for the
        // next, nor for the same reader coming back after a tenant/environment switch or expiry.
        forgetParkedCalls()
        const sameReader = previous !== null && account !== null
          && previous.user.id === account.user.id
          && (previous.organization?.id ?? null) === (account.organization?.id ?? null)
        const nextScope = accountScopeOf(account)
        const cachedScope = client.getQueryData<StudioScope | null>(["studio-scope"])
        // If only the environment changed and `useStudioScope` / `syncStudioScopeChange` already
        // switched `["studio-scope"]` and purged old caches, keep the new scope's in-flight mode read.
        if (!(sameReader && nextScope !== undefined && cachedScope !== undefined && sameStudioScope(cachedScope, nextScope))) {
          forgetReads(client)
        }
      }
      return account
    },
    staleTime: 60_000,
    retry: false,
  })
}

/** The current account: undefined while loading, null when signed out. */
export function useAccount() {
  const client = useQueryClient()
  return useQuery(accountQuery(client))
}

async function call(path: string, body: Record<string, string>): Promise<void> {
  const response = await fetch(`/api/auth${path}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })
  if (response.ok) return
  const reason = await response.json().catch(() => null) as { message?: string } | null
  throw new AuthRefusal(response.status === 429 ? "Too many attempts. Wait a few minutes and try again." : reason?.message ?? `Sign-in failed (${response.status}).`)
}

export const authApi = {
  signIn: (email: string, password: string) => call("/sign-in/email", { email, password }),
  signUp: (name: string, email: string, password: string) => call("/sign-up/email", { name, email, password }),
  emailLink: (email: string) => call("/sign-in/magic-link", { email, callbackURL: "/" }),
  signOut: () => call("/sign-out", {}),
}

/**
 * Re-read the session after signing in or out. A failed re-read never throws: the
 * sign-in itself succeeded, and the root shows the failure with a retry.
 */
export function useRefreshAccount() {
  const client = useQueryClient()
  return () => client.invalidateQueries({ queryKey: SESSION })
}

/**
 * End the session, then forget everything read or kept as that reader (records, picker choices,
 * the inbox, unsent drafts, parked calls) so the next reader in this tab never sees it, and re-read the session. A sign-out the
 * server did not complete forgets nothing and rethrows.
 */
export async function endSession(client: QueryClient, signOut: () => Promise<void> = authApi.signOut): Promise<void> {
  await signOut()
  forgetDrafts()
  forgetParkedCalls()
  forgetReads(client)
  await client.invalidateQueries({ queryKey: SESSION })
}
