import { useQuery } from "@tanstack/react-query"
import { useMemo } from "react"
import { useLocale } from "@/lib/locale"
import { catalogOf } from "./catalog.ts"
import { transport } from "./transport.ts"

/** The admitted business model, in the reader's language: one read, shared by every page that needs it. */
export function useCatalog() {
  const read = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  const { locale } = useLocale()
  const catalog = useMemo(() => read.data && catalogOf(read.data.document, locale), [read.data, locale])
  return { read, catalog }
}
