import { clsx, type ClassValue } from "clsx"
import { extendTailwindMerge } from "tailwind-merge"

/**
 * The locked five-step type scale (DESIGN.md 8.1). tailwind-merge only knows Tailwind's own sizes, so
 * without this it reads `text-meta` as a colour and drops it beside `text-muted-foreground`.
 */
export const TYPE_SCALE = ["meta", "body", "heading", "title", "figure"] as const

const twMerge = extendTailwindMerge({ extend: { classGroups: { "font-size": [{ text: [...TYPE_SCALE] }] } } })

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
