import { useQueryClient } from "@tanstack/react-query"
import { AlertCircle, Clock, CloudOff, Inbox, RefreshCw, type LucideIcon } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { BoneSkeleton, type StudioBoneName } from "@/components/bone-skeleton"
import { CopyId } from "@/components/copy-id"
import { Disclosure } from "@/components/disclosure"
import { EmptyState } from "@/components/system/panel"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { CellUnavailable, lastReachableAt, useCachedStudioScope } from "@/lib/definitions"
import { formatRelativeOrTimestamp } from "@/lib/format"
import { NotServed, retryWorkspace, useMode } from "@/lib/live-transport"
import { useLocale } from "@/lib/locale"
import { explainError } from "@/lib/plain-words"
import { useView } from "@/lib/view"
import { useRegisterWorkspaceUnavailable, type Workspace } from "@/lib/workspace"

/** Wait before showing a quiet "Taking longer than usual…" hint beneath a skeleton. */
export const SLOW_WAIT_MS = 4_000
/** Wait before replacing an unresolved skeleton with an honest timed-out state and Retry. */
export const BOUNDED_WAIT_MS = 12_000

export type SkeletonKind = "list" | "cards" | "detail" | "table" | "panel" | "inline"

/** Whether an error represents an unreachable, broken, or timed-out workspace cell. */
export function isWorkspaceUnavailableError(error: unknown): boolean {
  return error instanceof CellUnavailable
    && (error.status === 0 || error.status >= 500 || error.timedOut === true || error.retryable === false)
}

/** Human-readable relative/absolute timestamp for when the workspace last responded cleanly. */
export function formatLastReachable(atMs: number | null, now = Date.now()): string | null {
  return atMs === null ? null : formatRelativeOrTimestamp(atMs, now)
}

/**
 * Bounded wait state for any pending surface: transitions from normal skeleton → slow notice →
 * timed-out state with Retry so no loading surface ever spins indefinitely.
 */
export function useBoundedWait(pending: boolean, slowAfterMs = SLOW_WAIT_MS, timeoutMs = BOUNDED_WAIT_MS, attempt = 0): "normal" | "slow" | "timed-out" {
  const [stage, setStage] = useState<"normal" | "slow" | "timed-out">("normal")
  useEffect(() => {
    if (!pending) { setStage("normal"); return }
    setStage("normal")
    const slowTimer = setTimeout(() => setStage((current) => current === "normal" ? "slow" : current), slowAfterMs)
    const timeoutTimer = setTimeout(() => setStage("timed-out"), Math.max(timeoutMs, slowAfterMs))
    return () => { clearTimeout(slowTimer); clearTimeout(timeoutTimer) }
  }, [pending, slowAfterMs, timeoutMs, attempt])
  return pending ? stage : "normal"
}

/** Shared skeleton layouts for Studio's data-loading surfaces. */
export function SkeletonBlock({ kind = "list", label = "Loading" }: { readonly kind?: SkeletonKind; readonly label?: string }) {
  if (kind === "inline") {
    return <div className="flex items-center gap-2 py-2" role="status" aria-busy="true" aria-label={label}><Skeleton className="h-4 w-40" /></div>
  }
  if (kind === "cards") {
    return (
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3" role="status" aria-busy="true" aria-label={label}>
        {[0, 1, 2].map((i) => <Skeleton key={i} className="h-28 rounded-xl" />)}
      </div>
    )
  }
  if (kind === "detail") {
    return (
      <div className="space-y-4" role="status" aria-busy="true" aria-label={label}>
        <Skeleton className="h-8 w-64" />
        <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_22rem]">
          <div className="space-y-3">
            <Skeleton className="h-56 rounded-xl" />
            <Skeleton className="h-32 rounded-xl" />
          </div>
          <Skeleton className="h-64 rounded-xl" />
        </div>
      </div>
    )
  }
  if (kind === "table") {
    return (
      <div className="space-y-2 rounded-lg border bg-card p-3" role="status" aria-busy="true" aria-label={label}>
        <Skeleton className="h-8 w-full" />
        {[0, 1, 2, 3, 4].map((i) => <Skeleton key={i} className="h-7 w-full" />)}
      </div>
    )
  }
  if (kind === "panel") {
    return <div className="rounded-lg border bg-card p-3" role="status" aria-busy="true" aria-label={label}><Skeleton className="h-40 w-full rounded-md" /></div>
  }
  return (
    <div className="grid gap-2" role="status" aria-busy="true" aria-label={label}>
      {[0, 1, 2].map((i) => <Skeleton key={i} className="h-16 rounded-lg" />)}
    </div>
  )
}

/**
 * Honest state for a broken, unopenable, or unreachable workspace cell: names what the server
 * said, shows the incident id when logged, says when the workspace was last reachable, separates
 * non-retryable failures from transient ones, and offers an explicit Retry.
 *
 * It owns that Retry, and takes none from its page: while a page shows this state the shell's one-line
 * status steps aside, so this Retry is the only one and must do what the shell's would
 * (`retryWorkspace`: decide the mode again, then read every errored read again). It stays disabled
 * until both have settled. A page that passed its own read here instead left the mode unavailable.
 */
export function WorkspaceUnavailable({ error, compact = false }: {
  readonly error: unknown
  readonly compact?: boolean
}) {
  useRegisterWorkspaceUnavailable()
  const queries = useQueryClient()
  const [retrying, setRetrying] = useState(false)
  const retry = () => {
    setRetrying(true)
    void retryWorkspace(queries).finally(() => setRetrying(false))
  }
  const scope = useCachedStudioScope()
  const { locale } = useLocale()
  const { view } = useView()
  const info = explainError(error, locale, view)
  const reachable = lastReachableAt(scope)
  const reachableLabel = formatLastReachable(reachable)
  const nonRetryable = info.retryable === false
  return (
    <div role="alert" data-workspace-unavailable="" data-retryable={nonRetryable ? "false" : "true"}
      className={`rounded-(--radius-panel) border border-(--panel-border) bg-card text-body shadow-(--panel-shadow) ${compact ? "p-3.5" : "p-5"}`}>
      <div className="flex flex-wrap items-start gap-3">
        <span className="flex size-9 shrink-0 items-center justify-center rounded-(--radius-well) bg-(--well) text-muted-foreground ring-1 ring-line">
          <CloudOff aria-hidden className="size-4" strokeWidth={1.6} />
        </span>
        {/* The words keep at least 15rem: on a phone Retry moves under them rather than squeezing them. */}
        <div className="min-w-0 flex-1 basis-60 space-y-1.5">
          <div className="text-foreground">Workspace unavailable</div>
          <p className="text-pretty text-muted-foreground">{info.headline}</p>
          {info.detail && <p className="text-meta text-foreground [overflow-wrap:anywhere]">{info.detail}</p>}
          {nonRetryable && (
            <p className="text-meta text-muted-foreground">
              Automatic retries are off for this error. Check with your workspace administrator or support team if retrying does not clear it.
            </p>
          )}
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 pt-0.5 text-meta text-muted-foreground">
            {reachable !== null && reachableLabel !== null && (
              <span>Last reachable <time dateTime={new Date(reachable).toISOString()} title={new Date(reachable).toISOString()}>{reachableLabel}</time></span>
            )}
            {info.incidentId && (
              <span className="inline-flex items-center gap-1.5">Incident <CopyId value={info.incidentId} /></span>
            )}
          </div>
          {(info.code || info.status || info.raw) && (
            <Disclosure open={view === "technical"} summary="Technical" className="pt-1 text-meta text-muted-foreground">
              <div className="mt-1 grid gap-0.5 ps-5 font-mono text-meta [overflow-wrap:anywhere]">
                {(info.status || info.code) && <span>{[info.status ? `HTTP ${info.status}` : null, info.code].filter(Boolean).join(" · ")}</span>}
                {info.raw && <span>{info.raw}</span>}
              </div>
            </Disclosure>
          )}
        </div>
        <Button type="button" variant="outline" size="sm" disabled={retrying} onClick={retry}>
          <RefreshCw className={retrying ? "animate-spin" : undefined} />Retry
        </Button>
      </div>
    </div>
  )
}

/**
 * Honest state for a server error or refusal on a data read: plain-word headline in Business mode,
 * the server's explanation (never hidden), incident id when present, technical details under
 * Technical, and an explicit Retry.
 */
export function ReadErrorState({ error, onRetry, isFetching = false, title, inset = false, children }: {
  readonly error: unknown
  readonly onRetry?: () => void
  readonly isFetching?: boolean
  /** What could not be read, in plain words; the server's own words follow it. */
  readonly title?: string
  /** Drawn inside a panel's well: the same state with no card of its own (one way to nest). */
  readonly inset?: boolean
  /** One more honest line, such as what still works. */
  readonly children?: ReactNode
}) {
  const { locale } = useLocale()
  const { view } = useView()
  const info = explainError(error, locale, view)
  return (
    <div role={inset ? "status" : "alert"} data-load-state="error"
      className={inset ? "px-2.5 py-2.5 text-body" : "rounded-(--radius-panel) border border-(--panel-border) bg-card p-4 text-body shadow-(--panel-shadow)"}>
      <div className="flex flex-wrap items-start gap-3">
        <AlertCircle aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" strokeWidth={1.6} />
        <div className="min-w-0 flex-1 space-y-1">
          {title && <p className="text-foreground">{title}</p>}
          <p className={title ? "text-meta text-muted-foreground [overflow-wrap:anywhere]" : "text-foreground"}>{info.headline}</p>
          {info.detail && <p className="text-meta text-muted-foreground [overflow-wrap:anywhere]">{info.detail}</p>}
          {info.incidentId && <p className="inline-flex items-center gap-1.5 text-meta text-muted-foreground">Incident <CopyId value={info.incidentId} /></p>}
          {children}
          {(info.code || info.raw) && (
            <Disclosure open={view === "technical"} summary="Technical" className="pt-0.5 text-meta text-muted-foreground">
              <div className="mt-1 grid gap-0.5 ps-5 font-mono text-meta [overflow-wrap:anywhere]">
                {info.code && <span>{info.code}</span>}
                {info.raw && <span>{info.raw}</span>}
              </div>
            </Disclosure>
          )}
        </div>
        {onRetry && (
          <Button type="button" variant="outline" size="sm" disabled={isFetching} onClick={onRetry}>
            <RefreshCw className={isFetching ? "animate-spin" : undefined} />Retry
          </Button>
        )}
      </div>
    </div>
  )
}

export interface LoadStateEmpty {
  readonly icon?: LucideIcon
  readonly title: string
  readonly description: ReactNode
  readonly next?: ReactNode
}

export interface LoadStateProps<T> {
  readonly status: "pending" | "error" | "success"
  readonly data?: T
  readonly error?: unknown
  readonly isFetching?: boolean
  readonly onRetry?: () => void
  /** Named boneyard-js UI-shape skeleton capture from `src/bones/registry.ts`. */
  readonly bone?: StudioBoneName
  /** Skeleton shape while loading when no `bone` is named, or a custom ReactNode. */
  readonly skeleton?: SkeletonKind | ReactNode
  /** Accessible label for the skeleton region. */
  readonly loadingLabel?: string
  /** Elapsed ms before showing a slow-response hint beneath the skeleton (default 4 s). */
  readonly slowAfterMs?: number
  /** Elapsed ms before transitioning out of the skeleton into an honest timed-out state (default 12 s). */
  readonly timeoutMs?: number
  /** Check whether loaded data is empty. */
  readonly isEmpty?: (data: T) => boolean
  /** Empty state with a clear next step. */
  readonly empty?: LoadStateEmpty
  /** Custom copy when the cell does not serve this route (`NotServed`). */
  readonly notServed?: { readonly icon?: LucideIcon; readonly title?: string; readonly description?: ReactNode }
  /** Render the loaded, non-empty data. */
  readonly children: (data: T) => ReactNode
}

/**
 * Shared data-loading boundary for Studio surfaces (#1424):
 * skeleton → bounded wait (slow hint, then timed-out state with Retry) →
 * empty with a next step, error with server reason and Retry, or Workspace unavailable.
 */
export function LoadState<T>({
  status,
  data,
  error,
  isFetching = false,
  onRetry,
  bone,
  skeleton = "list",
  loadingLabel = "Loading",
  slowAfterMs = SLOW_WAIT_MS,
  timeoutMs = BOUNDED_WAIT_MS,
  isEmpty,
  empty,
  notServed,
  children,
}: LoadStateProps<T>) {
  const scope = useCachedStudioScope()
  const [retryAttempt, setRetryAttempt] = useState(0)
  const wait = useBoundedWait(status === "pending", slowAfterMs, timeoutMs, retryAttempt)
  const handleRetry = onRetry ? () => { setRetryAttempt((n) => n + 1); onRetry() } : undefined

  if (status === "pending") {
    if (wait === "timed-out") {
      const reachable = lastReachableAt(scope)
      const reachableLabel = formatLastReachable(reachable)
      return (
        <div role="status" data-load-state="timed-out" className="rounded-xl border border-node-border bg-card p-5 text-body shadow-elevation">
          <div className="flex flex-wrap items-start gap-3">
            <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-border bg-muted text-muted-foreground">
              <Clock aria-hidden className="size-4" />
            </span>
            <div className="min-w-0 flex-1 space-y-1">
              <p className="text-foreground">Taking too long to respond</p>
              <p className="text-meta text-muted-foreground">
                The workspace hasn't finished answering this request yet. It may be busy or unreachable.
              </p>
              {reachable !== null && reachableLabel !== null && (
                <p className="text-meta text-muted-foreground">
                  Last reachable <time dateTime={new Date(reachable).toISOString()} title={new Date(reachable).toISOString()}>{reachableLabel}</time>
                </p>
              )}
            </div>
            {handleRetry && (
              <Button type="button" variant="outline" size="sm" onClick={handleRetry}>
                <RefreshCw />Retry
              </Button>
            )}
          </div>
        </div>
      )
    }
    const skeletonBlock = typeof skeleton === "string"
      ? <SkeletonBlock kind={skeleton as SkeletonKind} label={loadingLabel} />
      : skeleton
    return (
      <div className="space-y-2.5" data-load-state={wait === "slow" ? "slow" : "loading"}>
        {bone
          ? <BoneSkeleton name={bone} loading label={loadingLabel} />
          : skeletonBlock}
        {wait === "slow" && (
          <div role="status" className="flex flex-wrap items-center gap-2 px-1 text-meta text-muted-foreground">
            <Clock aria-hidden className="size-3.5 shrink-0" />
            <span>Taking longer than usual to reach the workspace…</span>
            {handleRetry && <Button type="button" variant="ghost" size="xs" onClick={handleRetry}><RefreshCw />Retry</Button>}
          </div>
        )}
      </div>
    )
  }

  if (status === "error") {
    if (error instanceof NotServed) {
      return (
        <div className="rounded-xl border" data-load-state="not-served">
          <EmptyState icon={notServed?.icon ?? AlertCircle} title={notServed?.title ?? "Not served yet"}>
            {notServed?.description ?? error.message}
          </EmptyState>
        </div>
      )
    }
    if (isWorkspaceUnavailableError(error)) {
      return <WorkspaceUnavailable error={error} />
    }
    return <ReadErrorState error={error} onRetry={onRetry} isFetching={isFetching} />
  }

  if (data === undefined) return null
  if (isEmpty?.(data) && empty) {
    return (
      <div className="rounded-xl border" data-load-state="empty">
        <EmptyState icon={empty.icon ?? Inbox} title={empty.title} next={empty.next}>
          {empty.description}
        </EmptyState>
      </div>
    )
  }
  return <>{children(data)}</>
}

/**
 * A section before its workspace is decided (`lib/workspace.tsx`): its skeleton while the mode is read,
 * and Workspace unavailable with the mode's own Retry once the read failed. It reads nothing itself.
 */
export function WorkspaceUndecided({ workspace, bone, skeleton, loadingLabel }: {
  readonly workspace: Extract<Workspace, { readonly kind: "deciding" | "unavailable" }>
  readonly bone?: StudioBoneName
  readonly skeleton?: SkeletonKind
  readonly loadingLabel?: string
}) {
  const queries = useQueryClient()
  const mode = useMode()
  return (
    <LoadState
      status={workspace.kind === "deciding" ? "pending" : "error"}
      error={workspace.kind === "unavailable" ? workspace.error : null}
      isFetching={mode.isFetching}
      // The skeleton's and a refusal's Retry; Workspace unavailable owns its own, the same one.
      onRetry={() => void retryWorkspace(queries)}
      {...bone === undefined ? {} : { bone }}
      {...skeleton === undefined ? {} : { skeleton }}
      {...loadingLabel === undefined ? {} : { loadingLabel }}
    >
      {() => null}
    </LoadState>
  )
}
