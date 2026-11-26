import { Dialog, DialogBackdrop, DialogClose, DialogContent, DialogDescription, DialogTitle } from "@ui-sdk/react/dialog"
import { Check, Copy, Palette, RotateCcw, X } from "lucide-react"
import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { useAppearance } from "@/lib/appearance"
import {
  STUDIO_THEME_PRESETS,
  exportStudioThemeJson,
  parseStudioTheme,
  type StudioThemeValidationIssue,
} from "@/lib/studio-theme"

/**
 * VS Code / T3 Code-style custom JSON theme editor dialog.
 * Allows operators and tenants to inspect, customize, paste, or export a declarative
 * `StudioTheme` JSON token file with instant WCAG AA contrast validation.
 */
export function CustomThemeDialog({
  open,
  onOpenChange,
}: {
  readonly open: boolean
  readonly onOpenChange: (open: boolean) => void
}) {
  const { skin, customTheme, setCustomTheme, clearCustomTheme } = useAppearance()
  const [draft, setDraft] = useState("")
  const [issues, setIssues] = useState<readonly StudioThemeValidationIssue[]>([])
  const [copied, setCopied] = useState(false)
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => () => {
    if (copyTimer.current !== null) clearTimeout(copyTimer.current)
  }, [])

  useEffect(() => {
    if (!open) return
    setDraft(exportStudioThemeJson(skin, customTheme))
    setIssues([])
    setCopied(false)
    if (copyTimer.current !== null) {
      clearTimeout(copyTimer.current)
      copyTimer.current = null
    }
  }, [open, skin, customTheme])

  const applyDraft = () => {
    const result = parseStudioTheme(draft)
    if (!result.ok) {
      setIssues(result.issues)
      return
    }
    setIssues([])
    setCustomTheme(result.theme)
    onOpenChange(false)
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
            <DialogTitle className="text-body text-popover-foreground">Custom theme JSON</DialogTitle>
            <DialogDescription className="mt-0.5 text-meta text-muted-foreground">
              Override semantic tokens in JSON (VS Code / T3 Code style). Every theme is checked against WCAG AA contrast before applying.
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
                  setDraft(JSON.stringify(preset, null, 2))
                  setIssues([])
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
              setDraft(event.target.value)
              if (issues.length > 0) setIssues([])
            }}
            className="h-72 w-full resize-y rounded-xl border border-input bg-well p-3 font-mono text-meta leading-relaxed text-foreground outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
          />

          {issues.length > 0 && (
            <div role="alert" data-slot="theme-validation-errors" className="space-y-1 rounded-xl border border-destructive/30 bg-destructive/10 p-3 text-meta text-destructive-text">
              <div>Could not apply theme ({issues.length} {issues.length === 1 ? "issue" : "issues"}):</div>
              <ul className="list-disc space-y-0.5 ps-4">
                {issues.map((issue, idx) => (
                  <li key={`${issue.path}:${idx}`}>
                    <span className="font-mono">{issue.path}</span>: {issue.message}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border px-5 py-3">
          {customTheme ? (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={() => {
                clearCustomTheme()
                onOpenChange(false)
              }}
            >
              <RotateCcw aria-hidden className="size-3.5" />Reset to built-in skin
            </Button>
          ) : (
            <span className="text-meta text-muted-foreground">Active skin: <span className="text-foreground capitalize">{skin}</span></span>
          )}
          <div className="flex items-center gap-2">
            <Button type="button" variant="outline" size="sm" onClick={() => onOpenChange(false)}>Cancel</Button>
            <Button type="button" variant="default" size="sm" onClick={applyDraft}>Apply theme</Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
