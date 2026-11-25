import { useEffect, useState, type FormEvent } from "react"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { AuthRefusal, DEFAULT_AUTH_CONFIG, authApi, useAuthConfig, useRefreshAccount, type AuthConfig } from "@/lib/session"
import { cn } from "@/lib/utils"
import nornMark from "@/assets/norn-mark.webp"

export type Mode = "sign-in" | "sign-up" | "email-link"

/** Shown after an accepted sign-up response (covers both new accounts and existing ones without revealing which). */
export const SIGNED_UP = "If this address can sign up, you can now sign in with your email and password."

const MODES: readonly { readonly mode: Mode; readonly label: string }[] = [
  { mode: "sign-in", label: "Sign in" },
  { mode: "sign-up", label: "Create account" },
  { mode: "email-link", label: "Email link" },
]

function Field({ label, ...props }: { readonly label: string } & React.ComponentProps<typeof Input>) {
  return (
    <label className="grid gap-1.5 text-body">
      <span className="">{label}</span>
      <Input required {...props} />
    </label>
  )
}

/**
 * The only page a signed-out visitor sees: sign in, create an account, or (when configured on the
 * server) ask for an email link. Capabilities come from `/api/auth/config`, never a client flag.
 */
/** The invitation an operator's sign-up link carries in its fragment (`#invite=`), if any. */
export function inviteFromLocation(hash: string = typeof window === "undefined" ? "" : window.location.hash): string {
  return new URLSearchParams(hash.replace(/^#/, "")).get("invite")?.trim() ?? ""
}

/** Drop the invitation from the address bar once read, so it is not kept in history or copied along with the URL. */
function scrubInvite() {
  if (typeof window === "undefined" || inviteFromLocation() === "") return
  window.history.replaceState(window.history.state, "", `${window.location.pathname}${window.location.search}`)
}

export function LoginPage({ initial, config }: { readonly initial?: Mode; readonly config?: AuthConfig }) {
  const [invite] = useState(() => inviteFromLocation())
  useEffect(scrubInvite, [])
  const authConfig = useAuthConfig()
  const activeConfig = config ?? authConfig.data ?? DEFAULT_AUTH_CONFIG
  const modes = activeConfig.magicLink ? MODES : MODES.filter((entry) => entry.mode !== "email-link")
  // An invitation link opens on "Create account".
  const [mode, setMode] = useState<Mode>(initial ?? (invite === "" ? "sign-in" : "sign-up"))
  const activeMode: Mode = mode === "email-link" && !activeConfig.magicLink ? "sign-in" : mode
  const [pending, setPending] = useState(false)
  const [message, setMessage] = useState<{ readonly tone: "error" | "info"; readonly text: string } | null>(null)
  // The address carries across tabs: after an accepted sign-up the visitor signs in without retyping it.
  const [email, setEmail] = useState("")
  const refresh = useRefreshAccount()

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    const value = (name: string) => String(form.get(name) ?? "")
    setPending(true)
    setMessage(null)
    try {
      if (activeMode === "sign-in") await authApi.signIn(value("email"), value("password"))
      if (activeMode === "sign-up") {
        // Server refuses non-allowlisted sign-ups explicitly (403); only an accepted 2xx transitions to sign-in.
        await authApi.signUp(value("name"), value("email"), value("password"), value("invite"))
        setMode("sign-in")
        setMessage({ tone: "info", text: SIGNED_UP })
        setPending(false)
        return
      }
      if (activeMode === "email-link") {
        await authApi.emailLink(value("email"))
        setMessage({ tone: "info", text: "If that address has an account, a sign-in link is on its way. It expires in 10 minutes." })
        setPending(false)
        return
      }
    } catch (error) {
      setPending(false)
      setMessage({ tone: "error", text: error instanceof AuthRefusal ? error.message : "The sign-in service did not answer. Try again." })
      return
    }
    setPending(false)
    // Signed in: the root re-reads the account and shows its own retry if that read fails.
    await refresh()
  }

  const heading = modes.find((entry) => entry.mode === activeMode)?.label ?? "Sign in"

  return (
    <main className="flex min-h-svh items-center justify-center bg-surface-subtle p-4">
      <div className="w-full max-w-sm space-y-6">
        <div className="flex items-center gap-2">
          <img src={nornMark} alt="" width={32} height={32} className="size-8 object-contain" />
          <span className="">Norn Studio</span>
        </div>
        <Card>
          <CardHeader>
            <CardTitle>{heading}</CardTitle>
            <CardDescription>Studio is available to signed-in members of an organization.</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div role="tablist" aria-label="How to sign in" className={cn("grid gap-1 rounded-md border bg-surface-subtle p-1 text-meta", modes.length === 3 ? "grid-cols-3" : "grid-cols-2")}>
              {modes.map((entry) => (
                <button key={entry.mode} type="button" role="tab" aria-selected={activeMode === entry.mode} disabled={pending} onClick={() => { setMode(entry.mode); setMessage(null) }}
                  className={cn("rounded-sm px-2 py-1 outline-none focus-visible:ring-3 focus-visible:ring-ring/30 disabled:cursor-not-allowed disabled:opacity-60", activeMode === entry.mode ? "bg-card shadow-xs" : "text-muted-foreground hover:text-foreground")}>
                  {entry.label}
                </button>
              ))}
            </div>
            {config === undefined && authConfig.isError && (
              <p role="status" className="flex items-center justify-between gap-2 text-meta text-muted-foreground">
                <span>Could not load all sign-in options.</span>
                <button type="button" className="underline underline-offset-4 hover:text-foreground" onClick={() => void authConfig.refetch()}>Retry</button>
              </p>
            )}
            {/* While a request is out the form is frozen: its answer can only land on the mode and address it was sent with. */}
            <form key={activeMode} onSubmit={submit} className="grid gap-3" aria-busy={pending || undefined}>
              <fieldset disabled={pending} className="contents">
              {activeMode === "sign-up" && activeConfig.inviteOnly && (
                <p className="rounded-md border bg-surface-subtle px-3 py-2 text-meta text-muted-foreground">
                  This alpha is invite-only. Open the invitation link your admin sent, with your invited email address, or ask your admin for an invitation.
                </p>
              )}
              {activeMode === "sign-up" && activeConfig.inviteOnly && <Field label="Invitation" name="invite" autoComplete="off" defaultValue={invite} />}
              {activeMode === "sign-up" && <Field label="Name" name="name" autoComplete="name" />}
              <Field label="Email" name="email" type="email" autoComplete="email" defaultValue={email} onChange={(event) => setEmail(event.currentTarget.value)} />
              {activeMode !== "email-link" && (
                <Field label="Password" name="password" type="password" minLength={activeMode === "sign-up" ? 12 : undefined} maxLength={128}
                  autoComplete={activeMode === "sign-up" ? "new-password" : "current-password"} />
              )}
              {activeMode === "sign-up" && <p className="text-meta text-muted-foreground">At least 12 characters. You start with your own workspace.</p>}
              {message && <p role={message.tone === "error" ? "alert" : "status"} className={cn("text-meta", message.tone === "error" ? "text-destructive" : "text-muted-foreground")}>{message.text}</p>}
              <Button type="submit" disabled={pending}>{pending ? "Working…" : activeMode === "email-link" ? "Send link" : heading}</Button>
              </fieldset>
            </form>
          </CardContent>
        </Card>
      </div>
    </main>
  )
}
