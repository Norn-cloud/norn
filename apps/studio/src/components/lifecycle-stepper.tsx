import { cn } from "@/lib/utils"
import { conceptTitle, type Lifecycle } from "../lib/catalog.ts"
import { lifecycleOf } from "../lib/objects.ts"
import type { ObjectRecord } from "../lib/transport.ts"

/**
 * Where the record sits in a declared lifecycle, and the steps the definition declares from there.
 * Only the current state is marked: declaration order is not history, and no server document says which steps were taken.
 */
export function LifecycleStepper({ lifecycle, record }: { readonly lifecycle: Lifecycle; readonly record: ObjectRecord }) {
  const { states, state, terminal, next } = lifecycleOf(lifecycle, record)
  return (
    <div className="space-y-3">
      <ol aria-label={`${conceptTitle(record.objectType)} ${lifecycle.property.replaceAll("_", " ")}`} className="flex flex-wrap items-center gap-2">
        {states.map((step, index) => (
          <li key={step} aria-current={step === state ? "step" : undefined} className="flex items-center gap-2">
            {index > 0 && <span aria-hidden className="h-px w-6 bg-border" />}
            <span className={cn("rounded-full border px-3 py-1 text-sm", step === state ? "border-primary bg-primary text-primary-foreground" : "text-muted-foreground")}>
              {step.replaceAll("_", " ")}
            </span>
          </li>
        ))}
      </ol>
      <p className="text-sm text-muted-foreground">
        {state === undefined
          ? `The record has no ${lifecycle.property.replaceAll("_", " ")} yet.`
          : terminal || next.length === 0
            ? "The definition declares no step from here."
            : <>Next, the definition allows {next.map(({ id, to }, index) => <span key={id}>{index > 0 && ", "}<b className="font-medium text-foreground">{id.replaceAll("_", " ")}</b> (to {to.replaceAll("_", " ")})</span>)}. The server checks each step's rules when it is asked.</>}
      </p>
    </div>
  )
}
