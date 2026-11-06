import { BadgeCheck, ShieldCheck } from "lucide-react"
import { Panel } from "@/components/panel"
import { Button } from "@/components/ui/button"

/**
 * A definition in plain words and its sign-off (D59), as questions show it: the cell's words, their hash, and "This
 * is what I asked for", which signs exactly those words. The words come from the cell's restater, never a model.
 */
export function PlainWords({ words, signed, fromCell, onSign, note, testId = "restatement" }: {
  readonly words: { readonly text: string; readonly sha256: string }
  readonly signed: boolean
  /** The cell answered a sign-off with words of its own: say so above them. */
  readonly fromCell?: boolean
  readonly onSign: () => void
  readonly note?: string
  readonly testId?: string
}) {
  return (
    <Panel icon={ShieldCheck} className="rounded-node border-node-border shadow-node" label="In plain words" action={<span className="font-mono text-xs text-muted-foreground" title={words.sha256}>{words.sha256.slice(0, 12)}</span>}>
      <div className="space-y-3 p-2">
        {fromCell && <p className="font-medium">The cell reads it as:</p>}
        <p className="text-base whitespace-pre-line" data-testid={testId}>{words.text}</p>
        <p className="text-meta text-muted-foreground">{note ?? "Written by the cell from the definition itself, not by the assistant. Check that it says what you mean."}</p>
        <div className="flex flex-wrap items-center gap-3">
          {signed
            ? <span className="flex items-center gap-2 text-sm font-medium"><BadgeCheck className="size-4 text-success" />Signed off: this is what you asked for</span>
            : <Button type="button" size="sm" onClick={onSign}>This is what I asked for</Button>}
        </div>
      </div>
    </Panel>
  )
}
