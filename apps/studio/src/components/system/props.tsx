import type { ComponentProps, ReactNode } from "react"
import { cn } from "@/lib/utils"

/** One labelled fact in a {@link Props} list (DESIGN.md 10). */
export interface PropsItem {
  readonly key?: string
  readonly label: ReactNode
  readonly value: ReactNode
  /** Render the value in mono, isolated left-to-right (`<bdi dir="ltr">`) for ids, digests and codes. */
  readonly mono?: boolean
  /** Align digits for counts, amounts and timestamps. */
  readonly tabular?: boolean
}

/**
 * A single key/value row inside {@link Props} (DESIGN.md 10, Props):
 * `min-h-10 gap-4 px-2.5 py-2.5`; key column `w-32` muted at weight 400; value foreground at step 2.
 * Stacks on narrow phone widths (`< 640px`) so long labels and values never clip or overflow.
 */
export function PropRow({ label, value, children, mono = false, tabular = false, className, ...props }: Omit<ComponentProps<"div">, "children"> & {
  readonly label: ReactNode
  readonly value?: ReactNode
  readonly children?: ReactNode
  readonly mono?: boolean
  readonly tabular?: boolean
}) {
  const body = children ?? value
  return (
    <div
      data-slot="props-row"
      className={cn(
        "flex min-h-10 flex-col gap-1 px-2.5 py-2.5 text-body sm:flex-row sm:items-baseline sm:gap-4",
        className,
      )}
      {...props}
    >
      <dt data-slot="props-key" className="text-start text-body text-muted-foreground sm:w-32 sm:shrink-0">
        {label}
      </dt>
      <dd
        data-slot="props-value"
        className={cn(
          "min-w-0 flex-1 text-start text-body text-foreground [overflow-wrap:anywhere]",
          tabular && "tabular-nums",
        )}
      >
        {mono ? <bdi dir="ltr" className="font-mono text-meta tracking-[-0.01em]">{body}</bdi> : body}
      </dd>
    </div>
  )
}

/**
 * Key/value facts in a sheet, panel or refusal (DESIGN.md 10, Props):
 * a well (`p-1.5`) of quiet rows (`min-h-10 gap-4 px-2.5 py-2.5`) with a `w-32` muted key column
 * and foreground step-2 values, weight 400 only. Already inside a well (`[data-slot=well]`), it
 * drops its own rim and padding so nothing nests a frame inside a well.
 */
export function Props({ items, children, className, ...props }: ComponentProps<"dl"> & {
  readonly items?: readonly PropsItem[]
}) {
  return (
    <dl
      data-slot="props"
      className={cn(
        "divide-y divide-line/60 rounded-(--radius-well) border border-(--well-border) bg-(--well) p-1.5 text-body",
        "[[data-slot=well]_&]:rounded-none [[data-slot=well]_&]:border-0 [[data-slot=well]_&]:bg-transparent [[data-slot=well]_&]:p-0",
        className,
      )}
      {...props}
    >
      {items?.map((item, index) => (
        <PropRow
          key={item.key ?? (typeof item.label === "string" ? item.label : index)}
          label={item.label}
          value={item.value}
          mono={item.mono}
          tabular={item.tabular}
        />
      ))}
      {children}
    </dl>
  )
}
