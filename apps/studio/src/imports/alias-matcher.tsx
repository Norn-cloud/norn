import { Check, ChevronsUpDown, Link2, Plus, Search, UserX, X } from "lucide-react"
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react"
import { Chip } from "@/components/system/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { Initials, ListRow } from "@/components/system/list-row"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { SourceTag } from "@/components/system/source-tag"
import { Dot } from "@/components/system/status-dot"
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
          className="h-9 w-full justify-between rounded-lg px-3 font-normal shadow-none max-sm:min-h-[44px]"
        >
          <span className={cn("truncate", !selected && "text-muted-foreground")}>
            {selected ? <><bdi>{selected.label}</bdi> (<bdi>{selected.id}</bdi>)</> : "Search existing records…"}
          </span>
          <ChevronsUpDown aria-hidden className="ms-1.5 size-3.5 shrink-0 text-muted-foreground" />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 gap-0 p-0">
        <div className="flex items-center gap-1.5 border-b border-line px-3 py-1.5">
          <Search aria-hidden className="size-3.5 shrink-0 text-muted-foreground" strokeWidth={1.6} />
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
            className="h-9 w-full bg-transparent text-body outline-none placeholder:text-muted-foreground max-sm:min-h-[44px]"
            autoFocus
          />
          {query !== "" && (
            <button type="button" aria-label="Clear filter" onClick={() => setQuery("")} className="grid size-7 shrink-0 place-items-center rounded-full text-muted-foreground outline-none hover:bg-(--well-hover) hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring max-sm:size-[44px]">
              <X aria-hidden className="size-3.5" />
            </button>
          )}
        </div>
        <ul id={listboxId} role="listbox" aria-label={ariaLabel} data-slot="row-list" className="flex max-h-64 flex-col overflow-y-auto p-1.5">
          {filtered.length === 0 && (
            <li className="px-3 py-4 text-center text-meta text-muted-foreground">No matching records.</li>
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
                <ListRow
                  tabIndex={-1}
                  onMouseEnter={() => setActiveIndex(index)}
                  onClick={() => pick(entity.id)}
                  selected={isSelected}
                  className={cn(isHighlighted && !isSelected && "bg-(--well-hover)")}
                  lead={<Initials id={entity.id} name={entity.label} />}
                  title={<bdi>{entity.label}</bdi>}
                  code={entity.id}
                  description={entity.subtitle ? <bdi>{entity.subtitle}</bdi> : undefined}
                  end={(isSuggested || isSelected) ? (
                    <>
                      {isSuggested && <Chip tone="amber">Suggested</Chip>}
                      {isSelected && <Check aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />}
                    </>
                  ) : undefined}
                />
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

  // A segment of a rounded-full track: the pressed one is the card at 500 (DESIGN.md 8.2, SegmentedControl).
  const segment = "inline-flex h-8 items-center gap-1.5 rounded-full px-3 text-body text-muted-foreground transition-colors duration-150 outline-none hover:bg-(--well-hover) hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring disabled:pointer-events-none disabled:opacity-45 motion-reduce:transition-none max-sm:min-h-[44px] aria-pressed:bg-card aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-control aria-pressed:ring-1 aria-pressed:ring-(--panel-border) aria-pressed:hover:bg-card"

  return (
    <Panel icon={Link2} label="Match aliases and entities" action={`${unresolvedCount} unresolved`}>
      <Well>
        {/* Toolbar: one ruled row at the top of the well */}
        <div className="flex flex-wrap items-center gap-2 border-b border-(--well-border) px-3.5 py-2.5">
          <div className="relative min-w-52 flex-1 sm:max-w-xs">
            <Search aria-hidden className="pointer-events-none absolute start-3 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" strokeWidth={1.6} />
            <Input
              aria-label="Filter external handles"
              placeholder="Filter handles, channels, excerpts…"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              className="bg-card ps-8 max-sm:min-h-[44px]"
            />
          </div>
          <div role="group" aria-label="Alias status filter" className="inline-flex flex-wrap rounded-full bg-(--well) p-0.5 ring-1 ring-line">
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
                className={cn(segment, "tabular-nums")}
              >
                {label}
              </button>
            ))}
          </div>
          <span className="ms-auto text-meta text-muted-foreground">
            Suggestions never auto-link — every alias binding commits as a governed action.
          </span>
        </div>

        {/* Alias rows: hairline-ruled rows in the well, no frames inside */}
        {visible.length === 0 ? (
          <EmptyState icon={Search} title="No matching handles">
            No external handles match this filter.
          </EmptyState>
        ) : (
          <ul className="divide-y divide-(--well-border)" aria-label="External alias candidates">
            {visible.map((alias) => {
              const state = stateOf(alias)
              const resolved = alias.resolution
              return (
                <li
                  key={alias.id}
                  data-alias-row={alias.id}
                  className="grid gap-3 px-3.5 py-3.5 lg:grid-cols-[minmax(0,1fr)_minmax(22rem,28rem)] lg:items-start"
                >
                  {/* Left: raw handle, source context, advisory suggestion */}
                  <div className="min-w-0 space-y-1.5">
                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
                      <code className="min-w-0 font-mono text-body tracking-[-0.01em] text-foreground [overflow-wrap:anywhere]">
                        <bdi>{alias.externalHandle}</bdi>
                      </code>
                      {resolved === null ? (
                        <FactStatusBadge status="imported_unverified" />
                      ) : resolved.kind === "ignored" ? (
                        <Chip tone="gray" dot data-alias-status="ignored" title="Handle explicitly ignored through a governed action; not linked to a canonical record.">
                          Ignored
                        </Chip>
                      ) : (
                        <FactStatusBadge status="verified_fact" />
                      )}
                      <SourceTag mono={false}><bdi>{alias.sourceChannel}</bdi></SourceTag>
                      <span className="text-meta text-muted-foreground tabular-nums">
                        {alias.occurrences} {alias.occurrences === 1 ? "occurrence" : "occurrences"} · first seen {readableTime(alias.firstObservedAt)}
                      </span>
                    </div>
                    <p className="text-meta text-muted-foreground">
                      Context: <span dir="auto" className="text-foreground">“<bdi>{alias.sampleExcerpt}</bdi>”</span>
                    </p>
                    {resolved === null && alias.suggestedTargetId !== null && (
                      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 pt-0.5">
                        <Chip
                          tone="amber"
                          data-advisory-badge=""
                          title="Advisory extraction hint only. Never auto-linked — you decide."
                        >
                          Suggested — you decide
                        </Chip>
                        <span className="text-meta text-muted-foreground">
                          Candidate: <span className="text-foreground"><bdi>{alias.suggestedTargetLabel ?? alias.suggestedTargetId}</bdi></span>
                          {" "}
                          <span className="font-mono">(<bdi>{alias.suggestedTargetId}</bdi>)</span>
                          {alias.suggestionConfidenceBps !== null && (
                            <span className="ms-1 tabular-nums">· {formatConfidence(alias.suggestionConfidenceBps)}</span>
                          )}
                        </span>
                        {state.mode === "match" && state.selectedTargetId !== alias.suggestedTargetId && (
                          <button
                            type="button"
                            onClick={() => patch(alias, { selectedTargetId: alias.suggestedTargetId ?? "", notice: null })}
                            className="inline-flex items-center rounded-sm text-meta text-foreground underline decoration-border underline-offset-4 outline-none hover:decoration-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring max-sm:min-h-[44px]"
                          >
                            Select suggestion
                          </button>
                        )}
                      </div>
                    )}
                  </div>

                  {/* Right: governed resolution controls or committed receipt (no frame: the row is the unit) */}
                  <div className="min-w-0">
                    {resolved !== null ? (
                      <div
                        role="status"
                        data-alias-resolved={resolved.kind}
                        className="space-y-1 text-body"
                      >
                        <div className="flex flex-wrap items-center gap-2 text-foreground">
                          {resolved.kind === "bound" && (
                            <>
                              <Link2 aria-hidden className="size-3.5 text-muted-foreground" strokeWidth={1.6} />
                              <span>Bound to <bdi>{resolved.targetLabel}</bdi></span>
                              <code className="font-mono text-meta text-muted-foreground">(<bdi>{resolved.targetId}</bdi>)</code>
                            </>
                          )}
                          {resolved.kind === "created" && (
                            <>
                              <Plus aria-hidden className="size-3.5 text-muted-foreground" strokeWidth={1.6} />
                              <span>Created &amp; bound <bdi>{resolved.targetLabel}</bdi></span>
                              <code className="font-mono text-meta text-muted-foreground">(<bdi>{resolved.targetId}</bdi>)</code>
                            </>
                          )}
                          {resolved.kind === "ignored" && (
                            <>
                              <UserX aria-hidden className="size-3.5 text-muted-foreground" strokeWidth={1.6} />
                              <span>Ignored handle</span>
                              {resolved.note && <span dir="auto" className="text-muted-foreground">· <bdi>{resolved.note}</bdi></span>}
                            </>
                          )}
                        </div>
                        <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground tabular-nums">
                          <span>commit {resolved.receipt.receipt.commitSeq}</span>
                          <span aria-hidden>·</span>
                          <span>receipt</span>
                          <CopyId value={resolved.receipt.receipt.receiptHash} short={12} />
                        </div>
                      </div>
                    ) : (
                      <div className="space-y-2">
                        {/* Action mode bar: Match | Create | Ignore */}
                        <div className="inline-flex flex-wrap items-center rounded-full bg-(--well) p-0.5 ring-1 ring-line" role="group" aria-label={`Resolution mode for ${alias.externalHandle}`}>
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
                              className={segment}
                            >
                              <Icon aria-hidden className="size-3.5 text-muted-foreground" strokeWidth={1.6} />
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
                              className="h-9"
                            >
                              {state.pending ? "Binding…" : "Bind alias"}
                            </Button>
                          </div>
                        )}

                        {state.mode === "create" && (
                          <div className="grid gap-2">
                            <div className="grid gap-2 sm:grid-cols-2">
                              <Input
                                dir="auto"
                                aria-label={`New record label for ${alias.externalHandle}`}
                                placeholder="Canonical record label"
                                value={state.createLabel}
                                disabled={state.pending}
                                onChange={(event) => patch(alias, { createLabel: event.target.value, notice: null })}
                                className="bg-card max-sm:min-h-[44px]"
                              />
                              <Input
                                dir="auto"
                                aria-label={`External key for ${alias.externalHandle}`}
                                placeholder="External key (optional)"
                                value={state.createExternalKey}
                                disabled={state.pending}
                                onChange={(event) => patch(alias, { createExternalKey: event.target.value, notice: null })}
                                className="bg-card font-mono tracking-[-0.01em] max-sm:min-h-[44px]"
                              />
                            </div>
                            <div className="flex justify-end">
                              <Button
                                type="button"
                                size="sm"
                                disabled={state.pending || state.createLabel.trim() === ""}
                                onClick={() => void onCommitAlias(batchId, alias)}
                                className="h-9"
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
                              className="flex-1 bg-card max-sm:min-h-[44px]"
                            />
                            <Button
                              type="button"
                              size="sm"
                              variant="outline"
                              disabled={state.pending}
                              onClick={() => void onCommitAlias(batchId, alias)}
                              className="h-9"
                            >
                              {state.pending ? "Saving…" : "Confirm ignore"}
                            </Button>
                          </div>
                        )}

                        {state.notice !== null && (
                          <div
                            role={state.notice.kind === "pending" ? "status" : "alert"}
                            data-alias-notice={state.notice.kind}
                            className="flex flex-wrap items-center gap-2 text-meta text-foreground"
                          >
                            <Dot tone={state.notice.kind === "pending" ? "held" : "bad"} />
                            <span className="min-w-0 flex-1">{state.notice.message}</span>
                            {state.notice.kind === "error" && (
                              <Button
                                type="button"
                                size="xs"
                                variant="outline"
                                disabled={state.pending}
                                onClick={() => void onCommitAlias(batchId, alias)}
                                className="max-sm:min-h-[44px]"
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
      </Well>
    </Panel>
  )
}
