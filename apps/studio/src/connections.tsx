import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Cable, Plug, TriangleAlert } from "lucide-react"
import { useState, type ReactNode } from "react"
import { EmptyState, Panel, Well } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { api, ConnectorError, fixLabel, offersButton, startFix, type AccountDetail, type Inventory, type Requirement, type Status } from "./lib/connectors.ts"
import { workspaceScopeFromAccount, type WorkspaceScope } from "./lib/definitions.ts"
import { useAccount, usePinnedWorkspaceScope } from "./lib/session.ts"
import { CodedMessage } from "@/components/coded-message"

/**
 * Connections (#772): the person's sign-in to the tool gateway and the accounts they connected
 * there. Accounts are connected and reconnected in the gateway's own form, so credentials never
 * pass through Norn. Every refusal is the server's, shown with the fix it names.
 */

const Note = ({ children }: { children: ReactNode }) => <p className="px-4 py-3 text-sm text-muted-foreground">{children}</p>
const go = ({ url }: { url: string }) => window.location.assign(url)

/** A refusal: what went wrong, its code, and the button that fixes it when there is one. */
export function ConnectorProblem({ error, scope }: { readonly error: unknown; readonly scope?: WorkspaceScope | null }) {
  const [failed, setFailed] = useState<string | null>(null)
  const pinnedScope = usePinnedWorkspaceScope(error)
  const typed = error instanceof ConnectorError ? error : null
  // Prefer the error's own recorded originating scope so recovery actions stay bound to the
  // workspace that produced the failure even after a refresh or workspace switch.
  const effectiveScope = typed?.scope !== undefined ? typed.scope : scope !== undefined ? scope : pinnedScope
  const action = typed?.action ?? null
  return (
    <div role="alert" data-code={typed?.code} className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
      <TriangleAlert aria-hidden className="size-4 text-destructive" />
      <span className="min-w-0 flex-1">{typed ? <CodedMessage code={typed.code} message={typed.message} showCode /> : error instanceof Error ? error.message : String(error)}</span>
      {action !== null && offersButton(action) && <Button size="sm" variant="outline" onClick={() => startFix(action, undefined, effectiveScope).then((url) => window.location.assign(url), (e: unknown) => setFailed(e instanceof Error ? e.message : String(e)))}>{fixLabel(action)}</Button>}
      {action !== null && !offersButton(action) && <span className="text-muted-foreground">{fixLabel(action)}.</span>}
      {failed && <span className="w-full text-destructive">{failed}</span>}
    </div>
  )
}

function AccountRow({ app, requirement, label, scope }: { readonly app: string; readonly requirement: Requirement; readonly label: string | null; readonly scope?: WorkspaceScope | null }) {
  const client = useQueryClient()
  const account = requirement.account
  const [renaming, setRenaming] = useState<string | null>(null)
  const [open, setOpen] = useState(false)
  const pinnedRowScope = usePinnedWorkspaceScope(`${app}:${requirement.requirement}:${account ?? ""}`)
  const effectiveScope = scope !== undefined ? scope : pinnedRowScope
  const scopeKey = effectiveScope ? `${effectiveScope.orgId}:${effectiveScope.environmentId ?? ""}` : ""
  const refresh = () => client.invalidateQueries({ queryKey: ["connectors"] })
  const connect = useMutation({ mutationFn: () => api<{ url: string }>(`/apps/${encodeURIComponent(app)}/accounts/${encodeURIComponent(requirement.requirement)}/connect`, { method: "POST", body: account === null ? {} : { account }, scope: effectiveScope }), onSuccess: go })
  // Inherit the row's originating scope for rename so opening the input after a refresh never targets another workspace.
  const rename = useMutation({ mutationFn: (next: string) => api(`/accounts/${encodeURIComponent(account ?? "")}`, { method: "PATCH", body: { label: next }, scope: effectiveScope }), onSuccess: () => { setRenaming(null); return refresh() } })
  // Unlinks the account from this app's requirement; the gateway deletes it only when no other app uses it.
  const disconnect = useMutation({ mutationFn: () => api(`/apps/${encodeURIComponent(app)}/accounts/${encodeURIComponent(requirement.requirement)}/${encodeURIComponent(account ?? "")}`, { method: "DELETE", scope: effectiveScope }), onSuccess: refresh })
  const detail = useQuery({ queryKey: ["connectors", "account", account, scopeKey], queryFn: () => api<AccountDetail>(`/accounts/${encodeURIComponent(account ?? "")}`, { scope: effectiveScope }), enabled: open && account !== null })
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
          {detail.isPending ? <dd className="col-span-2 text-muted-foreground">Loading…</dd> : detail.isError ? <dd className="col-span-2"><ConnectorProblem error={detail.error} scope={effectiveScope} /></dd> : (
            <>
              <dt className="text-muted-foreground">Provider</dt><dd>{detail.data.provider}</dd>
              <dt className="text-muted-foreground">Used by</dt><dd>{detail.data.apps.join(", ") || "—"}</dd>
              <dt className="text-muted-foreground">Health</dt><dd>{detail.data.health ?? "not checked"}</dd>
              <dt className="text-muted-foreground">Account id</dt><dd className="font-mono">{detail.data.id}</dd>
            </>
          )}
        </dl>
      )}
      {error && <ConnectorProblem error={error} scope={effectiveScope} />}
    </div>
  )
}

function AppAccounts({ app, name, inventory, scope }: { readonly app: string; readonly name: string; readonly inventory: Inventory; readonly scope?: WorkspaceScope | null }) {
  const scopeKey = scope ? `${scope.orgId}:${scope.environmentId ?? ""}` : ""
  const requirements = useQuery({ queryKey: ["connectors", "requirements", app, scopeKey], queryFn: () => api<{ requirements: readonly Requirement[] }>(`/apps/${encodeURIComponent(app)}/accounts`, { scope }) })
  const labelOf = (account: string | null) => inventory.accounts.find((entry) => entry.id === account)?.label ?? null
  return (
    <section aria-label={name} className="rounded-md border bg-background">
      <header className="px-4 pt-3 text-sm font-medium">{name}</header>
      {requirements.isPending ? <Note>Loading…</Note>
        : requirements.isError ? <ConnectorProblem error={requirements.error} scope={scope} />
        : requirements.data.requirements.length === 0 ? <Note>This app needs no account of yours.</Note>
        : requirements.data.requirements.map((requirement) => <AccountRow key={`${requirement.requirement}:${requirement.account ?? ""}`} app={app} requirement={requirement} label={labelOf(requirement.account)} scope={scope} />)}
    </section>
  )
}

export function ConnectionsPage({ children }: { readonly children?: ReactNode }) {
  const client = useQueryClient()
  const account = useAccount()
  const activeScope = account.data !== undefined ? workspaceScopeFromAccount(account.data) : undefined
  const scopeKey = activeScope ? `${activeScope.orgId}:${activeScope.environmentId ?? ""}` : ""
  const status = useQuery({ queryKey: ["connectors", "status", scopeKey], queryFn: () => api<Status>("/status", { scope: activeScope }), retry: false })
  const linked = status.data?.linked === true
  const inventory = useQuery({ queryKey: ["connectors", "inventory", scopeKey], queryFn: () => api<Inventory>("/inventory", { scope: activeScope }), enabled: linked, retry: false })
  const fallbackPinnedScope = usePinnedWorkspaceScope(status.data?.gateway ?? true)
  const pinnedScope = activeScope !== undefined ? activeScope : fallbackPinnedScope
  const link = useMutation({ mutationFn: () => api<{ url: string }>("/link", { method: "POST", scope: pinnedScope }), onSuccess: go })
  const unlink = useMutation({ mutationFn: () => api("/unlink", { method: "POST", scope: pinnedScope }), onSuccess: () => client.invalidateQueries({ queryKey: ["connectors"] }) })

  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-semibold tracking-tight">Connections</h1>
        <p className="max-w-2xl text-muted-foreground">Sign in to the tool gateway and connect the accounts your tools use. Credentials stay in the gateway; Norn keeps only the tool bindings your organization makes.</p>
      </div>
      <Panel icon={Plug} label="Tool gateway" action={status.data?.gateway ?? undefined}>
        <Well>
          {status.isPending ? <Note>Checking…</Note>
            : status.isError ? <ConnectorProblem error={status.error} scope={pinnedScope} />
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
          {(link.error ?? unlink.error) && <ConnectorProblem error={link.error ?? unlink.error} scope={pinnedScope} />}
        </Well>
      </Panel>
      {linked && (
        <Panel icon={Cable} label="Connected apps">
          <Well className="space-y-3 p-3">
            {inventory.isPending ? <Note>Loading…</Note>
              : inventory.isError ? <ConnectorProblem error={inventory.error} scope={pinnedScope} />
              : inventory.data.apps.length === 0 ? <EmptyState icon={Cable}>No apps yet. Add one in the gateway.</EmptyState>
              : inventory.data.apps.map((app) => <AppAccounts key={app.id} app={app.id} name={app.name} inventory={inventory.data} scope={pinnedScope} />)}
          </Well>
        </Panel>
      )}
      {children}
    </section>
  )
}
