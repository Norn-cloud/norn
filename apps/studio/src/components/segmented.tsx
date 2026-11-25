import type { ComponentProps } from "react"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { cn } from "@/lib/utils"

type SingleGroup = Extract<ComponentProps<typeof ToggleGroup>, { type: "single" }>

/**
 * A segmented control (DESIGN.md 10): one choice of a few, drawn as a `--well` track with the chosen
 * segment raised on `--card` at 500. It is a single-select toggle group underneath, so it keeps the
 * radiogroup and radio roles and the arrow keys. An unchosen segment is muted ink on the track, never
 * a separate outlined button, so it cannot be mistaken for a disabled one.
 */
export function Segmented({ className, ...props }: Omit<SingleGroup, "type" | "variant" | "spacing" | "size">) {
  return (
    <ToggleGroup type="single" data-slot="segmented" {...props}
      className={cn("gap-[2px] rounded-full bg-(--well) p-[3px] ring-1 ring-line ring-inset", className)} />
  )
}

/** One segment: muted at rest, foreground on hover, raised on the card with a hairline when chosen. */
export function SegmentedItem({ className, ...props }: Omit<ComponentProps<typeof ToggleGroupItem>, "variant" | "size">) {
  return (
    <ToggleGroupItem {...props}
      className={cn(
        "h-7 min-w-0 rounded-full border-0 bg-transparent px-3 text-body font-normal text-muted-foreground transition-[color,background-color,box-shadow] duration-150 hover:bg-transparent hover:text-foreground motion-reduce:transition-none max-sm:min-h-[44px]",
        "aria-checked:bg-card aria-checked:font-medium aria-checked:text-foreground aria-checked:shadow-(--panel-shadow) aria-checked:ring-1 aria-checked:ring-panel-border data-pressed:bg-card aria-pressed:bg-card",
        className,
      )} />
  )
}
