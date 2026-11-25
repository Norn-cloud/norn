import { Dialog, DialogBackdrop, DialogClose, DialogContent, DialogDescription, DialogTitle } from "@ui-sdk/react/dialog"
import { useQueryClient } from "@tanstack/react-query"
import { Check, Copy, Loader2, Palette, RotateCcw, X } from "lucide-react"
import { useEffect, useState } from "react"
import { Button } from "@/components/ui/button"
import { useAppearance } from "@/lib/appearance"
import { NO_DATA_CELL, type StudioScope } from "@/lib/definitions"
import {
  STUDIO_THEME_PRESETS,
  exportStudioThemeJson,
  normalizeStudioTheme,
} from "@/lib/studio-theme"
import { applyWorkspaceTheme, resetWorkspaceTheme, servedThemeKey, useServedTheme, type ThemeApplyResult } from "@/lib/workspace-theme"

/** One line under the editor: where it came from (Studio reading the JSON, or the cell), and what it said. */
interface Problem {
  readonly path: string | null
  readonly message: string
}

const problemsOf = (result: ThemeApplyResult): readonly Problem[] => {
  if (result.applied) return []
  if ("findings" in result) return result.findings.map((finding) => ({ path: finding.pointer, message: finding.message }))
  if ("conflict" in result) return [{ path: null, message: "Someone changed this workspace's theme since you opened it. Close and reopen to edit the current one." }]
  return [{ path: null, message: result.message }]
}

/**
 * The workspace theme editor (themes as data). The theme is the workspace's `theme` definition: the
 * JSON here is applied through the definitions apply call, the cell's checker admits or refuses it
 * (its refusal is shown in its own words), and the cell decides whether this reader may apply
 * definitions at all. Every open page in the workspace repaints from what the cell then serves.
 */
export function CustomThemeDialog({
  open,
  onOpenChange,
}: {
  readonly open: boolean
  readonly onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const { skin, customTheme, served } = useAppearance()
  const { scope: liveScope } = useServedTheme()
  const [scope, setScope] = useState<StudioScope | null>(null)
  const [draft, setDraft] = useState("")
  const [problems, setProblems] = useState<readonly Problem[]>([])
  const [refusedBy, setRefusedBy] = useState<"studio" | "cell">("studio")
  const [pending, setPending] = useState<"apply" | "reset" | null>(null)
  const [copied, setCopied] = useState(false)
  const version = served.kind === "theme" || served.kind === "invalid" ? served.version : undefined

  // The editor opens on the workspace's current theme, and writes stay bound to the workspace it opened in.
  useEffect(() => {
    if (!open) return
    setDraft(exportStudioThemeJson(skin, customTheme))
    setProblems([])
    setCopied(false)
    setScope(liveScope)
    // Only on open: a theme arriving on the feed while the editor is open never replaces the draft.
  }, [open])

  const settle = async (result: ThemeApplyResult) => {
    if (result.applied) {
      setProblems([])
      await queryClient.invalidateQueries({ queryKey: servedThemeKey(scope) })
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
    if (scope === null) {
      setRefusedBy("studio")
      setProblems([{ path: null, message: NO_DATA_CELL }])
      return
    }
    setPending("apply")
    try { await settle(await applyWorkspaceTheme(scope, read.theme, version)) } finally { setPending(null) }
  }

  const reset = async () => {
    if (scope === null || version === undefined) return
    setPending("reset")
    try { await settle(await resetWorkspaceTheme(scope, version)) } finally { setPending(null) }
  }

  const copyJson = async () => {
    try {
      await navigator.clipboard?.writeText(draft)
      setCopied(true)
      setTimeout(() => setCopied(false), 1800)
    } catch {
      /* clipboard blocked */
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogBackdrop className="fixed inset-0 z-50 bg-black/45 backdrop-blur-[2px] transition-opacity duration-150 data-starting-style:opacity-0 data-ending-style:opacity-0" />
      <DialogContent
        className="fixed top-[8vh] left-1/2 z-50 m-0 flex max-h-[84vh] w-[min(40rem,calc(100vw-2rem))] -translate-x-1/2 flex-col overflow-hidden rounded-2xl bg-popover text-popover-foreground shadow-float ring-1 ring-border outline-none transition-[opacity,scale] duration-150 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0"
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
            className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
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
                  setDraft(JSON.stringify(preset, null, 2))
                  setProblems([])
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
            spellCheck={false}
            value={draft}
            onChange={(event) => {
              setDraft(event.target.value)
              if (problems.length > 0) setProblems([])
            }}
            className="h-72 w-full resize-y rounded-xl border border-input bg-well p-3 font-mono text-meta leading-relaxed text-foreground outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
          />

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
          {version !== undefined ? (
            <Button type="button" variant="ghost" size="sm" disabled={pending !== null} onClick={() => void reset()}>
              {pending === "reset" ? <Loader2 aria-hidden className="size-3.5 animate-spin" /> : <RotateCcw aria-hidden className="size-3.5" />}Reset to default
            </Button>
          ) : (
            <span className="text-meta text-muted-foreground">This workspace uses the default theme (<span className="text-foreground capitalize">{skin}</span>).</span>
          )}
          <div className="flex items-center gap-2">
            <Button type="button" variant="outline" size="sm" onClick={() => onOpenChange(false)}>Cancel</Button>
            <Button type="button" variant="default" size="sm" disabled={pending !== null} onClick={() => void applyToWorkspace()}>
              {pending === "apply" && <Loader2 aria-hidden className="size-3.5 animate-spin" />}Use for this workspace
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
