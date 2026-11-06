import { useQuery, useQueryClient } from "@tanstack/react-query"

/** Who is signed in, as the Worker reports it (/api/me). Display only: the server decides every permission. */
export interface Account {
  readonly user: { readonly id: string; readonly name: string; readonly email: string }
  readonly organization: { readonly id: string; readonly name: string; readonly role: string } | null
}

const SESSION = ["session"] as const

/** A refusal the auth server explained, shown to the user as is. */
export class AuthRefusal extends Error {}

async function readAccount(): Promise<Account | null> {
  const response = await fetch("/api/me", { headers: { accept: "application/json" } })
  if (response.status === 401) return null
  if (!response.ok) throw new Error(`The session check failed (${response.status}).`)
  return (await response.json()) as Account
}

/** The current account: undefined while loading, null when signed out. */
export function useAccount() {
  return useQuery({ queryKey: SESSION, queryFn: readAccount, staleTime: 60_000, retry: false })
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
