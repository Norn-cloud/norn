import { clsx, type ClassValue } from "clsx"
import { extendTailwindMerge } from "tailwind-merge"

/**
 * The locked five-step type scale (DESIGN.md 8.1). tailwind-merge only knows Tailwind's own sizes, so
 * without this it reads `text-meta` as a colour and drops it beside `text-muted-foreground`.
 */
export const TYPE_SCALE = ["meta", "body", "heading", "title", "figure"] as const

/**
 * The steps are their own class group, which conflicts with Tailwind's font sizes both ways and with
 * nothing else. Inside `font-size` they would inherit its conflict with `leading-*`, and
 * `cn("leading-snug", "text-body")` would drop the leading, although CSS keeps a `leading-*` beside a
 * step in either order.
 */
const twMerge = extendTailwindMerge<"type-step">({
  extend: {
    classGroups: { "type-step": [{ text: [...TYPE_SCALE] }] },
    conflictingClassGroups: { "type-step": ["font-size"], "font-size": ["type-step"] },
  },
})

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
