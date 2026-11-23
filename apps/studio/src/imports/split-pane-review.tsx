import { useQuery } from "@tanstack/react-query"
import {
  AlertTriangle,
  CheckCheck,
  CheckCircle2,
  Eye,
  EyeOff,
  FileSearch,
  MessageSquare,
  RotateCcw,
  ShieldAlert,
  ZoomIn,
  ZoomOut,
} from "lucide-react"
import { useMemo, useRef, useState, type PointerEvent } from "react"
import { Chip } from "@/components/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { Button } from "@/components/ui/button"
import { readScan } from "@/evidence/review"
import { newIdempotencyKey, type ActionReceipt } from "@/lib/actions"
import { readableTime } from "@/lib/format"
import {
  commitRowPromotion,
  formatConfidence,
  formatTemporal,
  hasOpenLawViolation,
  hasResolvedEntity,
  isLowConfidence,
  openFindingsOf,
  partitionPromotionBatch,
  type AliasResolution,
  type CandidateRow,
  type ExtractedField,
  type StagedArtifact,
  type TriageActions,
} from "@/lib/import-triage"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"

type RowFilter = "all" | "clean" | "low_confidence" | "violations" | "verified"

const ZOOM_MIN = 50
const ZOOM_MAX = 300
const ZOOM_STEP = 25

const pct = (fraction: string) => `${Number(fraction) * 100}%`

/**
 * Resolve an artifact image URL: direct web/data URIs load immediately, while object-store URIs
 * go through the cell's policy-checked `/v1/uploads:read` endpoint.
 */
function useArtifactImage(uri: string | undefined) {
  const direct = uri !== undefined && /^(https?:\/\/|data:)/.test(uri)
  return useQuery({
    queryKey: ["triage-artifact-scan", uri],
    queryFn: () => direct
      ? Promise.resolve({ document: { getUrl: uri!, expiresAt: "" } })
      : readScan(uri!),
    enabled: uri !== undefined,
    retry: false,
    staleTime: 4 * 60_000,
  })
}

interface OverlayBox {
  readonly rowId: string
  readonly fieldId: string
  readonly label: string
  readonly box: NonNullable<ExtractedField["box"]>
  readonly status: "violation" | "low_confidence" | "clean"
}

/**
 * Step 2 — Split-Pane Artifact Triage.
 * Left pane: zoomable source artifact image with status-coloured bounding boxes + surrounding
 * source context messages.
 * Right pane: editable extracted candidate rows (amber = low-confidence OCR/extraction,
 * red = law/contract violation, green = verified fact) + one-click batch governed promotion
 * of clean rows while keeping conflicts/breaches quarantined.
 */
export function SplitPaneReview({
  batchId,
  artifacts,
  rows,
  lowConfidenceThresholdBps,
  actions,
  resolvedAliases,
  onJumpToAliases,
  onRowPromoted,
  onRowRefused,
}: {
  readonly batchId: string
  readonly artifacts: readonly StagedArtifact[]
  readonly rows: readonly CandidateRow[]
  readonly lowConfidenceThresholdBps: number
  readonly actions: TriageActions
  readonly resolvedAliases: ReadonlyMap<string, AliasResolution>
  readonly onJumpToAliases: () => void
  readonly onRowPromoted: (rowId: string, values: Readonly<Record<string, string>>, receipt: ActionReceipt) => void
  readonly onRowRefused: (rowId: string, code: string, message: string) => void
}) {
  const { view } = useView()
  const [selectedArtifactId, setSelectedArtifactId] = useState<string>(() => artifacts[0]?.id ?? "")
  const activeArtifact = artifacts.find((item) => item.id === selectedArtifactId) ?? artifacts[0]

  const [zoom, setZoom] = useState(100)
  const [pan, setPan] = useState<{ readonly x: number; readonly y: number }>({ x: 0, y: 0 })
  const [showBoxes, setShowBoxes] = useState(true)
  const dragOrigin = useRef<{ x: number; y: number; panX: number; panY: number } | null>(null)

  const [focusedRowId, setFocusedRowId] = useState<string | null>(() => rows[0]?.id ?? null)
  const [focusedFieldId, setFocusedFieldId] = useState<string | null>(null)
  const [filter, setFilter] = useState<RowFilter>("all")

  // Local editable field values keyed by `${rowId}:${fieldName}` (never mutating raw extraction).
  const [edits, setEdits] = useState<Readonly<Record<string, string>>>({})
  const [rowBusy, setRowBusy] = useState<ReadonlySet<string>>(new Set())
  const [batchRunning, setBatchRunning] = useState(false)
  const [batchReport, setBatchReport] = useState<{ readonly promoted: number; readonly refused: number; readonly keptQuarantined: number } | null>(null)

  const scan = useArtifactImage(activeArtifact?.documentUri)

  const artifactRows = useMemo(
    () => activeArtifact ? rows.filter((row) => row.artifactId === activeArtifact.id) : rows,
    [rows, activeArtifact],
  )

  const fieldValue = (row: CandidateRow, field: ExtractedField): string =>
    edits[`${row.id}:${field.name}`] ?? field.value

  const setFieldValue = (rowId: string, fieldName: string, next: string) =>
    setEdits((current) => ({ ...current, [`${rowId}:${fieldName}`]: next }))

  const resetFieldValue = (rowId: string, fieldName: string) =>
    setEdits((current) => {
      const next = { ...current }
      delete next[`${rowId}:${fieldName}`]
      return next
    })

  const entityLabelFor = (row: CandidateRow): { readonly label: string; readonly resolved: boolean; readonly targetId: string | null } => {
    if (row.entityRecordId !== null) return { label: row.entityLabel, resolved: true, targetId: row.entityRecordId }
    if (row.entityAliasId !== null) {
      const aliasRes = resolvedAliases.get(row.entityAliasId)
      if (aliasRes?.kind === "bound" || aliasRes?.kind === "created") {
        return { label: aliasRes.targetLabel, resolved: true, targetId: aliasRes.targetId }
      }
    }
    return { label: row.entityLabel, resolved: false, targetId: null }
  }

  const overlays = useMemo<readonly OverlayBox[]>(() => {
    const list: OverlayBox[] = []
    for (const row of artifactRows) {
      const rowHasViolation = hasOpenLawViolation(row)
      for (const field of row.fields) {
        if (field.box === null) continue
        const fieldHasViolation = row.findings.some((f) => f.status === "open" && (f.fieldName === field.name || field.findingIds.includes(f.id)))
        const status: OverlayBox["status"] = fieldHasViolation || (rowHasViolation && field.findingIds.length > 0)
          ? "violation"
          : isLowConfidence(field.confidenceBps, lowConfidenceThresholdBps)
            ? "low_confidence"
            : "clean"
        list.push({ rowId: row.id, fieldId: field.id, label: `${row.entityLabel} · ${field.label}`, box: field.box, status })
      }
    }
    return list
  }, [artifactRows, lowConfidenceThresholdBps])

  const partition = useMemo(() => partitionPromotionBatch(artifactRows, resolvedAliases), [artifactRows, resolvedAliases])

  const filteredRows = useMemo(() => {
    return artifactRows.filter((row) => {
      if (filter === "all") return true
      if (filter === "verified") return row.status === "verified_fact"
      if (filter === "violations") return row.status === "quarantined" || hasOpenLawViolation(row)
      if (filter === "low_confidence") return row.fields.some((f) => isLowConfidence(f.confidenceBps, lowConfidenceThresholdBps))
      // "clean"
      return row.status !== "verified_fact" && row.status !== "quarantined" && !hasOpenLawViolation(row) && hasResolvedEntity(row, resolvedAliases)
    })
  }, [artifactRows, filter, lowConfidenceThresholdBps, resolvedAliases])

  const promoteOne = async (row: CandidateRow): Promise<"promoted" | "refused" | "skipped"> => {
    const artifact = artifacts.find((item) => item.id === row.artifactId) ?? activeArtifact
    const resolved = entityLabelFor(row)
    if (!artifact || !resolved.resolved || resolved.targetId === null || hasOpenLawViolation(row) || row.status === "quarantined") {
      return "skipped"
    }
    const values = Object.fromEntries(row.fields.map((field) => [field.name, fieldValue(row, field)]))
    setRowBusy((current) => new Set(current).add(row.id))
    try {
      const result = await commitRowPromotion(actions.promoteRow, {
        batchId,
        rowId: row.id,
        artifactId: artifact.id,
        blobDigest: artifact.blobDigest,
        functionReceiptDigest: artifact.functionReceiptDigest,
        targetRecordId: resolved.targetId,
        effectiveAt: row.effectiveAt,
        observedAt: row.observedAt,
        values,
      }, newIdempotencyKey())
      if (result.kind === "receipt") {
        onRowPromoted(row.id, values, result.receipt)
        return "promoted"
      }
      if (result.kind === "refused") {
        onRowRefused(row.id, result.code, result.message)
        return "refused"
      }
      onRowRefused(row.id, "triage.unavailable", "The cell could not evaluate this promotion right now.")
      return "refused"
    } catch (error) {
      onRowRefused(row.id, "triage.network_error", error instanceof Error ? error.message : "The cell did not respond.")
      return "refused"
    } finally {
      setRowBusy((current) => {
        const next = new Set(current)
        next.delete(row.id)
        return next
      })
    }
  }

  const promoteCleanBatch = async () => {
    if (partition.eligible.length === 0 || batchRunning) return
    setBatchRunning(true)
    setBatchReport(null)
    let promoted = 0
    let refused = 0
    for (const row of partition.eligible) {
      const outcome = await promoteOne(row)
      if (outcome === "promoted") promoted += 1
      else if (outcome === "refused") refused += 1
    }
    setBatchReport({ promoted, refused, keptQuarantined: partition.quarantined.length + refused })
    setBatchRunning(false)
  }

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (zoom <= 100) return
    if ((event.target as HTMLElement).closest("button")) return
    dragOrigin.current = { x: event.clientX, y: event.clientY, panX: pan.x, panY: pan.y }
    event.currentTarget.setPointerCapture(event.pointerId)
  }

  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (dragOrigin.current === null) return
    setPan({
      x: dragOrigin.current.panX + (event.clientX - dragOrigin.current.x),
      y: dragOrigin.current.panY + (event.clientY - dragOrigin.current.y),
    })
  }

  const onPointerUp = () => { dragOrigin.current = null }

  const adjustZoom = (delta: number) => {
    setZoom((current) => {
      const next = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, current + delta))
      if (next <= 100) setPan({ x: 0, y: 0 })
      return next
    })
  }

  const resetZoom = () => { setZoom(100); setPan({ x: 0, y: 0 }) }

  const lowConfidenceCount = artifactRows.filter((row) =>
    row.fields.some((field) => isLowConfidence(field.confidenceBps, lowConfidenceThresholdBps))).length

  return (
    <section
      aria-label="Split-pane artifact triage"
      className="grid overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-[minmax(0,46%)_minmax(0,54%)] lg:divide-x lg:divide-border"
    >
      {/* LEFT PANE: Zoomable source artifact + surrounding source context */}
      <div className="flex min-w-0 flex-col divide-y divide-border" data-pane="artifact">
        {/* Artifact switcher & zoom controls */}
        <div className="flex flex-wrap items-center justify-between gap-2 bg-surface-subtle px-3 py-2">
          <div className="flex min-w-0 items-center gap-1.5">
            {artifacts.length > 1 ? (
              <select
                aria-label="Source artifact"
                value={activeArtifact?.id ?? ""}
                onChange={(event) => { setSelectedArtifactId(event.target.value); resetZoom() }}
                className="h-7 max-w-52 rounded border border-border bg-background px-2 text-xs font-medium text-foreground"
              >
                {artifacts.map((artifact) => (
                  <option key={artifact.id} value={artifact.id}>{artifact.label}</option>
                ))}
              </select>
            ) : (
              <span className="truncate text-xs font-medium text-foreground">{activeArtifact?.label ?? "Source artifact"}</span>
            )}
            {activeArtifact && <Chip tone="gray">{activeArtifact.sourceChannel}</Chip>}
          </div>
          <div className="flex items-center gap-1" role="toolbar" aria-label="Artifact zoom controls">
            <Button
              type="button"
              variant="outline"
              size="icon-sm"
              aria-label="Zoom out"
              disabled={zoom <= ZOOM_MIN}
              onClick={() => adjustZoom(-ZOOM_STEP)}
              className="size-7 rounded border-border bg-background shadow-none"
            >
              <ZoomOut className="size-3.5" />
            </Button>
            <span data-zoom-level="" className="w-11 text-center font-mono text-xs tabular-nums text-muted-foreground">
              {zoom}%
            </span>
            <Button
              type="button"
              variant="outline"
              size="icon-sm"
              aria-label="Zoom in"
              disabled={zoom >= ZOOM_MAX}
              onClick={() => adjustZoom(ZOOM_STEP)}
              className="size-7 rounded border-border bg-background shadow-none"
            >
              <ZoomIn className="size-3.5" />
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              aria-label="Reset zoom"
              onClick={resetZoom}
              className="h-7 rounded border-border bg-background px-2 text-xs shadow-none"
            >
              <RotateCcw className="size-3" />
              Fit
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              aria-pressed={showBoxes}
              aria-label={showBoxes ? "Hide extraction boxes" : "Show extraction boxes"}
              onClick={() => setShowBoxes((current) => !current)}
              className="h-7 rounded border-border bg-background px-2 text-xs shadow-none"
            >
              {showBoxes ? <Eye className="size-3" /> : <EyeOff className="size-3" />}
              Boxes
            </Button>
          </div>
        </div>

        {/* Immutable artifact provenance strip */}
        {activeArtifact && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 bg-background px-3 py-2 text-[0.6875rem] text-muted-foreground">
            <span className="inline-flex items-center gap-1">
              <span>Blob</span>
              <CopyId value={activeArtifact.blobDigest} short={12} />
            </span>
            <span className="inline-flex items-center gap-1">
              <span>Receipt</span>
              <CopyId value={activeArtifact.functionReceiptDigest} short={12} />
            </span>
            <span className="tabular-nums">Observed {readableTime(activeArtifact.observedAt)}</span>
            {view === "technical" && (
              <span className="font-mono">
                {activeArtifact.extractor} {activeArtifact.extractorVersion}
                {activeArtifact.promptDigest ? ` · prompt ${activeArtifact.promptDigest.slice(0, 10)}…` : ""}
              </span>
            )}
            {activeArtifact.markings.map((marking) => (
              <Chip key={marking} tone="gray" mono>{marking}</Chip>
            ))}
          </div>
        )}

        {/* Zoomable image viewport */}
        <div
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          className={cn(
            "relative min-h-72 overflow-hidden bg-muted/40 p-3 select-none",
            zoom > 100 && "cursor-grab active:cursor-grabbing",
          )}
        >
          {scan.isPending && (
            <div className="flex h-64 items-center justify-center text-xs text-muted-foreground">
              Loading source artifact…
            </div>
          )}
          {scan.isError && (
            <div className="flex h-64 flex-col items-center justify-center gap-1.5 text-xs text-muted-foreground">
              <FileSearch aria-hidden className="size-5" />
              <span>Source artifact image could not be read from the cell.</span>
            </div>
          )}
          {scan.data && activeArtifact && (
            <div
              className="relative mx-auto origin-top-left transition-transform duration-150"
              style={{
                transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom / 100})`,
              }}
            >
              <img
                src={scan.data.document.getUrl}
                alt={`Captured source artifact ${activeArtifact.label}`}
                draggable={false}
                className="block w-full rounded border border-border bg-white"
              />
              {showBoxes && overlays.map((overlay) => {
                const active = focusedFieldId === overlay.fieldId || (focusedFieldId === null && focusedRowId === overlay.rowId)
                const boxStyle = overlay.status === "violation"
                  ? "border-tone-red bg-tone-red/12"
                  : overlay.status === "low_confidence"
                    ? "border-tone-amber bg-tone-amber/15"
                    : "border-tone-green/70 bg-tone-green/8"
                return (
                  <button
                    key={`${overlay.rowId}:${overlay.fieldId}`}
                    type="button"
                    data-bbox-field={overlay.fieldId}
                    data-bbox-status={overlay.status}
                    aria-label={`Inspect ${overlay.label}`}
                    onClick={() => { setFocusedRowId(overlay.rowId); setFocusedFieldId(overlay.fieldId) }}
                    className={cn(
                      "absolute rounded-[2px] border-[1.5px] transition-shadow outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      boxStyle,
                      active && "ring-2 ring-foreground/80 ring-offset-1 ring-offset-background",
                    )}
                    style={{
                      left: pct(overlay.box.x),
                      top: pct(overlay.box.y),
                      width: pct(overlay.box.w),
                      height: pct(overlay.box.h),
                    }}
                  />
                )
              })}
            </div>
          )}
        </div>

        {/* Surrounding message / source context */}
        <div className="flex flex-col bg-background">
          <div className="flex items-center justify-between border-b border-border bg-surface-subtle px-3 py-2">
            <span className="inline-flex items-center gap-1.5 text-xs font-medium text-foreground">
              <MessageSquare aria-hidden className="size-3.5 text-muted-foreground" />
              Surrounding source context
            </span>
            <span className="text-[0.6875rem] text-muted-foreground tabular-nums">
              {activeArtifact?.contextMessages.length ?? 0} entries
            </span>
          </div>
          <ol className="max-h-56 divide-y divide-border overflow-y-auto" aria-label="Surrounding source messages">
            {(activeArtifact?.contextMessages ?? []).map((message) => {
              const boundAlias = message.aliasId ? resolvedAliases.get(message.aliasId) : undefined
              const resolvedName = boundAlias && (boundAlias.kind === "bound" || boundAlias.kind === "created")
                ? boundAlias.targetLabel
                : null
              return (
                <li
                  key={message.id}
                  data-anchor={message.isAnchor ? "true" : undefined}
                  className={cn("space-y-1 px-3 py-2 text-xs", message.isAnchor && "bg-surface-subtle")}
                >
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="font-mono font-medium text-foreground">{message.senderHandle}</span>
                    {resolvedName !== null && (
                      <Chip tone="green">{resolvedName}</Chip>
                    )}
                    {message.aliasId && resolvedName === null && (
                      <Chip tone="amber">Unresolved alias</Chip>
                    )}
                    {message.isAnchor && <Chip tone="gray">Artifact attachment</Chip>}
                    <span className="ms-auto font-mono text-[0.6875rem] text-muted-foreground tabular-nums">
                      {readableTime(message.observedAt)}
                    </span>
                  </div>
                  <p className="text-foreground/90">{message.body}</p>
                </li>
              )
            })}
          </ol>
        </div>
      </div>

      {/* RIGHT PANE: Editable extracted candidate rows + batch governed promotion */}
      <div className="flex min-w-0 flex-col divide-y divide-border" data-pane="rows">
        {/* Filter bar */}
        <div className="flex flex-wrap items-center gap-2 bg-surface-subtle px-3 py-2">
          <div role="group" aria-label="Candidate row filter" className="inline-flex flex-wrap rounded-md border border-border bg-background p-0.5 text-xs">
            {([
              ["all", `All (${artifactRows.length})`],
              ["clean", `Clean (${partition.eligible.length})`],
              ["low_confidence", `Low confidence (${lowConfidenceCount})`],
              ["violations", `Violations (${partition.quarantined.length})`],
              ["verified", `Verified (${partition.verified.length})`],
            ] as const).map(([key, label]) => (
              <button
                key={key}
                type="button"
                aria-pressed={filter === key}
                onClick={() => setFilter(key)}
                className={cn(
                  "rounded px-2 py-1 font-medium transition-colors",
                  filter === key ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground",
                )}
              >
                {label}
              </button>
            ))}
          </div>
          <span className="ms-auto text-[0.6875rem] text-muted-foreground tabular-nums">
            Threshold &lt; {formatConfidence(lowConfidenceThresholdBps)} = amber
          </span>
        </div>

        {/* Candidate rows */}
        <div className="min-h-80 flex-1 divide-y divide-border overflow-y-auto">
          {filteredRows.length === 0 ? (
            <div className="px-4 py-12 text-center text-sm text-muted-foreground">
              No candidate rows match this filter.
            </div>
          ) : (
            filteredRows.map((row) => {
              const selected = focusedRowId === row.id
              const entity = entityLabelFor(row)
              const openFindings = openFindingsOf(row)
              const violating = row.status === "quarantined" || openFindings.length > 0
              const verified = row.status === "verified_fact"
              const busy = rowBusy.has(row.id)
              const canPromote = !verified && !violating && entity.resolved && !busy && !batchRunning

              return (
                <article
                  key={row.id}
                  data-candidate-row={row.id}
                  data-row-status={row.status}
                  onClick={() => setFocusedRowId(row.id)}
                  className={cn(
                    "space-y-2.5 px-4 py-3 transition-colors",
                    selected && "bg-surface-subtle/80",
                    violating && "border-s-2 border-s-tone-red",
                  )}
                >
                  {/* Row header */}
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-medium text-foreground">{entity.label}</span>
                    {entity.targetId && (
                      <code className="font-mono text-[0.6875rem] text-muted-foreground">{entity.targetId}</code>
                    )}
                    {!entity.resolved && (
                      <button
                        type="button"
                        onClick={(event) => { event.stopPropagation(); onJumpToAliases() }}
                        className="inline-flex items-center gap-1 rounded border border-tone-amber/40 bg-tone-amber/10 px-1.5 py-0.5 text-[0.6875rem] font-medium text-tone-amber hover:bg-tone-amber/20"
                      >
                        Unresolved alias — match in Step 1
                      </button>
                    )}
                    <FactStatusBadge status={row.status} />
                    <span className="ms-auto font-mono text-[0.6875rem] text-muted-foreground tabular-nums">
                      effective {formatTemporal(row.effectiveAt)}
                    </span>
                  </div>

                  {/* Editable fields grid */}
                  <div className="grid gap-2 sm:grid-cols-2">
                    {row.fields.map((field) => {
                      const current = fieldValue(row, field)
                      const edited = current !== field.extractedValue
                      const lowConf = isLowConfidence(field.confidenceBps, lowConfidenceThresholdBps)
                      const fieldFinding = openFindings.find((f) => f.fieldName === field.name || field.findingIds.includes(f.id))
                      const hasRed = fieldFinding !== undefined
                      const hasAmber = !hasRed && lowConf
                      const isFieldFocused = focusedRowId === row.id && focusedFieldId === field.id

                      return (
                        <div
                          key={field.id}
                          data-field-name={field.name}
                          data-field-tone={hasRed ? "red" : hasAmber ? "amber" : verified ? "green" : "neutral"}
                          className={cn(
                            "space-y-1 rounded-md border p-2 transition-colors",
                            hasRed
                              ? "border-tone-red/60 bg-tone-red/5"
                              : hasAmber
                                ? "border-tone-amber/60 bg-tone-amber/8"
                                : verified
                                  ? "border-tone-green/40 bg-tone-green/5"
                                  : "border-border bg-background",
                            isFieldFocused && "ring-2 ring-ring/50",
                          )}
                        >
                          <div className="flex items-center justify-between gap-1.5 text-[0.6875rem]">
                            <label htmlFor={`${row.id}-${field.id}`} className="font-medium text-muted-foreground">
                              {field.label}
                            </label>
                            <div className="flex items-center gap-1">
                              {hasRed && (
                                <span className="inline-flex items-center gap-0.5 font-mono font-semibold text-tone-red">
                                  <ShieldAlert aria-hidden className="size-3" />
                                  {fieldFinding.code}
                                </span>
                              )}
                              {hasAmber && (
                                <span
                                  data-low-confidence=""
                                  className="inline-flex items-center gap-0.5 font-mono font-medium text-tone-amber tabular-nums"
                                  title="Low extraction confidence — review against the artifact before promotion"
                                >
                                  <AlertTriangle aria-hidden className="size-3" />
                                  {formatConfidence(field.confidenceBps)}
                                </span>
                              )}
                              {!hasRed && !hasAmber && field.confidenceBps !== null && (
                                <span className="font-mono text-muted-foreground tabular-nums">
                                  {formatConfidence(field.confidenceBps)}
                                </span>
                              )}
                            </div>
                          </div>

                          <div className="flex items-center gap-1">
                            <input
                              id={`${row.id}-${field.id}`}
                              type="text"
                              value={current}
                              readOnly={verified}
                              disabled={busy}
                              onFocus={() => { setFocusedRowId(row.id); setFocusedFieldId(field.id) }}
                              onChange={(event) => setFieldValue(row.id, field.name, event.target.value)}
                              aria-invalid={hasRed || undefined}
                              className={cn(
                                "h-7 min-w-0 flex-1 rounded border bg-background px-2 font-mono text-xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring",
                                hasRed ? "border-tone-red/50" : hasAmber ? "border-tone-amber/50" : "border-border",
                                verified && "cursor-default bg-muted/40 text-foreground/85",
                              )}
                            />
                            {edited && !verified && (
                              <button
                                type="button"
                                aria-label={`Reset ${field.label} to extracted value`}
                                title={`Extracted as "${field.extractedValue}" — click to reset`}
                                onClick={(event) => { event.stopPropagation(); resetFieldValue(row.id, field.name) }}
                                className="rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                              >
                                <RotateCcw className="size-3" />
                              </button>
                            )}
                          </div>
                          {edited && (
                            <div className="text-[0.6875rem] text-muted-foreground">
                              Extracted: <span className="font-mono">{field.extractedValue}</span> (preserved in lineage)
                            </div>
                          )}
                        </div>
                      )
                    })}
                  </div>

                  {/* Open law/contract violations */}
                  {openFindings.length > 0 && (
                    <div role="alert" className="space-y-1 rounded-md border border-tone-red/40 bg-tone-red/5 px-2.5 py-2 text-xs">
                      {openFindings.map((finding) => (
                        <div key={finding.id} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                          <span className="inline-flex items-center gap-1 font-mono font-semibold text-tone-red">
                            <ShieldAlert aria-hidden className="size-3.5 shrink-0 self-center" />
                            {finding.code}
                          </span>
                          <span className="font-mono text-[0.6875rem] text-muted-foreground">{finding.ruleId}</span>
                          <span className="text-foreground">{finding.message}</span>
                        </div>
                      ))}
                      <p className="text-[0.6875rem] text-muted-foreground">
                        Laws are never bypassed on import — this row stays quarantined with immutable evidence lineage.
                      </p>
                    </div>
                  )}

                  {/* Row footer: receipt or single-row governed promote */}
                  <div className="flex flex-wrap items-center justify-between gap-2 pt-0.5 text-[0.6875rem] text-muted-foreground">
                    <span className="tabular-nums">
                      observed {formatTemporal(row.observedAt)}
                      {row.recordedAt ? ` · recorded ${formatTemporal(row.recordedAt)}` : ""}
                    </span>
                    {verified && row.receipt ? (
                      <span className="inline-flex items-center gap-1.5 text-tone-green tabular-nums">
                        <CheckCircle2 aria-hidden className="size-3.5" />
                        <span>commit {row.receipt.receipt.commitSeq}</span>
                        <CopyId value={row.receipt.receipt.receiptHash} short={10} />
                      </span>
                    ) : violating ? (
                      <span className="font-medium text-tone-red">Held in quarantine</span>
                    ) : (
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        disabled={!canPromote}
                        onClick={(event) => { event.stopPropagation(); void promoteOne(row) }}
                        className="h-7 rounded border-border bg-background px-2.5 text-xs shadow-none"
                      >
                        {busy ? "Promoting…" : "Promote row"}
                      </Button>
                    )}
                  </div>
                </article>
              )
            })
          )}
        </div>

        {/* Sticky batch governed promotion bar */}
        <div className="space-y-2 bg-surface-subtle px-4 py-3">
          {batchReport !== null && (
            <div role="status" className="flex flex-wrap items-center gap-2 rounded border border-border bg-background px-2.5 py-1.5 text-xs">
              <CheckCircle2 aria-hidden className="size-3.5 text-tone-green" />
              <span>
                Batch run complete: <strong className="font-medium text-tone-green">{batchReport.promoted} promoted</strong>
                {batchReport.refused > 0 && <>, <strong className="font-medium text-tone-red">{batchReport.refused} refused by law</strong></>}
                , <span className="text-muted-foreground">{batchReport.keptQuarantined} held in quarantine.</span>
              </span>
            </div>
          )}
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="space-y-0.5 text-xs">
              <div className="font-medium text-foreground tabular-nums">
                {partition.eligible.length} clean {partition.eligible.length === 1 ? "row" : "rows"} ready for governed promotion
                {partition.quarantined.length > 0 && (
                  <span className="font-normal text-tone-red">
                    {" "}· {partition.quarantined.length} {partition.quarantined.length === 1 ? "row stays" : "rows stay"} quarantined
                  </span>
                )}
                {partition.blockedByAlias.length > 0 && (
                  <span className="font-normal text-tone-amber">
                    {" "}· {partition.blockedByAlias.length} awaiting alias match
                  </span>
                )}
              </div>
              <p className="text-[0.6875rem] text-muted-foreground">
                Extraction confidence is advisory — batch promotion runs every row through cell laws and preserves conflicts in quarantine.
              </p>
            </div>
            <Button
              type="button"
              size="sm"
              disabled={partition.eligible.length === 0 || batchRunning}
              onClick={() => void promoteCleanBatch()}
              className="h-8 shrink-0 gap-1.5 px-3 text-xs"
            >
              <CheckCheck aria-hidden className="size-3.5" />
              {batchRunning
                ? "Promoting clean rows…"
                : `Promote ${partition.eligible.length} clean ${partition.eligible.length === 1 ? "row" : "rows"}`}
            </Button>
          </div>
        </div>
      </div>
    </section>
  )
}
