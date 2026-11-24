import { cn } from "@/lib/utils"
import { lifecycleStateRole } from "../../../../packages/norn-demo-ui/src/elements/lifecycle-view.ts"
import { conceptTitle, type Concept, type Lifecycle } from "../lib/catalog.ts"
import { useLocale, type Locale } from "../lib/locale.tsx"
import { declaredLifecycleStates, lifecycleOf } from "../lib/objects.ts"
import type { ObjectRecord } from "../lib/transport.ts"
import { lifecycleLabel, sentence } from "./lifecycle-view.tsx"

const COPY = {
  en: {
    unset: (property: string) => `The record has no ${property} yet.`,
    noStep: "The definition declares no step from here.",
    nextLead: "Next, the definition allows ",
    to: (state: string) => `to ${state}`,
    nextTail: ". The server checks each step's rules when it is asked.",
  },
  ar: {
    unset: (property: string) => `لا يملك هذا السجل قيمة لـ ${property} بعد.`,
    noStep: "لا يعلن التعريف أي خطوة من هنا.",
    nextLead: "التالي، يسمح التعريف بـ ",
    to: (state: string) => `إلى ${state}`,
    nextTail: ". يتحقق الخادم من قواعد كل خطوة عند طلبها.",
  },
} as const

/**
 * Where one record sits in a declared lifecycle, and the steps the definition declares from there.
 * Shares state ordering, labels, roles (`start`, `ending`, `can rest here`), ending styling, and
 * RTL layout with the type-wide `<norn-lifecycle-view>`. Only the current state is marked:
 * declaration order is not history, and no server document says which steps were taken.
 */
export function LifecycleStepper({ lifecycle, record, concept, locale: explicitLocale }: {
  readonly lifecycle: Lifecycle
  readonly record: ObjectRecord
  readonly concept?: Concept
  readonly locale?: Locale
}) {
  const { locale: activeLocale } = useLocale()
  const locale = explicitLocale ?? activeLocale
  const rtl = locale === "ar"
  const copy = COPY[locale]
  const field = concept?.fields.find((candidate) => candidate.id === lifecycle.property)
  const declared = declaredLifecycleStates(lifecycle, field)
  const { states, state, terminal, next } = lifecycleOf(lifecycle, record, declared)
  const terminalList = lifecycle.terminal ?? []
  const roleDoc = { initial: lifecycle.initial, terminal: terminalList, endings: lifecycle.endings ?? [] }
  const typeLabel = concept?.title ?? conceptTitle(record.objectType)
  const propertyLabel = field?.label ?? sentence(lifecycle.property)
  const display = (id: string) => lifecycleLabel(id, field)
  return (
    <div dir={rtl ? "rtl" : "ltr"} lang={locale} className="space-y-3">
      <ol aria-label={`${typeLabel} ${propertyLabel}`} className="flex flex-wrap items-stretch gap-2">
        {states.map((step, index) => {
          const current = step === state
          const isEnding = terminalList.includes(step)
          const role = lifecycleStateRole(roleDoc, step, locale)
          return (
            <li key={step} data-state={step} data-current={current ? "" : undefined} data-ending={isEnding ? "" : undefined}
              aria-current={current ? "step" : undefined} className="flex items-center gap-2">
              {index > 0 && <span aria-hidden className="text-xs text-muted-foreground/70">{rtl ? "←" : "→"}</span>}
              <span className={cn(
                "grid min-w-[6.5rem] self-stretch content-center gap-0.5 rounded-lg border px-3 py-1.5 text-sm transition-[border-color,box-shadow]",
                isEnding && "border-[3px] border-double px-[calc(0.75rem-2px)] py-[calc(0.375rem-2px)]",
                current
                  ? "border-primary bg-card font-medium text-foreground shadow-elevation ring-2 ring-primary/25"
                  : "border-border/90 bg-surface-subtle/70 text-muted-foreground",
              )}>
                <span className={cn("leading-snug", current && "text-foreground")}>{display(step)}</span>
                {role !== "" && <span className="text-[11px] font-normal text-muted-foreground">{role}</span>}
              </span>
            </li>
          )
        })}
      </ol>
      <p className="text-sm text-muted-foreground">
        {state === undefined
          ? copy.unset(propertyLabel)
          : terminal || next.length === 0
            ? copy.noStep
            : <>{copy.nextLead}{next.map(({ id, to }, index) => <span key={id}>{index > 0 && (rtl ? "، " : ", ")}<b className="font-medium text-foreground">{display(id)}</b> ({copy.to(display(to))})</span>)}{copy.nextTail}</>}
      </p>
    </div>
  )
}
