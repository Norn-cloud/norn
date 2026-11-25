import { useQuery } from "@tanstack/react-query"
import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  CheckCheck,
  Eye,
  EyeOff,
  FileSearch,
  ListChecks,
  ListFilter,
  LockKeyhole,
  MessageSquare,
  RotateCcw,
  ZoomIn,
  ZoomOut,
} from "lucide-react"
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from "react"
import { Chip } from "@/components/system/chip"
import { CopyId } from "@/components/copy-id"
import { FactStatusBadge } from "@/components/fact-status-badge"
import { EmptyState, Panel, Well } from "@/components/system/panel"
import { SourceTag } from "@/components/system/source-tag"
import { Dot, StatusDot } from "@/components/system/status-dot"
import { Button } from "@/components/ui/button"
import { readScan } from "@/evidence/review"
import {
  batchLockedRowIds,
  effectiveRowStatus,
  fetchScopedArtifactBlob,
  fieldEditKey,
  formatConfidence,
  formatTemporal,
  hasOpenLawViolation,
  hasResolvedEntity,
  isFieldWithheld,
  isLowConfidence,
  isOutcomeLocked,
  openFindingsOf,
  partitionPromotionBatch,
  resolveArtifactMediaSource,
  resolveRowEntity,
  rowBasisKey,
  snapshotPromotionQueue,
  type AliasResolution,
  type BatchRunReport,
  type CandidateRow,
  type ExtractedField,
  type QueuedPromotionItem,
  type RowActionOutcome,
  type StagedArtifact,
} from "@/lib/import-triage"
import { cn } from "@/lib/utils"
import { useView } from "@/lib/view"

type RowFilter = "all" | "clean" | "low_confidence" | "violations" | "verified"

const ZOOM_MIN = 50
const ZOOM_MAX = 300
const ZOOM_STEP = 25
const PAN_STEP = 40

const pct = (fraction: string) => `${Number(fraction) * 100}%`

/** An icon control in the artifact toolbar: the outline pill, 44px below 640px. */
const ICON_BUTTON = "max-sm:size-[44px]"

/** A segment of the row filter (DESIGN.md 10, SegmentedControl): the active one is the card at 500. */
const SEGMENT = "shrink-0 rounded-full px-2.5 py-1 text-meta whitespace-nowrap text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring aria-pressed:bg-card aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:ring-1 aria-pressed:ring-(--panel-border) max-sm:min-h-[44px]"

/**
 * Load source artifact media strictly through cell-authorized paths:
 * - safe inline synthetic image data URIs load directly
 * - normalized `/v1/(blobs|uploads)/...` paths fetch through Studio's scoped host transport into
 *   a revocable Blob URL (never raw unauthenticated `<img src="/v1/...">`)
 * - object-store URIs (`s3://`, `r2://`, `blob://`, ...) exchange via `/v1/uploads:read`
 * - arbitrary external `http(s)` URLs and traversal paths are rejected before any request runs.
 */
function useArtifactImage(uri: string | undefined) {
  const source = useMemo(() => (uri === undefined ? null : resolveArtifactMediaSource(uri)), [uri])
  const [blobUrl, setBlobUrl] = useState<string | null>(null)
  const query = useQuery({
    queryKey: ["triage-artifact-scan", uri],
    queryFn: async (): Promise<{ readonly kind: "url"; readonly url: string } | { readonly kind: "blob"; readonly blob: Blob }> => {
      if (source === null || source.kind === "rejected") {
        throw new Error(source?.reason ?? "Artifact media unavailable.")
      }
      if (source.kind === "data-uri") {
        return { kind: "url", url: source.url }
      }
      if (source.kind === "cell-media") {
        const blob = await fetchScopedArtifactBlob(source.path)
        return { kind: "blob", blob }
      }
      const read = await readScan(source.uri)
      return { kind: "url", url: read.document.getUrl }
    },
    enabled: source !== null && source.kind !== "rejected",
    retry: false,
    staleTime: 4 * 60_000,
  })

  useEffect(() => {
    if (query.data?.kind !== "blob") {
      setBlobUrl(null)
      return
    }
    const created = URL.createObjectURL(query.data.blob)
    setBlobUrl(created)
    return () => { URL.revokeObjectURL(created) }
  }, [query.data])

  const imageUrl = query.data?.kind === "url" ? query.data.url : blobUrl
  return { source, query, imageUrl }
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
 * Left pane: zoomable/pannable source artifact with status-coloured bounding boxes + surrounding
 * source context messages.
 * Right pane: editable extracted candidate rows (a held dot = low-confidence extraction, a bad dot =
 * law/contract violation, a live dot = verified fact, withheld fields read-only) + one-click batch
 * governed promotion of clean rows while keeping conflicts/breaches quarantined. Each pane is a
 * ShellCard with one well (DESIGN.md 10); states read dot-first (DESIGN.md 13).
 *
 * All in-flight / queued locks and batch queue execution live in `ImportTriagePage` keyed by
 * `batchId`, so switching between steps never drops promotion locks or unlocks queued rows.
 */
export function SplitPaneReview({
  batchId,
  artifacts,
  rows,
  lowConfidenceThresholdBps,
  resolvedAliases,
  edits,
  rowOutcomes,
  busyRowIds,
  queuedRowIds,
  batchRunning,
  batchReport,
  onSetFieldValue,
  onResetFieldValue,
  onPromoteRow,
  onPromoteCleanBatch,
  onJumpToAliases,
}: {
  readonly batchId: string
  readonly artifacts: readonly StagedArtifact[]
  readonly rows: readonly CandidateRow[]
  readonly lowConfidenceThresholdBps: number
  readonly resolvedAliases: ReadonlyMap<string, AliasResolution>
  readonly edits: Readonly<Record<string, string>>
  readonly rowOutcomes: Readonly<Record<string, RowActionOutcome>>
  readonly busyRowIds: ReadonlySet<string>
  readonly queuedRowIds: ReadonlySet<string>
  readonly batchRunning: boolean
  readonly batchReport: BatchRunReport | null
  readonly onSetFieldValue: (batchId: string, rowId: string, basisKey: string, fieldName: string, baselineValue: string, currentValue: string, next: string) => void
  readonly onResetFieldValue: (batchId: string, rowId: string, basisKey: string, fieldName: string, baselineValue: string) => void
  readonly onPromoteRow: (batchId: string, row: CandidateRow, artifact: StagedArtifact, expectedTargetRecordId?: string) => Promise<void>
  readonly onPromoteCleanBatch: (batchId: string, renderedQueue: readonly QueuedPromotionItem[], keptQuarantined: number) => Promise<void>
  readonly onJumpToAliases: () => void
}) {
  const { view } = useView()
  const [selectedArtifactId, setSelectedArtifactId] = useState<string>(() => artifacts[0]?.id ?? "")
  const activeArtifact = artifacts.find((item) => item.id === selectedArtifactId) ?? artifacts[0]

  const [zoom, setZoom] = useState(100)
  const [pan, setPan] = useState<{ readonly x: number; readonly y: number }>({ x: 0, y: 0 })
  const [showBoxes, setShowBoxes] = useState(true)
  const [imgError, setImgError] = useState(false)
  const dragOrigin = useRef<{ x: number; y: number; panX: number; panY: number } | null>(null)

  const [focusedRowId, setFocusedRowId] = useState<string | null>(() => rows[0]?.id ?? null)
  const [focusedFieldId, setFocusedFieldId] = useState<string | null>(null)
  const [pendingScrollTarget, setPendingScrollTarget] = useState<{ readonly rowId: string; readonly fieldId: string } | null>(null)
  const [filter, setFilter] = useState<RowFilter>("all")

  const { source: mediaSource, query: scan, imageUrl } = useArtifactImage(activeArtifact?.documentUri)

  useEffect(() => {
    setImgError(false)
  }, [activeArtifact?.id, imageUrl])

  const artifactRows = useMemo(
    () => activeArtifact ? rows.filter((row) => row.artifactId === activeArtifact.id) : rows,
    [rows, activeArtifact],
  )

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

  const lockedRowIds = useMemo(
    () => batchLockedRowIds(busyRowIds, queuedRowIds, rowOutcomes),
    [busyRowIds, queuedRowIds, rowOutcomes],
  )

  const partition = useMemo(
    () => partitionPromotionBatch(artifactRows, resolvedAliases, lockedRowIds),
    [artifactRows, resolvedAliases, lockedRowIds],
  )

  const filteredRows = useMemo(() => {
    return artifactRows.filter((row) => {
      const status = effectiveRowStatus(row)
      if (filter === "all") return true
      if (filter === "verified") return status === "verified_fact"
      if (filter === "violations") return status === "quarantined"
      if (filter === "low_confidence") return row.fields.some((f) => isLowConfidence(f.confidenceBps, lowConfidenceThresholdBps))
      // "clean"
      return status === "imported_unverified" && hasResolvedEntity(row, resolvedAliases)
    })
  }, [artifactRows, filter, lowConfidenceThresholdBps, resolvedAliases])

  // Scroll the row/field activated from a bounding box into view after any filter reset renders.
  useEffect(() => {
    if (pendingScrollTarget === null) return
    const { rowId, fieldId } = pendingScrollTarget
    const rowEl = document.querySelector<HTMLElement>(`[data-candidate-row="${CSS.escape(rowId)}"]`)
    rowEl?.scrollIntoView({ behavior: "smooth", block: "nearest" })
    const inputEl = document.getElementById(`${rowId}-${fieldId}`)
    if (inputEl instanceof HTMLInputElement && !inputEl.disabled && !inputEl.readOnly) {
      inputEl.focus({ preventScroll: true })
    }
    setPendingScrollTarget(null)
  }, [pendingScrollTarget, filteredRows])

  const activateBoundingBox = (overlay: OverlayBox) => {
    setFocusedRowId(overlay.rowId)
    setFocusedFieldId(overlay.fieldId)
    if (!filteredRows.some((candidate) => candidate.id === overlay.rowId)) {
      setFilter("all")
    }
    setPendingScrollTarget({ rowId: overlay.rowId, fieldId: overlay.fieldId })
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

  const nudgePan = (dx: number, dy: number) => {
    setPan((current) => ({ x: current.x + dx, y: current.y + dy }))
  }

  const resetZoom = () => { setZoom(100); setPan({ x: 0, y: 0 }) }

  const onViewportKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.target !== event.currentTarget) return
    if (event.key === "+" || event.key === "=") { event.preventDefault(); adjustZoom(ZOOM_STEP) }
    else if (event.key === "-") { event.preventDefault(); adjustZoom(-ZOOM_STEP) }
    else if (event.key === "0") { event.preventDefault(); resetZoom() }
    else if (zoom > 100 && event.key === "ArrowUp") { event.preventDefault(); nudgePan(0, PAN_STEP) }
    else if (zoom > 100 && event.key === "ArrowDown") { event.preventDefault(); nudgePan(0, -PAN_STEP) }
    else if (zoom > 100 && event.key === "ArrowLeft") { event.preventDefault(); nudgePan(PAN_STEP, 0) }
    else if (zoom > 100 && event.key === "ArrowRight") { event.preventDefault(); nudgePan(-PAN_STEP, 0) }
  }

  const lowConfidenceCount = artifactRows.filter((row) =>
    row.fields.some((field) => isLowConfidence(field.confidenceBps, lowConfidenceThresholdBps))).length

  const batchDisabled = partition.eligible.length === 0 || batchRunning || busyRowIds.size > 0 || queuedRowIds.size > 0

  return (
    <section
      aria-label="Split-pane artifact triage"
      className="grid items-start gap-4 lg:grid-cols-[minmax(0,46fr)_minmax(0,54fr)]"
    >
      {/* LEFT PANE: Zoomable source artifact + surrounding source context */}
      <div className="flex min-w-0 flex-col gap-4" data-pane="artifact">
        <Panel
          icon={FileSearch}
          label="Source artifact"
          action={activeArtifact ? <SourceTag mono={false}><bdi>{activeArtifact.sourceChannel}</bdi></SourceTag> : undefined}
        >
          <Well className="divide-y divide-(--well-border)">
            {/* Artifact switcher & zoom/pan controls */}
            <div className="flex flex-wrap items-center justify-between gap-2 px-3 py-2">
              <div className="flex min-w-0 items-center gap-1.5">
                {artifacts.length > 1 ? (
                  <select
                    aria-label="Source artifact"
                    value={activeArtifact?.id ?? ""}
                    onChange={(event) => { setSelectedArtifactId(event.target.value); resetZoom() }}
                    className="h-8 max-w-52 rounded-lg border border-input bg-card px-2 text-meta text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring max-sm:min-h-[44px]"
                  >
                    {artifacts.map((artifact) => (
                      <option key={artifact.id} value={artifact.id}>{artifact.label}</option>
                    ))}
                  </select>
                ) : (
                  <span className="truncate text-body text-foreground"><bdi>{activeArtifact?.label ?? "Source artifact"}</bdi></span>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-1" role="toolbar" aria-label="Artifact zoom and pan controls">
                <Button
                  type="button"
                  variant="outline"
                  size="icon-sm"
                  aria-label="Zoom out"
                  disabled={zoom <= ZOOM_MIN}
                  onClick={() => adjustZoom(-ZOOM_STEP)}
                  className={ICON_BUTTON}
                >
                  <ZoomOut className="size-3.5" />
                </Button>
                <span data-zoom-level="" className="w-11 text-center font-mono text-meta tabular-nums text-muted-foreground">
                  {zoom}%
                </span>
                <Button
                  type="button"
                  variant="outline"
                  size="icon-sm"
                  aria-label="Zoom in"
                  disabled={zoom >= ZOOM_MAX}
                  onClick={() => adjustZoom(ZOOM_STEP)}
                  className={ICON_BUTTON}
                >
                  <ZoomIn className="size-3.5" />
                </Button>
                {zoom > 100 && (
                  <span className="inline-flex items-center gap-0.5" role="group" aria-label="Pan artifact">
                    <Button type="button" variant="outline" size="icon-sm" aria-label="Pan left" onClick={() => nudgePan(PAN_STEP, 0)} className={ICON_BUTTON}><ArrowLeft className="size-3.5" /></Button>
                    <Button type="button" variant="outline" size="icon-sm" aria-label="Pan up" onClick={() => nudgePan(0, PAN_STEP)} className={ICON_BUTTON}><ArrowUp className="size-3.5" /></Button>
                    <Button type="button" variant="outline" size="icon-sm" aria-label="Pan down" onClick={() => nudgePan(0, -PAN_STEP)} className={ICON_BUTTON}><ArrowDown className="size-3.5" /></Button>
                    <Button type="button" variant="outline" size="icon-sm" aria-label="Pan right" onClick={() => nudgePan(-PAN_STEP, 0)} className={ICON_BUTTON}><ArrowRight className="size-3.5" /></Button>
                  </span>
                )}
                <Button type="button" variant="outline" size="sm" aria-label="Reset zoom" onClick={resetZoom}>
                  <RotateCcw className="size-3.5" />
                  Fit
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  aria-pressed={showBoxes}
                  aria-label={showBoxes ? "Hide extraction boxes" : "Show extraction boxes"}
                  onClick={() => setShowBoxes((current) => !current)}
                >
                  {showBoxes ? <Eye className="size-3.5" /> : <EyeOff className="size-3.5" />}
                  Boxes
                </Button>
              </div>
            </div>

            {/* Immutable artifact provenance strip */}
            {activeArtifact && (
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-meta text-muted-foreground">
                <span className="inline-flex items-center gap-1">
                  <span>Blob</span>
                  <CopyId value={activeArtifact.blobDigest} short={12} />
                </span>
                <span className="inline-flex items-center gap-1">
                  <span>Receipt</span>
                  <CopyId value={activeArtifact.functionReceiptDigest} short={12} />
                </span>
                <span className="tabular-nums">Observed {formatTemporal(activeArtifact.observedAt)}</span>
                {view === "technical" && (
                  <span className="font-mono">
                    {activeArtifact.extractor} {activeArtifact.extractorVersion}
                    {activeArtifact.promptDigest ? ` · prompt ${activeArtifact.promptDigest.slice(0, 10)}…` : ""}
                  </span>
                )}
                {activeArtifact.markings.map((marking) => (
                  <SourceTag key={marking}>{marking}</SourceTag>
                ))}
              </div>
            )}

            {/* Zoomable image viewport (keyboard focusable for +/-/0/Arrow pan) */}
            <div
              tabIndex={0}
              role="region"
              aria-label="Source artifact viewer"
              onKeyDown={onViewportKeyDown}
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerCancel={onPointerUp}
              className={cn(
                "relative min-h-72 overflow-hidden p-3 outline-none select-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
                zoom > 100 && "cursor-grab active:cursor-grabbing",
              )}
            >
              {mediaSource?.kind === "rejected" && (
                <div role="alert" data-media-rejected="" className="flex h-64 flex-col items-center justify-center gap-1.5 px-4 text-center text-meta text-muted-foreground">
                  <LockKeyhole aria-hidden className="size-5" strokeWidth={1.6} />
                  <span className="text-body text-foreground">Artifact media blocked</span>
                  <span>{mediaSource.reason}</span>
                </div>
              )}
              {mediaSource?.kind !== "rejected" && scan.isPending && (
                <div className="flex h-64 items-center justify-center text-meta text-muted-foreground">
                  Loading source artifact…
                </div>
              )}
              {mediaSource?.kind !== "rejected" && (scan.isError || imgError) && (
                <div role="alert" data-media-error="" className="flex h-64 flex-col items-center justify-center gap-1.5 px-4 text-center text-meta text-muted-foreground">
                  <FileSearch aria-hidden className="size-5" strokeWidth={1.6} />
                  <span className="text-body text-foreground">Source artifact image could not be loaded</span>
                  <span>Bounding-box overlays are hidden until the artifact image loads cleanly.</span>
                </div>
              )}
              {mediaSource?.kind !== "rejected" && !scan.isError && !imgError && imageUrl !== null && activeArtifact && (
                <div
                  data-pan-offset={`${pan.x},${pan.y}`}
                  className="relative mx-auto origin-top-left transition-transform duration-150"
                  style={{
                    transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom / 100})`,
                  }}
                >
                  <img
                    src={imageUrl}
                    alt={`Captured source artifact ${activeArtifact.label}`}
                    draggable={false}
                    onError={() => setImgError(true)}
                    onLoad={() => setImgError(false)}
                    className="block w-full rounded-sm bg-card ring-1 ring-line"
                  />
                  {showBoxes && !imgError && overlays.map((overlay) => {
                    const active = focusedFieldId === overlay.fieldId || (focusedFieldId === null && focusedRowId === overlay.rowId)
                    // Data marks on the image: the tone colours the box edge and a faint wash over the scan.
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
                        onClick={() => activateBoundingBox(overlay)}
                        className={cn(
                          "absolute rounded-xs border-[1.5px] transition-shadow outline-none focus-visible:ring-2 focus-visible:ring-ring",
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
          </Well>
        </Panel>

        {/* Surrounding message / source context */}
        <Panel
          icon={MessageSquare}
          label="Surrounding source context"
          action={<span className="tabular-nums">{activeArtifact?.contextMessages.length ?? 0} entries</span>}
        >
          <Well>
            <ol className="max-h-56 divide-y divide-(--well-border) overflow-y-auto" aria-label="Surrounding source messages">
              {(activeArtifact?.contextMessages ?? []).map((message) => {
                const boundAlias = message.aliasId ? resolvedAliases.get(message.aliasId) : undefined
                const resolvedName = boundAlias && (boundAlias.kind === "bound" || boundAlias.kind === "created")
                  ? boundAlias.targetLabel
                  : null
                return (
                  <li
                    key={message.id}
                    data-anchor={message.isAnchor ? "true" : undefined}
                    className={cn("space-y-1 px-3.5 py-2 text-meta", message.isAnchor && "bg-card")}
                  >
                    <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                      <span className="font-mono text-foreground"><bdi>{message.senderHandle}</bdi></span>
                      {resolvedName !== null && (
                        <Chip tone="green"><bdi>{resolvedName}</bdi></Chip>
                      )}
                      {message.aliasId && boundAlias?.kind === "ignored" && (
                        <Chip tone="gray" dot>Ignored alias</Chip>
                      )}
                      {message.aliasId && boundAlias === undefined && (
                        <Chip tone="amber">Unresolved alias</Chip>
                      )}
                      {message.isAnchor && <SourceTag mono={false}>Artifact attachment</SourceTag>}
                      <span className="ms-auto font-mono text-meta text-muted-foreground tabular-nums">
                        {formatTemporal(message.observedAt)}
                      </span>
                    </div>
                    <p dir="auto" className="text-body text-foreground"><bdi>{message.body}</bdi></p>
                  </li>
                )
              })}
            </ol>
          </Well>
        </Panel>
      </div>

      {/* RIGHT PANE: Editable extracted candidate rows + batch governed promotion */}
      <div className="flex min-w-0 flex-col" data-pane="rows">
        <Panel
          icon={ListChecks}
          label="Extracted candidate rows"
          action={`Low confidence below ${formatConfidence(lowConfidenceThresholdBps)}`}
        >
          <Well className="divide-y divide-(--well-border)">
            {/* Filter bar */}
            <div className="px-3 py-2">
              <div role="group" aria-label="Candidate row filter" className="inline-flex max-w-full overflow-x-auto rounded-full bg-(--well) p-0.5 ring-1 ring-line ring-inset">
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
                    className={SEGMENT}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>

            {/* Candidate rows */}
            <div className="min-h-80 divide-y divide-(--well-border) overflow-y-auto">
              {filteredRows.length === 0 ? (
                <EmptyState icon={ListFilter} title="No matching rows">
                  No candidate rows match this filter.
                </EmptyState>
              ) : (
                filteredRows.map((row) => {
                  const selected = focusedRowId === row.id
                  const entity = resolveRowEntity(row, resolvedAliases)
                  const rowStatus = effectiveRowStatus(row)
                  const openFindings = openFindingsOf(row)
                  const violating = rowStatus === "quarantined"
                  const withheldRow = rowStatus === "withheld"
                  const verified = rowStatus === "verified_fact"
                  const busy = busyRowIds.has(row.id)
                  const queued = queuedRowIds.has(row.id)
                  const locked = busy || queued
                  const outcome = rowOutcomes[row.id] ?? null
                  const outcomeLocked = isOutcomeLocked(outcome)
                  const rowArtifact = artifacts.find((item) => item.id === row.artifactId)
                  const basisKey = rowBasisKey(row, rowArtifact)
                  const canPromote = rowStatus === "imported_unverified"
                    && entity.kind === "resolved"
                    && !locked
                    && !batchRunning
                    && !outcomeLocked
                    && rowArtifact !== undefined

                  return (
                    <article
                      key={row.id}
                      data-candidate-row={row.id}
                      data-row-status={rowStatus}
                      onClick={() => setFocusedRowId(row.id)}
                      className={cn(
                        "space-y-3 px-4 py-3 transition-colors",
                        selected && "bg-card",
                        violating && "border-s-2 border-s-tone-red",
                      )}
                    >
                      {/* Row header */}
                      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                        <span className="text-body text-foreground"><bdi>{entity.label}</bdi></span>
                        {entity.kind === "resolved" && (
                          <code className="font-mono text-meta text-muted-foreground"><bdi>{entity.targetId}</bdi></code>
                        )}
                        {entity.kind === "unresolved" && (
                          <button
                            type="button"
                            onClick={(event) => { event.stopPropagation(); onJumpToAliases() }}
                            className="inline-flex items-center gap-1.5 rounded-full px-1.5 py-0.5 text-meta text-muted-foreground underline-offset-2 outline-none hover:text-foreground hover:underline focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring max-sm:min-h-[44px]"
                          >
                            <Dot tone="held" />
                            Unresolved alias — match in Step 1
                          </button>
                        )}
                        {entity.kind === "ignored" && (
                          <Chip tone="gray" dot>Alias ignored — excluded</Chip>
                        )}
                        <FactStatusBadge status={rowStatus} />
                        {queued && !busy && <Chip tone="gray" dot>Queued for promotion…</Chip>}
                        <span className="ms-auto font-mono text-meta text-muted-foreground tabular-nums">
                          effective <bdi>{formatTemporal(row.effectiveAt)}</bdi>
                        </span>
                      </div>

                      {/* Extracted fields grid */}
                      <div className="grid gap-x-3 gap-y-2 sm:grid-cols-2">
                        {row.fields.map((field) => {
                          const lowConf = isLowConfidence(field.confidenceBps, lowConfidenceThresholdBps)
                          const fieldFinding = openFindings.find((f) => f.fieldName === field.name || field.findingIds.includes(f.id))
                          const hasRed = fieldFinding !== undefined
                          const isFieldFocused = focusedRowId === row.id && focusedFieldId === field.id

                          if (isFieldWithheld(field)) {
                            return (
                              <div
                                key={field.id}
                                data-field-name={field.name}
                                data-field-withheld={field.masking}
                                className={cn("space-y-1 rounded-(--radius-row) p-1.5", isFieldFocused && "ring-2 ring-ring/50")}
                              >
                                <div className="flex items-center justify-between gap-1.5 text-meta">
                                  <span className="text-muted-foreground">{field.label}</span>
                                  <span className="inline-flex items-center gap-1 font-mono text-muted-foreground">
                                    <LockKeyhole aria-hidden className="size-3" />
                                    Withheld ({field.masking})
                                  </span>
                                </div>
                                <div className="flex h-8 items-center gap-1.5 rounded-lg border border-input bg-muted/40 px-2 font-mono text-meta text-muted-foreground">
                                  <bdi>{field.displayMask ?? "Redacted from your role"}</bdi>
                                </div>
                                {field.markings && field.markings.length > 0 && (
                                  <div className="flex flex-wrap gap-1 pt-0.5">
                                    {field.markings.map((marking) => <SourceTag key={marking}>{marking}</SourceTag>)}
                                  </div>
                                )}
                              </div>
                            )
                          }

                          // Verified rows ALWAYS show the authoritative cell value, never stale local drafts.
                          // Local edits are scoped strictly to `(row.id, basisKey, field.name)` so edits from an
                          // earlier capture never leak across supersession or baseline refreshes.
                          const localEdit = edits[fieldEditKey(row.id, basisKey, field.name)]
                          const current = verified ? field.value : (localEdit ?? field.value)
                          const hasLocalEdit = !verified && localEdit !== undefined && localEdit !== field.value
                          const differsFromExtracted = !verified && current !== field.extractedValue
                          const hasAmber = !hasRed && lowConf

                          return (
                            <div
                              key={field.id}
                              data-field-name={field.name}
                              data-field-tone={hasRed ? "red" : hasAmber ? "amber" : verified ? "green" : "neutral"}
                              className={cn("space-y-1 rounded-(--radius-row) p-1.5", isFieldFocused && "ring-2 ring-ring/50")}
                            >
                              <div className="flex flex-wrap items-center justify-between gap-1.5 text-meta">
                                <label htmlFor={`${row.id}-${field.id}`} className="text-muted-foreground">
                                  {field.label}
                                </label>
                                {hasRed ? (
                                  <StatusDot tone="bad" className="font-mono">{fieldFinding.code}</StatusDot>
                                ) : hasAmber ? (
                                  <StatusDot
                                    tone="held"
                                    data-low-confidence=""
                                    className="tabular-nums"
                                    title="Low extraction confidence — review against the artifact before promotion"
                                  >
                                    Low confidence {formatConfidence(field.confidenceBps)}
                                  </StatusDot>
                                ) : field.confidenceBps !== null ? (
                                  <span className="font-mono text-muted-foreground tabular-nums">
                                    {formatConfidence(field.confidenceBps)}
                                  </span>
                                ) : null}
                              </div>

                              <div className="flex items-center gap-1">
                                <input
                                  id={`${row.id}-${field.id}`}
                                  type="text"
                                  dir="auto"
                                  value={current}
                                  readOnly={verified}
                                  disabled={locked || outcomeLocked}
                                  onFocus={() => { setFocusedRowId(row.id); setFocusedFieldId(field.id) }}
                                  onChange={(event) => onSetFieldValue(batchId, row.id, basisKey, field.name, field.value, current, event.target.value)}
                                  aria-invalid={hasRed || undefined}
                                  className={cn(
                                    "h-8 min-w-0 flex-1 rounded-lg border bg-card px-2 font-mono text-meta text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring max-sm:min-h-[44px]",
                                    hasRed ? "border-tone-red/60" : hasAmber ? "border-tone-amber/60" : "border-input",
                                    (verified || locked || outcomeLocked) && "cursor-default bg-muted/40 text-foreground/85",
                                  )}
                                />
                                {hasLocalEdit && (
                                  <button
                                    type="button"
                                    disabled={locked || outcomeLocked}
                                    aria-label={`Reset ${field.label} edit`}
                                    title={`Row baseline is "${field.value}" — click to undo local edit`}
                                    onClick={(event) => { event.stopPropagation(); onResetFieldValue(batchId, row.id, basisKey, field.name, field.value) }}
                                    className="inline-flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none hover:bg-(--well-hover) hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-ring disabled:opacity-40 max-sm:size-[44px]"
                                  >
                                    <RotateCcw className="size-3.5" />
                                  </button>
                                )}
                              </div>
                              {differsFromExtracted && (
                                <div className="text-meta text-muted-foreground">
                                  Extracted: <span className="font-mono"><bdi>{field.extractedValue}</bdi></span> (preserved in lineage)
                                </div>
                              )}
                            </div>
                          )
                        })}
                      </div>

                      {/* Cell-reported open law/contract violations */}
                      {openFindings.length > 0 && (
                        <div role="alert" data-cell-findings="" className="space-y-1 text-meta">
                          {openFindings.map((finding) => (
                            <div key={finding.id} className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
                              <StatusDot tone="bad" className="font-mono">{finding.code}</StatusDot>
                              <span className="font-mono text-meta text-muted-foreground">{finding.ruleId}</span>
                              <span dir="auto" className="text-foreground"><bdi>{finding.message}</bdi></span>
                            </div>
                          ))}
                          <p className="text-meta text-muted-foreground">
                            Laws are never bypassed on import — this row stays quarantined with immutable evidence lineage.
                          </p>
                        </div>
                      )}

                      {/* Distinct request outcome line (never fabricated into a cell quarantine finding or optimistic Verified fact) */}
                      {outcome !== null && !verified && (
                        <div
                          role={outcome.kind === "committed" || outcome.kind === "pending" || outcome.kind === "stale" ? "status" : "alert"}
                          data-row-outcome={outcome.kind}
                          className="flex flex-wrap items-center gap-2 text-meta text-foreground"
                        >
                          <Dot tone={outcome.kind === "committed" ? "live" : outcome.kind === "pending" || outcome.kind === "stale" ? "held" : "bad"} />
                          {outcome.kind === "committed" && (
                            <>
                              <span className="tabular-nums">
                                Committed (commit {outcome.receipt.receipt.commitSeq}) — awaiting authoritative read from cell.
                              </span>
                              <CopyId value={outcome.receipt.receipt.receiptHash} short={10} />
                            </>
                          )}
                          {outcome.kind === "pending" && (
                            <span>
                              Pending approval decision <code className="font-mono">{outcome.decisionId}</code> — nothing is committed until approved.
                            </span>
                          )}
                          {outcome.kind === "stale" && <span>{outcome.message}</span>}
                          {outcome.kind === "refused" && (
                            <span>
                              <span className="font-mono">{outcome.code}</span>: {outcome.message}
                            </span>
                          )}
                          {outcome.kind === "error" && (
                            <>
                              <span className="min-w-0 flex-1">{outcome.message}</span>
                              <Button
                                type="button"
                                size="sm"
                                variant="outline"
                                disabled={!canPromote}
                                onClick={(event) => { event.stopPropagation(); if (rowArtifact && entity.kind === "resolved") void onPromoteRow(batchId, row, rowArtifact, entity.targetId) }}
                              >
                                Retry
                              </Button>
                            </>
                          )}
                        </div>
                      )}

                      {/* Row footer: receipt or single-row governed promote */}
                      <div className="flex flex-wrap items-center justify-between gap-2 text-meta text-muted-foreground">
                        <span className="tabular-nums">
                          observed {formatTemporal(row.observedAt)}
                          {row.recordedAt ? ` · recorded ${formatTemporal(row.recordedAt)}` : ""}
                        </span>
                        {verified && row.receipt ? (
                          <span className="inline-flex items-center gap-1.5 tabular-nums">
                            <StatusDot tone="live">commit {row.receipt.receipt.commitSeq}</StatusDot>
                            <CopyId value={row.receipt.receipt.receiptHash} short={10} />
                          </span>
                        ) : violating ? (
                          <StatusDot tone="bad">Held in quarantine</StatusDot>
                        ) : withheldRow ? (
                          <span>Withheld fields cannot be promoted by this role</span>
                        ) : (
                          <Button
                            type="button"
                            size="sm"
                            variant="outline"
                            disabled={!canPromote}
                            onClick={(event) => { event.stopPropagation(); if (rowArtifact && entity.kind === "resolved") void onPromoteRow(batchId, row, rowArtifact, entity.targetId) }}
                          >
                            {busy
                              ? "Promoting…"
                              : queued
                                ? "Queued…"
                                : outcome?.kind === "committed"
                                  ? "Committed — syncing…"
                                  : outcome?.kind === "pending"
                                    ? "Pending approval…"
                                    : "Promote row"}
                          </Button>
                        )}
                      </div>
                    </article>
                  )
                })
              )}
            </div>

            {/* Batch governed promotion bar */}
            <div className="space-y-3 px-4 py-3">
              {batchReport !== null && (
                <div role="status" className="flex flex-wrap items-center gap-x-3 gap-y-1 text-meta text-muted-foreground">
                  <span className="text-foreground">Batch run complete:</span>
                  <StatusDot tone="live">{batchReport.promoted} promoted</StatusDot>
                  {batchReport.pendingApproval > 0 && <StatusDot tone="held">{batchReport.pendingApproval} pending approval</StatusDot>}
                  {batchReport.skippedStale > 0 && <StatusDot tone="held">{batchReport.skippedStale} skipped (basis changed)</StatusDot>}
                  {batchReport.refused > 0 && <StatusDot tone="bad">{batchReport.refused} refused by action</StatusDot>}
                  {batchReport.failed > 0 && <StatusDot tone="bad">{batchReport.failed} failed to send</StatusDot>}
                  <StatusDot tone="quiet">{batchReport.keptQuarantined} held in quarantine</StatusDot>
                </div>
              )}
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div className="min-w-0 flex-1 basis-64 space-y-1 text-meta">
                  <div className="text-body text-foreground tabular-nums">
                    {partition.eligible.length} clean {partition.eligible.length === 1 ? "row" : "rows"} ready for governed promotion
                  </div>
                  {(partition.quarantined.length > 0 || partition.withheld.length > 0 || partition.blockedByAlias.length > 0) && (
                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 tabular-nums">
                      {partition.quarantined.length > 0 && (
                        <StatusDot tone="bad">
                          {partition.quarantined.length} {partition.quarantined.length === 1 ? "row stays" : "rows stay"} quarantined
                        </StatusDot>
                      )}
                      {partition.withheld.length > 0 && (
                        <StatusDot tone="quiet">{partition.withheld.length} withheld</StatusDot>
                      )}
                      {partition.blockedByAlias.length > 0 && (
                        <StatusDot tone="held">{partition.blockedByAlias.length} awaiting alias match</StatusDot>
                      )}
                    </div>
                  )}
                  <p className="text-meta text-muted-foreground">
                    Extraction confidence is advisory — batch promotion runs every row through cell laws and preserves conflicts in quarantine.
                  </p>
                </div>
                <Button
                  type="button"
                  disabled={batchDisabled}
                  onClick={() => {
                    const queue = snapshotPromotionQueue(partition.eligible, artifacts, resolvedAliases, edits, lockedRowIds)
                    void onPromoteCleanBatch(batchId, queue, partition.quarantined.length)
                  }}
                >
                  <CheckCheck aria-hidden className="size-4" />
                  {batchRunning
                    ? "Promoting clean rows…"
                    : `Promote ${partition.eligible.length} clean ${partition.eligible.length === 1 ? "row" : "rows"}`}
                </Button>
              </div>
            </div>
          </Well>
        </Panel>
      </div>
    </section>
  )
}
