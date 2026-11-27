import { Dialog, DialogBackdrop, DialogClose, DialogContent, DialogDescription, DialogTitle } from "@ui-sdk/react/dialog"
import { useQueryClient } from "@tanstack/react-query"
import { Check, Copy, Loader2, Palette, RotateCcw, X } from "lucide-react"
import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { useAppearance } from "@/lib/appearance"
import { NO_DATA_CELL, WORKSPACE_CHANGED_MESSAGE, sameStudioScope, type StudioScope } from "@/lib/definitions"
import {
  STUDIO_THEME_PRESETS,
  exportStudioThemeJson,
  normalizeStudioTheme,
} from "@/lib/studio-theme"
import {
  applyWorkspaceTheme,
  resetWorkspaceTheme,
  servedThemeKey,
  useServedTheme,
  type ServedTheme,
  type ThemeApplyResult,
} from "@/lib/workspace-theme"
import { WORKSPACE_THEME_ID } from "../../../../tools/norn-ts/src/builder/theme.ts"

/** One line under the editor: where it came from (Studio reading the JSON, or the cell), and what it said. */
interface Problem {
  readonly path: string | null
  readonly message: string
}

/**
 * Immutable workspace + revision snapshot captured when the editor opens. Both apply and reset send
 * this snapshot's `expectedVersion` so a concurrent theme change from another tab or agent triggers
 * a `409 defs.stale` conflict instead of being silently overwritten.
 */
export interface ThemeEditorSnapshot {
  readonly scope: StudioScope
  readonly expectedVersion: number
  readonly artifactId: string
  readonly artifactVersion: string | undefined
  readonly hasActiveTheme: boolean
}

export function captureThemeEditorSnapshot(scope: StudioScope | null, served: ServedTheme): ThemeEditorSnapshot | null {
  if (scope === null) return null
  if (served.kind === "theme" || served.kind === "invalid") {
    return {
      scope,
      expectedVersion: served.version,
      artifactId: served.id,
      artifactVersion: served.artifactVersion,
      hasActiveTheme: true,
    }
  }
  if (served.kind === "none") {
    return {
      scope,
      expectedVersion: served.version,
      artifactId: served.artifactId ?? WORKSPACE_THEME_ID,
      artifactVersion: served.artifactVersion,
      hasActiveTheme: false,
    }
  }
  return null
}

const EMPTY_PROBLEMS: readonly Problem[] = []

/**
 * Immutable session state for one open lifecycle of `CustomThemeDialog`.
 *
 * - `openedScope` is pinned once on open (even while `served.kind === "loading"`) and never
 *   overwritten while the dialog remains open.
 * - If the decided workspace changes away from `openedScope` at any point (including before the
 *   initial load finishes), `scopeChanged` latches `true` and `snapshot` is cleared so Apply/Reset
 *   can never target another workspace.
 * - Once `served` settles for `openedScope`, `snapshot` and `captured: true` lock the revision so
 *   background feed updates never overwrite `expectedVersion` or the user's draft.
 */
export interface ThemeEditorSession {
  readonly openedScope: StudioScope | null
  readonly snapshot: ThemeEditorSnapshot | null
  readonly captured: boolean
  readonly scopeChanged: boolean
}

export function openThemeEditorSession(
  liveScope: StudioScope | null,
  served: ServedTheme,
): ThemeEditorSession {
  return {
    openedScope: liveScope,
    snapshot: captureThemeEditorSnapshot(liveScope, served),
    captured: served.kind !== "loading",
    scopeChanged: false,
  }
}

export function reconcileThemeEditorSession(
  session: ThemeEditorSession,
  liveScope: StudioScope | null,
  served: ServedTheme,
): ThemeEditorSession {
  if (session.scopeChanged) return session
  if (!sameStudioScope(liveScope, session.openedScope)) {
    return {
      openedScope: session.openedScope,
      snapshot: null,
      captured: true,
      scopeChanged: true,
    }
  }
  if (session.captured || served.kind === "loading") return session
  return {
    openedScope: session.openedScope,
    snapshot: captureThemeEditorSnapshot(session.openedScope, served),
    captured: true,
    scopeChanged: false,
  }
}

const problemsOf = (result: ThemeApplyResult): readonly Problem[] => {
  if (result.applied) return []
  if ("findings" in result) return result.findings.map((finding) => ({ path: finding.pointer, message: finding.message }))
  if ("conflict" in result) return [{ path: null, message: "Someone changed this workspace's theme since you opened it. Close and reopen to edit the current one." }]
  return [{ path: null, message: result.message }]
}

/**
 * The workspace theme editor (themes as data). The theme is the workspace's `theme` artifact in the
 * cell's definition set: the JSON here is applied through `POST /v1/cell/definitions:apply` against
 * the exact definitions version captured when the editor opened. The cell's Ring-1 checker admits
 * or refuses it, and every open page in the workspace repaints from `GET /v1/definitions`.
 */
export function CustomThemeDialog({
  open,
  onOpenChange,
}: {
  readonly open: boolean
  readonly onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const { skin, customTheme } = useAppearance()
  const { scope: liveScope, served } = useServedTheme()
  const [session, setSession] = useState<ThemeEditorSession | null>(null)
  const [draft, setDraft] = useState("")
  const [problems, setProblems] = useState<readonly Problem[]>(EMPTY_PROBLEMS)
  const [refusedBy, setRefusedBy] = useState<"studio" | "cell">("studio")
  const [pending, setPending] = useState<"apply" | "reset" | null>(null)
  const [copied, setCopied] = useState(false)
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const sessionRef = useRef<ThemeEditorSession | null>(null)
  const dirtyRef = useRef(false)

  useEffect(() => () => {
    if (copyTimer.current !== null) clearTimeout(copyTimer.current)
  }, [])

  // Pin `openedScope` immediately on open (even while loading), reconcile once when that scope's
  // initial read settles, and immediately invalidate if the decided workspace changes before or
  // after capture. Repeated loading renders for the same scope are strict no-ops.
  useEffect(() => {
    if (!open) {
      if (sessionRef.current !== null) {
        sessionRef.current = null
        dirtyRef.current = false
        setSession(null)
      }
      return
    }
    const current = sessionRef.current
    if (current === null) {
      const opened = openThemeEditorSession(liveScope, served)
      sessionRef.current = opened
      dirtyRef.current = false
      setSession(opened)
      setDraft(exportStudioThemeJson(skin, served.kind === "theme" ? served.theme : customTheme))
      setProblems(EMPTY_PROBLEMS)
      setCopied(false)
      if (copyTimer.current !== null) {
        clearTimeout(copyTimer.current)
        copyTimer.current = null
      }
      return
    }
    const next = reconcileThemeEditorSession(current, liveScope, served)
    if (next === current) return
    sessionRef.current = next
    setSession(next)
    if (!next.scopeChanged && !dirtyRef.current) {
      setDraft(exportStudioThemeJson(skin, served.kind === "theme" ? served.theme : null))
    }
  }, [open, liveScope, served, skin, customTheme])

  const snapshot = session?.snapshot ?? null
  const scopeChanged = session?.scopeChanged ?? false
  const loadingInitial = open && (session === null || !session.captured)

  const settle = async (targetScope: StudioScope, result: ThemeApplyResult) => {
    if (result.applied) {
      setProblems(EMPTY_PROBLEMS)
      await queryClient.invalidateQueries({ queryKey: servedThemeKey(targetScope) })
      onOpenChange(false)
      return
    }
    setRefusedBy("cell")
    setProblems(problemsOf(result))
  }

  const applyToWorkspace = async () => {
    const read = normalizeStudioTheme(draft)
    if (!read.ok) {
      setRefusedBy("studio")
      setProblems(read.issues.map((issue) => ({ path: issue.path, message: issue.message })))
      return
    }
    if (scopeChanged) {
      setRefusedBy("studio")
      setProblems([{ path: null, message: WORKSPACE_CHANGED_MESSAGE }])
      return
    }
    if (loadingInitial) {
      setRefusedBy("studio")
      setProblems([{ path: null, message: "Still loading this workspace's current theme revision. Try again in a moment." }])
      return
    }
    if (snapshot === null) {
      setRefusedBy("studio")
      setProblems([{ path: null, message: NO_DATA_CELL }])
      return
    }
    setPending("apply")
    try {
      await settle(
        snapshot.scope,
        await applyWorkspaceTheme(snapshot.scope, read.theme, snapshot.expectedVersion, {
          id: snapshot.artifactId,
          previousArtifactVersion: snapshot.artifactVersion,
        }),
      )
    } finally {
      setPending(null)
    }
  }

  const reset = async () => {
    if (scopeChanged || loadingInitial || snapshot === null || !snapshot.hasActiveTheme) return
    setPending("reset")
    try {
      await settle(
        snapshot.scope,
        await resetWorkspaceTheme(snapshot.scope, snapshot.expectedVersion, {
          id: snapshot.artifactId,
          previousArtifactVersion: snapshot.artifactVersion,
        }),
      )
    } finally {
      setPending(null)
    }
  }

  const copyJson = async () => {
    try {
      await navigator.clipboard?.writeText(draft)
      setCopied(true)
      if (copyTimer.current !== null) clearTimeout(copyTimer.current)
      copyTimer.current = setTimeout(() => {
        copyTimer.current = null
        setCopied(false)
      }, 1800)
    } catch {
      /* clipboard blocked */
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogBackdrop className="fixed inset-0 z-50 bg-black/45 backdrop-blur-[2px] transition-opacity duration-150 motion-reduce:transition-none data-starting-style:opacity-0 data-ending-style:opacity-0" />
      <DialogContent
        className="fixed top-[8vh] start-1/2 z-50 m-0 flex max-h-[84vh] w-[min(40rem,calc(100vw-2rem))] ltr:-translate-x-1/2 rtl:translate-x-1/2 flex-col overflow-hidden rounded-2xl bg-popover text-popover-foreground shadow-float ring-1 ring-border outline-none transition-[opacity,scale] duration-150 motion-reduce:transition-none data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0"
      >
        <div className="flex items-start justify-between gap-3 border-b border-border px-5 py-4">
          <div>
            <DialogTitle className="text-body text-popover-foreground">Workspace theme</DialogTitle>
            <DialogDescription className="mt-0.5 text-meta text-muted-foreground">
              Semantic tokens as JSON, for everyone in this workspace. The cell checks every theme (WCAG AA contrast, monochrome chrome) before it applies; your light or dark choice stays yours.
            </DialogDescription>
          </div>
          <DialogClose
            aria-label="Close"
            className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring max-sm:size-[44px]"
          >
            <X aria-hidden className="size-4" />
          </DialogClose>
        </div>

        <div className="flex-1 space-y-3 overflow-y-auto px-5 py-4">
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="me-1 text-meta text-muted-foreground">Presets:</span>
            {STUDIO_THEME_PRESETS.map((preset) => (
              <Button
                key={preset.name}
                type="button"
                variant="outline"
                size="xs"
                onClick={() => {
                  dirtyRef.current = true
                  setDraft(JSON.stringify(preset, null, 2))
                  setProblems(EMPTY_PROBLEMS)
                }}
              >
                <Palette aria-hidden className="size-3" />
                {preset.name}
              </Button>
            ))}
            <Button type="button" variant="ghost" size="xs" className="ms-auto" onClick={() => void copyJson()}>
              {copied ? <Check aria-hidden className="size-3.5" /> : <Copy aria-hidden className="size-3.5" />}
              {copied ? "Copied" : "Copy JSON"}
            </Button>
          </div>

          <textarea
            aria-label="Theme JSON definition"
            dir="ltr"
            spellCheck={false}
            value={draft}
            onChange={(event) => {
              dirtyRef.current = true
              setDraft(event.target.value)
              if (problems.length > 0) setProblems(EMPTY_PROBLEMS)
            }}
            className="h-72 w-full resize-y rounded-xl border border-input bg-well p-3 font-mono text-meta leading-relaxed text-foreground outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
          />

          {scopeChanged && (
            <div role="alert" data-slot="theme-scope-changed" className="rounded-xl border border-destructive/30 bg-destructive/10 p-3 text-meta text-destructive-text">
              {WORKSPACE_CHANGED_MESSAGE}
            </div>
          )}

          {problems.length > 0 && (
            <div role="alert" data-slot="theme-validation-errors" data-refused-by={refusedBy} className="space-y-1 rounded-xl border border-destructive/30 bg-destructive/10 p-3 text-meta text-destructive-text">
              <div>{refusedBy === "cell" ? "The cell refused this theme" : "This JSON is not a theme"} ({problems.length} {problems.length === 1 ? "issue" : "issues"}):</div>
              <ul className="list-disc space-y-0.5 ps-4">
                {problems.map((problem, idx) => (
                  <li key={`${problem.path ?? ""}:${idx}`}>
                    {problem.path && <><span className="font-mono">{problem.path}</span>: </>}{problem.message}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border px-5 py-3">
          {snapshot?.hasActiveTheme ? (
            <Button type="button" variant="ghost" size="sm" disabled={pending !== null || scopeChanged || loadingInitial} onClick={() => void reset()}>
              {pending === "reset" ? <Loader2 aria-hidden className="size-3.5 animate-spin" /> : <RotateCcw aria-hidden className="size-3.5" />}Reset to default
            </Button>
          ) : (
            <span className="text-meta text-muted-foreground">This workspace uses the default theme (<span className="text-foreground capitalize">{skin}</span>).</span>
          )}
          <div className="flex items-center gap-2">
            <Button type="button" variant="outline" size="sm" onClick={() => onOpenChange(false)}>Cancel</Button>
            <Button type="button" variant="default" size="sm" disabled={pending !== null || scopeChanged || loadingInitial} onClick={() => void applyToWorkspace()}>
              {pending === "apply" && <Loader2 aria-hidden className="size-3.5 animate-spin" />}Use for this workspace
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
