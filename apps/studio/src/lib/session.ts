import { useContext, useRef, useSyncExternalStore } from "react"
import { type QueryClient, QueryClientContext, queryOptions, useQuery, useQueryClient } from "@tanstack/react-query"
import { forgetDrafts, sessionKey } from "../actions/kept-drafts.ts"
import { forgetParkedCalls } from "../actions/parked-calls.ts"
import {
  CELL_FETCH_TIMEOUT_MS,
  defaultFetch,
  fetchWithTimeout,
  getDisplayedScope,
  getDisplayedWorkspaceScope,
  sameStudioScope,
  scopeFromAccount,
  setDisplayedWorkspaceScope,
  workspaceScopeFromAccount,
  type StudioScope,
  type WorkspaceScope,
} from "./definitions.ts"
import { closeLiveClients } from "./live-reads.ts"
import { refreshMode } from "./live-transport.ts"
import { markRecentSignOut } from "./nav-storage.ts"

/** Who is signed in, as the Worker reports it (/api/me). Display only: the server decides every permission. */
export interface Account {
  readonly user: { readonly id: string; readonly name: string; readonly email: string }
  readonly organization: { readonly id: string; readonly name: string; readonly role: string } | null
  readonly environmentId?: string | null
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

/** Read who is signed in, bounded: a server that never answers is a failed check the root can retry, never a blank Studio. */
async function readAccount(fetcher: Fetch, timeoutMs = CELL_FETCH_TIMEOUT_MS): Promise<Account | null> {
  const response = await fetchWithTimeout(fetcher, "/api/me", { headers: { accept: "application/json" } }, timeoutMs)
  if (response.status === 401) {
    markRecentSignOut()
    return null
  }
  if (!response.ok) throw new Error(`The session check failed (${response.status}).`)
  return (await response.json()) as Account
}

/** Read which sign-in methods this deployment actually supports; throws on transient failures so queries can retry. */
export async function readAuthConfig(fetcher: Fetch = (input, init) => fetch(input, init)): Promise<AuthConfig> {
  const response = await fetcher("/api/auth/config", { headers: { accept: "application/json" } })
  if (!response.ok) throw new Error(`Auth config check failed (${response.status}).`)
  const body = (await response.json().catch(() => null)) as { magicLink?: unknown; inviteOnly?: unknown } | null
  if (body === null || typeof body !== "object") throw new Error("Auth config response was invalid.")
  return { magicLink: body.magicLink === true, inviteOnly: body.inviteOnly === true }
}

/** Server-reported auth capabilities for the login screen; hides unsupported methods until confirmed. */
export function useAuthConfig() {
  return useQuery({ queryKey: AUTH_CONFIG, queryFn: () => readAuthConfig(), staleTime: 60_000, retry: 1 })
}

const isReadQuery = (query: { readonly queryKey: readonly unknown[] }) =>
  query.queryKey[0] !== SESSION[0] && query.queryKey[0] !== AUTH_CONFIG[0]

/** Forget every read made as the reader who made it (records, picker choices, the inbox): all but the session itself. */
export function forgetReads(client: QueryClient, options: { readonly refetchActive?: boolean; readonly deferActiveRefetch?: boolean } = {}): void {
  closeLiveClients()
  refreshMode()
  void client.cancelQueries({ predicate: isReadQuery })
  if (options.refetchActive) {
    client.removeQueries({ predicate: isReadQuery, type: "inactive" })
    const resetActive = () => { void client.resetQueries({ predicate: isReadQuery, type: "active" }) }
    if (options.deferActiveRefetch) queueMicrotask(resetActive)
    else resetActive()
    return
  }
  client.removeQueries({ predicate: isReadQuery })
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
export function accountQuery(client: QueryClient, fetcher: Fetch = defaultFetch, timeoutMs = CELL_FETCH_TIMEOUT_MS) {
  return queryOptions({
    queryKey: SESSION,
    queryFn: async () => {
      const account = await readAccount(fetcher, timeoutMs)
      if (fetcher === defaultFetch) setDisplayedWorkspaceScope(workspaceScopeFromAccount(account))
      const previous = client.getQueryData<Account | null>(SESSION)
      if (previous !== undefined && sessionKey(previous) !== sessionKey(account)) {
        // A parked call's exact input and key are the previous reader/scope's: never kept for the
        // next, nor for the same reader coming back after a tenant/environment switch or expiry.
        forgetParkedCalls()
        // A role change (a downgrade above all) is a different reader: the previous role's catalog,
        // counts and offered actions are forgotten even when the workspace scope stayed the same.
        const sameReader = previous !== null && account !== null
          && previous.user.id === account.user.id
          && (previous.organization?.id ?? null) === (account.organization?.id ?? null)
          && (previous.organization?.role ?? null) === (account.organization?.role ?? null)
        const nextScope = accountScopeOf(account)
        const cachedScope = client.getQueryData<StudioScope | null>(["studio-scope"])
        // If only the environment changed and `useStudioScope` / `syncStudioScopeChange` already
        // switched `["studio-scope"]` and purged old caches, keep the new scope's in-flight mode read.
        if (!(sameReader && nextScope !== undefined && cachedScope !== undefined && sameStudioScope(cachedScope, nextScope))) {
          // Defer active query resets until the new session is published.
          forgetReads(client, { refetchActive: account !== null, deferActiveRefetch: true })
        }
      }
      return account
    },
    staleTime: 60_000,
    retry: false,
  })
}

const noSubscription = () => () => {}

/** Who reads now, from the cached session answer (no `/api/me` read): a callback's view of the reader, never a render's. */
export function readerKeyIn(client: QueryClient | undefined): string {
  return sessionKey(client?.getQueryData<Account | null>(SESSION))
}

/**
 * Who reads (`sessionKey` of the cached session), re-rendered whenever the session answer changes.
 * Reads only the cache: it never asks `/api/me`, and works without a QueryClientProvider.
 */
export function useReaderKey(): string {
  const client = useContext(QueryClientContext)
  const read = () => readerKeyIn(client)
  return useSyncExternalStore(client === undefined ? noSubscription : (notify) => client.getQueryCache().subscribe(notify), read, read)
}

/** The current account: undefined while loading, null when signed out. */
export function useAccount() {
  const client = useQueryClient()
  return useQuery(accountQuery(client))
}

function usePinned<T>(key: unknown, current: T | undefined): T | undefined {
  const pinnedRef = useRef<T | undefined>(undefined)
  const prevKeyRef = useRef<unknown>(Symbol("uninitialized"))
  const active = key !== null && key !== false
  if (!Object.is(key, prevKeyRef.current)) {
    prevKeyRef.current = key
    pinnedRef.current = active ? current : undefined
  } else if (active && pinnedRef.current === undefined && current !== undefined) {
    pinnedRef.current = current
  }
  return pinnedRef.current
}

/**
 * Freeze the workspace scope that was active when a form/workflow opened (`key` changed), so a
 * background `/api/me` session refresh while the form stays open never shifts its target workspace.
 * Reads the cached session / displayed scope without issuing extra `/api/me` requests or requiring
 * a QueryClientProvider in isolated component tests.
 */
export function usePinnedWorkspaceScope(key: unknown = true): WorkspaceScope | null | undefined {
  const account = useContext(QueryClientContext)?.getQueryData<Account | null>(SESSION)
  return usePinned(key, account !== undefined ? workspaceScopeFromAccount(account) : getDisplayedWorkspaceScope())
}

/** Like `usePinnedWorkspaceScope`, requiring a provisioned cell environment (`StudioScope`). */
export function usePinnedScope(key: unknown = true): StudioScope | null | undefined {
  const account = useContext(QueryClientContext)?.getQueryData<Account | null>(SESSION)
  return usePinned(key, account !== undefined ? scopeFromAccount(account) : getDisplayedScope())
}

/** The header carrying a sign-up invitation (worker/auth.ts SIGNUP_INVITE_HEADER). */
export const SIGNUP_INVITE_HEADER = "x-norn-signup-invite"

async function call(path: string, body: Record<string, string>, headers: Record<string, string> = {}): Promise<void> {
  const response = await fetch(`/api/auth${path}`, { method: "POST", headers: { "content-type": "application/json", ...headers }, body: JSON.stringify(body) })
  if (response.ok) return
  const reason = await response.json().catch(() => null) as { message?: string; error?: string } | null
  throw new AuthRefusal(response.status === 429 ? "Too many attempts. Wait a few minutes and try again." : reason?.message ?? reason?.error ?? `Sign-in failed (${response.status}).`)
}

export const authApi = {
  signIn: (email: string, password: string) => call("/sign-in/email", { email, password }),
  signUp: (name: string, email: string, password: string, invite = "") =>
    call("/sign-up/email", { name, email, password }, invite.trim() === "" ? {} : { [SIGNUP_INVITE_HEADER]: invite.trim() }),
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
  if (signOut === authApi.signOut) setDisplayedWorkspaceScope(null)
  markRecentSignOut()
  forgetDrafts()
  forgetParkedCalls()
  forgetReads(client)
  await client.invalidateQueries({ queryKey: SESSION })
}
