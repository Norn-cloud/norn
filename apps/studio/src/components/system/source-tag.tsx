import type { LucideIcon } from "lucide-react"
import type { ComponentProps, ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * An outline tag for a source system, a kind or a short code (DESIGN.md 10, SourceTag): step 1, the
 * chip radius, a `--line` hairline, no fill and muted text. Codes are mono and kept left-to-right in
 * an RTL page (`<bdi>`); a word tag (`mono={false}`) reads in the page's direction.
 */
export function SourceTag({ icon: Icon, mono = true, children, className, ...props }: {
  readonly icon?: LucideIcon
  readonly mono?: boolean
  readonly children: ReactNode
} & Omit<ComponentProps<"span">, "children">) {
  return (
    <span data-slot="source-tag" className={cn("inline-flex h-[1.2rem] max-w-full shrink-0 items-center gap-1.5 rounded-(--radius-chip) px-1.5 text-meta whitespace-nowrap text-muted-foreground ring-1 ring-line ring-inset",
      mono && "font-mono tracking-[-0.01em]", className)} {...props}>
      {Icon && <Icon aria-hidden className="size-3 shrink-0" strokeWidth={1.7} />}
      {mono ? <bdi className="min-w-0 truncate">{children}</bdi> : children}
    </span>
  )
}
