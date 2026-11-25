import { useEffect, useState, type FormEvent } from "react"
import { Button } from "@/components/ui/button"
import { Dot } from "@/components/system/status-dot"
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { EXPECTED_ORG_HEADER } from "@/lib/definitions"
import nornMark from "@/assets/norn-mark.webp"

/**
 * The two pages an assistant's OAuth flow sends a person to (MCP authorization): sign in with
 * their Studio account, then see who is asking for what and allow or deny it. The authorization
 * server signed the query these pages carry; they pass it back unchanged and follow the URL it
 * answers with. They grant nothing themselves.
 */

/** The authorization server's signed query, passed back so it can resume the flow it started. */
export function signedQuery(search: string): string | undefined {
  const params = new URLSearchParams(search)
  if (!params.has("sig")) return undefined
  const signed = new Set(params.getAll("ba_param"))
  if (signed.size === 0) return undefined
  const kept = new URLSearchParams()
  for (const [key, value] of params) if (key === "sig" || key === "ba_param" || signed.has(key)) kept.append(key, value)
  return kept.toString()
}

/** Where to go next: a relative path on this origin, or the assistant's https callback. */
export function nextUrl(body: unknown, origin: string): string | null {
  const url = (body as { url?: unknown } | null)?.url
  if (typeof url !== "string") return null
  try {
    const next = new URL(url, origin)
    return next.origin === origin || next.protocol === "https:" || next.hostname === "localhost" || next.hostname === "127.0.0.1" ? next.href : null
  } catch {
    return null
  }
}

async function post(path: string, body: Record<string, unknown>, extraHeaders: HeadersInit = {}): Promise<{ readonly ok: boolean; readonly body: unknown; readonly status: number }> {
  const response = await fetch(`/api/auth${path}`, { method: "POST", headers: { "content-type": "application/json", accept: "application/json", ...extraHeaders }, body: JSON.stringify(body) })
  return { ok: response.ok, status: response.status, body: await response.json().catch(() => null) }
}

function Frame({ title, description, children }: { readonly title: string; readonly description: string; readonly children: React.ReactNode }) {
  return (
    <main className="flex min-h-svh items-center justify-center bg-canvas p-4">
      <div className="w-full max-w-sm space-y-6">
        <div className="flex items-center gap-2">
          <img src={nornMark} alt="" width={32} height={32} className="size-8 object-contain" />
          <span>Norn</span>
        </div>
        <Card>
          <CardHeader>
            <CardTitle>{title}</CardTitle>
            <CardDescription>{description}</CardDescription>
          </CardHeader>
          {children}
        </Card>
      </div>
    </main>
  )
}

function Failure({ text }: { readonly text: string | null }) {
  return text === null ? null : <p role="alert" className="flex items-center gap-2 text-body text-foreground"><Dot tone="bad" />{text}</p>
}

/** Sign in to continue connecting an assistant. Accounts are made in Studio, not here. */
export function OAuthSignIn() {
  const [pending, setPending] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)
  const query = signedQuery(window.location.search)

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    setPending(true)
    setFailure(null)
    const answer = await post("/sign-in/email", { email: String(form.get("email") ?? ""), password: String(form.get("password") ?? ""), oauth_query: query }).catch(() => null)
    const next = answer?.ok === true ? nextUrl(answer.body, window.location.origin) : null
    if (next !== null) {
      window.location.assign(next)
      return
    }
    setPending(false)
    setFailure(answer === null ? "The sign-in service did not answer. Try again." : answer.status === 429 ? "Too many attempts. Wait a few minutes and try again." : answer.ok ? "Signed in, but the connection could not continue. Start it again from your assistant." : ((answer.body as { message?: string } | null)?.message ?? "Sign-in failed."))
  }

  if (query === undefined) return <Frame title="Link expired" description="Start connecting Norn again from your assistant.">{null}</Frame>
  return (
    <Frame title="Sign in to Norn" description="Your assistant asked to connect to Norn. Sign in with your Studio account to continue.">
      <form onSubmit={(event) => void submit(event)}>
        <CardContent className="grid gap-4">
          <label className="grid gap-1.5 text-body"><span>Email</span><Input required name="email" type="email" autoComplete="email" /></label>
          <label className="grid gap-1.5 text-body"><span>Password</span><Input required name="password" type="password" autoComplete="current-password" /></label>
          <Failure text={failure} />
        </CardContent>
        <CardFooter><Button type="submit" className="w-full" disabled={pending}>{pending ? "Signing in…" : "Sign in"}</Button></CardFooter>
      </form>
    </Frame>
  )
}

const CONSENT_APPROVAL_HEADER = "x-norn-consent-approval"

interface Requester { readonly name: string; readonly site: string | null }
interface Workspace { readonly user: string; readonly organizationId: string | null; readonly organization: string | null; readonly approvalToken: string | null }

/** What the person is agreeing to: the client's name, where the answer goes, and the workspace it acts in. */
export function consentFacts(search: string, client: { readonly client_name?: unknown; readonly client_uri?: unknown } | null): { readonly requester: Requester; readonly callbackHost: string | null } {
  const params = new URLSearchParams(search)
  const host = (value: unknown) => { try { return typeof value === "string" ? new URL(value).host : null } catch { return null } }
  const name = typeof client?.client_name === "string" && client.client_name.trim() !== "" ? client.client_name : "An unnamed application"
  return { requester: { name, site: host(client?.client_uri) }, callbackHost: host(params.get("redirect_uri")) }
}

/** Allow or deny an assistant acting as you in your workspace. */
export function OAuthConsent() {
  const query = signedQuery(window.location.search)
  const clientId = new URLSearchParams(window.location.search).get("client_id")
  const [client, setClient] = useState<{ client_name?: unknown; client_uri?: unknown } | null | undefined>(undefined)
  const [workspace, setWorkspace] = useState<Workspace | null | undefined>(undefined)
  const [pending, setPending] = useState(false)
  const [failure, setFailure] = useState<string | null>(null)

  useEffect(() => {
    if (clientId === null) { setClient(null); return }
    void fetch(`/api/auth/oauth2/public-client?${new URLSearchParams({ client_id: clientId })}`, { headers: { accept: "application/json" } })
      .then(async (response) => setClient(response.ok ? await response.json() : null))
      .catch(() => setClient(null))
    if (query === undefined) { setWorkspace(null); return }
    void fetch("/api/oauth/consent-context", {
      method: "POST",
      credentials: "same-origin",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: JSON.stringify({ oauth_query: query }),
    })
      .then(async (response) => {
        if (!response.ok) { setWorkspace(null); return }
        const context = await response.json() as {
          user: { email: string }
          organization: { id: string; name: string } | null
          approvalToken: string | null
        }
        setWorkspace({
          user: context.user.email,
          organizationId: context.organization?.id ?? null,
          organization: context.organization?.name ?? null,
          approvalToken: typeof context.approvalToken === "string" && context.approvalToken !== "" ? context.approvalToken : null,
        })
      })
      .catch(() => setWorkspace(null))
  }, [clientId, query])

  async function decide(accept: boolean) {
    if (accept && (workspace?.organizationId == null || workspace.approvalToken == null)) {
      setFailure("Select an active workspace in Studio before authorizing an assistant.")
      return
    }
    setPending(true)
    setFailure(null)
    const headers: HeadersInit = accept && workspace?.organizationId != null && workspace.approvalToken != null
      ? { [EXPECTED_ORG_HEADER]: workspace.organizationId, [CONSENT_APPROVAL_HEADER]: workspace.approvalToken }
      : {}
    const answer = await post("/oauth2/consent", { accept, oauth_query: query }, headers).catch(() => null)
    const next = answer?.ok === true ? nextUrl(answer.body, window.location.origin) : null
    if (next !== null) {
      window.location.assign(next)
      return
    }
    setPending(false)
    const serverMessage = (answer?.body as { message?: unknown } | null)?.message
    setFailure(typeof serverMessage === "string" && serverMessage !== "" ? serverMessage : "The decision was not recorded. Start connecting again from your assistant.")
  }

  if (query === undefined || clientId === null) return <Frame title="Link expired" description="Start connecting Norn again from your assistant.">{null}</Frame>
  if (client === undefined || workspace === undefined) return null
  const { requester, callbackHost } = consentFacts(window.location.search, client)
  const readyWorkspace = workspace?.organizationId != null && workspace.organization != null && workspace.approvalToken != null
  return (
    <Frame
      title={`Allow ${requester.name} to use Norn?`}
      description={readyWorkspace ? `It will act as ${workspace.user} in ${workspace.organization}.` : "You do not have an active workspace to connect right now."}
    >
      <CardContent className="grid gap-3 text-body">
        <p>It can read what you may read and ask Norn to run actions you may run. Norn's rules decide every request, exactly as they do in Studio, and refuse anything you could not do yourself.</p>
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-muted-foreground">
          {requester.site === null ? null : <><dt>Site</dt><dd className="font-mono">{requester.site}</dd></>}
          {callbackHost === null ? null : <><dt>Returns to</dt><dd className="font-mono">{callbackHost}</dd></>}
        </dl>
        <p className="text-muted-foreground">You can disconnect it at any time in your assistant's settings.</p>
        <Failure text={failure} />
      </CardContent>
      <CardFooter className="flex gap-2 pt-4">
        <Button variant="outline" className="flex-1" disabled={pending} onClick={() => void decide(false)}>Deny</Button>
        <Button className="flex-1" disabled={pending || !readyWorkspace} onClick={() => void decide(true)}>Allow</Button>
      </CardFooter>
    </Frame>
  )
}

/** The OAuth page for this path, or null when the path is not one. */
export function oauthPage(pathname: string): (() => React.ReactNode) | null {
  if (pathname === "/oauth/sign-in") return OAuthSignIn
  if (pathname === "/oauth/consent") return OAuthConsent
  return null
}
