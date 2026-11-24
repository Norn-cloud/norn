import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Link, useNavigate, useParams, useSearch } from "@tanstack/react-router"
import { CheckCircle2, CircleAlert, Clock, Code2, Copy, Eye, FlaskConical, Inbox, Pause, Pencil, Play, Plus, RefreshCw, Shapes, Trash2, Undo2, Zap } from "lucide-react"
import { useEffect, useMemo, useRef, useState } from "react"
import { DirtyGuard, EditorToolbar, ValidationPanel, useEditor } from "@/components/editor"
import { EmptyState, PageHeader, Panel } from "@/components/panel"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { useActions } from "./actions/action-sheet.tsx"
import { fieldsOf, type ActionSummary } from "./lib/actions.ts"
import {
  AUTOMATION_ROLES,
  GUARD_OP_LABEL,
  automationField,
  blankGuardClause,
  canRenderGuardVisually,
  cellAutomations,
  compileGuard,
  describeThen,
  describeWhen,
  duplicateAutomation,
  durationParts,
  encodeStaticActionInputs,
  guardOpsFor,
  guardableFields,
  msFromDuration,
  newAutomationId,
  parseGuard,
  validateGuardClauses,
  type AutomationDefinition,
  type AutomationPreviewResponse,
  type AutomationRunOutcome,
  type AutomationRunRecord,
  type AutomationSaved,
  type DurationUnit,
  type GuardClause,
  type GuardOp,
  type StoredAutomation,
} from "./lib/automations.ts"
import type { Concept } from "./lib/catalog.ts"
import type { Risk } from "./lib/decisions.ts"
import { NO_DATA_CELL, useStudioScope, type Finding } from "./lib/definitions.ts"
import type { Issue } from "./lib/editor.ts"
import { useMode } from "./lib/live-transport.ts"
import { lifecycleStates } from "./lib/objects.ts"
import type { MenuEntry } from "./shell.tsx"
import { useCatalog } from "./sources.tsx"

const automations = cellAutomations()
const HIDDEN_FIELDS = new Set(["id", "label", "display_name"])

const failure = (error: unknown) => error instanceof Error ? error.message : String(error)

function settled(result: AutomationSaved) {
  if (result.saved) return result
  throw new Error("conflict" in result
    ? "Someone changed this automation since it was read; reload and try again."
    : result.findings.map(({ message }) => message).join(" ") || "The cell refused this automation.")
}

const issuesOf = (findings: readonly Finding[]): readonly Issue[] =>
  findings.map(({ code, message, pointer }) => ({ code, message, field: automationField(pointer) }))

interface RemovedAutomation {
  readonly id: string
  readonly version: number
  readonly automation: AutomationDefinition
}

function UndoDeleteAutomation({ removed, pending, error, onUndo }: {
  readonly removed: RemovedAutomation
  readonly pending: boolean
  readonly error?: string
  readonly onUndo: () => void
}) {
  return (
    <div role="status" className="flex flex-wrap items-center gap-2 rounded-xl border bg-card p-3 text-sm shadow-elevation">
      <span>Deleted “{removed.automation.title || "Untitled automation"}”. Its history stays in the cell.</span>
      <Button type="button" size="sm" variant="outline" className="ms-auto" disabled={pending} onClick={onUndo}>
        <Undo2 />{pending ? "Restoring…" : "Undo"}
      </Button>
      {error && <p role="alert" className="basis-full text-destructive">Not restored. {error}</p>}
    </div>
  )
}

function OutcomeBadge({ outcome }: { readonly outcome: AutomationRunOutcome }) {
  switch (outcome) {
    case "proposed":
      return <Badge variant="secondary"><Inbox className="size-3" /> Proposed</Badge>
    case "committed":
      return <Badge variant="outline"><CheckCircle2 className="size-3 text-[var(--tint-outcome)]" /> Committed</Badge>
    case "refused":
      return <Badge variant="destructive"><CircleAlert className="size-3" /> Refused</Badge>
    case "skipped":
      return <Badge variant="outline"><Clock className="size-3" /> Skipped</Badge>
  }
}

/** Automations sidebar menu for the Builder perspective. Returns [] when not connected to a live cell. */
export function useAutomationsMenu(): readonly MenuEntry[] {
  const live = useMode().data?.kind === "live"
  const scope = useStudioScope()
  const enabled = live && scope.isSuccess && scope.data !== null
  const activeScope = scope.data ?? null
  const list = useQuery({
    queryKey: ["automations", "all", activeScope?.orgId, activeScope?.environmentId],
    queryFn: () => automations.list(activeScope),
    enabled,
    retry: false,
    staleTime: 30_000,
  })
  if (!enabled || !list.data) return []
  return list.data.map((stored) => ({
    href: `/automations/${encodeURIComponent(stored.id)}`,
    label: stored.automation.title || stored.id,
    icon: Zap,
  }))
}

/** `/automations`: all server-admitted When/If/Then automations with enable/disable, duplicate, delete and undo. */
export function AutomationsPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()
  const activeScope = scope.data ?? null
  const list = useQuery({
    queryKey: ["automations", "all", activeScope?.orgId, activeScope?.environmentId],
    queryFn: () => automations.list(activeScope),
    enabled: cellAvailable,
    retry: false,
  })
  const refresh = () => client.invalidateQueries({ queryKey: ["automations"] })
  const [removed, setRemoved] = useState<RemovedAutomation>()

  const toggleEnabled = useMutation({
    mutationFn: async (stored: StoredAutomation) =>
      settled(await automations.save(stored.id, { ...stored.automation, enabled: !stored.automation.enabled }, stored.version)),
    onSuccess: refresh,
  })

  const duplicate = useMutation({
    mutationFn: async (stored: StoredAutomation) => {
      const copy = duplicateAutomation(stored.automation)
      return settled(await automations.save(newAutomationId(copy.title), copy))
    },
    onSuccess: refresh,
  })

  const remove = useMutation({
    mutationFn: async (stored: StoredAutomation) => {
      const retired = settled(await automations.retire(stored.id, stored.version))
      return retired.saved ? { id: stored.id, version: retired.version, automation: stored.automation } : undefined
    },
    onSuccess: setRemoved,
    onSettled: refresh,
  })

  const undo = useMutation({
    mutationFn: async (gone: RemovedAutomation) => settled(await automations.save(gone.id, gone.automation, gone.version)),
    onSuccess: () => setRemoved(undefined),
    onSettled: refresh,
  })

  return (
    <section className="space-y-6">
      <PageHeader
        title="Automations"
        description="Server-run When / If / Then rules admitted by the cell. Proposals land in the Inbox; automatic actions obey every law and permission."
        primary={<Button disabled={!cellAvailable} onClick={() => void navigate({ to: "/automations/new" })}><Plus />New automation</Button>}
      />
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {cellAvailable && list.isPending && <p className="text-muted-foreground">Reading automations…</p>}
      {list.isError && <EmptyState icon={Zap}>{failure(list.error)}</EmptyState>}
      {toggleEnabled.isError && <p role="alert" className="text-destructive">Not updated. {failure(toggleEnabled.error)}</p>}
      {duplicate.isError && <p role="alert" className="text-destructive">Not duplicated. {failure(duplicate.error)}</p>}
      {remove.isError && <p role="alert" className="text-destructive">Not deleted. {failure(remove.error)}</p>}
      {removed && <UndoDeleteAutomation removed={removed} pending={undo.isPending} error={undo.isError ? failure(undo.error) : undefined} onUndo={() => undo.mutate(removed)} />}
      {list.data?.length === 0 && (
        <EmptyState
          icon={Zap}
          title="No automations yet"
          next={cellAvailable && <Button size="sm" variant="outline" onClick={() => void navigate({ to: "/automations/new" })}><Plus />New automation</Button>}
        >
          Define a When / If / Then rule to watch for events, lifecycle states or staleness, and propose or run a governed action.
        </EmptyState>
      )}
      <div className="grid gap-3">
        {(list.data ?? []).map((stored) => {
          const def = stored.automation
          const concept = catalog?.byName.get(def.objectType)
          return (
            <Panel
              key={stored.id}
              icon={Zap}
              label={
                <span className="flex flex-wrap items-center gap-2">
                  <span>{def.title || "Untitled automation"}</span>
                  <Badge variant={def.enabled ? "secondary" : "outline"}>{def.enabled ? "Enabled" : "Disabled"}</Badge>
                </span>
              }
              action={
                <div className="flex flex-wrap items-center gap-1">
                  <Button asChild variant="ghost" size="sm"><Link to="/automations/$id" params={{ id: stored.id }}><Eye />Open</Link></Button>
                  <Button asChild variant="ghost" size="sm"><Link to="/automations/$id/edit" params={{ id: stored.id }}><Pencil />Edit</Link></Button>
                  <Button variant="ghost" size="sm" disabled={toggleEnabled.isPending} onClick={() => toggleEnabled.mutate(stored)}>
                    {def.enabled ? <><Pause />Disable</> : <><Play />Enable</>}
                  </Button>
                  <Button variant="ghost" size="sm" disabled={duplicate.isPending} onClick={() => duplicate.mutate(stored)}><Copy />Duplicate</Button>
                  <Button variant="ghost" size="sm" onClick={() => remove.mutate(stored)}><Trash2 />Delete</Button>
                </div>
              }
            >
              <div className="grid gap-1 text-sm text-muted-foreground">
                <div className="flex flex-wrap items-center gap-2">
                  <Badge variant="outline">{concept?.title ?? def.objectType}</Badge>
                  <span><strong>When</strong> {describeWhen(def.when)}</span>
                  {def.if?.expression && <span>· <strong>If</strong> <code className="rounded bg-muted px-1 py-0.5 text-xs">{def.if.expression}</code></span>}
                  <span>· <strong>Then</strong> {describeThen(def.then)}</span>
                </div>
                {def.description && <p>{def.description}</p>}
              </div>
            </Panel>
          )
        })}
      </div>
    </section>
  )
}

function PreviewResults({ preview }: { readonly preview: AutomationPreviewResponse }) {
  return (
    <div className="space-y-3" data-preview-results>
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <Badge variant={preview.matchedCount > 0 ? "secondary" : "outline"}>
          {preview.matchedCount} match{preview.matchedCount === 1 ? "" : "es"}
        </Badge>
        <span className="text-muted-foreground">
          Scanned {preview.scannedCount} record{preview.scannedCount === 1 ? "" : "s"}{preview.approximate ? " (bounded)" : ""} · dry run, nothing fired
        </span>
      </div>
      {preview.restatement && <p className="rounded-lg border border-node-border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">{preview.restatement}</p>}
      {preview.matches.length === 0 ? (
        <p className="text-sm text-muted-foreground">No current records match this automation.</p>
      ) : (
        <div className="overflow-x-auto rounded-lg border">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b bg-muted/50 text-start text-xs text-muted-foreground">
                <th className="px-3 py-2 text-start font-medium">Record</th>
                <th className="px-3 py-2 text-start font-medium">Why it matched</th>
                <th className="px-3 py-2 text-start font-medium">Action</th>
                <th className="px-3 py-2 text-start font-medium">Values</th>
              </tr>
            </thead>
            <tbody className="divide-y">
              {preview.matches.map((match) => (
                <tr key={match.id}>
                  <td className="px-3 py-2 font-mono text-xs font-medium">{match.id}</td>
                  <td className="px-3 py-2 text-muted-foreground">{match.reason}</td>
                  <td className="px-3 py-2 font-mono text-xs">{match.proposedAction.actionId}</td>
                  <td className="px-3 py-2 text-xs text-muted-foreground">
                    {Object.entries(match.values).filter(([k]) => k !== "id").slice(0, 4).map(([k, v]) => `${k}: ${String(v)}`).join(" · ")}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}

/** `/automations/$id`: inspect an automation, test against current records, evaluate now, toggle, and inspect run history. */
export function AutomationDetailPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { id = "" } = useParams({ strict: false }) as { id?: string }
  const { catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()

  const stored = useQuery({
    queryKey: ["automation", id, scope.data?.orgId, scope.data?.environmentId],
    queryFn: async () => (await automations.get(id)) ?? null,
    enabled: cellAvailable && id !== "",
    retry: false,
  })
  const runs = useQuery({
    queryKey: ["automation-runs", id, scope.data?.orgId, scope.data?.environmentId],
    queryFn: () => automations.runs(id),
    enabled: cellAvailable && id !== "" && stored.data !== null,
    retry: false,
  })

  const [removed, setRemoved] = useState<RemovedAutomation>()
  const [previewData, setPreviewData] = useState<AutomationPreviewResponse | null>(null)
  const [runNotice, setRunNotice] = useState<string | null>(null)

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: ["automations"] })
    void client.invalidateQueries({ queryKey: ["automation", id] })
    void client.invalidateQueries({ queryKey: ["automation-runs", id] })
  }

  const toggleEnabled = useMutation({
    mutationFn: async (from: StoredAutomation) =>
      settled(await automations.save(from.id, { ...from.automation, enabled: !from.automation.enabled }, from.version)),
    onSuccess: invalidate,
  })

  const preview = useMutation({
    mutationFn: () => automations.preview({ id }),
    onSuccess: setPreviewData,
  })

  const runNow = useMutation({
    mutationFn: () => automations.run(id),
    onSuccess: (res) => {
      setRunNotice(`Evaluated ${res.scannedCount} records (${res.matchedCount} matched): ${res.proposed} proposed, ${res.committed} committed, ${res.refused} refused, ${res.skipped} skipped.`)
      void client.invalidateQueries({ queryKey: ["automation-runs", id] })
      void client.invalidateQueries({ queryKey: ["decisions"] })
      void client.invalidateQueries({ queryKey: ["records"] })
    },
  })

  const duplicate = useMutation({
    mutationFn: async (from: StoredAutomation) => {
      const copy = duplicateAutomation(from.automation)
      const nextId = newAutomationId(copy.title)
      settled(await automations.save(nextId, copy))
      return nextId
    },
    onSuccess: (copied) => {
      void client.invalidateQueries({ queryKey: ["automations"] })
      void navigate({ to: "/automations/$id", params: { id: copied } })
    },
  })

  const remove = useMutation({
    mutationFn: async (from: StoredAutomation) => {
      const retired = settled(await automations.retire(from.id, from.version))
      return retired.saved ? { id: from.id, version: retired.version, automation: from.automation } : undefined
    },
    onSuccess: (gone) => { setRemoved(gone); void client.invalidateQueries({ queryKey: ["automations"] }) },
  })

  const undo = useMutation({
    mutationFn: async (gone: RemovedAutomation) => settled(await automations.save(gone.id, gone.automation, gone.version)),
    onSuccess: () => { setRemoved(undefined); invalidate(); void stored.refetch() },
  })

  const active = removed ? undefined : stored.data
  const def = active?.automation
  const concept = def ? catalog?.byName.get(def.objectType) : undefined

  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-start gap-2">
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <h1 className="text-title font-semibold tracking-tight">{def?.title || "Automation"}</h1>
            {def && <Badge variant={def.enabled ? "secondary" : "outline"}>{def.enabled ? "Enabled" : "Disabled"}</Badge>}
            {def && <Badge variant="outline">{concept?.title ?? def.objectType}</Badge>}
            {active && <Badge variant="outline">Version {active.version}</Badge>}
          </div>
          {def?.description && <p className="text-sm text-muted-foreground">{def.description}</p>}
        </div>
        {active && def && (
          <div className="flex flex-wrap items-center gap-1">
            <Button type="button" variant="outline" size="sm" disabled={preview.isPending} onClick={() => preview.mutate()}>
              <FlaskConical />{preview.isPending ? "Testing…" : "Test against current records"}
            </Button>
            <Button type="button" variant="outline" size="sm" disabled={!def.enabled || runNow.isPending} onClick={() => runNow.mutate()}>
              <Play />{runNow.isPending ? "Evaluating…" : "Evaluate now"}
            </Button>
            <Button type="button" variant="ghost" size="sm" disabled={toggleEnabled.isPending} onClick={() => toggleEnabled.mutate(active)}>
              {def.enabled ? <><Pause />Disable</> : <><Play />Enable</>}
            </Button>
            <Button asChild size="sm"><Link to="/automations/$id/edit" params={{ id: active.id }}><Pencil />Edit</Link></Button>
            <Button variant="ghost" size="sm" disabled={duplicate.isPending} onClick={() => duplicate.mutate(active)}><Copy />Duplicate</Button>
            <Button variant="ghost" size="sm" disabled={remove.isPending} onClick={() => remove.mutate(active)}><Trash2 />Delete</Button>
          </div>
        )}
      </div>

      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {preview.isError && <p role="alert" className="text-destructive">{failure(preview.error)}</p>}
      {runNow.isError && <p role="alert" className="text-destructive">{failure(runNow.error)}</p>}
      {runNotice && <p role="status" className="rounded-lg border bg-card px-3 py-2 text-sm">{runNotice}</p>}
      {(scope.isPending || (cellAvailable && stored.isPending)) && <p className="text-muted-foreground">Reading automation…</p>}
      {stored.isError && <EmptyState icon={Zap}>{failure(stored.error)}</EmptyState>}
      {removed && <UndoDeleteAutomation removed={removed} pending={undo.isPending} error={undo.isError ? failure(undo.error) : undefined} onUndo={() => undo.mutate(removed)} />}
      {removed && <Button asChild variant="ghost" size="sm"><Link to="/automations">Back to automations</Link></Button>}
      {stored.isSuccess && active === null && <EmptyState icon={Zap}>No automation {id} is saved.</EmptyState>}

      {active && def && (
        <>
          <div className="grid gap-4 md:grid-cols-3">
            <Panel label="1. When">
              <div className="border-t px-4 py-3">
                <p className="text-sm">{describeWhen(def.when)}</p>
              </div>
            </Panel>
            <Panel label="2. If">
              <div className="border-t px-4 py-3">
                {def.if?.expression
                  ? <code className="block rounded-md bg-muted px-2.5 py-1.5 font-mono text-xs">{def.if.expression}</code>
                  : <p className="text-sm text-muted-foreground">Always (no guard condition)</p>}
              </div>
            </Panel>
            <Panel label="3. Then">
              <div className="border-t px-4 py-3">
                <p className="text-sm">{describeThen(def.then)}</p>
                {def.then.kind === "propose" && (
                  <p className="mt-1 text-meta text-muted-foreground">Question: “{def.then.question}” · Risk: {def.then.risk}</p>
                )}
              </div>
            </Panel>
          </div>

          {previewData && (
            <Panel label="Preview against current records">
              <div className="border-t px-4 py-3">
                <PreviewResults preview={previewData} />
              </div>
            </Panel>
          )}

          <Panel
            label="Run history"
            action={
              <Button type="button" variant="ghost" size="sm" disabled={runs.isFetching} onClick={() => void runs.refetch()}>
                <RefreshCw className={runs.isFetching ? "animate-spin" : undefined} />Refresh
              </Button>
            }
          >
            <div className="border-t px-4 py-3">
            {runs.isPending && <p className="text-sm text-muted-foreground">Reading run history…</p>}
            {runs.isError && <p role="alert" className="text-sm text-destructive">{failure(runs.error)}</p>}
            {runs.data && runs.data.items.length === 0 && (
              <p className="text-sm text-muted-foreground">This automation has not fired yet.</p>
            )}
            {runs.data && runs.data.items.length > 0 && (
              <div className="overflow-x-auto">
                <table className="w-full text-sm" aria-label="Automation run history">
                  <thead>
                    <tr className="border-b text-start text-xs text-muted-foreground">
                      <th className="py-2 pe-3 text-start font-medium">When</th>
                      <th className="px-3 py-2 text-start font-medium">Trigger</th>
                      <th className="px-3 py-2 text-start font-medium">Record</th>
                      <th className="px-3 py-2 text-start font-medium">Action</th>
                      <th className="px-3 py-2 text-start font-medium">Outcome</th>
                      <th className="ps-3 py-2 text-start font-medium">Detail</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y">
                    {runs.data.items.map((item: AutomationRunRecord) => (
                      <tr key={item.id}>
                        <td className="py-2 pe-3 text-xs tabular-nums text-muted-foreground">
                          {new Date(item.recordedAtMs).toLocaleString()}
                        </td>
                        <td className="px-3 py-2 text-xs text-muted-foreground">{item.trigger.replaceAll("_", " ")}</td>
                        <td className="px-3 py-2 font-mono text-xs">
                          <Link to="/sources/$concept/$" params={{ concept: item.objectType, _splat: item.objectId }} className="underline-offset-4 hover:underline">
                            {item.objectId}
                          </Link>
                        </td>
                        <td className="px-3 py-2 font-mono text-xs">{item.actionId}</td>
                        <td className="px-3 py-2"><OutcomeBadge outcome={item.outcome} /></td>
                        <td className="ps-3 py-2 text-xs text-muted-foreground">
                          {item.outcome === "proposed" && item.decisionId && (
                            <Link to="/decisions" search={{ open: item.decisionId } as never} className="text-foreground underline underline-offset-4">
                              Open in Inbox
                            </Link>
                          )}
                          {item.outcome === "committed" && item.commitSeq !== undefined && `Commit #${item.commitSeq}`}
                          {item.outcome === "refused" && (
                            <span className="text-destructive">
                              {item.refusalCode ? `${item.refusalCode}: ` : ""}{item.refusalMessage ?? "Refused by cell"}
                            </span>
                          )}
                          {item.outcome === "skipped" && "Already evaluated for this record version"}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
            </div>
          </Panel>
        </>
      )}
    </section>
  )
}

function defaultAutomationFor(concept: Concept | undefined, firstAction?: ActionSummary): AutomationDefinition {
  const lifecycle = concept?.lifecycles[0]
  const states = lifecycle ? lifecycleStates(lifecycle) : []
  const targetState = states[1] ?? states[0] ?? ""
  const actionId = firstAction?.actionId ?? ""
  return {
    title: "",
    description: "",
    enabled: true,
    objectType: concept?.name ?? "",
    when: lifecycle && targetState
      ? { kind: "lifecycle_entered", property: lifecycle.property, state: targetState }
      : { kind: "event", event: "updated" },
    then: {
      kind: "propose",
      actionId,
      question: `Review and run ${firstAction?.title ?? "action"} on {id}?`,
      risk: "medium",
      decider: { roles: ["admin"] },
      expiresInMs: 86_400_000,
    },
  }
}

function AutomationEditor({ concepts, initialType, stored, cellAvailable, onSaved, onReload }: {
  readonly concepts: readonly Concept[]
  readonly initialType?: string
  readonly stored?: StoredAutomation
  readonly cellAvailable: boolean
  readonly onSaved: (saved: { readonly id: string; readonly version: number }) => void
  readonly onReload?: () => void
}) {
  const [at, setAt] = useState(stored ? { id: stored.id, version: stored.version } : undefined)
  const [findings, setFindings] = useState<readonly Finding[]>([])
  const [restatement, setRestatement] = useState<string | null>(null)
  const [conflict, setConflict] = useState(false)
  const [previewData, setPreviewData] = useState<AutomationPreviewResponse | null>(null)

  const defaultConcept = concepts.find((c) => c.name === initialType) ?? concepts[0]
  const initial = useMemo(() => stored?.automation ?? defaultAutomationFor(defaultConcept), [stored, defaultConcept])

  const saving = useRef(false)
  const [pendingNav, setPendingNav] = useState<{ readonly id: string; readonly version: number } | null>(null)
  const save = useMutation({
    mutationFn: async ({ def, overwrite }: { readonly def: AutomationDefinition; readonly overwrite?: boolean }) => {
      const id = at?.id ?? newAutomationId(def.title)
      return { def, result: await automations.save(id, def, overwrite ? undefined : at?.version) }
    },
    onSuccess: ({ def, result }) => {
      setConflict("conflict" in result)
      setFindings("findings" in result ? result.findings : [])
      if (result.saved) {
        setAt({ id: result.id, version: result.version })
        editor.markSaved(def)
        setPendingNav({ id: result.id, version: result.version })
      }
    },
  })
  const [localBlock, setLocalBlock] = useState<readonly string[]>([])
  const saveRef = useRef<(next: AutomationDefinition, overwrite?: boolean) => void>(() => {})
  const editor = useEditor<AutomationDefinition>({ initial, onSave: (d) => saveRef.current(d) })
  const { value: def, set } = editor
  // Navigate only after React commits `editor.dirty === false` so DirtyGuard never blocks a completed save.
  useEffect(() => {
    if (pendingNav !== null && !editor.dirty) {
      const next = pendingNav
      setPendingNav(null)
      onSaved(next)
    }
  }, [pendingNav, editor.dirty, onSaved])

  const concept = concepts.find((c) => c.name === def.objectType) ?? concepts[0]
  const fields = useMemo(() => (concept?.fields ?? []).filter(({ id }) => !HIDDEN_FIELDS.has(id)), [concept])
  const guardFields = useMemo(() => guardableFields(fields), [fields])
  const lifecycles = concept?.lifecycles ?? []
  const actionsQuery = useActions(def.objectType, undefined, cellAvailable && def.objectType !== "")
  const offeredActions = actionsQuery.data?.kind === "catalog" ? actionsQuery.data.items : []
  const selectedAction = offeredActions.find((a) => a.actionId === def.then.actionId)
  const extraInputs = useMemo(() => {
    if (!selectedAction) return []
    return fieldsOf(selectedAction.inputSchema).filter((f) => f.name !== selectedAction.objectIdField)
  }, [selectedAction])

  // Default first offered action when creating a new automation and the catalog arrives.
  useEffect(() => {
    if (stored !== undefined || def.then.actionId !== "" || offeredActions.length === 0) return
    const pick = offeredActions.find((a) => a.effect === "fire_transition") ?? offeredActions[0]!
    set((cur) => ({
      ...cur,
      then: cur.then.kind === "propose"
        ? { ...cur.then, actionId: pick.actionId, question: `Run ${pick.title} on {id}?` }
        : { ...cur.then, actionId: pick.actionId },
    }))
  }, [stored, def.then.actionId, offeredActions, set])

  // Visual guard clauses synced with `def.if?.expression`; stay in raw mode unless visual controls represent every clause.
  const parsedInitial = useMemo(() => parseGuard(def.if?.expression), [])
  const [rawMode, setRawMode] = useState(() => !canRenderGuardVisually(parsedInitial, fields))
  const [clauses, setClauses] = useState<readonly GuardClause[]>(() => canRenderGuardVisually(parsedInitial, fields) ? (parsedInitial ?? []) : [])
  useEffect(() => {
    if (!rawMode && fields.length > 0 && def.if?.expression && clauses.length > 0 && !canRenderGuardVisually(clauses, fields)) {
      // Only switch to raw mode if an operator or property is unsupported by the controls (not while typing an incomplete value).
      const unsupported = clauses.some((c) => {
        const f = fields.find((x) => x.id === c.property)
        return !f || !guardOpsFor(f.scalar).includes(c.op)
      })
      if (unsupported) setRawMode(true)
    }
  }, [rawMode, fields, def.if?.expression, clauses])

  const guardErrors = useMemo(
    () => (rawMode ? [] : validateGuardClauses(clauses, fields)),
    [rawMode, clauses, fields],
  )
  const rawCanSwitchToVisual = useMemo(
    () => canRenderGuardVisually(parseGuard(def.if?.expression), fields),
    [def.if?.expression, fields],
  )

  const updateClauses = (next: readonly GuardClause[]) => {
    setClauses(next)
    setLocalBlock([])
    const compiled = compileGuard(next, fields)
    set((cur) => ({ ...cur, if: compiled ? { expression: compiled } : undefined }))
  }

  // Raw string drafts for static action inputs so non-string scalar types are parsed and validated cleanly.
  const [staticDrafts, setStaticDrafts] = useState<Record<string, string>>(() => {
    const out: Record<string, string> = {}
    for (const [k, v] of Object.entries(def.then.input ?? {})) {
      out[k] = v === undefined || v === null ? "" : String(v)
    }
    return out
  })
  const prevActionKey = useRef(`${def.objectType}:${def.then.actionId}`)
  useEffect(() => {
    const key = `${def.objectType}:${def.then.actionId}`
    if (prevActionKey.current !== key) {
      prevActionKey.current = key
      const out: Record<string, string> = {}
      for (const [k, v] of Object.entries(def.then.input ?? {})) {
        out[k] = v === undefined || v === null ? "" : String(v)
      }
      setStaticDrafts(out)
    }
  }, [def.objectType, def.then.actionId, def.then.input])

  const staticCheck = useMemo(() => encodeStaticActionInputs(extraInputs, staticDrafts), [extraInputs, staticDrafts])
  const staticInputErrors = staticCheck.errors
  const updateStaticDraft = (name: string, nextValue: string) => {
    const nextDrafts = { ...staticDrafts, [name]: nextValue }
    setStaticDrafts(nextDrafts)
    setLocalBlock([])
    const encoded = encodeStaticActionInputs(extraInputs, nextDrafts)
    if (encoded.errors.length === 0) {
      set((cur) => ({ ...cur, then: { ...cur.then, input: encoded.input } }))
    }
  }

  const hasLocalErrors = guardErrors.length > 0 || staticInputErrors.length > 0
  const requestSave = (next: AutomationDefinition, overwrite = false) => {
    if (!cellAvailable || saving.current || (!overwrite && !editor.dirty)) return
    const blockers = [...guardErrors, ...staticInputErrors]
    if (blockers.length > 0) {
      setLocalBlock(blockers)
      return
    }
    setLocalBlock([])
    saving.current = true
    save.mutate({ def: next, overwrite }, { onSettled: () => { saving.current = false } })
  }
  saveRef.current = requestSave

  // Live server admission check (side-effect free).
  useEffect(() => {
    if (!cellAvailable || def.title.trim() === "" || def.objectType === "" || def.then.actionId === "" || hasLocalErrors) {
      setFindings([])
      setRestatement(null)
      return
    }
    let cancelled = false
    const timer = setTimeout(() => {
      const id = at?.id ?? newAutomationId(def.title, () => "check")
      automations.admit(id, def).then(
        (admission) => {
          if (cancelled) return
          if (admission.admitted) {
            setFindings([])
            setRestatement(admission.restatement)
          } else {
            setFindings(admission.findings)
            setRestatement(null)
          }
        },
        () => {},
      )
    }, 350)
    return () => { cancelled = true; clearTimeout(timer) }
  }, [cellAvailable, at?.id, def, hasLocalErrors])

  const preview = useMutation({
    mutationFn: () => automations.preview({ payload: def }),
    onSuccess: setPreviewData,
  })

  const issues = issuesOf(findings)
  const invalid = (field: string) => issues.some((i) => i.field === field)
  const selectCls = "h-9 rounded-md border bg-background px-2.5 text-sm"

  const pickType = (nextType: string) => {
    const nextConcept = concepts.find((c) => c.name === nextType)
    const nextLifecycle = nextConcept?.lifecycles[0]
    const states = nextLifecycle ? lifecycleStates(nextLifecycle) : []
    setClauses([])
    setRawMode(false)
    setStaticDrafts({})
    setLocalBlock([])
    setPreviewData(null)
    set((cur) => ({
      ...cur,
      objectType: nextType,
      when: nextLifecycle && states[0]
        ? { kind: "lifecycle_entered", property: nextLifecycle.property, state: states[1] ?? states[0] }
        : { kind: "event", event: "updated" },
      if: undefined,
      then: cur.then.kind === "propose"
        ? { ...cur.then, actionId: "", input: undefined }
        : { ...cur.then, actionId: "", input: undefined },
    }))
  }

  const setWhenKind = (kind: "lifecycle_entered" | "event" | "stale") => {
    if (kind === "lifecycle_entered") {
      const lc = lifecycles[0]
      const states = lc ? lifecycleStates(lc) : []
      set((cur) => ({ ...cur, when: { kind: "lifecycle_entered", property: lc?.property ?? "", state: states[1] ?? states[0] ?? "" } }))
    } else if (kind === "stale") {
      const lc = lifecycles[0]
      const states = lc ? lifecycleStates(lc) : []
      set((cur) => ({
        ...cur,
        when: {
          kind: "stale",
          afterMs: 3_600_000,
          ...(lc && states[0] ? { property: lc.property, state: states[0] } : {}),
        },
        // Stale timer wakes run without a verified caller principal, so stale rules propose to the Inbox.
        then: cur.then.kind === "run"
          ? {
            kind: "propose",
            actionId: cur.then.actionId,
            input: cur.then.input,
            question: `Run ${selectedAction?.title ?? "action"} on {id}?`,
            risk: "medium",
            decider: { roles: ["admin"] },
            expiresInMs: 86_400_000,
          }
          : cur.then,
      }))
    } else {
      set((cur) => ({ ...cur, when: { kind: "event", event: "updated" } }))
    }
  }

  const staleParts = def.when.kind === "stale" ? durationParts(def.when.afterMs) : { amount: 1, unit: "hours" as DurationUnit }
  const expiresParts = def.then.kind === "propose" ? durationParts(def.then.expiresInMs ?? 86_400_000) : { amount: 1, unit: "days" as DurationUnit }

  return (
    <div className="space-y-6">
      <DirtyGuard dirty={editor.dirty} />
      <EditorToolbar editor={editor} onSave={() => requestSave(def)} saving={save.isPending || !cellAvailable || hasLocalErrors}>
        {at && <Badge variant="outline">Version {at.version}</Badge>}
        <Button type="button" variant="outline" size="sm" className="ms-auto" disabled={!cellAvailable || def.then.actionId === "" || hasLocalErrors || preview.isPending} onClick={() => preview.mutate()}>
          <FlaskConical />{preview.isPending ? "Testing…" : "Test against current records"}
        </Button>
      </EditorToolbar>

      {save.isError && <p role="alert" className="text-destructive">Not saved. {failure(save.error)}</p>}
      {preview.isError && <p role="alert" className="text-destructive">Preview failed. {failure(preview.error)}</p>}
      {localBlock.length > 0 && (
        <div role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
          {localBlock.map((msg) => <p key={msg}>{msg}</p>)}
        </div>
      )}
      {conflict && (
        <div role="alert" className="flex flex-wrap items-center gap-2 rounded-xl border border-destructive/40 bg-card p-3 text-sm">
          <span>Someone saved a newer version of this automation since you opened it.</span>
          <Button type="button" size="sm" variant="outline" onClick={() => onReload?.()}>Reload theirs</Button>
          <Button type="button" size="sm" variant="destructive" disabled={!cellAvailable} onClick={() => requestSave(def, true)}>Overwrite with mine</Button>
        </div>
      )}
      <ValidationPanel issues={issues} title="The cell refused this automation" />
      {restatement && (
        <p role="status" className="rounded-lg border border-node-border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
          {restatement}
        </p>
      )}

      <div className="grid gap-4 md:grid-cols-2">
        <label className="grid gap-1.5 text-sm" data-field="title">
          <span className={invalid("title") ? "font-medium text-destructive" : "font-medium"}>Automation title</span>
          <Input
            aria-label="Automation title"
            aria-invalid={invalid("title") || undefined}
            placeholder="e.g. Propose follow-up for stalled records"
            value={def.title}
            onChange={(e) => set((cur) => ({ ...cur, title: e.target.value }))}
          />
        </label>

        <label className="grid gap-1.5 text-sm" data-field="objectType">
          <span className={invalid("objectType") ? "font-medium text-destructive" : "font-medium"}>Record type</span>
          <select aria-label="Record type" className={selectCls} value={def.objectType} onChange={(e) => pickType(e.target.value)}>
            {concepts.map((c) => <option key={c.name} value={c.name}>{c.title} ({c.name})</option>)}
          </select>
        </label>

        <label className="grid gap-1.5 text-sm" data-field="description">
          <span className="font-medium">Description <span className="font-normal text-muted-foreground">(optional)</span></span>
          <Input
            aria-label="Description"
            placeholder="Why this automation exists"
            value={def.description ?? ""}
            onChange={(e) => set((cur) => ({ ...cur, description: e.target.value }))}
          />
        </label>

        <label className="flex items-center gap-2 self-end pb-2 text-sm font-medium">
          <input
            type="checkbox"
            aria-label="Automation enabled"
            checked={def.enabled}
            onChange={(e) => set((cur) => ({ ...cur, enabled: e.target.checked }))}
          />
          <span>Enabled (runs on the server when triggered)</span>
        </label>
      </div>

      {/* 1. WHEN */}
      <div data-field="when">
        <Panel label="1. When">
          <div className="space-y-4">
            <div className="flex flex-wrap gap-2" role="radiogroup" aria-label="Trigger kind">
              {([
                ["lifecycle_entered", "Lifecycle state entered"],
                ["event", "Record event"],
                ["stale", "Record becomes stale"],
              ] as const).map(([kind, label]) => (
                <Button
                  key={kind}
                  type="button"
                  size="sm"
                  variant={def.when.kind === kind ? "default" : "outline"}
                  disabled={kind === "lifecycle_entered" && lifecycles.length === 0}
                  onClick={() => setWhenKind(kind)}
                >
                  {label}
                </Button>
              ))}
            </div>

            {def.when.kind === "lifecycle_entered" && (
              <div className="flex flex-wrap items-end gap-3">
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Lifecycle property</span>
                  <select
                    aria-label="Lifecycle property"
                    className={selectCls}
                    value={def.when.property}
                    onChange={(e) => {
                      const lc = lifecycles.find((l) => l.property === e.target.value)
                      const nextState = lc ? (lifecycleStates(lc)[0] ?? "") : ""
                      set((cur) => ({ ...cur, when: { kind: "lifecycle_entered", property: e.target.value, state: nextState } }))
                    }}
                  >
                    {lifecycles.map((lc) => <option key={lc.property} value={lc.property}>{lc.property.replaceAll("_", " ")}</option>)}
                  </select>
                </label>
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Enters state</span>
                  <select
                    aria-label="Target lifecycle state"
                    className={selectCls}
                    value={def.when.state}
                    onChange={(e) => set((cur) => cur.when.kind === "lifecycle_entered" ? { ...cur, when: { ...cur.when, state: e.target.value } } : cur)}
                  >
                    {(() => {
                      const lc = lifecycles.find((l) => l.property === (def.when.kind === "lifecycle_entered" ? def.when.property : "")) ?? lifecycles[0]
                      return (lc ? lifecycleStates(lc) : []).map((state) => (
                        <option key={state} value={state}>{state.replaceAll("_", " ")}</option>
                      ))
                    })()}
                  </select>
                </label>
              </div>
            )}

            {def.when.kind === "event" && (
              <div className="flex flex-wrap items-end gap-3">
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Event</span>
                  <select
                    aria-label="Record event"
                    className={selectCls}
                    value={def.when.event}
                    onChange={(e) => {
                      const ev = e.target.value as "created" | "updated" | "action"
                      set((cur) => ({ ...cur, when: { kind: "event", event: ev, ...(ev === "action" ? { actionId: offeredActions[0]?.actionId } : {}) } }))
                    }}
                  >
                    <option value="created">Record is created</option>
                    <option value="updated">Record is updated</option>
                    <option value="action">Specific action commits</option>
                  </select>
                </label>
                {def.when.event === "action" && (
                  <label className="grid gap-1 text-sm">
                    <span className="text-muted-foreground">Triggering action</span>
                    <select
                      aria-label="Triggering action"
                      className={selectCls}
                      value={def.when.actionId ?? ""}
                      onChange={(e) => set((cur) => cur.when.kind === "event" ? { ...cur, when: { ...cur.when, actionId: e.target.value } } : cur)}
                    >
                      {offeredActions.map((a) => <option key={a.actionId} value={a.actionId}>{a.title}</option>)}
                    </select>
                  </label>
                )}
              </div>
            )}

            {def.when.kind === "stale" && (
              <div className="flex flex-wrap items-end gap-3">
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Unchanged for</span>
                  <Input
                    type="number"
                    min={1}
                    aria-label="Stale duration amount"
                    className="h-9 w-24"
                    value={staleParts.amount}
                    onChange={(e) => set((cur) => cur.when.kind === "stale"
                      ? { ...cur, when: { ...cur.when, afterMs: msFromDuration(Number(e.target.value) || 1, staleParts.unit) } }
                      : cur)}
                  />
                </label>
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Unit</span>
                  <select
                    aria-label="Stale duration unit"
                    className={selectCls}
                    value={staleParts.unit}
                    onChange={(e) => set((cur) => cur.when.kind === "stale"
                      ? { ...cur, when: { ...cur.when, afterMs: msFromDuration(staleParts.amount, e.target.value as DurationUnit) } }
                      : cur)}
                  >
                    <option value="minutes">Minutes</option>
                    <option value="hours">Hours</option>
                    <option value="days">Days</option>
                  </select>
                </label>
                {lifecycles.length > 0 && (
                  <>
                    <label className="grid gap-1 text-sm">
                      <span className="text-muted-foreground">While property (optional)</span>
                      <select
                        aria-label="Stale state property"
                        className={selectCls}
                        value={def.when.property ?? ""}
                        onChange={(e) => {
                          const prop = e.target.value
                          const lc = lifecycles.find((l) => l.property === prop)
                          const st = lc ? lifecycleStates(lc)[0] : undefined
                          set((cur) => cur.when.kind === "stale"
                            ? { ...cur, when: { kind: "stale", afterMs: cur.when.afterMs, ...(prop && st ? { property: prop, state: st } : {}) } }
                            : cur)
                        }}
                      >
                        <option value="">Any state</option>
                        {lifecycles.map((lc) => <option key={lc.property} value={lc.property}>{lc.property.replaceAll("_", " ")}</option>)}
                      </select>
                    </label>
                    {def.when.property && (
                      <label className="grid gap-1 text-sm">
                        <span className="text-muted-foreground">Equals state</span>
                        <select
                          aria-label="Stale state value"
                          className={selectCls}
                          value={def.when.state ?? ""}
                          onChange={(e) => set((cur) => cur.when.kind === "stale"
                            ? { ...cur, when: { ...cur.when, state: e.target.value } }
                            : cur)}
                        >
                          {(() => {
                            const lc = lifecycles.find((l) => l.property === (def.when.kind === "stale" ? def.when.property : "")) ?? lifecycles[0]
                            return (lc ? lifecycleStates(lc) : []).map((state) => (
                              <option key={state} value={state}>{state.replaceAll("_", " ")}</option>
                            ))
                          })()}
                        </select>
                      </label>
                    )}
                  </>
                )}
              </div>
            )}
          </div>
        </Panel>
      </div>

      {/* 2. IF */}
      <div data-field="if">
        <Panel
          label="2. If (guard condition)"
          action={
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={rawMode && !rawCanSwitchToVisual}
              title={rawMode && !rawCanSwitchToVisual ? "This expression uses syntax or operators outside the visual controls" : undefined}
              onClick={() => {
                if (rawMode) {
                  const parsed = parseGuard(def.if?.expression)
                  if (parsed !== null && canRenderGuardVisually(parsed, fields)) {
                    setClauses(parsed)
                    setRawMode(false)
                  }
                } else {
                  setRawMode(true)
                }
              }}
            >
              <Code2 />{rawMode ? "Visual conditions" : "Edit expression"}
            </Button>
          }
        >
          {rawMode ? (
            <div className="space-y-2">
              <label className="grid gap-1 text-sm">
                <span className="text-muted-foreground">Boolean expression over <code className="font-mono">self</code></span>
                <Textarea
                  aria-label="Guard expression"
                  className="font-mono text-xs"
                  rows={2}
                  placeholder="e.g. self.score < 60"
                  value={def.if?.expression ?? ""}
                  onChange={(e) => set((cur) => ({ ...cur, if: e.target.value.trim() ? { expression: e.target.value } : undefined }))}
                />
              </label>
              <p className="text-meta text-muted-foreground">Checked by the cell against the record type; must return a boolean.</p>
            </div>
          ) : (
            <div className="space-y-3">
              {clauses.length === 0 && <p className="text-sm text-muted-foreground">No guard conditions. Runs for every matching trigger.</p>}
              {clauses.map((clause, index) => {
                const field = guardFields.find((f) => f.id === clause.property) ?? guardFields[0]
                if (!field) return null
                const ops = guardOpsFor(field.scalar)
                return (
                  <div key={index} className="flex flex-wrap items-center gap-2">
                    <select
                      aria-label={`Guard ${index + 1} property`}
                      className={selectCls}
                      value={clause.property}
                      onChange={(e) => {
                        const nextField = guardFields.find((f) => f.id === e.target.value)
                        if (nextField) updateClauses(clauses.map((c, i) => i === index ? blankGuardClause(nextField) : c))
                      }}
                    >
                      {guardFields.map((f) => <option key={f.id} value={f.id}>{f.id.replaceAll("_", " ")}</option>)}
                    </select>
                    <select
                      aria-label={`Guard ${index + 1} operator`}
                      className={selectCls}
                      value={clause.op}
                      onChange={(e) => updateClauses(clauses.map((c, i) => i === index ? { ...c, op: e.target.value as GuardOp } : c))}
                    >
                      {ops.map((op) => <option key={op} value={op}>{GUARD_OP_LABEL[op]}</option>)}
                    </select>
                    {field.scalar.kind === "enum" ? (
                      <select
                        aria-label={`Guard ${index + 1} value`}
                        className={selectCls}
                        value={clause.value}
                        onChange={(e) => updateClauses(clauses.map((c, i) => i === index ? { ...c, value: e.target.value } : c))}
                      >
                        {field.scalar.values.map((val) => <option key={val} value={val}>{val.replaceAll("_", " ")}</option>)}
                      </select>
                    ) : field.scalar.kind === "boolean" ? (
                      <select
                        aria-label={`Guard ${index + 1} value`}
                        className={selectCls}
                        value={clause.value}
                        onChange={(e) => updateClauses(clauses.map((c, i) => i === index ? { ...c, value: e.target.value } : c))}
                      >
                        <option value="true">Yes</option>
                        <option value="false">No</option>
                      </select>
                    ) : (
                      <Input
                        aria-label={`Guard ${index + 1} value`}
                        className="h-9 w-40"
                        inputMode={field.scalar.kind === "integer" || field.scalar.kind === "decimal" ? "decimal" : undefined}
                        value={clause.value}
                        onChange={(e) => updateClauses(clauses.map((c, i) => i === index ? { ...c, value: e.target.value } : c))}
                      />
                    )}
                    <Button type="button" variant="ghost" size="sm" onClick={() => updateClauses(clauses.filter((_, i) => i !== index))}>Remove</Button>
                  </div>
                )
              })}
              <div className="flex flex-wrap items-center gap-3">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={guardFields.length === 0}
                  onClick={() => { const first = guardFields[0]; if (first) updateClauses([...clauses, blankGuardClause(first)]) }}
                >
                  <Plus />Add guard condition
                </Button>
                {def.if?.expression && guardErrors.length === 0 && (
                  <span className="text-meta text-muted-foreground">
                    Expression: <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{def.if.expression}</code>
                  </span>
                )}
              </div>
              {guardErrors.length > 0 && (
                <div role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-meta text-destructive">
                  {guardErrors.map((msg) => <p key={msg}>{msg}</p>)}
                </div>
              )}
            </div>
          )}
        </Panel>
      </div>

      {/* 3. THEN */}
      <div data-field="then">
        <Panel label="3. Then">
          <div className="space-y-4">
            <div className="flex flex-wrap gap-2" role="radiogroup" aria-label="Action mode">
              <Button
                type="button"
                size="sm"
                variant={def.then.kind === "propose" ? "default" : "outline"}
                onClick={() => {
                  if (def.then.kind === "propose") return
                  set((cur) => ({
                    ...cur,
                    then: {
                      kind: "propose",
                      actionId: cur.then.actionId,
                      input: cur.then.input,
                      question: `Run ${selectedAction?.title ?? "action"} on {id}?`,
                      risk: "medium",
                      decider: { roles: ["admin"] },
                      expiresInMs: 86_400_000,
                    },
                  }))
                }}
              >
                Propose to Inbox
              </Button>
              <Button
                type="button"
                size="sm"
                variant={def.then.kind === "run" ? "default" : "outline"}
                disabled={def.when.kind === "stale"}
                title={def.when.kind === "stale" ? "Stale triggers propose to the Inbox because alarms run without a signed-in principal" : undefined}
                onClick={() => {
                  if (def.then.kind === "run" || def.when.kind === "stale") return
                  set((cur) => ({ ...cur, then: { kind: "run", actionId: cur.then.actionId, input: cur.then.input } }))
                }}
              >
                Run action automatically
              </Button>
            </div>
            {def.when.kind === "stale" && (
              <p className="text-meta text-muted-foreground">Stale triggers propose actions into the Inbox so a verified human approves execution.</p>
            )}

            <label className="grid max-w-md gap-1.5 text-sm">
              <span className="font-medium">Action</span>
              <select
                aria-label="Target action"
                className={selectCls}
                value={def.then.actionId}
                onChange={(e) => {
                  const nextAction = offeredActions.find((a) => a.actionId === e.target.value)
                  setStaticDrafts({})
                  set((cur) => ({
                    ...cur,
                    then: cur.then.kind === "propose"
                      ? { ...cur.then, actionId: e.target.value, input: undefined, question: cur.then.question || `Run ${nextAction?.title ?? e.target.value} on {id}?` }
                      : { ...cur.then, actionId: e.target.value, input: undefined },
                  }))
                }}
              >
                {offeredActions.length === 0 ? (
                  <option value="">No offered actions on this type</option>
                ) : (
                  <>
                    {def.then.actionId === "" && <option value="">Select an action…</option>}
                    {offeredActions.map((a) => <option key={a.actionId} value={a.actionId}>{a.title} ({a.actionId})</option>)}
                  </>
                )}
              </select>
            </label>

            {extraInputs.length > 0 && (
              <div className="grid max-w-lg gap-2">
                <span className="text-sm font-medium">Static action inputs</span>
                {extraInputs.map((field) => (
                  <label key={field.name} className="grid gap-1 text-sm">
                    <span className="text-muted-foreground">{field.label}</span>
                    {field.spec.kind === "choice" ? (
                      <select
                        aria-label={`Action input ${field.label}`}
                        className={selectCls}
                        value={staticDrafts[field.name] ?? ""}
                        onChange={(e) => updateStaticDraft(field.name, e.target.value)}
                      >
                        <option value="">Default / unset</option>
                        {field.spec.values.map((choice) => <option key={choice} value={choice}>{choice}</option>)}
                      </select>
                    ) : field.spec.kind === "boolean" ? (
                      <select
                        aria-label={`Action input ${field.label}`}
                        className={selectCls}
                        value={staticDrafts[field.name] ?? ""}
                        onChange={(e) => updateStaticDraft(field.name, e.target.value)}
                      >
                        <option value="">Default / unset</option>
                        <option value="true">Yes</option>
                        <option value="false">No</option>
                      </select>
                    ) : (
                      <Input
                        aria-label={`Action input ${field.label}`}
                        readOnly={field.spec.kind === "fixed"}
                        inputMode={field.spec.kind === "integer" || field.spec.kind === "number" ? "decimal" : undefined}
                        value={staticDrafts[field.name] ?? ""}
                        onChange={(e) => updateStaticDraft(field.name, e.target.value)}
                      />
                    )}
                  </label>
                ))}
                {staticInputErrors.length > 0 && (
                  <div role="alert" className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-meta text-destructive">
                    {staticInputErrors.map((msg) => <p key={msg}>{msg}</p>)}
                  </div>
                )}
              </div>
            )}

            {def.then.kind === "propose" ? (
              <div className="grid gap-4 md:grid-cols-2">
                <label className="grid gap-1.5 text-sm md:col-span-2">
                  <span className="font-medium">Inbox question <span className="font-normal text-muted-foreground">(use {"{id}"} for the record id)</span></span>
                  <Input
                    aria-label="Inbox question"
                    value={def.then.question}
                    onChange={(e) => set((cur) => cur.then.kind === "propose" ? { ...cur, then: { ...cur.then, question: e.target.value } } : cur)}
                  />
                </label>

                <label className="grid gap-1.5 text-sm">
                  <span className="font-medium">Risk level</span>
                  <select
                    aria-label="Risk level"
                    className={selectCls}
                    value={def.then.risk}
                    onChange={(e) => set((cur) => cur.then.kind === "propose" ? { ...cur, then: { ...cur.then, risk: e.target.value as Risk } } : cur)}
                  >
                    <option value="low">Low risk</option>
                    <option value="medium">Medium risk</option>
                    <option value="high">High risk</option>
                  </select>
                </label>

                <div className="flex flex-wrap items-end gap-2">
                  <label className="grid gap-1.5 text-sm">
                    <span className="font-medium">Proposal expires in</span>
                    <Input
                      type="number"
                      min={1}
                      aria-label="Expiry amount"
                      className="h-9 w-24"
                      value={expiresParts.amount}
                      onChange={(e) => set((cur) => cur.then.kind === "propose"
                        ? { ...cur, then: { ...cur.then, expiresInMs: msFromDuration(Number(e.target.value) || 1, expiresParts.unit) } }
                        : cur)}
                    />
                  </label>
                  <select
                    aria-label="Expiry unit"
                    className={selectCls}
                    value={expiresParts.unit}
                    onChange={(e) => set((cur) => cur.then.kind === "propose"
                      ? { ...cur, then: { ...cur.then, expiresInMs: msFromDuration(expiresParts.amount, e.target.value as DurationUnit) } }
                      : cur)}
                  >
                    <option value="hours">Hours</option>
                    <option value="days">Days</option>
                  </select>
                </div>

                <div className="space-y-1.5 md:col-span-2">
                  <span className="text-sm font-medium">Decider roles (who sees this proposal in their Inbox)</span>
                  <div className="flex flex-wrap gap-4">
                    {AUTOMATION_ROLES.map((role) => {
                      const checked = def.then.kind === "propose" && def.then.decider.roles.includes(role)
                      return (
                        <label key={role} className="inline-flex items-center gap-1.5 text-sm">
                          <input
                            type="checkbox"
                            aria-label={`Decider role ${role}`}
                            checked={checked}
                            onChange={(e) => set((cur) => {
                              if (cur.then.kind !== "propose") return cur
                              const roles = e.target.checked
                                ? [...new Set([...cur.then.decider.roles, role])]
                                : cur.then.decider.roles.filter((r) => r !== role)
                              return { ...cur, then: { ...cur.then, decider: { ...cur.then.decider, roles } } }
                            })}
                          />
                          <span>{role}</span>
                        </label>
                      )
                    })}
                  </div>
                </div>
              </div>
            ) : (
              <p className="text-meta text-muted-foreground">
                The action runs automatically under the cell's normal admission. Every law, precondition and permission still applies; refusals are recorded in run history.
              </p>
            )}
          </div>
        </Panel>
      </div>

      {previewData && (
        <Panel label="Test against current records">
          <div className="border-t px-4 py-3">
            <PreviewResults preview={previewData} />
          </div>
        </Panel>
      )}
    </div>
  )
}

/** `/automations/new` and `/automations/$id/edit`: build or edit a server-admitted When/If/Then automation. */
export function AutomationEditorPage() {
  const scope = useStudioScope()
  const cellAvailable = scope.isSuccess && scope.data !== null
  const { id } = useParams({ strict: false }) as { id?: string }
  const search = useSearch({ strict: false }) as { objectType?: string }
  const { read, catalog } = useCatalog()
  const navigate = useNavigate()
  const client = useQueryClient()
  const concepts = useMemo(() => (catalog?.concepts ?? []).filter((c) => c.definition === "object type"), [catalog])
  const stored = useQuery({
    queryKey: ["automation-edit", id, scope.data?.orgId, scope.data?.environmentId],
    queryFn: async () => (id === undefined ? null : (await automations.get(id)) ?? null),
    enabled: id !== undefined && cellAvailable,
    retry: false,
    staleTime: Infinity,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
  const [opened, setOpened] = useState(0)
  const fresh = id === undefined

  return (
    <section className="space-y-6">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="text-title font-semibold tracking-tight">{fresh ? "New automation" : (stored.data?.automation.title || "Edit automation")}</h1>
        {id !== undefined && (
          <Button asChild variant="outline" size="sm" className="ms-auto">
            <Link to="/automations/$id" params={{ id }}><Eye />Open &amp; history</Link>
          </Button>
        )}
      </div>
      {scope.isSuccess && scope.data === null && <p role="status" className="text-muted-foreground">{NO_DATA_CELL}</p>}
      {scope.isError && <p role="alert" className="text-destructive">{scope.error.message}</p>}
      {(scope.isPending || read.isPending || (!fresh && cellAvailable && stored.isPending)) && <p className="text-muted-foreground">Reading automation…</p>}
      {read.isError && <EmptyState icon={Zap}>{read.error.message}</EmptyState>}
      {stored.isError && <EmptyState icon={Zap}>{failure(stored.error)}</EmptyState>}
      {!fresh && stored.isSuccess && stored.data === null && <EmptyState icon={Zap}>No automation {id} is saved.</EmptyState>}
      {!scope.isPending && concepts.length === 0 && catalog && (
        <EmptyState icon={Shapes}>No record types are installed in this cell yet.</EmptyState>
      )}
      {!scope.isPending && concepts.length > 0 && (fresh || (stored.data && stored.isFetchedAfterMount)) && (
        <AutomationEditor
          key={`${scope.data?.orgId ?? "none"}:${scope.data?.environmentId ?? "none"}:${stored.data?.id ?? "new"}:${opened}`}
          concepts={concepts}
          initialType={search.objectType}
          stored={stored.data ?? undefined}
          cellAvailable={cellAvailable}
          onReload={() => void stored.refetch().then((r) => { if (r.isSuccess && r.data) setOpened((n) => n + 1) })}
          onSaved={(saved) => {
            void client.invalidateQueries({ queryKey: ["automations"] })
            void client.invalidateQueries({ queryKey: ["automation", saved.id] })
            void navigate({ to: "/automations/$id", params: { id: saved.id }, replace: fresh })
          }}
        />
      )}
    </section>
  )
}
