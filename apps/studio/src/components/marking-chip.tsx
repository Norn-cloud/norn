import { useQuery } from "@tanstack/react-query"
import { Fingerprint, LockKeyhole, ShieldAlert } from "lucide-react"
import { useMemo } from "react"
import { Chip } from "@/components/system/chip"
import { markingCatalog, markingLabel, projectionWords, type MarkingCatalog } from "@/lib/markings"
import { transport } from "@/lib/transport"

/** The admitted marking catalog and reveal rules (the same catalog read every page shares). */
export function useMarkingCatalog(): MarkingCatalog | undefined {
  const read = useQuery({ queryKey: ["catalog"], queryFn: transport.catalog })
  return useMemo(() => read.data && markingCatalog(read.data.document), [read.data])
}

/**
 * One PRIVACY-1 marking on a fact, in the catalog's own words ("Contact"), coloured by its declared
 * severity; its title says how a marked value is shown to someone who may not see it.
 */
export function MarkingChip({ marking, catalog, className }: { readonly marking: string; readonly catalog: MarkingCatalog | undefined; readonly className?: string }) {
  const decl = catalog?.declared.get(marking)
  const Icon = decl?.mask === "refuse" ? LockKeyhole : decl?.mask === "pseudonym" ? Fingerprint : ShieldAlert
  const tone = decl?.severity === "critical" ? "red" : decl?.severity === "high" ? "amber" : "gray"
  return (
    <Chip tone={tone} icon={Icon} className={className} data-marking={marking} title={`Marked ${marking}. ${projectionWords(decl)}; revealing it leaves a receipt.`}>
      {markingLabel(catalog, marking)}
    </Chip>
  )
}

/** A fact's markings as chips, or nothing when it carries none. */
export function MarkingChips({ markings, catalog, className }: { readonly markings: readonly string[] | undefined; readonly catalog: MarkingCatalog | undefined; readonly className?: string }) {
  if (markings === undefined || markings.length === 0) return null
  return <span className={className ?? "inline-flex flex-wrap items-center gap-1"}>{markings.map((marking) => <MarkingChip key={marking} marking={marking} catalog={catalog} />)}</span>
}
