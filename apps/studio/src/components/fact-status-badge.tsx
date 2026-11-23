import { Clock, ShieldAlert, ShieldCheck, type LucideIcon } from "lucide-react"
import { Chip, type Tone } from "@/components/chip"

/**
 * Honest verification state of a record or staged candidate row across progressive go-live:
 * - `imported_unverified`: staged from an external capture or extraction, not yet admitted as verified.
 * - `quarantined`: held out of authoritative reads because of an open law finding or feed conflict.
 * - `verified_fact`: admitted through governed cell actions and laws with receipt and lineage.
 */
export type FactStatus = "imported_unverified" | "quarantined" | "verified_fact"

export interface FactStatusMeta {
  readonly label: string
  readonly shortLabel: string
  readonly tone: Tone
  readonly icon: LucideIcon
  readonly description: string
}

export const FACT_STATUS_META: Readonly<Record<FactStatus, FactStatusMeta>> = {
  imported_unverified: {
    label: "Imported (unverified)",
    shortLabel: "Unverified",
    tone: "amber",
    icon: Clock,
    description: "Staged from an external import or extraction; not yet promoted as a verified fact.",
  },
  quarantined: {
    label: "Quarantined",
    shortLabel: "Quarantined",
    tone: "red",
    icon: ShieldAlert,
    description: "Held out of authoritative reads due to an open finding or conflict; immutable source evidence is preserved.",
  },
  verified_fact: {
    label: "Verified fact",
    shortLabel: "Verified",
    tone: "green",
    icon: ShieldCheck,
    description: "Admitted through governed cell rules with receipt and lineage.",
  },
}

/** Derive the honest status for any record view from cell-reported quarantine, finding, and import flags. */
export function deriveFactStatus(input: {
  readonly quarantined?: boolean
  readonly openFindings?: number
  readonly unverifiedImport?: boolean
}): FactStatus {
  if (input.quarantined === true || (input.openFindings ?? 0) > 0) return "quarantined"
  if (input.unverifiedImport === true) return "imported_unverified"
  return "verified_fact"
}

/**
 * Persistent honest status badge distinguishing Imported (unverified), Quarantined, and Verified fact.
 * Pairs a restrained semantic tone (amber / red / green) with a distinct icon so status never relies on colour alone.
 */
export function FactStatusBadge({ status, compact = false, className, title }: {
  readonly status: FactStatus
  readonly compact?: boolean
  readonly className?: string
  readonly title?: string
}) {
  const meta = FACT_STATUS_META[status]
  return (
    <Chip
      tone={meta.tone}
      icon={meta.icon}
      data-fact-status={status}
      title={title ?? meta.description}
      className={className}
    >
      {compact ? meta.shortLabel : meta.label}
    </Chip>
  )
}
