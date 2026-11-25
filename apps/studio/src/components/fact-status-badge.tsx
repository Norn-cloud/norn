import { Clock, EyeOff, ShieldAlert, ShieldCheck, type LucideIcon } from "lucide-react"
import { Chip, type Tone } from "@/components/system/chip"

/**
 * Honest verification state of a record or staged candidate row across progressive go-live:
 * - `imported_unverified`: staged from an external capture or extraction, not yet admitted as verified.
 * - `quarantined`: held out of authoritative reads because of an open law finding or feed conflict.
 * - `withheld`: one or more fields or findings are withheld from this role; verification cannot be claimed.
 * - `verified_fact`: explicitly admitted through governed cell actions and laws with receipt and lineage.
 */
export type FactStatus = "imported_unverified" | "quarantined" | "withheld" | "verified_fact"

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
  withheld: {
    label: "Withheld",
    shortLabel: "Withheld",
    tone: "gray",
    icon: EyeOff,
    description: "One or more fields or findings are withheld from your role; full verification status is not disclosed.",
  },
  verified_fact: {
    label: "Verified fact",
    shortLabel: "Verified",
    tone: "green",
    icon: ShieldCheck,
    description: "Admitted through governed cell rules with receipt and lineage.",
  },
}

/**
 * Derive the honest status for any record view from cell-reported quarantine, finding, withheld,
 * and explicit verification flags. Precedence is strictly conservative:
 *   Quarantined > Withheld > Imported (unverified) > Verified fact > null (unknown).
 * Never infers "Verified fact" from the mere absence of visible findings.
 */
export function deriveFactStatus(input: {
  readonly quarantined?: boolean
  readonly openFindings?: number
  readonly withheld?: boolean
  readonly unverifiedImport?: boolean
  readonly verified?: boolean
}): FactStatus | null {
  if (input.quarantined === true || (input.openFindings ?? 0) > 0) return "quarantined"
  if (input.withheld === true) return "withheld"
  if (input.unverifiedImport === true) return "imported_unverified"
  if (input.verified === true) return "verified_fact"
  return null
}

/**
 * Persistent honest status distinguishing Imported (unverified), Quarantined, Withheld, and Verified
 * fact, dot-first (DESIGN.md 12): the state's dot (`--dot-bad`, `--dot-quiet`, `--dot-held`,
 * `--dot-live`) and its plain label, with the description in the tooltip. The word always says the
 * state, so colour is never its only carrier.
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
      dot
      data-fact-status={status}
      title={title ?? meta.description}
      className={className}
    >
      {compact ? meta.shortLabel : meta.label}
    </Chip>
  )
}
