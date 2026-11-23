import { Archive, ArrowRight, CheckCircle2, Clock, GitCompareArrows, ShieldAlert } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
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
      <section className="rounded-lg border border-border bg-background px-4 py-10 text-center text-sm text-muted-foreground">
        No overlapping artifacts or supersession conflicts in this batch.
      </section>
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
    <article
      data-supersession-card={conflict.id}
      className="overflow-hidden rounded-lg border border-border bg-background"
    >
      {/* Card header */}
      <header className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <GitCompareArrows aria-hidden className="size-4 text-muted-foreground" />
          <h2 className="text-sm font-medium text-foreground">
            Window <bdi>{conflict.windowLabel}</bdi>
          </h2>
          <Chip tone="red" icon={ShieldAlert} mono>{conflict.findingCode}</Chip>
          {factStatus !== null ? (
            <FactStatusBadge status={factStatus} />
          ) : (
            <Chip tone="gray" icon={CheckCircle2} data-supersession-status="resolved" title="Conflict resolved through a governed action; affected rows remain subject to cell verification.">
              Conflict resolved
            </Chip>
          )}
          <span className="font-mono text-xs text-muted-foreground"><bdi>{conflict.scopeKey}</bdi></span>
        </div>
        <div className="flex items-center gap-2 text-xs text-muted-foreground tabular-nums">
          <span className="inline-flex items-center gap-1">
            <Clock aria-hidden className="size-3.5" />
            {delta}
          </span>
          <span>·</span>
          <span>{changedCells.length} changed {changedCells.length === 1 ? "cell" : "cells"}</span>
        </div>
      </header>

      {/* Immutable artifact pair */}
      <div className="grid divide-y divide-border border-b border-border sm:grid-cols-2 sm:divide-x sm:divide-y-0">
        <div className="space-y-1 px-4 py-3 text-xs" data-artifact-side="earlier">
          <div className="flex items-center justify-between gap-2">
            <span className="font-medium text-muted-foreground">Earlier artifact (preserved)</span>
            <Chip tone="gray" icon={Archive}>Immutable evidence</Chip>
          </div>
          <div className="font-medium text-foreground"><bdi>{conflict.earlier.label}</bdi></div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[0.6875rem] text-muted-foreground tabular-nums">
            <span>Observed {readableTime(conflict.earlier.observedAt)}</span>
            <span className="inline-flex items-center gap-1">Blob <CopyId value={conflict.earlier.blobDigest} short={12} /></span>
            <span className="inline-flex items-center gap-1">Receipt <CopyId value={conflict.earlier.functionReceiptDigest} short={10} /></span>
          </div>
        </div>
        <div className="space-y-1 px-4 py-3 text-xs" data-artifact-side="newer">
          <div className="flex items-center justify-between gap-2">
            <span className="font-medium text-foreground">Newer artifact (superseding)</span>
            <Chip tone="amber">{delta}</Chip>
          </div>
          <div className="font-medium text-foreground"><bdi>{conflict.newer.label}</bdi></div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[0.6875rem] text-muted-foreground tabular-nums">
            <span>Observed {readableTime(conflict.newer.observedAt)}</span>
            <span className="inline-flex items-center gap-1">Blob <CopyId value={conflict.newer.blobDigest} short={12} /></span>
            <span className="inline-flex items-center gap-1">Receipt <CopyId value={conflict.newer.functionReceiptDigest} short={10} /></span>
          </div>
        </div>
      </div>

      {/* Diff table */}
      <div className="border-b border-border">
        <div className="flex items-center justify-between bg-surface-subtle/60 px-4 py-2 text-xs">
          <span className="font-medium text-muted-foreground">Cell differences</span>
          <div role="group" aria-label="Diff visibility" className="inline-flex rounded border border-border bg-background p-0.5 text-xs">
            <button
              type="button"
              aria-pressed={changedOnly}
              onClick={() => setChangedOnly(true)}
              className={cn("rounded px-2 py-0.5 font-medium", changedOnly ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground")}
            >
              Changed only ({changedCells.length})
            </button>
            <button
              type="button"
              aria-pressed={!changedOnly}
              onClick={() => setChangedOnly(false)}
              className={cn("rounded px-2 py-0.5 font-medium", !changedOnly ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground")}
            >
              All cells ({conflict.cells.length})
            </button>
          </div>
        </div>

        <div className="overflow-x-auto">
          <table className="w-full border-collapse text-xs tabular-nums">
            <thead>
              <tr className="border-y border-border bg-surface-subtle text-start text-muted-foreground">
                <th className="px-4 py-2 text-start font-medium">Record</th>
                <th className="px-4 py-2 text-start font-medium">Field</th>
                <th className="px-4 py-2 text-start font-medium">Earlier value</th>
                <th className="px-4 py-2 text-start font-medium">Newer value</th>
                <th className="px-4 py-2 text-end font-medium">Confidence</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {shownCells.map((cell) => {
                const lowConf = cell.newerConfidenceBps !== null && cell.newerConfidenceBps < lowConfidenceThresholdBps
                return (
                  <tr
                    key={cell.id}
                    data-diff-cell={cell.id}
                    data-changed={cell.changed ? "true" : "false"}
                    className={cn(cell.changed && "bg-tone-amber/5")}
                  >
                    <td className="px-4 py-2 font-medium text-foreground"><bdi>{cell.entityLabel}</bdi></td>
                    <td className="px-4 py-2 text-muted-foreground">{cell.fieldLabel}</td>
                    <td className="px-4 py-2 font-mono">
                      <span className={cn(cell.changed && "text-muted-foreground line-through")}>
                        <bdi>{cell.earlierValue}</bdi>
                      </span>
                    </td>
                    <td className="px-4 py-2 font-mono">
                      <span className="inline-flex items-center gap-1.5">
                        {cell.changed && <ArrowRight aria-hidden className="size-3 text-tone-amber" />}
                        <span className={cn(cell.changed ? "font-semibold text-foreground" : "text-muted-foreground")}>
                          <bdi>{cell.newerValue}</bdi>
                        </span>
                        {cell.hasFinding && <Chip tone="red" mono>Finding</Chip>}
                      </span>
                    </td>
                    <td className="px-4 py-2 text-end font-mono">
                      <span className={cn(lowConf ? "font-medium text-tone-amber" : "text-muted-foreground")}>
                        {formatConfidence(cell.newerConfidenceBps)}
                      </span>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      </div>

      {/* Governed resolution section */}
      <div className="space-y-3 bg-surface-subtle/50 px-4 py-3.5">
        {resolved !== null ? (
          <div
            role="status"
            data-supersession-resolved={resolved.choice}
            className="space-y-1.5 rounded-md border border-tone-green/35 bg-tone-green/5 px-3 py-2.5 text-xs"
          >
            <div className="flex flex-wrap items-center gap-2 font-medium text-foreground">
              <CheckCircle2 aria-hidden className="size-4 text-tone-green" />
              <span>
                {resolved.choice === "accept_newer"
                  ? "Accepted newer artifact as superseding correction"
                  : resolved.choice === "keep_earlier"
                    ? "Kept earlier artifact as authoritative"
                    : "Kept both artifacts quarantined as disputed"}
              </span>
            </div>
            <p className="text-muted-foreground">
              Historical evidence preserved: immutable artifact <code className="font-mono text-foreground"><bdi>{resolved.preservedHistoricalArtifactId}</bdi></code> remains linked and readable.
              {resolved.note ? <> Rationale: “<bdi>{resolved.note}</bdi>”.</> : null}
            </p>
            <div className="flex flex-wrap items-center gap-2 text-[0.6875rem] text-muted-foreground tabular-nums">
              <span>commit {resolved.receipt.receipt.commitSeq}</span>
              <span>·</span>
              <span>receipt</span>
              <CopyId value={resolved.receipt.receipt.receiptHash} short={12} />
            </div>
          </div>
        ) : (
          <>
            <fieldset className="grid gap-2 sm:grid-cols-3">
              <legend className="mb-1.5 text-xs font-medium text-foreground">Governed supersession decision</legend>
              {CHOICES.map((option) => {
                const selected = draft.choice === option.value
                return (
                  <label
                    key={option.value}
                    className={cn(
                      "flex cursor-pointer items-start gap-2 rounded-md border p-2.5 text-xs transition-colors",
                      selected
                        ? "border-foreground/40 bg-background"
                        : "border-border bg-background/50 hover:bg-background",
                    )}
                  >
                    <input
                      type="radio"
                      name={`supersession-${conflict.id}`}
                      value={option.value}
                      checked={selected}
                      disabled={draft.pending}
                      onChange={() => {
                        if (draft.choice === option.value) return
                        onPatchDraft({ choice: option.value, notice: null })
                      }}
                      className="mt-0.5 size-3.5 accent-[var(--primary)]"
                    />
                    <span className="space-y-0.5">
                      <span className="block font-medium text-foreground">{option.label}</span>
                      <span className="block text-[0.6875rem] text-muted-foreground">{option.detail}</span>
                    </span>
                  </label>
                )
              })}
            </fieldset>

            <div className="flex flex-wrap items-center gap-2">
              <Input
                dir="auto"
                aria-label={`Supersession rationale for ${conflict.windowLabel}`}
                placeholder="Rationale note (optional, stored with the governed receipt)"
                value={draft.note}
                disabled={draft.pending}
                onChange={(event) => onPatchDraft({ note: event.target.value, notice: null })}
                className="h-8 min-w-56 flex-1 bg-background text-xs shadow-none"
              />
              <Button
                type="button"
                size="sm"
                disabled={draft.pending}
                onClick={() => void onCommit()}
                className="h-8 shrink-0 px-3 text-xs"
              >
                {draft.pending ? "Committing…" : "Resolve supersession"}
              </Button>
            </div>

            <p className="flex items-center gap-1.5 text-[0.6875rem] text-muted-foreground">
              <Archive aria-hidden className="size-3.5 shrink-0" />
              <span>
                Earlier artifact <code className="font-mono"><bdi>{conflict.earlier.blobDigest.slice(0, 14)}…</bdi></code> is never deleted and stays preserved as immutable historical evidence.
              </span>
            </p>

            {draft.notice !== null && (
              <div
                role={draft.notice.kind === "pending" ? "status" : "alert"}
                data-supersession-notice={draft.notice.kind}
                className={cn(
                  "flex flex-wrap items-center gap-2 rounded border px-2.5 py-1.5 text-xs",
                  draft.notice.kind === "pending"
                    ? "border-tone-amber/40 bg-tone-amber/8 text-tone-amber"
                    : "border-destructive/30 bg-destructive/5 text-destructive",
                )}
              >
                <span className="min-w-0 flex-1">{draft.notice.message}</span>
                {draft.notice.kind === "error" && (
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    disabled={draft.pending}
                    onClick={() => void onCommit()}
                    className="h-6 shrink-0 border-border bg-background px-2 text-xs text-foreground shadow-none"
                  >
                    Retry
                  </Button>
                )}
              </div>
            )}
          </>
        )}
      </div>
    </article>
  )
}
