import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * Choices laid out left to right, one numbered column per step, as in the
 * agent decision tree: each option is a card with its number, label and
 * detail; the server's top pick reads "Recommended" and the assistant's
 * "Agent's pick". The chosen path reads underneath as "Then 1 – 2 – 1".
 * The one implementation of choice columns: the question tree uses it, and CANVAS's decision tree moves onto it.
 */
export type Mark = "recommended" | "agent"

export interface ColumnOption {
  readonly id: string
  readonly label: ReactNode
  readonly detail?: ReactNode
  readonly marks?: readonly Mark[]
}

const MARK_TEXT: Record<Mark, string> = { recommended: "Recommended", agent: "Agent's pick" }

export function ChoiceColumn({ title, options, selected, onSelect, invalid, field, children }: {
  readonly title: string
  readonly options: readonly ColumnOption[]
  readonly selected: string | undefined
  readonly onSelect: (id: string) => void
  /** The server named a problem here. */
  readonly invalid?: boolean
  /** The `data-field` the validation panel jumps to. */
  readonly field?: string
  /** More under the options, e.g. the editor for a custom value. */
  readonly children?: ReactNode
}) {
  return (
    <div className="flex w-64 shrink-0 flex-col gap-2" data-field={field} data-invalid={invalid || undefined}>
      <div className={cn("w-fit rounded-md border bg-card px-3 py-1.5 text-sm font-medium shadow-xs", invalid && "border-destructive text-destructive")}>{title}</div>
      <ol className="flex flex-col gap-2" aria-label={title}>
        {options.map((option, index) => {
          const active = option.id === selected
          return (
            <li key={option.id}>
              <button type="button" aria-pressed={active} onClick={() => onSelect(option.id)}
                className={cn("w-full rounded-lg border bg-card px-3 py-2 text-left text-sm transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/30 focus-visible:outline-none",
                  active ? "border-primary ring-2 ring-primary/20" : selected !== undefined && "opacity-60")}>
                {/* The number stays beside the label, however long the label wraps. */}
                <span className="grid grid-cols-[auto_minmax(0,1fr)] items-baseline gap-x-2">
                  <span className="rounded border px-1.5 font-mono text-xs text-muted-foreground">{index + 1}</span>
                  <span className="flex flex-wrap items-baseline gap-x-2">
                    <span className="font-medium">{option.label}</span>
                    {(option.marks ?? []).map((mark) => <span key={mark} className="text-xs text-primary">{MARK_TEXT[mark]}</span>)}
                  </span>
                  {option.detail && <span className="col-start-2 mt-1 block text-meta text-muted-foreground">{option.detail}</span>}
                </span>
              </button>
            </li>
          )
        })}
      </ol>
      {children}
    </div>
  )
}

/** The chosen path by option number, "Then 1 – 2 – 1 – 3"; a step not chosen reads "–". */
export function PathSummary({ numbers, children }: { readonly numbers: readonly (number | undefined)[]; readonly children?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-3 text-sm" aria-label="Chosen path">
      <span className="text-muted-foreground">Then</span>
      {numbers.map((number, index) => (
        <span key={index} className="flex items-center gap-1">
          {index > 0 && <span className="text-muted-foreground">–</span>}
          <span className="rounded border px-1.5 font-mono text-xs">{number ?? "?"}</span>
        </span>
      ))}
      {children}
    </div>
  )
}

/** Each column's chosen option number (1-based), from the options shown and the ids chosen. */
export const pathNumbers = (columns: readonly { readonly options: readonly { readonly id: string }[]; readonly selected: string | undefined }[]) =>
  columns.map(({ options, selected }) => {
    const index = options.findIndex(({ id }) => id === selected)
    return index < 0 ? undefined : index + 1
  })
