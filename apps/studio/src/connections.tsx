import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Cable, Plug, TriangleAlert } from "lucide-react"
import { useState, type ReactNode } from "react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { api, ConnectorError, fixLabel, type AccountDetail, type FixAction, type Inventory, type Requirement, type Status } from "./lib/connectors.ts"

/**
 * Connections (#772): the person's sign-in to the tool gateway and the accounts they connected
 * there. Accounts are connected and reconnected in the gateway's own form, so credentials never
 * pass through Norn. Every refusal is the server's, shown with the fix it names.
 */

const Note = ({ children }: { children: ReactNode }) => <p className="px-4 py-3 text-sm text-muted-foreground">{children}</p>
const go = ({ url }: { url: string }) => window.location.assign(url)

/** Start the fix a refusal names: each one ends in the gateway's sign-in or connection form. */
async function fix(action: FixAction): Promise<void> {
  if (action.kind === "sign_in_gateway") return go(await api<{ url: string }>("/link", { method: "POST" }))
  if (action.kind === "connect_account") return go(await api<{ url: string }>(`/apps/${encodeURIComponent(action.app)}/accounts/${encodeURIComponent(action.requirement)}/connect`, { method: "POST", body: {} }))
  if (action.kind === "reconnect_account" && action.app !== null) {
    const { requirements } = await api<{ requirements: readonly Requirement[] }>(`/apps/${encodeURIComponent(action.app)}/accounts`)
    const requirement = requirements.find((entry) => entry.account === action.account)?.requirement
    if (requirement !== undefined) return go(await api<{ url: string }>(`/apps/${encodeURIComponent(action.app)}/accounts/${encodeURIComponent(requirement)}/connect`, { method: "POST", body: { account: action.account } }))
  }
  throw new ConnectorError(409, "connectors.fix_unavailable", "Reconnect this account from its app below.")
}

/** A refusal: what went wrong, its code, and the button that fixes it when there is one. */
export function ConnectorProblem({ error }: { readonly error: unknown }) {
  const [failed, setFailed] = useState<string | null>(null)
  const typed = error instanceof ConnectorError ? error : null
  const action = typed?.action ?? null
  return (
    <div role="alert" data-code={typed?.code} className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
      <TriangleAlert aria-hidden className="size-4 text-destructive" />
      <span className="min-w-0 flex-1">{error instanceof Error ? error.message : String(error)} {typed && <code className="text-meta text-muted-foreground">{typed.code}</code>}</span>
      {action !== null && action.kind !== "ask_admin" && <Button size="sm" variant="outline" onClick={() => fix(action).catch((e: unknown) => setFailed(e instanceof Error ? e.message : String(e)))}>{fixLabel(action)}</Button>}
      {action?.kind === "ask_admin" && <span className="text-muted-foreground">{fixLabel(action)}.</span>}
      {failed && <span className="w-full text-destructive">{failed}</span>}
    </div>
  )
}

function AccountRow({ app, requirement, label }: { readonly app: string; readonly requirement: Requirement; readonly label: string | null }) {
  const client = useQueryClient()
  const account = requirement.account
  const [renaming, setRenaming] = useState<string | null>(null)
  const [open, setOpen] = useState(false)
  const refresh = () => client.invalidateQueries({ queryKey: ["connectors"] })
  const connect = useMutation({ mutationFn: () => api<{ url: string }>(`/apps/${encodeURIComponent(app)}/accounts/${encodeURIComponent(requirement.requirement)}/connect`, { method: "POST", body: account === null ? {} : { account } }), onSuccess: go })
  const rename = useMutation({ mutationFn: (next: string) => api(`/accounts/${encodeURIComponent(account ?? "")}`, { method: "PATCH", body: { label: next } }), onSuccess: () => { setRenaming(null); return refresh() } })
  const disconnect = useMutation({ mutationFn: () => api(`/accounts/${encodeURIComponent(account ?? "")}`, { method: "DELETE" }), onSuccess: refresh })
  const detail = useQuery({ queryKey: ["connectors", "account", account], queryFn: () => api<AccountDetail>(`/accounts/${encodeURIComponent(account ?? "")}`), enabled: open && account !== null })
  const error = connect.error ?? rename.error ?? disconnect.error
  return (
    <div className="border-t first:border-t-0" data-requirement={requirement.requirement}>
      <div className="flex flex-wrap items-center gap-2 px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="font-medium">{account === null ? requirement.requirement : label ?? account}</div>
          <div className="text-meta text-muted-foreground">{requirement.requirement}{requirement.state && ` · ${requirement.state.replaceAll("_", " ")}`}</div>
        </div>
        {account === null
          ? <Button size="sm" onClick={() => connect.mutate()} disabled={connect.isPending}>Connect</Button>
          : (
            <>
              <Button size="sm" variant="ghost" aria-expanded={open} onClick={() => setOpen(!open)}>View</Button>
              <Button size="sm" variant="outline" onClick={() => connect.mutate()} disabled={connect.isPending}>Reconnect</Button>
              <Button size="sm" variant="outline" onClick={() => setRenaming(label ?? "")}>Rename</Button>
              <Button size="sm" variant="outline" onClick={() => { if (window.confirm(`Disconnect ${label ?? account}? Apps that use it stop working until you connect another.`)) disconnect.mutate() }} disabled={disconnect.isPending}>Disconnect</Button>
            </>
          )}
      </div>
      {renaming !== null && (
        <form className="flex gap-2 px-4 pb-3" onSubmit={(event) => { event.preventDefault(); rename.mutate(renaming) }}>
          <Input aria-label="Account name" value={renaming} maxLength={120} onChange={(event) => setRenaming(event.target.value)} autoFocus />
          <Button size="sm" type="submit" disabled={rename.isPending}>Save</Button>
          <Button size="sm" variant="ghost" type="button" onClick={() => setRenaming(null)}>Cancel</Button>
        </form>
      )}
      {open && account !== null && (
        <dl className="grid grid-cols-[8rem_1fr] gap-x-3 gap-y-1 px-4 pb-3 text-meta">
          {detail.isPending ? <dd className="col-span-2 text-muted-foreground">Loading…</dd> : detail.isError ? <dd className="col-span-2"><ConnectorProblem error={detail.error} /></dd> : (
            <>
              <dt className="text-muted-foreground">Provider</dt><dd>{detail.data.provider}</dd>
              <dt className="text-muted-foreground">Used by</dt><dd>{detail.data.apps.join(", ") || "—"}</dd>
              <dt className="text-muted-foreground">Health</dt><dd>{detail.data.health ?? "not checked"}</dd>
              <dt className="text-muted-foreground">Account id</dt><dd className="font-mono">{detail.data.id}</dd>
            </>
          )}
        </dl>
      )}
      {error && <ConnectorProblem error={error} />}
    </div>
  )
}

function AppAccounts({ app, name, inventory }: { readonly app: string; readonly name: string; readonly inventory: Inventory }) {
  const requirements = useQuery({ queryKey: ["connectors", "requirements", app], queryFn: () => api<{ requirements: readonly Requirement[] }>(`/apps/${encodeURIComponent(app)}/accounts`) })
  const labelOf = (account: string | null) => inventory.accounts.find((entry) => entry.id === account)?.label ?? null
  return (
    <section aria-label={name} className="rounded-md border bg-background">
      <header className="px-4 pt-3 text-sm font-medium">{name}</header>
      {requirements.isPending ? <Note>Loading…</Note>
        : requirements.isError ? <ConnectorProblem error={requirements.error} />
        : requirements.data.requirements.length === 0 ? <Note>This app needs no account of yours.</Note>
        : requirements.data.requirements.map((requirement) => <AccountRow key={`${requirement.requirement}:${requirement.account ?? ""}`} app={app} requirement={requirement} label={labelOf(requirement.account)} />)}
    </section>
  )
}

export function ConnectionsPage({ children }: { readonly children?: ReactNode }) {
  const client = useQueryClient()
  const status = useQuery({ queryKey: ["connectors", "status"], queryFn: () => api<Status>("/status"), retry: false })
  const linked = status.data?.linked === true
  const inventory = useQuery({ queryKey: ["connectors", "inventory"], queryFn: () => api<Inventory>("/inventory"), enabled: linked, retry: false })
  const link = useMutation({ mutationFn: () => api<{ url: string }>("/link", { method: "POST" }), onSuccess: go })
  const unlink = useMutation({ mutationFn: () => api("/unlink", { method: "POST" }), onSuccess: () => client.invalidateQueries({ queryKey: ["connectors"] }) })

  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">Connections</h1>
        <p className="max-w-2xl text-muted-foreground">Sign in to the tool gateway and connect the accounts your tools use. Credentials stay in the gateway; Norn keeps only the tool bindings your organization makes.</p>
      </div>
      <Panel icon={Plug} label="Tool gateway" action={status.data?.gateway ?? undefined}>
        <Well>
          {status.isPending ? <Note>Checking…</Note>
            : status.isError ? <ConnectorProblem error={status.error} />
            : status.data.gateway === null ? <Note>No tool gateway is configured for this deployment.</Note>
            : (
              <div className="flex flex-wrap items-center gap-3 px-4 py-3">
                <Badge variant={linked ? "secondary" : "outline"}>{linked ? "Signed in" : "Not signed in"}</Badge>
                {linked ? (
                  <>
                    <Button size="sm" variant="outline" onClick={() => link.mutate()} disabled={link.isPending}>Reconnect</Button>
                    <Button size="sm" variant="outline" onClick={() => unlink.mutate()} disabled={unlink.isPending}>Disconnect</Button>
                  </>
                ) : <Button size="sm" onClick={() => link.mutate()} disabled={link.isPending}>Sign in to the gateway</Button>}
              </div>
            )}
          {(link.error ?? unlink.error) && <ConnectorProblem error={link.error ?? unlink.error} />}
        </Well>
      </Panel>
      {linked && (
        <Panel icon={Cable} label="Connected apps">
          <Well className="space-y-3 p-3">
            {inventory.isPending ? <Note>Loading…</Note>
              : inventory.isError ? <ConnectorProblem error={inventory.error} />
              : inventory.data.apps.length === 0 ? <EmptyState icon={Cable}>No apps yet. Add one in the gateway.</EmptyState>
              : inventory.data.apps.map((app) => <AppAccounts key={app.id} app={app.id} name={app.name} inventory={inventory.data} />)}
          </Well>
        </Panel>
      )}
      {children}
    </section>
  )
}
