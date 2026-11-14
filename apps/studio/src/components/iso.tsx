import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

/**
 * An id, code, phone number or other value inside running text: isolated
 * (`<bdi>`), so it keeps its own direction in Arabic text and never reorders
 * the words around it. Ids are left-to-right by nature.
 */
export function Iso({ children, className, ltr = true, lang }: { readonly children: ReactNode; readonly className?: string; readonly ltr?: boolean; readonly lang?: string }) {
  return <bdi dir={ltr ? "ltr" : "auto"} lang={lang} className={cn("[unicode-bidi:isolate]", className)}>{children}</bdi>
}
