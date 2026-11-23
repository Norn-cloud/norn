import { Check, ChevronsUpDown, Link2, Plus, Search, Sparkles, UserX, X } from "lucide-react"
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react"
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

export type AliasDecisionMode = "match" | "create" | "ignore"

export interface AliasDraftState {
  readonly mode: AliasDecisionMode
  readonly selectedTargetId: string
  readonly createLabel: string
  readonly createExternalKey: string
  readonly ignoreNote: string
  /** Reused across retries of the same draft; rotated only when the operator edits the draft. */
  readonly idempotencyKey: string
  readonly pending: boolean
  readonly notice: { readonly kind: "pending" | "error"; readonly message: string } | null
}

export function initialAliasDraft(alias: AliasCandidate): AliasDraftState {
  return {
    mode: "match",
    // Strictly never pre-commit or auto-select a fuzzy ML guess: the operator picks explicitly.
    selectedTargetId: "",
    createLabel: alias.externalHandle.replace(/^@/, ""),
    createExternalKey: alias.externalHandle,
    ignoreNote: "",
    idempotencyKey: newIdempotencyKey(),
    pending: false,
    notice: null,
  }
}

/**
 * Searchable record picker with full keyboard navigation (ArrowDown / ArrowUp / Enter / Escape)
 * and mixed-direction `<bdi>` isolation for Arabic/Latin/numeric labels and IDs.
 */
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
  const listboxId = useId()
  const triggerRef = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState("")
  const [activeIndex, setActiveIndex] = useState(0)
  const selected = entities.find((entity) => entity.id === value)
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (q === "") return entities
    return entities.filter((entity) =>
      entity.label.toLowerCase().includes(q)
      || entity.id.toLowerCase().includes(q)
      || (entity.subtitle?.toLowerCase().includes(q) ?? false))
  }, [entities, query])

  useEffect(() => {
    setActiveIndex(0)
  }, [query, open])

  const clampedIndex = filtered.length === 0 ? -1 : Math.min(activeIndex, filtered.length - 1)
  const activeEntity = clampedIndex >= 0 ? filtered[clampedIndex] : undefined
  const activeOptionId = activeEntity ? `${listboxId}-opt-${activeEntity.id}` : undefined

  const pick = (id: string) => {
    onSelect(id)
    setOpen(false)
    setQuery("")
    triggerRef.current?.focus()
  }

  const onInputKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault()
      if (filtered.length > 0) setActiveIndex((current) => (current + 1) % filtered.length)
    } else if (event.key === "ArrowUp") {
      event.preventDefault()
      if (filtered.length > 0) setActiveIndex((current) => (current - 1 + filtered.length) % filtered.length)
    } else if (event.key === "Enter") {
      event.preventDefault()
      if (activeEntity !== undefined) pick(activeEntity.id)
    } else if (event.key === "Escape") {
      event.preventDefault()
      setOpen(false)
      setQuery("")
      triggerRef.current?.focus()
    }
  }

  return (
    <Popover open={open} onOpenChange={(next) => { setOpen(next); if (!next) setQuery("") }}>
      <PopoverTrigger asChild>
        <Button
          ref={triggerRef}
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          aria-controls={open ? listboxId : undefined}
          aria-label={ariaLabel}
          disabled={disabled}
          className="h-8 w-full justify-between rounded-md border-border bg-background px-2.5 text-xs font-normal shadow-none"
        >
          <span className={cn("truncate", !selected && "text-muted-foreground")}>
            {selected ? <><bdi>{selected.label}</bdi> (<bdi>{selected.id}</bdi>)</> : "Search existing records…"}
          </span>
          <ChevronsUpDown aria-hidden className="ms-1.5 size-3.5 shrink-0 text-muted-foreground" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 rounded-lg border border-border bg-popover p-0 shadow-float">
        <div className="flex items-center gap-1.5 border-b border-border px-2.5 py-2">
          <Search aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
          <input
            type="text"
            role="combobox"
            aria-expanded={open}
            aria-controls={listboxId}
            aria-activedescendant={activeOptionId}
            aria-autocomplete="list"
            aria-label="Filter records"
            placeholder="Filter by name or ID…"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={onInputKeyDown}
            className="w-full bg-transparent text-xs outline-none placeholder:text-muted-foreground"
            autoFocus
          />
          {query !== "" && (
            <button type="button" aria-label="Clear filter" onClick={() => setQuery("")} className="text-muted-foreground hover:text-foreground">
              <X className="size-3" />
            </button>
          )}
        </div>
        <ul id={listboxId} role="listbox" aria-label={ariaLabel} className="max-h-56 overflow-y-auto p-1">
          {filtered.length === 0 && (
            <li className="px-2.5 py-4 text-center text-xs text-muted-foreground">No matching records.</li>
          )}
          {filtered.map((entity, index) => {
            const isSelected = entity.id === value
            const isHighlighted = index === clampedIndex
            const isSuggested = entity.id === suggestedId
            return (
              <li
                key={entity.id}
                id={`${listboxId}-opt-${entity.id}`}
                role="option"
                aria-selected={isSelected}
                data-highlighted={isHighlighted ? "true" : undefined}
              >
                <button
                  type="button"
                  tabIndex={-1}
                  onMouseEnter={() => setActiveIndex(index)}
                  onClick={() => pick(entity.id)}
                  className={cn(
                    "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start text-xs outline-none hover:bg-muted",
                    (isHighlighted || isSelected) && "bg-muted",
                    isSelected && "font-medium",
                  )}
                >
                  <Check aria-hidden className={cn("size-3.5 shrink-0", isSelected ? "opacity-100 text-tone-green" : "opacity-0")} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-foreground"><bdi>{entity.label}</bdi></span>
                    <span className="block truncate font-mono text-[0.6875rem] text-muted-foreground">
                      <bdi>{entity.id}</bdi>{entity.subtitle ? <> · <bdi>{entity.subtitle}</bdi></> : null}
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
 * searchable picker, Create, or Ignore. Advisory ML suggestions are explicitly badged
 * "Suggested — you decide" and never auto-linked.
 */
export function AliasMatcher({
  batchId,
  aliases,
  entities,
  actions,
  drafts,
  onPatchDraft,
  onResolved,
}: {
  readonly batchId: string
  readonly aliases: readonly AliasCandidate[]
  readonly entities: readonly TargetEntity[]
  readonly actions: TriageActions
  readonly drafts: Readonly<Record<string, AliasDraftState>>
  readonly onPatchDraft: (batchId: string, alias: AliasCandidate, update: Partial<AliasDraftState>, rotateKey?: boolean) => void
  readonly onResolved: (batchId: string, aliasId: string, resolution: AliasResolution, createdEntity?: TargetEntity) => void
}) {
  const [filter, setFilter] = useState("")
  const [statusFilter, setStatusFilter] = useState<"all" | "unresolved" | "resolved">("all")
  const inFlightRef = useRef<Set<string>>(new Set())

  const stateOf = (alias: AliasCandidate): AliasDraftState => drafts[alias.id] ?? initialAliasDraft(alias)
  const patch = (alias: AliasCandidate, update: Partial<AliasDraftState>, rotateKey = false) =>
    onPatchDraft(batchId, alias, update, rotateKey)

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
    if (inFlightRef.current.has(alias.id)) return
    const state = stateOf(alias)
    const requestBatchId = batchId
    const key = state.idempotencyKey
    inFlightRef.current.add(alias.id)
    patch(alias, { pending: true, notice: null })
    try {
      if (state.mode === "match") {
        const target = entities.find((entity) => entity.id === state.selectedTargetId)
        if (!target) {
          patch(alias, { pending: false, notice: { kind: "error", message: "Select a canonical record before binding this alias." } })
          return
        }
        const result = await commitAliasBinding(actions.bindAlias, {
          batchId: requestBatchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          targetRecordId: target.id,
        }, key)
        handleCallResult(requestBatchId, alias, result, (receipt) => {
          onResolved(requestBatchId, alias.id, { kind: "bound", targetId: target.id, targetLabel: target.label, receipt })
        })
      } else if (state.mode === "create") {
        const label = state.createLabel.trim()
        if (label === "") {
          patch(alias, { pending: false, notice: { kind: "error", message: "Enter a record label before creating and binding." } })
          return
        }
        const externalKey = state.createExternalKey.trim()
        const result = await commitEntityCreateAndBind(actions.createEntity, {
          batchId: requestBatchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          label,
          ...externalKey !== "" ? { externalKey } : {},
        }, key)
        handleCallResult(requestBatchId, alias, result, (receipt) => {
          const created: TargetEntity = { id: receipt.objectId, label, subtitle: externalKey || undefined }
          onResolved(requestBatchId, alias.id, { kind: "created", targetId: receipt.objectId, targetLabel: label, receipt }, created)
        })
      } else {
        const note = state.ignoreNote.trim()
        const result = await commitAliasIgnore(actions.ignoreAlias, {
          batchId: requestBatchId,
          aliasId: alias.id,
          externalHandle: alias.externalHandle,
          ...note !== "" ? { note } : {},
        }, key)
        handleCallResult(requestBatchId, alias, result, (receipt) => {
          onResolved(requestBatchId, alias.id, { kind: "ignored", ...note !== "" ? { note } : {}, receipt })
        })
      }
    } catch (error) {
      onPatchDraft(requestBatchId, alias, {
        pending: false,
        notice: { kind: "error", message: error instanceof Error ? error.message : "The cell did not respond. Retry reuses the same idempotency key." },
      })
    } finally {
      inFlightRef.current.delete(alias.id)
    }
  }

  const handleCallResult = (
    requestBatchId: string,
    alias: AliasCandidate,
    result: Awaited<ReturnType<typeof commitAliasBinding>>,
    onCommitted: (receipt: ActionReceipt) => void,
  ) => {
    if (result.kind === "receipt") {
      onPatchDraft(requestBatchId, alias, { pending: false, notice: null })
      onCommitted(result.receipt)
    } else if (result.kind === "refused") {
      onPatchDraft(requestBatchId, alias, { pending: false, notice: { kind: "error", message: `${result.code}: ${result.message}` } })
    } else if (result.kind === "pending") {
      onPatchDraft(requestBatchId, alias, { pending: false, notice: { kind: "pending", message: `Pending approval decision (${result.decision.id}).` } })
    } else {
      onPatchDraft(requestBatchId, alias, { pending: false, notice: { kind: "error", message: "The cell could not commit this alias action right now." } })
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
                      <bdi>{alias.externalHandle}</bdi>
                    </code>
                    {resolved === null ? (
                      <FactStatusBadge status="imported_unverified" />
                    ) : resolved.kind === "ignored" ? (
                      <Chip tone="gray" icon={UserX} data-alias-status="ignored" title="Handle explicitly ignored through a governed action; not linked to a canonical record.">
                        Ignored
                      </Chip>
                    ) : (
                      <FactStatusBadge status="verified_fact" />
                    )}
                    <Chip tone="gray"><bdi>{alias.sourceChannel}</bdi></Chip>
                    <span className="text-xs text-muted-foreground tabular-nums">
                      {alias.occurrences} {alias.occurrences === 1 ? "occurrence" : "occurrences"} · first seen {readableTime(alias.firstObservedAt)}
                    </span>
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Context: <span dir="auto" className="text-foreground">“<bdi>{alias.sampleExcerpt}</bdi>”</span>
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
                        Candidate: <span className="font-medium text-foreground"><bdi>{alias.suggestedTargetLabel ?? alias.suggestedTargetId}</bdi></span>
                        {" "}
                        <span className="font-mono text-[0.6875rem]">(<bdi>{alias.suggestedTargetId}</bdi>)</span>
                        {alias.suggestionConfidenceBps !== null && (
                          <span className="ms-1 tabular-nums text-tone-amber">· {formatConfidence(alias.suggestionConfidenceBps)}</span>
                        )}
                      </span>
                      {state.mode === "match" && state.selectedTargetId !== alias.suggestedTargetId && (
                        <button
                          type="button"
                          onClick={() => patch(alias, { selectedTargetId: alias.suggestedTargetId ?? "", notice: null }, true)}
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
                      className={cn(
                        "space-y-1.5 rounded-md border px-3 py-2 text-xs",
                        resolved.kind === "ignored"
                          ? "border-border bg-surface-subtle"
                          : "border-tone-green/30 bg-tone-green/5",
                      )}
                    >
                      <div className="flex flex-wrap items-center gap-2 font-medium text-foreground">
                        {resolved.kind === "bound" && (
                          <>
                            <Link2 aria-hidden className="size-3.5 text-tone-green" />
                            <span>Bound to <bdi>{resolved.targetLabel}</bdi></span>
                            <code className="font-mono text-[0.6875rem] text-muted-foreground">(<bdi>{resolved.targetId}</bdi>)</code>
                          </>
                        )}
                        {resolved.kind === "created" && (
                          <>
                            <Plus aria-hidden className="size-3.5 text-tone-green" />
                            <span>Created &amp; bound <bdi>{resolved.targetLabel}</bdi></span>
                            <code className="font-mono text-[0.6875rem] text-muted-foreground">(<bdi>{resolved.targetId}</bdi>)</code>
                          </>
                        )}
                        {resolved.kind === "ignored" && (
                          <>
                            <UserX aria-hidden className="size-3.5 text-muted-foreground" />
                            <span>Ignored handle</span>
                            {resolved.note && <span dir="auto" className="font-normal text-muted-foreground">· <bdi>{resolved.note}</bdi></span>}
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
                            disabled={state.pending}
                            onClick={() => patch(alias, { mode, notice: null }, true)}
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
                              onSelect={(id) => patch(alias, { selectedTargetId: id, notice: null }, true)}
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
                              dir="auto"
                              aria-label={`New record label for ${alias.externalHandle}`}
                              placeholder="Canonical record label"
                              value={state.createLabel}
                              disabled={state.pending}
                              onChange={(event) => patch(alias, { createLabel: event.target.value, notice: null }, true)}
                              className="h-8 bg-background text-xs shadow-none"
                            />
                            <Input
                              dir="auto"
                              aria-label={`External key for ${alias.externalHandle}`}
                              placeholder="External key (optional)"
                              value={state.createExternalKey}
                              disabled={state.pending}
                              onChange={(event) => patch(alias, { createExternalKey: event.target.value, notice: null }, true)}
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
                            dir="auto"
                            aria-label={`Reason to ignore ${alias.externalHandle}`}
                            placeholder="Reason (optional, recorded in receipt)"
                            value={state.ignoreNote}
                            disabled={state.pending}
                            onChange={(event) => patch(alias, { ignoreNote: event.target.value, notice: null }, true)}
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

                      {state.notice !== null && (
                        <p
                          role={state.notice.kind === "error" ? "alert" : "status"}
                          className={cn(
                            "rounded border px-2 py-1 text-xs",
                            state.notice.kind === "pending"
                              ? "border-tone-amber/40 bg-tone-amber/8 text-tone-amber"
                              : "border-destructive/30 bg-destructive/5 text-destructive",
                          )}
                        >
                          {state.notice.message}
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
