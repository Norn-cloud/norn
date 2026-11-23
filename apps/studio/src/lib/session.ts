import { type QueryClient, queryOptions, useQuery, useQueryClient } from "@tanstack/react-query"
import { forgetDrafts, sessionKey } from "../actions/kept-drafts.ts"
import { forgetParkedCalls } from "../actions/parked-calls.ts"
import { refreshMode } from "./live-transport.ts"

/** Who is signed in, as the Worker reports it (/api/me). Display only: the server decides every permission. */
export interface Account {
  readonly user: { readonly id: string; readonly name: string; readonly email: string }
  readonly organization: { readonly id: string; readonly name: string; readonly role: string } | null
}

/** Public auth capabilities the server advertises (/api/auth/config). Display only; the server still enforces everything. */
export interface AuthConfig {
  readonly magicLink: boolean
  readonly inviteOnly: boolean
}

export const DEFAULT_AUTH_CONFIG: AuthConfig = { magicLink: false, inviteOnly: false }

const SESSION = ["session"] as const
const AUTH_CONFIG = ["auth-config"] as const

/** A refusal the auth server explained, shown to the user as is. */
export class AuthRefusal extends Error {}

type Fetch = (input: string, init?: RequestInit) => Promise<Response>

async function readAccount(fetcher: Fetch): Promise<Account | null> {
  const response = await fetcher("/api/me", { headers: { accept: "application/json" } })
  if (response.status === 401) return null
  if (!response.ok) throw new Error(`The session check failed (${response.status}).`)
  return (await response.json()) as Account
}

/** Read which sign-in methods this deployment actually supports; fails closed (magicLink: false) when unavailable. */
export async function readAuthConfig(fetcher: Fetch = (input, init) => fetch(input, init)): Promise<AuthConfig> {
  const response = await fetcher("/api/auth/config", { headers: { accept: "application/json" } }).catch(() => null)
  if (response === null || !response.ok) return DEFAULT_AUTH_CONFIG
  const body = (await response.json().catch(() => null)) as { magicLink?: unknown; inviteOnly?: unknown } | null
  return { magicLink: body?.magicLink === true, inviteOnly: body?.inviteOnly === true }
}

/** Server-reported auth capabilities for the login screen; hides unsupported methods until confirmed. */
export function useAuthConfig() {
  return useQuery({ queryKey: AUTH_CONFIG, queryFn: () => readAuthConfig(), staleTime: 60_000, retry: false })
}

/** Forget every read made as the reader who made it (records, picker choices, the inbox): all but the session itself. */
export function forgetReads(client: QueryClient): void {
  client.removeQueries({ predicate: (query) => query.queryKey[0] !== SESSION[0] && query.queryKey[0] !== AUTH_CONFIG[0] })
}

/**
 * The session query. Whenever a read of it names a different reader than the last one (the session
 * expired and `/api/me` answered 401, someone else signed in, or the organization changed), the
 * previous reader's reads and parked calls are forgotten before the new answer is shown, the way a
 * sign-out forgets them. Hygiene only: every read is the server's decision for whoever asks.
 */
export function accountQuery(client: QueryClient, fetcher: Fetch = (input, init) => fetch(input, init)) {
  return queryOptions({
    queryKey: SESSION,
    queryFn: async () => {
      const account = await readAccount(fetcher)
      const previous = client.getQueryData<Account | null>(SESSION)
      if (previous !== undefined && sessionKey(previous) !== sessionKey(account)) {
        // A parked call's exact input and key are the previous reader's: never kept for the next,
        // nor for the same reader coming back after a tenant switch or an expiry.
        forgetParkedCalls()
        refreshMode()
        forgetReads(client)
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
  const reason = await response.json().catch(() => null) as { message?: string; error?: string } | null
  throw new AuthRefusal(response.status === 429 ? "Too many attempts. Wait a few minutes and try again." : reason?.message ?? reason?.error ?? `Sign-in failed (${response.status}).`)
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
  refreshMode()
  forgetReads(client)
  await client.invalidateQueries({ queryKey: SESSION })
}
