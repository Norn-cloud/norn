import { issueSignupInvite, SIGNUP_INVITE_MAX_TTL_MS } from "../worker/auth.ts"

/**
 * Issue a sign-up invitation for an invite-only Studio deployment (#1703): a single-use link bound to one
 * email address and an expiry, signed with that deployment's BETTER_AUTH_SECRET. The address must also be
 * in the deployment's STUDIO_SIGNUP_EMAILS. Run it with the deployment's secret in the environment, by
 * name, from the operator's secret store, e.g.
 *
 *   infisical run --env prod --path /auth -- bun scripts/signup-invite.ts --url https://studio.norn.cloud ada@example.com
 *
 * It prints the link to send the invitee (the invitation is in its `#invite=` fragment); nothing is stored until they sign up with it.
 */
export function parseArgs(argv: readonly string[]): { readonly url: string; readonly email: string; readonly days: number } {
  let url = ""
  let days = 7
  const rest: string[] = []
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index] ?? ""
    if (arg === "--url") url = argv[++index] ?? ""
    else if (arg === "--days") days = Number(argv[++index])
    else rest.push(arg)
  }
  const [email, ...extra] = rest
  if (email === undefined || extra.length > 0 || !/^https:\/\/[^/\s]+$/.test(url)) throw new Error("usage: bun scripts/signup-invite.ts --url https://<studio-host> [--days 1-30] <email>")
  if (!Number.isInteger(days) || days < 1 || days * 24 * 60 * 60_000 > SIGNUP_INVITE_MAX_TTL_MS) throw new Error("--days must be a whole number from 1 to 30")
  return { url, email, days }
}

export async function inviteLink(secret: string | undefined, argv: readonly string[], now = Date.now()): Promise<string> {
  if (secret === undefined || secret.length < 32) throw new Error("BETTER_AUTH_SECRET (the deployment's auth secret) is required in the environment")
  const { url, email, days } = parseArgs(argv)
  const { token, expiresAt } = await issueSignupInvite(secret, email, { now, ttlMs: days * 24 * 60 * 60_000 })
  // In the fragment: never sent to a server, so it stays out of request logs and Referer headers.
  const link = new URL("/", url)
  link.hash = `invite=${token}`
  return `${link.href}\n(for ${email.trim().toLowerCase()}, single use, expires ${new Date(expiresAt).toISOString()})`
}

if (import.meta.main) {
  try {
    console.log(await inviteLink(process.env.BETTER_AUTH_SECRET, process.argv.slice(2)))
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error))
    process.exit(2)
  }
}
