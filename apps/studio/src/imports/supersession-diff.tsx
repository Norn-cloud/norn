import { Archive, ArrowRight, CheckCircle2, Clock, GitCompareArrows } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/system/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { SourceTag } from "@/components/system/source-tag"
import { Dot, StatusDot } from "@/components/system/status-dot"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { readableTime } from "@/lib/format"
import {
  formatConfidence,
  initialSupersessionDraft,
  supersessionFactStatus,
  timeDeltaLabel,
  type SupersessionChoice,
  type SupersessionConflict,
  type SupersessionDraftState,
} from "@/lib/import-triage"
import { cn } from "@/lib/utils"

const CHOICES: readonly {
  readonly value: SupersessionChoice
  readonly label: string
  readonly detail: string
}[] = [
  {
    value: "accept_newer",
    label: "Accept newer artifact (default)",
    detail: "Promotes the newer snapshot as the superseding correction while preserving the earlier artifact as immutable historical evidence.",
  },
  {
    value: "keep_earlier",
    label: "Keep earlier artifact",
    detail: "Keeps the earlier snapshot authoritative and retains the newer artifact as superseded historical evidence.",
  },
  {
    value: "keep_disputed",
    label: "Keep both quarantined (disputed)",
    detail: "Leaves this window held out of authoritative reads until further investigation.",
  },
]

/** A segment of the diff's visibility switch (DESIGN.md 10, SegmentedControl): the active one is the card at 500. */
const SEGMENT = "rounded-full px-2.5 py-1 text-meta text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring aria-pressed:bg-card aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:ring-1 aria-pressed:ring-(--panel-border) max-sm:min-h-[44px]"

/**
 * Step 3 — Supersession Diff Card (`FEED-CONFLICT`).
 * Groups two immutable artifacts for the same window, displays changed cells and capture
 * timestamps, and defaults to accepting the newer artifact while preserving the earlier artifact
 * as immutable historical evidence. All locks and stable idempotency keys live in `ImportTriagePage`.
 */
export function SupersessionDiffList({
  batchId,
  conflicts,
  lowConfidenceThresholdBps,
  drafts,
  onPatchDraft,
  onCommitSupersession,
}: {
  readonly batchId: string
  readonly conflicts: readonly SupersessionConflict[]
  readonly lowConfidenceThresholdBps: number
  readonly drafts: Readonly<Record<string, SupersessionDraftState>>
  readonly onPatchDraft: (batchId: string, conflict: SupersessionConflict, update: Partial<SupersessionDraftState>) => void
  readonly onCommitSupersession: (batchId: string, conflict: SupersessionConflict) => Promise<void>
}) {
  if (conflicts.length === 0) {
    return (
      <Panel icon={GitCompareArrows} label="Supersession conflicts">
        <Well>
          <EmptyState icon={CheckCircle2} title="No conflicts">
            No overlapping artifacts or supersession conflicts in this batch.
          </EmptyState>
        </Well>
      </Panel>
    )
  }

  return (
    <div className="space-y-4" aria-label="Supersession conflicts">
      {conflicts.map((conflict) => (
        <SupersessionDiffCard
          key={conflict.id}
          batchId={batchId}
          conflict={conflict}
          lowConfidenceThresholdBps={lowConfidenceThresholdBps}
          draft={drafts[conflict.id] ?? initialSupersessionDraft(conflict)}
          onPatchDraft={(update) => onPatchDraft(batchId, conflict, update)}
          onCommit={() => onCommitSupersession(batchId, conflict)}
        />
      ))}
    </div>
  )
}

function SupersessionDiffCard({
  conflict,
  lowConfidenceThresholdBps,
  draft,
  onPatchDraft,
  onCommit,
}: {
  readonly batchId: string
  readonly conflict: SupersessionConflict
  readonly lowConfidenceThresholdBps: number
  readonly draft: SupersessionDraftState
  readonly onPatchDraft: (update: Partial<SupersessionDraftState>) => void
  readonly onCommit: () => Promise<void>
}) {
  const [changedOnly, setChangedOnly] = useState(true)

  const changedCells = conflict.cells.filter((cell) => cell.changed)
  const shownCells = changedOnly ? changedCells : conflict.cells
  const resolved = conflict.resolution
  const delta = timeDeltaLabel(conflict.earlier.observedAt, conflict.newer.observedAt)
  const factStatus = supersessionFactStatus(conflict)

  return (
    // One ShellCard per conflict: the header names the window and its state, the well holds ruled sections.
    <article data-supersession-card={conflict.id}>
      <Panel
        icon={GitCompareArrows}
        label={<>Window <bdi>{conflict.windowLabel}</bdi></>}
        action={factStatus !== null ? (
          <FactStatusBadge status={factStatus} />
        ) : (
          <Chip tone="gray" dot data-supersession-status="resolved" title="Conflict resolved through a governed action; affected rows remain subject to cell verification.">
            Conflict resolved
          </Chip>
        )}
      >
        <Well className="divide-y divide-(--well-border)">
          {/* Conflict facts */}
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-4 py-2.5 text-meta text-muted-foreground tabular-nums">
            <Chip tone="red" mono>{conflict.findingCode}</Chip>
            <SourceTag>{conflict.scopeKey}</SourceTag>
            <span className="inline-flex items-center gap-1">
              <Clock aria-hidden className="size-3.5" strokeWidth={1.6} />
              {delta}
            </span>
            <span>{changedCells.length} changed {changedCells.length === 1 ? "cell" : "cells"}</span>
          </div>

          {/* Immutable artifact pair */}
          <div className="grid divide-y divide-(--well-border) sm:grid-cols-2 sm:divide-x sm:divide-y-0">
            <div className="space-y-1 px-4 py-3 text-meta" data-artifact-side="earlier">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-muted-foreground">Earlier artifact (preserved)</span>
                <SourceTag icon={Archive} mono={false}>Immutable evidence</SourceTag>
              </div>
              <div className="text-body text-foreground"><bdi>{conflict.earlier.label}</bdi></div>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-meta text-muted-foreground tabular-nums">
                <span>Observed {readableTime(conflict.earlier.observedAt)}</span>
                <span className="inline-flex items-center gap-1">Blob <CopyId value={conflict.earlier.blobDigest} short={12} /></span>
                <span className="inline-flex items-center gap-1">Receipt <CopyId value={conflict.earlier.functionReceiptDigest} short={10} /></span>
              </div>
            </div>
            <div className="space-y-1 px-4 py-3 text-meta" data-artifact-side="newer">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-muted-foreground">Newer artifact (superseding)</span>
                <StatusDot tone="held">{delta}</StatusDot>
              </div>
              <div className="text-body text-foreground"><bdi>{conflict.newer.label}</bdi></div>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-meta text-muted-foreground tabular-nums">
                <span>Observed {readableTime(conflict.newer.observedAt)}</span>
                <span className="inline-flex items-center gap-1">Blob <CopyId value={conflict.newer.blobDigest} short={12} /></span>
                <span className="inline-flex items-center gap-1">Receipt <CopyId value={conflict.newer.functionReceiptDigest} short={10} /></span>
              </div>
            </div>
          </div>

          {/* Diff table */}
          <div>
            <div className="flex flex-wrap items-center justify-between gap-2 px-4 py-2 text-meta">
              <span className="text-muted-foreground">Cell differences</span>
              <div role="group" aria-label="Diff visibility" className="inline-flex rounded-full bg-(--well) p-0.5 ring-1 ring-line ring-inset">
                <button type="button" aria-pressed={changedOnly} onClick={() => setChangedOnly(true)} className={SEGMENT}>
                  Changed only ({changedCells.length})
                </button>
                <button type="button" aria-pressed={!changedOnly} onClick={() => setChangedOnly(false)} className={SEGMENT}>
                  All cells ({conflict.cells.length})
                </button>
              </div>
            </div>

            <div className="overflow-x-auto">
              <table className="w-full border-collapse text-meta tabular-nums">
                <thead>
                  <tr className="border-y border-(--well-border) text-muted-foreground">
                    <th className="px-4 py-2 text-start font-normal">Record</th>
                    <th className="px-4 py-2 text-start font-normal">Field</th>
                    <th className="px-4 py-2 text-start font-normal">Earlier value</th>
                    <th className="px-4 py-2 text-start font-normal">Newer value</th>
                    <th className="px-4 py-2 text-end font-normal">Confidence</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-(--well-border)">
                  {shownCells.map((cell) => {
                    const lowConf = cell.newerConfidenceBps !== null && cell.newerConfidenceBps < lowConfidenceThresholdBps
                    return (
                      <tr
                        key={cell.id}
                        data-diff-cell={cell.id}
                        data-changed={cell.changed ? "true" : "false"}
                        className="transition-colors hover:bg-(--well-hover)"
                      >
                        <td className="px-4 py-2 text-foreground"><bdi>{cell.entityLabel}</bdi></td>
                        <td className="px-4 py-2 text-muted-foreground">{cell.fieldLabel}</td>
                        <td className="px-4 py-2 font-mono">
                          <span className={cn(cell.changed ? "text-muted-foreground line-through" : "text-muted-foreground")}>
                            <bdi>{cell.earlierValue}</bdi>
                          </span>
                        </td>
                        <td className="px-4 py-2 font-mono">
                          <span className="inline-flex items-center gap-1.5">
                            {cell.changed && <ArrowRight aria-hidden className="size-3 text-muted-foreground rtl:rotate-180" />}
                            <span className={cn(cell.changed ? "text-foreground" : "text-muted-foreground")}>
                              <bdi>{cell.newerValue}</bdi>
                            </span>
                            {cell.hasFinding && <Chip tone="red">Finding</Chip>}
                          </span>
                        </td>
                        <td className="px-4 py-2 text-end font-mono text-muted-foreground">
                          {lowConf ? (
                            <StatusDot tone="held" title="Low extraction confidence">{formatConfidence(cell.newerConfidenceBps)} low</StatusDot>
                          ) : formatConfidence(cell.newerConfidenceBps)}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          </div>

          {/* Governed resolution section */}
          <div className="space-y-3 px-4 py-3.5">
            {resolved !== null ? (
              <div role="status" data-supersession-resolved={resolved.choice} className="space-y-1.5 text-meta">
                <div className="flex flex-wrap items-center gap-2 text-body text-foreground">
                  <Dot tone="live">
                    <span>
                      {resolved.choice === "accept_newer"
                        ? "Accepted newer artifact as superseding correction"
                        : resolved.choice === "keep_earlier"
                          ? "Kept earlier artifact as authoritative"
                          : "Kept both artifacts quarantined as disputed"}
                    </span>
                  </Dot>
                </div>
                <p className="text-muted-foreground">
                  Historical evidence preserved: immutable artifact <code className="font-mono text-foreground"><bdi>{resolved.preservedHistoricalArtifactId}</bdi></code> remains linked and readable.
                  {resolved.note ? <> Rationale: “<bdi>{resolved.note}</bdi>”.</> : null}
                </p>
                <div className="flex flex-wrap items-center gap-2 text-meta text-muted-foreground tabular-nums">
                  <span>commit {resolved.receipt.receipt.commitSeq}</span>
                  <span aria-hidden>·</span>
                  <span>receipt</span>
                  <CopyId value={resolved.receipt.receipt.receiptHash} short={12} />
                </div>
              </div>
            ) : (
              <>
                <fieldset className="space-y-1">
                  <legend className="mb-1.5 text-meta text-muted-foreground">Governed supersession decision</legend>
                  {CHOICES.map((option) => (
                    <label
                      key={option.value}
                      className="flex cursor-pointer items-start gap-3 rounded-(--radius-row) px-3 py-2 text-meta transition-colors hover:bg-(--well-hover) has-checked:bg-card has-checked:ring-1 has-checked:ring-(--panel-border) has-checked:ring-inset has-disabled:cursor-default max-sm:min-h-[44px]"
                    >
                      <input
                        type="radio"
                        name={`supersession-${conflict.id}`}
                        value={option.value}
                        checked={draft.choice === option.value}
                        disabled={draft.pending}
                        onChange={() => {
                          if (draft.choice === option.value) return
                          onPatchDraft({ choice: option.value, notice: null })
                        }}
                        className="mt-1 size-3.5 shrink-0 accent-primary"
                      />
                      <span className="space-y-0.5">
                        <span className="block text-body text-foreground">{option.label}</span>
                        <span className="block text-meta text-muted-foreground">{option.detail}</span>
                      </span>
                    </label>
                  ))}
                </fieldset>

                <div className="flex flex-wrap items-center gap-2">
                  <Input
                    dir="auto"
                    aria-label={`Supersession rationale for ${conflict.windowLabel}`}
                    placeholder="Rationale note (optional, stored with the governed receipt)"
                    value={draft.note}
                    disabled={draft.pending}
                    onChange={(event) => onPatchDraft({ note: event.target.value, notice: null })}
                    className="min-w-56 flex-1 bg-card shadow-none max-sm:min-h-[44px]"
                  />
                  <Button type="button" disabled={draft.pending} onClick={() => void onCommit()}>
                    {draft.pending ? "Committing…" : "Resolve supersession"}
                  </Button>
                </div>

                <p className="flex items-center gap-1.5 text-meta text-muted-foreground">
                  <Archive aria-hidden className="size-3.5 shrink-0" strokeWidth={1.6} />
                  <span>
                    Earlier artifact <code className="font-mono"><bdi>{conflict.earlier.blobDigest.slice(0, 14)}…</bdi></code> is never deleted and stays preserved as immutable historical evidence.
                  </span>
                </p>

                {draft.notice !== null && (
                  <div
                    role={draft.notice.kind === "pending" ? "status" : "alert"}
                    data-supersession-notice={draft.notice.kind}
                    className="flex flex-wrap items-center gap-2 text-meta text-foreground"
                  >
                    <Dot tone={draft.notice.kind === "pending" ? "held" : "bad"}>
                      <span className="min-w-0 flex-1">{draft.notice.message}</span>
                    </Dot>
                    {draft.notice.kind === "error" && (
                      <Button type="button" size="sm" variant="outline" disabled={draft.pending} onClick={() => void onCommit()}>
                        Retry
                      </Button>
                    )}
                  </div>
                )}
              </>
            )}
          </div>
        </Well>
      </Panel>
    </article>
  )
}
