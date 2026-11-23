import { Chip } from "@/components/chip"
import { systemName, systemTone } from "@/lib/catalog"

/** The source system a value was read from, as a chip in that system's tone ("ERPNext"). */
export function SourceChip({ system, className, title }: { readonly system: string; readonly className?: string; readonly title?: string }) {
  return <Chip tone={systemTone(system)} dot data-source={systemName(system)} className={className} title={title ?? `Read from ${systemName(system)}`}>{systemName(system)}</Chip>
}
