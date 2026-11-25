import { Check, ChevronsUpDown, Link2, Plus, Search, Sparkles, UserX, X } from "lucide-react"
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react"
import { Chip } from "@/components/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { readableTime } from "@/lib/format"
import {
  formatConfidence,
  initialAliasDraft,
  type AliasCandidate,
  type AliasDraftState,
  type TargetEntity,
} from "@/lib/import-triage"
import { cn } from "@/lib/utils"

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
          className="h-8 w-full justify-between rounded-md border-border bg-background px-2.5 text-meta font-normal shadow-none"
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
            className="w-full bg-transparent text-meta outline-none placeholder:text-muted-foreground"
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
            <li className="px-2.5 py-4 text-center text-meta text-muted-foreground">No matching records.</li>
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
                    "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start text-meta outline-none hover:bg-muted",
                    (isHighlighted || isSelected) && "bg-muted",
                    isSelected && "",
                  )}
                >
                  <Check aria-hidden className={cn("size-3.5 shrink-0", isSelected ? "opacity-100 text-tone-green" : "opacity-0")} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-foreground"><bdi>{entity.label}</bdi></span>
                    <span className="block truncate font-mono text-meta text-muted-foreground">
                      <bdi>{entity.id}</bdi>{entity.subtitle ? <> · <bdi>{entity.subtitle}</bdi></> : null}
                    </span>
                  </span>
                  {isSuggested && (
                    <Chip tone="amber" className="text-meta">Suggested</Chip>
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
 * "Suggested — you decide" and never auto-linked. All async locks and idempotency keys live in
 * `ImportTriagePage` so switching steps never drops in-flight locks or rotates unchanged keys.
 */
export function AliasMatcher({
  batchId,
  aliases,
  entities,
  drafts,
  onPatchDraft,
  onCommitAlias,
}: {
  readonly batchId: string
  readonly aliases: readonly AliasCandidate[]
  readonly entities: readonly TargetEntity[]
  readonly drafts: Readonly<Record<string, AliasDraftState>>
  readonly onPatchDraft: (batchId: string, alias: AliasCandidate, update: Partial<AliasDraftState>) => void
  readonly onCommitAlias: (batchId: string, alias: AliasCandidate) => Promise<void>
}) {
  const [filter, setFilter] = useState("")
  const [statusFilter, setStatusFilter] = useState<"all" | "unresolved" | "resolved">("all")

  const stateOf = (alias: AliasCandidate): AliasDraftState => drafts[alias.id] ?? initialAliasDraft(alias)
  const patch = (alias: AliasCandidate, update: Partial<AliasDraftState>) =>
    onPatchDraft(batchId, alias, update)

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
            className="h-8 bg-background ps-8 text-meta shadow-none"
          />
        </div>
        <div role="group" aria-label="Alias status filter" className="inline-flex rounded-md border border-border bg-background p-0.5 text-meta">
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
                "rounded px-2.5 py-1 transition-colors",
                statusFilter === key ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {label}
            </button>
          ))}
        </div>
        <span className="ms-auto text-meta text-muted-foreground">
          Suggestions never auto-link — every alias binding commits as a governed action.
        </span>
      </div>

      {/* Alias rows */}
      {visible.length === 0 ? (
        <div className="px-4 py-10 text-center text-body text-muted-foreground">
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
                    <code className="rounded border border-border bg-surface-subtle px-1.5 py-0.5 font-mono text-meta text-foreground">
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
                    <span className="text-meta text-muted-foreground tabular-nums">
                      {alias.occurrences} {alias.occurrences === 1 ? "occurrence" : "occurrences"} · first seen {readableTime(alias.firstObservedAt)}
                    </span>
                  </div>
                  <p className="text-meta text-muted-foreground">
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
                      <span className="text-meta text-muted-foreground">
                        Candidate: <span className="text-foreground"><bdi>{alias.suggestedTargetLabel ?? alias.suggestedTargetId}</bdi></span>
                        {" "}
                        <span className="font-mono text-meta">(<bdi>{alias.suggestedTargetId}</bdi>)</span>
                        {alias.suggestionConfidenceBps !== null && (
                          <span className="ms-1 tabular-nums text-tone-amber">· {formatConfidence(alias.suggestionConfidenceBps)}</span>
                        )}
                      </span>
                      {state.mode === "match" && state.selectedTargetId !== alias.suggestedTargetId && (
                        <button
                          type="button"
                          onClick={() => patch(alias, { selectedTargetId: alias.suggestedTargetId ?? "", notice: null })}
                          className="text-meta underline decoration-border underline-offset-4 hover:decoration-foreground"
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
                        "space-y-1.5 rounded-md border px-3 py-2 text-meta",
                        resolved.kind === "ignored"
                          ? "border-border bg-surface-subtle"
                          : "border-tone-green/30 bg-tone-green/5",
                      )}
                    >
                      <div className="flex flex-wrap items-center gap-2 text-foreground">
                        {resolved.kind === "bound" && (
                          <>
                            <Link2 aria-hidden className="size-3.5 text-tone-green" />
                            <span>Bound to <bdi>{resolved.targetLabel}</bdi></span>
                            <code className="font-mono text-meta text-muted-foreground">(<bdi>{resolved.targetId}</bdi>)</code>
                          </>
                        )}
                        {resolved.kind === "created" && (
                          <>
                            <Plus aria-hidden className="size-3.5 text-tone-green" />
                            <span>Created &amp; bound <bdi>{resolved.targetLabel}</bdi></span>
                            <code className="font-mono text-meta text-muted-foreground">(<bdi>{resolved.targetId}</bdi>)</code>
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
                      <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground tabular-nums">
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
                            onClick={() => {
                              if (state.mode === mode) return
                              patch(alias, { mode, notice: null })
                            }}
                            className={cn(
                              "inline-flex h-7 items-center gap-1 rounded border px-2 text-meta transition-colors",
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
                              onSelect={(id) => patch(alias, { selectedTargetId: id, notice: null })}
                            />
                          </div>
                          <Button
                            type="button"
                            size="sm"
                            disabled={state.pending || state.selectedTargetId === ""}
                            onClick={() => void onCommitAlias(batchId, alias)}
                            className="h-8 shrink-0 px-3 text-meta"
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
                              onChange={(event) => patch(alias, { createLabel: event.target.value, notice: null })}
                              className="h-8 bg-background text-meta shadow-none"
                            />
                            <Input
                              dir="auto"
                              aria-label={`External key for ${alias.externalHandle}`}
                              placeholder="External key (optional)"
                              value={state.createExternalKey}
                              disabled={state.pending}
                              onChange={(event) => patch(alias, { createExternalKey: event.target.value, notice: null })}
                              className="h-8 bg-background font-mono text-meta shadow-none"
                            />
                          </div>
                          <div className="flex justify-end">
                            <Button
                              type="button"
                              size="sm"
                              disabled={state.pending || state.createLabel.trim() === ""}
                              onClick={() => void onCommitAlias(batchId, alias)}
                              className="h-8 px-3 text-meta"
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
                            onChange={(event) => patch(alias, { ignoreNote: event.target.value, notice: null })}
                            className="h-8 flex-1 bg-background text-meta shadow-none"
                          />
                          <Button
                            type="button"
                            size="sm"
                            variant="outline"
                            disabled={state.pending}
                            onClick={() => void onCommitAlias(batchId, alias)}
                            className="h-8 shrink-0 px-3 text-meta"
                          >
                            {state.pending ? "Saving…" : "Confirm ignore"}
                          </Button>
                        </div>
                      )}

                      {state.notice !== null && (
                        <div
                          role={state.notice.kind === "pending" ? "status" : "alert"}
                          data-alias-notice={state.notice.kind}
                          className={cn(
                            "flex flex-wrap items-center gap-2 rounded border px-2 py-1 text-meta",
                            state.notice.kind === "pending"
                              ? "border-tone-amber/40 bg-tone-amber/8 text-tone-amber"
                              : "border-destructive/30 bg-destructive/5 text-destructive",
                          )}
                        >
                          <span className="min-w-0 flex-1">{state.notice.message}</span>
                          {state.notice.kind === "error" && (
                            <Button
                              type="button"
                              size="sm"
                              variant="outline"
                              disabled={state.pending}
                              onClick={() => void onCommitAlias(batchId, alias)}
                              className="h-6 shrink-0 border-border bg-background px-2 text-meta text-foreground shadow-none"
                            >
                              Retry
                            </Button>
                          )}
                        </div>
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
