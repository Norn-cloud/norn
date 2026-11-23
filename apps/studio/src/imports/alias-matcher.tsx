import { Check, ChevronsUpDown, Link2, Plus, Search, Sparkles, UserX, X } from "lucide-react"
import { useMemo, useState } from "react"
import { Chip } from "@/components/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { newIdempotencyKey, type ActionReceipt } from "@/lib/actions"
import { readableTime } from "@/lib/format"
import {
  commitAliasBinding,
  commitAliasIgnore,
  commitEntityCreateAndBind,
  formatConfidence,
  type AliasCandidate,
  type AliasResolution,
  type TargetEntity,
  type TriageActions,
} from "@/lib/import-triage"
import { cn } from "@/lib/utils"

type DecisionMode = "match" | "create" | "ignore"

interface RowActionState {
  readonly mode: DecisionMode
  readonly selectedTargetId: string
  readonly createLabel: string
  readonly createExternalKey: string
  readonly ignoreNote: string
  readonly pending: boolean
  readonly error: string | null
}

function initialRowState(alias: AliasCandidate): RowActionState {
  return {
    mode: "match",
    // Strictly never pre-commit or auto-select a fuzzy ML guess: the operator picks explicitly.
    selectedTargetId: "",
    createLabel: alias.externalHandle.replace(/^@/, ""),
    createExternalKey: alias.externalHandle,
    ignoreNote: "",
    pending: false,
    error: null,
  }
}

/** Searchable record picker for mapping an external handle to an existing canonical record. */
function SearchableRecordPicker({
  entities,
  value,
  suggestedId,
  disabled,
  ariaLabel,
  onSelect,
}: {
  readonly entities: readonly TargetEntity[]
  readonly value: string
  readonly suggestedId: string | null
  readonly disabled?: boolean
  readonly ariaLabel: string
  readonly onSelect: (id: string) => void
}) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState("")
  const selected = entities.find((entity) => entity.id === value)
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (q === "") return entities
    return entities.filter((entity) =>
      entity.label.toLowerCase().includes(q)
      || entity.id.toLowerCase().includes(q)
      || (entity.subtitle?.toLowerCase().includes(q) ?? false))
  }, [entities, query])

  return (
    <Popover open={open} onOpenChange={(next) => { setOpen(next); if (!next) setQuery("") }}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          aria-label={ariaLabel}
          disabled={disabled}
          className="h-8 w-full justify-between rounded-md border-border bg-background px-2.5 text-xs font-normal shadow-none"
        >
          <span className={cn("truncate", !selected && "text-muted-foreground")}>
            {selected ? `${selected.label} (${selected.id})` : "Search existing records…"}
          </span>
          <ChevronsUpDown aria-hidden className="ms-1.5 size-3.5 shrink-0 text-muted-foreground" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 rounded-lg border border-border bg-popover p-0 shadow-float">
        <div className="flex items-center gap-1.5 border-b border-border px-2.5 py-2">
          <Search aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
          <input
            type="text"
            aria-label="Filter records"
            placeholder="Filter by name or ID…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            className="w-full bg-transparent text-xs outline-none placeholder:text-muted-foreground"
            autoFocus
          />
          {query !== "" && (
            <button type="button" aria-label="Clear filter" onClick={() => setQuery("")} className="text-muted-foreground hover:text-foreground">
              <X className="size-3" />
            </button>
          )}
        </div>
        <ul role="listbox" aria-label={ariaLabel} className="max-h-56 overflow-y-auto p-1">
          {filtered.length === 0 && (
            <li className="px-2.5 py-4 text-center text-xs text-muted-foreground">No matching records.</li>
          )}
          {filtered.map((entity) => {
            const isSelected = entity.id === value
            const isSuggested = entity.id === suggestedId
            return (
              <li key={entity.id} role="option" aria-selected={isSelected}>
                <button
                  type="button"
                  onClick={() => { onSelect(entity.id); setOpen(false); setQuery("") }}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start text-xs outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
                    isSelected && "bg-muted font-medium",
                  )}
                >
                  <Check aria-hidden className={cn("size-3.5 shrink-0", isSelected ? "opacity-100 text-tone-green" : "opacity-0")} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-foreground">{entity.label}</span>
                    <span className="block truncate font-mono text-[0.6875rem] text-muted-foreground">
                      {entity.id}{entity.subtitle ? ` · ${entity.subtitle}` : ""}
                    </span>
                  </span>
                  {isSuggested && (
                    <Chip tone="amber" className="text-[0.6875rem]">Suggested</Chip>
                  )}
                </button>
              </li>
            )
          })}
        </ul>
      </PopoverContent>
    </Popover>
  )
}

/**
 * Step 1 — Match Aliases / Entities Screen.
 * Maps recurring external handles/nicknames from external feeds/captures to canonical records via
 * searchable picker, + Create, or Ignore. Advisory ML suggestions are explicitly badged
 * "Suggested — you decide" and never auto-linked.
 */
export function AliasMatcher({
  batchId,
  aliases,
  entities,
  actions,
  onResolved,
}: {
  readonly batchId: string
  readonly aliases: readonly AliasCandidate[]
  readonly entities: readonly TargetEntity[]
  readonly actions: TriageActions
  readonly onResolved: (aliasId: string, resolution: AliasResolution, createdEntity?: TargetEntity) => void
}) {
  const [filter, setFilter] = useState("")
  const [statusFilter, setStatusFilter] = useState<"all" | "unresolved" | "resolved">("all")
  const [drafts, setDrafts] = useState<Readonly<Record<string, RowActionState>>>({})

  const stateOf = (alias: AliasCandidate): RowActionState => drafts[alias.id] ?? initialRowState(alias)
  const patch = (alias: AliasCandidate, update: Partial<RowActionState>) =>
    setDrafts((current) => ({ ...current, [alias.id]: { ...(current[alias.id] ?? initialRowState(alias)), ...update } }))

  const visible = useMemo(() => {
    const q = filter.trim().toLowerCase()
    return aliases.filter((alias) => {
      if (statusFilter === "unresolved" && alias.resolution !== null) return false
      if (statusFilter === "resolved" && alias.resolution === null) return false
      if (q === "") return true
      return alias.externalHandle.toLowerCase().includes(q)
        || alias.sourceChannel.toLowerCase().includes(q)
        || alias.sampleExcerpt.toLowerCase().includes(q)
        || (alias.suggestedTargetLabel?.toLowerCase().includes(q) ?? false)
    })
  }, [aliases, filter, statusFilter])

  const unresolvedCount = aliases.filter((alias) => alias.resolution === null).length

  const commit = async (alias: AliasCandidate) => {
    const state = stateOf(alias)
    patch(alias, { pending: true, error: null })
    const key = newIdempotencyKey()
    try {
      if (state.mode === "match") {
        const target = entities.find((entity) => entity.id === state.selectedTargetId)
        if (!target) {
          patch(alias, { pending: false, error: "Select a canonical record before binding this alias." })
          return
        }
        const result = await commitAliasBinding(actions.bindAlias, {
          batchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          targetRecordId: target.id,
        }, key)
        handleCallResult(alias, result, (receipt) => {
          onResolved(alias.id, { kind: "bound", targetId: target.id, targetLabel: target.label, receipt })
        })
      } else if (state.mode === "create") {
        const label = state.createLabel.trim()
        if (label === "") {
          patch(alias, { pending: false, error: "Enter a record label before creating and binding." })
          return
        }
        const externalKey = state.createExternalKey.trim()
        const result = await commitEntityCreateAndBind(actions.createEntity, {
          batchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          label,
          ...externalKey !== "" ? { externalKey } : {},
        }, key)
        handleCallResult(alias, result, (receipt) => {
          const created: TargetEntity = { id: receipt.objectId, label, subtitle: externalKey || undefined }
          onResolved(alias.id, { kind: "created", targetId: receipt.objectId, targetLabel: label, receipt }, created)
        })
      } else {
        const note = state.ignoreNote.trim()
        const result = await commitAliasIgnore(actions.ignoreAlias, {
          batchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          ...note !== "" ? { note } : {},
        }, key)
        handleCallResult(alias, result, (receipt) => {
          onResolved(alias.id, { kind: "ignored", ...note !== "" ? { note } : {}, receipt })
        })
      }
    } catch (error) {
      patch(alias, { pending: false, error: error instanceof Error ? error.message : "The cell did not respond." })
    }
  }

  const handleCallResult = (
    alias: AliasCandidate,
    result: Awaited<ReturnType<typeof commitAliasBinding>>,
    onCommitted: (receipt: ActionReceipt) => void,
  ) => {
    if (result.kind === "receipt") {
      patch(alias, { pending: false, error: null })
      onCommitted(result.receipt)
    } else if (result.kind === "refused") {
      patch(alias, { pending: false, error: `${result.code}: ${result.message}` })
    } else if (result.kind === "pending") {
      patch(alias, { pending: false, error: `Pending approval decision (${result.decision.id}).` })
    } else {
      patch(alias, { pending: false, error: "The cell could not commit this alias action right now." })
    }
  }

  return (
    <section aria-label="Match aliases and entities" className="overflow-hidden rounded-lg border border-border bg-background">
      {/* Flat toolbar */}
      <div className="flex flex-wrap items-center gap-2 border-b border-border bg-surface-subtle px-4 py-2.5">
        <div className="relative min-w-52 flex-1 sm:max-w-xs">
          <Search aria-hidden className="pointer-events-none absolute start-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            aria-label="Filter external handles"
            placeholder="Filter handles, channels, excerpts…"
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            className="h-8 bg-background ps-8 text-xs shadow-none"
          />
        </div>
        <div role="group" aria-label="Alias status filter" className="inline-flex rounded-md border border-border bg-background p-0.5 text-xs">
          {([
            ["all", `All (${aliases.length})`],
            ["unresolved", `Unresolved (${unresolvedCount})`],
            ["resolved", `Resolved (${aliases.length - unresolvedCount})`],
          ] as const).map(([key, label]) => (
            <button
              key={key}
              type="button"
              aria-pressed={statusFilter === key}
              onClick={() => setStatusFilter(key)}
              className={cn(
                "rounded px-2.5 py-1 font-medium transition-colors",
                statusFilter === key ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {label}
            </button>
          ))}
        </div>
        <span className="ms-auto text-xs text-muted-foreground">
          Suggestions never auto-link — every alias binding commits as a governed action.
        </span>
      </div>

      {/* Alias rows */}
      {visible.length === 0 ? (
        <div className="px-4 py-10 text-center text-sm text-muted-foreground">
          No external handles match this filter.
        </div>
      ) : (
        <ul className="divide-y divide-border" aria-label="External alias candidates">
          {visible.map((alias) => {
            const state = stateOf(alias)
            const resolved = alias.resolution
            return (
              <li
                key={alias.id}
                data-alias-row={alias.id}
                className="grid gap-3 px-4 py-3.5 lg:grid-cols-[minmax(0,1fr)_minmax(22rem,28rem)] lg:items-start"
              >
                {/* Left: raw handle, source context, advisory suggestion */}
                <div className="min-w-0 space-y-1.5">
                  <div className="flex flex-wrap items-center gap-2">
                    <code className="rounded border border-border bg-surface-subtle px-1.5 py-0.5 font-mono text-xs font-medium text-foreground">
                      {alias.externalHandle}
                    </code>
                    <FactStatusBadge status={resolved === null ? "imported_unverified" : "verified_fact"} />
                    <Chip tone="gray">{alias.sourceChannel}</Chip>
                    <span className="text-xs text-muted-foreground tabular-nums">
                      {alias.occurrences} {alias.occurrences === 1 ? "occurrence" : "occurrences"} · first seen {readableTime(alias.firstObservedAt)}
                    </span>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Context: <span className="text-foreground">“{alias.sampleExcerpt}”</span>
                  </p>
                  {resolved === null && alias.suggestedTargetId !== null && (
                    <div className="flex flex-wrap items-center gap-2 pt-0.5">
                      <Chip
                        tone="amber"
                        icon={Sparkles}
                        data-advisory-badge=""
                        title="Advisory extraction hint only. Never auto-linked — you decide."
                      >
                        Suggested — you decide
                      </Chip>
                      <span className="text-xs text-muted-foreground">
                        Candidate: <span className="font-medium text-foreground">{alias.suggestedTargetLabel ?? alias.suggestedTargetId}</span>
                        {" "}
                        <span className="font-mono text-[0.6875rem]">({alias.suggestedTargetId})</span>
                        {alias.suggestionConfidenceBps !== null && (
                          <span className="ms-1 tabular-nums text-tone-amber">· {formatConfidence(alias.suggestionConfidenceBps)}</span>
                        )}
                      </span>
                      {state.mode === "match" && state.selectedTargetId !== alias.suggestedTargetId && (
                        <button
                          type="button"
                          onClick={() => patch(alias, { selectedTargetId: alias.suggestedTargetId ?? "", error: null })}
                          className="text-xs font-medium underline decoration-border underline-offset-4 hover:decoration-foreground"
                        >
                          Select suggestion
                        </button>
                      )}
                    </div>
                  )}
                </div>

                {/* Right: governed resolution controls or committed receipt */}
                <div className="min-w-0">
                  {resolved !== null ? (
                    <div
                      role="status"
                      data-alias-resolved={resolved.kind}
                      className="space-y-1.5 rounded-md border border-tone-green/30 bg-tone-green/5 px-3 py-2 text-xs"
                    >
                      <div className="flex flex-wrap items-center gap-2 font-medium text-foreground">
                        {resolved.kind === "bound" && (
                          <>
                            <Link2 aria-hidden className="size-3.5 text-tone-green" />
                            <span>Bound to {resolved.targetLabel}</span>
                            <code className="font-mono text-[0.6875rem] text-muted-foreground">({resolved.targetId})</code>
                          </>
                        )}
                        {resolved.kind === "created" && (
                          <>
                            <Plus aria-hidden className="size-3.5 text-tone-green" />
                            <span>Created &amp; bound {resolved.targetLabel}</span>
                            <code className="font-mono text-[0.6875rem] text-muted-foreground">({resolved.targetId})</code>
                          </>
                        )}
                        {resolved.kind === "ignored" && (
                          <>
                            <UserX aria-hidden className="size-3.5 text-muted-foreground" />
                            <span>Ignored handle</span>
                            {resolved.note && <span className="font-normal text-muted-foreground">· {resolved.note}</span>}
                          </>
                        )}
                      </div>
                      <div className="flex flex-wrap items-center gap-2 text-[0.6875rem] text-muted-foreground tabular-nums">
                        <span>commit {resolved.receipt.receipt.commitSeq}</span>
                        <span>·</span>
                        <span>receipt</span>
                        <CopyId value={resolved.receipt.receipt.receiptHash} short={12} />
                      </div>
                    </div>
                  ) : (
                    <div className="space-y-2 rounded-md border border-border bg-surface-subtle p-2.5">
                      {/* Action mode bar: Match | Create | Ignore */}
                      <div className="flex items-center gap-1" role="group" aria-label={`Resolution mode for ${alias.externalHandle}`}>
                        {([
                          ["match", "Match record", Link2],
                          ["create", "Create", Plus],
                          ["ignore", "Ignore", UserX],
                        ] as const).map(([mode, label, Icon]) => (
                          <button
                            key={mode}
                            type="button"
                            aria-pressed={state.mode === mode}
                            onClick={() => patch(alias, { mode, error: null })}
                            className={cn(
                              "inline-flex h-7 items-center gap-1 rounded border px-2 text-xs font-medium transition-colors",
                              state.mode === mode
                                ? "border-foreground/25 bg-background text-foreground"
                                : "border-transparent text-muted-foreground hover:bg-background/70 hover:text-foreground",
                            )}
                          >
                            <Icon aria-hidden className="size-3" />
                            {label}
                          </button>
                        ))}
                      </div>

                      {state.mode === "match" && (
                        <div className="flex items-center gap-2">
                          <div className="min-w-0 flex-1">
                            <SearchableRecordPicker
                              entities={entities}
                              value={state.selectedTargetId}
                              suggestedId={alias.suggestedTargetId}
                              disabled={state.pending}
                              ariaLabel={`Pick canonical record for ${alias.externalHandle}`}
                              onSelect={(id) => patch(alias, { selectedTargetId: id, error: null })}
                            />
                          </div>
                          <Button
                            type="button"
                            size="sm"
                            disabled={state.pending || state.selectedTargetId === ""}
                            onClick={() => void commit(alias)}
                            className="h-8 shrink-0 px-3 text-xs"
                          >
                            {state.pending ? "Binding…" : "Bind alias"}
                          </Button>
                        </div>
                      )}

                      {state.mode === "create" && (
                        <div className="grid gap-2">
                          <div className="grid gap-1.5 sm:grid-cols-2">
                            <Input
                              aria-label={`New record label for ${alias.externalHandle}`}
                              placeholder="Canonical record label"
                              value={state.createLabel}
                              disabled={state.pending}
                              onChange={(event) => patch(alias, { createLabel: event.target.value, error: null })}
                              className="h-8 bg-background text-xs shadow-none"
                            />
                            <Input
                              aria-label={`External key for ${alias.externalHandle}`}
                              placeholder="External key (optional)"
                              value={state.createExternalKey}
                              disabled={state.pending}
                              onChange={(event) => patch(alias, { createExternalKey: event.target.value, error: null })}
                              className="h-8 bg-background font-mono text-xs shadow-none"
                            />
                          </div>
                          <div className="flex justify-end">
                            <Button
                              type="button"
                              size="sm"
                              disabled={state.pending || state.createLabel.trim() === ""}
                              onClick={() => void commit(alias)}
                              className="h-8 px-3 text-xs"
                            >
                              {state.pending ? "Creating…" : "Create & bind"}
                            </Button>
                          </div>
                        </div>
                      )}

                      {state.mode === "ignore" && (
                        <div className="flex items-center gap-2">
                          <Input
                            aria-label={`Reason to ignore ${alias.externalHandle}`}
                            placeholder="Reason (optional, recorded in receipt)"
                            value={state.ignoreNote}
                            disabled={state.pending}
                            onChange={(event) => patch(alias, { ignoreNote: event.target.value, error: null })}
                            className="h-8 flex-1 bg-background text-xs shadow-none"
                          />
                          <Button
                            type="button"
                            size="sm"
                            variant="outline"
                            disabled={state.pending}
                            onClick={() => void commit(alias)}
                            className="h-8 shrink-0 px-3 text-xs"
                          >
                            {state.pending ? "Saving…" : "Confirm ignore"}
                          </Button>
                        </div>
                      )}

                      {state.error !== null && (
                        <p role="alert" className="rounded border border-destructive/30 bg-destructive/5 px-2 py-1 text-xs text-destructive">
                          {state.error}
                        </p>
                      )}
                    </div>
                  )}
                </div>
              </li>
            )
          })}
        </ul>
      )}
    </section>
  )
}
