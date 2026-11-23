import { Archive, ArrowRight, CheckCircle2, Clock, GitCompareArrows, ShieldAlert } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { newIdempotencyKey } from "@/lib/actions"
import { readableTime } from "@/lib/format"
import {
  commitSupersessionDecision,
  formatConfidence,
  timeDeltaLabel,
  type SupersessionChoice,
  type SupersessionConflict,
  type SupersessionResolution,
  type TriageActions,
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
 * as immutable historical evidence.
 */
export function SupersessionDiffList({
  batchId,
  conflicts,
  actions,
  lowConfidenceThresholdBps,
  onResolved,
}: {
  readonly batchId: string
  readonly conflicts: readonly SupersessionConflict[]
  readonly actions: TriageActions
  readonly lowConfidenceThresholdBps: number
  readonly onResolved: (conflictId: string, resolution: SupersessionResolution) => void
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
          actionId={actions.resolveSupersession}
          lowConfidenceThresholdBps={lowConfidenceThresholdBps}
          onResolved={onResolved}
        />
      ))}
    </div>
  )
}

function SupersessionDiffCard({
  batchId,
  conflict,
  actionId,
  lowConfidenceThresholdBps,
  onResolved,
}: {
  readonly batchId: string
  readonly conflict: SupersessionConflict
  readonly actionId: string
  readonly lowConfidenceThresholdBps: number
  readonly onResolved: (conflictId: string, resolution: SupersessionResolution) => void
}) {
  const [changedOnly, setChangedOnly] = useState(true)
  const [choice, setChoice] = useState<SupersessionChoice>(conflict.defaultChoice)
  const [note, setNote] = useState("")
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const changedCells = conflict.cells.filter((cell) => cell.changed)
  const shownCells = changedOnly ? changedCells : conflict.cells
  const resolved = conflict.resolution
  const delta = timeDeltaLabel(conflict.earlier.observedAt, conflict.newer.observedAt)

  const submit = async () => {
    setPending(true)
    setError(null)
    try {
      const trimmed = note.trim()
      const result = await commitSupersessionDecision(actionId, {
        batchId,
        conflictId: conflict.id,
        choice,
        earlierArtifactId: conflict.earlier.artifactId,
        earlierBlobDigest: conflict.earlier.blobDigest,
        newerArtifactId: conflict.newer.artifactId,
        newerBlobDigest: conflict.newer.blobDigest,
        preserveEarlierEvidence: true,
        ...trimmed !== "" ? { note: trimmed } : {},
      }, newIdempotencyKey())
      if (result.kind === "receipt") {
        onResolved(conflict.id, {
          choice,
          ...trimmed !== "" ? { note: trimmed } : {},
          preservedHistoricalArtifactId: choice === "keep_earlier" ? conflict.newer.artifactId : conflict.earlier.artifactId,
          receipt: result.receipt,
        })
      } else if (result.kind === "refused") {
        setError(`${result.code}: ${result.message}`)
      } else if (result.kind === "pending") {
        setError(`Pending approval decision (${result.decision.id}).`)
      } else {
        setError("The cell could not commit this supersession resolution right now.")
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "The cell did not respond.")
    } finally {
      setPending(false)
    }
  }

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
            Window {conflict.windowLabel}
          </h2>
          <Chip tone="red" icon={ShieldAlert} mono>{conflict.findingCode}</Chip>
          <FactStatusBadge
            status={resolved === null || resolved.choice === "keep_disputed" ? "quarantined" : "verified_fact"}
          />
          <span className="font-mono text-xs text-muted-foreground">{conflict.scopeKey}</span>
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
          <div className="font-medium text-foreground">{conflict.earlier.label}</div>
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
          <div className="font-medium text-foreground">{conflict.newer.label}</div>
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
                    <td className="px-4 py-2 font-medium text-foreground">{cell.entityLabel}</td>
                    <td className="px-4 py-2 text-muted-foreground">{cell.fieldLabel}</td>
                    <td className="px-4 py-2 font-mono">
                      <span className={cn(cell.changed && "text-muted-foreground line-through")}>
                        {cell.earlierValue}
                      </span>
                    </td>
                    <td className="px-4 py-2 font-mono">
                      <span className="inline-flex items-center gap-1.5">
                        {cell.changed && <ArrowRight aria-hidden className="size-3 text-tone-amber" />}
                        <span className={cn(cell.changed ? "font-semibold text-foreground" : "text-muted-foreground")}>
                          {cell.newerValue}
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
              Historical evidence preserved: immutable artifact <code className="font-mono text-foreground">{resolved.preservedHistoricalArtifactId}</code> remains linked and readable.
              {resolved.note ? ` Rationale: “${resolved.note}”.` : ""}
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
                const selected = choice === option.value
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
                      disabled={pending}
                      onChange={() => setChoice(option.value)}
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
                aria-label={`Supersession rationale for ${conflict.windowLabel}`}
                placeholder="Rationale note (optional, stored with the governed receipt)"
                value={note}
                disabled={pending}
                onChange={(event) => setNote(event.target.value)}
                className="h-8 min-w-56 flex-1 bg-background text-xs shadow-none"
              />
              <Button
                type="button"
                size="sm"
                disabled={pending}
                onClick={() => void submit()}
                className="h-8 shrink-0 px-3 text-xs"
              >
                {pending ? "Committing…" : "Resolve supersession"}
              </Button>
            </div>

            <p className="flex items-center gap-1.5 text-[0.6875rem] text-muted-foreground">
              <Archive aria-hidden className="size-3.5 shrink-0" />
              <span>
                Earlier artifact <code className="font-mono">{conflict.earlier.blobDigest.slice(0, 14)}…</code> is never deleted and stays preserved as immutable historical evidence.
              </span>
            </p>

            {error !== null && (
              <p role="alert" className="rounded border border-destructive/30 bg-destructive/5 px-2.5 py-1.5 text-xs text-destructive">
                {error}
              </p>
            )}
          </>
        )}
      </div>
    </article>
  )
}
