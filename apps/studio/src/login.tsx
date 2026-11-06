import { useState, type FormEvent } from "react"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { AuthRefusal, authApi, useRefreshAccount } from "@/lib/session"
import { cn } from "@/lib/utils"

export type Mode = "sign-in" | "sign-up" | "email-link"

/** Shown after sign-up whether or not the address already had an account. */
export const SIGNED_UP = "Account ready. Sign in with your email and password."

const MODES: readonly { readonly mode: Mode; readonly label: string }[] = [
  { mode: "sign-in", label: "Sign in" },
  { mode: "sign-up", label: "Create account" },
  { mode: "email-link", label: "Email link" },
]

function Field({ label, ...props }: { readonly label: string } & React.ComponentProps<typeof Input>) {
  return (
    <label className="grid gap-1.5 text-sm">
      <span className="font-medium">{label}</span>
      <Input required {...props} />
    </label>
  )
}

/** The only page a signed-out visitor sees: sign in, create an account, or ask for an email link. */
export function LoginPage({ initial = "sign-in" }: { readonly initial?: Mode }) {
  const [mode, setMode] = useState<Mode>(initial)
  const [pending, setPending] = useState(false)
  const [message, setMessage] = useState<{ readonly tone: "error" | "info"; readonly text: string } | null>(null)
  const refresh = useRefreshAccount()

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    const value = (name: string) => String(form.get(name) ?? "")
    setPending(true)
    setMessage(null)
    try {
      if (mode === "sign-in") await authApi.signIn(value("email"), value("password"))
      if (mode === "sign-up") {
        // Sign-up answers the same whether or not the address is registered, and starts no session.
        await authApi.signUp(value("name"), value("email"), value("password"))
        setMode("sign-in")
        setMessage({ tone: "info", text: SIGNED_UP })
        setPending(false)
        return
      }
      if (mode === "email-link") {
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

  return (
    <main className="flex min-h-svh items-center justify-center bg-surface-subtle p-4">
      <div className="w-full max-w-sm space-y-6">
        <div className="flex items-center gap-2">
          <span className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary text-sm font-semibold text-primary-foreground">N</span>
          <span className="font-medium">Norn Studio</span>
        </div>
        <Card>
          <CardHeader>
            <CardTitle>{MODES.find((entry) => entry.mode === mode)?.label}</CardTitle>
            <CardDescription>Studio is available to signed-in members of an organization.</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div role="tablist" aria-label="How to sign in" className="grid grid-cols-3 gap-1 rounded-md border bg-surface-subtle p-1 text-meta">
              {MODES.map((entry) => (
                <button key={entry.mode} type="button" role="tab" aria-selected={mode === entry.mode} onClick={() => { setMode(entry.mode); setMessage(null) }}
                  className={cn("rounded-sm px-2 py-1 outline-none focus-visible:ring-3 focus-visible:ring-ring/30", mode === entry.mode ? "bg-card font-medium shadow-xs" : "text-muted-foreground hover:text-foreground")}>
                  {entry.label}
                </button>
              ))}
            </div>
            <form key={mode} onSubmit={submit} className="grid gap-3">
              {mode === "sign-up" && <Field label="Name" name="name" autoComplete="name" />}
              <Field label="Email" name="email" type="email" autoComplete="email" />
              {mode !== "email-link" && (
                <Field label="Password" name="password" type="password" minLength={mode === "sign-up" ? 12 : undefined} maxLength={128}
                  autoComplete={mode === "sign-up" ? "new-password" : "current-password"} />
              )}
              {mode === "sign-up" && <p className="text-meta text-muted-foreground">At least 12 characters. You start with your own organization.</p>}
              {message && <p role={message.tone === "error" ? "alert" : "status"} className={cn("text-meta", message.tone === "error" ? "text-destructive" : "text-muted-foreground")}>{message.text}</p>}
              <Button type="submit" disabled={pending}>{pending ? "Working…" : mode === "email-link" ? "Send link" : MODES.find((entry) => entry.mode === mode)?.label}</Button>
            </form>
          </CardContent>
        </Card>
      </div>
    </main>
  )
}
