import { Link, useLocation } from "@tanstack/react-router"
import { ArrowRight, BadgeCheck, Database, Fingerprint, Layers, Network, RotateCcw, Search, Shield, ShieldX, SlidersHorizontal } from "lucide-react"
import { useEffect, useState } from "react"
import { Chip } from "@/components/chip"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { AGGREGATES, COMMANDS, isFinanceCommand, type RuleId } from "./behavior.ts"
import { CLIENT, ROW_COUNT, SYSTEMS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { RECORDS } from "./records.ts"
import { SystemDependencies, TypeTile } from "./workspace.tsx"

type Systems = readonly [MarkId, MarkId, ...MarkId[]]

interface Refusal {
  readonly action: string
  readonly reason: string
  readonly evidence: string
  readonly reference: string
  readonly href: string
}

interface RuleParam {
  readonly label: string
  readonly value: number
  readonly unit: string
  readonly step?: number
}

type RuleObject = FinanceType | "lease" | "gl-entry"

const SOURCE_OBJECTS = {
  lease: { name: "Lease", tone: "teal" },
  "gl-entry": { name: "GL entry", tone: "violet" },
} as const

interface FinanceRule {
  readonly id: RuleId
  readonly title: string
  readonly guards: string
  readonly reads: readonly [RuleObject, RuleObject, ...RuleObject[]]
  readonly path: string
  readonly condition: string
  readonly systems: Systems
  readonly needs: string
  readonly param?: RuleParam
  readonly refusal?: Refusal
}

export const RULES: readonly FinanceRule[] = [
  {
    id: "kyc", title: "No disbursement without fresh evidence",
    guards: "Loan.disburse", reads: ["loan", "client", "kyc-check", "credit-report", "hardship-case"],
    path: "Loan ← holds ← Client → verified by → KYC check · Client → reported by → Credit report · Client → has → Hardship case",
    systems: ["valify", "iscore", "salesforce", "postgres"],
    condition: "LMS approval requires current verified Valify KYC, an I-Score inquiry within 30 days and no open CRM fraud or hardship flag.",
    needs: "Only the joined record can establish LMS approval, KYC validity, bureau freshness and CRM clearance.",
    param: { label: "Max I-Score freshness", value: 30, unit: "days", step: 5 },
    refusal: {
      action: "Disbursement LN-1042-C refused", reason: "Valify KYC expired 3 Nov. LMS approved, I-Score fresh.",
      evidence: "Valify expiry 3 Nov 2026 · I-Score inquiry 22 Oct 2026 · LMS approved 4 Nov 2026 · CRM clearance recorded.",
      reference: "KYC-1042", href: "/sources/kyc-check/KYC-1042",
    },
  },
  {
    id: "exposure", title: "Total exposure per client ≤ EGP 5M",
    guards: "Loan.approve, Order.placeMargin", reads: ["client", "loan", "brokerage-account", "lease"],
    path: "Client → holds → {Loan, Brokerage account, Lease} (sum)",
    systems: ["postgres", "egx", "mcdr", "leasing"],
    condition: "Sum outstanding loans, margin credit and leases by national ID. Refuse an increase above EGP 5,000,000.",
    needs: "Loans, margin positions and lease obligations are recorded separately and must be merged by national ID.",
    param: { label: "Client exposure ceiling", value: 5_000_000, unit: "EGP", step: 250_000 },
  },
  {
    id: "settlement", title: "Match every settlement within T+2",
    guards: "Payment", reads: ["payment", "installment", "loan"],
    path: "Payment → settles → Installment ← has ← Loan",
    systems: ["fawry", "instapay", "postgres"],
    condition: "Match each Fawry or InstaPay settlement to an LMS installment within two business days, or open an exception.",
    needs: "Payment providers confirm settled amounts. The LMS provides the installment allocation and due date.",
    param: { label: "Settlement match window", value: 2, unit: "business days", step: 1 },
  },
  {
    id: "hardship", title: "An open hardship case pauses collections",
    guards: "Installment.dun, Installment.lateFee", reads: ["hardship-case", "client", "loan", "installment"],
    path: "Hardship case ← has ← Client → holds → Loan → has → Installment",
    systems: ["salesforce", "postgres"],
    condition: "An open hardship case blocks LMS dunning and late fees.",
    needs: "CRM owns the case and the LMS schedules calls, reminders and late fees.",
  },
  {
    id: "margin", title: "No margin or withdrawal while in arrears",
    guards: "Order.placeMargin, BrokerageAccount.withdraw", reads: ["brokerage-account", "client", "loan", "installment"],
    path: "Brokerage account ← holds ← Client → holds → Loan → has → Installment (DPD)",
    systems: ["postgres", "egx", "mcdr"],
    condition: "At 30 or more days past due in the LMS, block margin orders and brokerage cash withdrawals.",
    needs: "The outstanding loan is held by the LMS while the withdrawal or margin request arrives through brokerage.",
    param: { label: "Arrears hold trigger", value: 30, unit: "DPD", step: 5 },
    refusal: {
      action: "Margin order BRK-55012 refused", reason: "Client 34 DPD on LN-1042-A.",
      evidence: "Brokerage order BRK-55012 · matched to client CL-1042 · LMS delinquency at decision exceeded the 30-day threshold.",
      reference: "LN-1042-A", href: "/sources/loan/LN-1042-A",
    },
  },
  {
    id: "identity", title: "Conflicting identity blocks credit decisions",
    guards: "Loan.approve", reads: ["client", "credit-report", "kyc-check"],
    path: "Client → reported by → Credit report · Client → verified by → KYC check (compare name and DOB)",
    systems: ["salesforce", "iscore", "valify"],
    condition: "Send matching national IDs with conflicting names or dates of birth to identity review before a credit decision.",
    needs: "Only a comparison of the CRM, bureau and verified identity records can detect the conflict.",
  },
  {
    id: "aml", title: "Screen third-party payers",
    guards: "Payment", reads: ["payment", "client", "loan"],
    path: "Payment → payer · Loan ← holds ← Client (compare payer with borrower)",
    systems: ["fawry", "salesforce", "postgres"],
    condition: "Raise an AML review when a payer other than the borrower sends more than EGP 50,000 in a month.",
    needs: "Fawry identifies the payer. The client model resolves the borrower across the CRM and LMS.",
    param: { label: "Monthly third-party threshold", value: 50_000, unit: "EGP", step: 10_000 },
  },
  {
    id: "reporting", title: "FRA return equals source totals",
    guards: "RegulatoryReturn.file", reads: ["regulatory-return", "loan", "brokerage-account", "gl-entry"],
    path: "Regulatory return → aggregate by period → {Loan, Brokerage account, GL entry}",
    systems: ["fra", "postgres", "egx", "mcdr", "ledger"],
    condition: "Reconcile the monthly FRA return to LMS outstanding, brokerage positions and the general ledger for the same period.",
    needs: "The filed return has to match separately maintained balances and posted journal entries.",
  },
]

const refusedCount = RULES.filter((rule) => rule.refusal).length

export const financeRuleMenu = (): readonly MenuEntry[] => RULES.map(({ id, title }) => ({ href: `#${id}`, label: title }))

const objectLabel = (type: RuleObject) => (type === "lease" || type === "gl-entry" ? SOURCE_OBJECTS[type].name : TYPES[type].name)

export function RuleScope({ rule }: { readonly rule: Pick<FinanceRule, "guards" | "reads" | "path"> }) {
  const { guards, reads, path } = rule
  return (
    <dl className="space-y-3 text-meta">
      <div>
        <dt className="text-xs text-muted-foreground">Guards</dt>
        <dd className="mt-1 flex flex-wrap gap-1.5">
          {guards.split(", ").map((guard) =>
            isFinanceCommand(guard) ? (
              <Link key={guard} to="/sources/$concept" params={{ concept: COMMANDS[guard].on }} search={{ tab: "commands" }} hash={guard}
                className="rounded-sm border bg-muted/40 px-1.5 py-0.5 font-mono text-xs text-foreground hover:border-foreground/40">
                {guard}
              </Link>
            ) : (
              <code key={guard} className="rounded-sm border bg-muted/40 px-1.5 py-0.5 text-xs text-foreground">{guard}</code>
            ),
          )}
        </dd>
      </div>
      <div>
        <dt className="text-xs text-muted-foreground">Reads ({reads.length} objects)</dt>
        <dd className="mt-1 flex flex-wrap gap-1.5">
          {reads.map((type) => {
            const auxiliary = type === "lease" || type === "gl-entry"
            return (
              <Link key={type} to={auxiliary ? `/policies/${type}` : `/sources/${type}`}
                className="inline-flex items-center rounded-sm border bg-card px-1.5 py-0.5 text-xs text-foreground hover:border-foreground/40">
                {objectLabel(type)}
              </Link>
            )
          })}
        </dd>
      </div>
      <div>
        <dt className="text-xs text-muted-foreground">Model path</dt>
        <dd className="mt-1 font-mono text-xs leading-relaxed text-muted-foreground">{path}</dd>
      </div>
    </dl>
  )
}

/** Flat Snowsight-style master-detail Rules workbench: dense table + hairline side inspector. */
export function FinanceRules() {
  const hash = useLocation({ select: (location) => location.hash })
  const [filter, setFilter] = useState<"all" | "refused">("all")
  const [query, setQuery] = useState("")
  const [selectedId, setSelectedId] = useState<RuleId>(() => RULES.find((r) => r.id === hash)?.id ?? RULES[0]!.id)
  const [draftParams, setDraftParams] = useState<Readonly<Record<string, number>>>({})

  useEffect(() => {
    const match = RULES.find((r) => r.id === hash)
    if (match) setSelectedId(match.id)
  }, [hash])

  const filtered = RULES.filter((rule) => {
    if (filter === "refused" && !rule.refusal) return false
    const q = query.trim().toLowerCase()
    if (!q) return true
    return `${rule.title} ${rule.guards} ${rule.condition} ${rule.reads.map(objectLabel).join(" ")}`.toLowerCase().includes(q)
  })

  const selected = filtered.find((r) => r.id === selectedId) ?? RULES.find((r) => r.id === selectedId) ?? RULES[0]!
  const draftValue = selected.param ? (draftParams[selected.id] ?? selected.param.value) : undefined
  const isDraft = selected.param !== undefined && draftValue !== selected.param.value

  return (
    <section className="space-y-6">
      <PageHeader
        title="Rules"
        meta={
          <>
            <Chip tone="green" dot>{RULES.length} active</Chip>
            <Chip tone="red" dot>{refusedCount} refused in sample</Chip>
          </>
        }
        description="Cross-object invariants enforced before actions commit, with refusal evidence and draft threshold tuning."
      />

      <Panel
        icon={Shield}
        label="Cross-object invariants"
        action={
          <div className="flex flex-wrap items-center gap-2">
            <div role="group" aria-label="Filter rules" className="inline-flex h-7 items-center rounded-md bg-muted p-0.5 text-xs">
              <button type="button" aria-pressed={filter === "all"} onClick={() => setFilter("all")}
                className="rounded-[0.3125rem] px-2 py-0.5 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent">
                All ({RULES.length})
              </button>
              <button type="button" aria-pressed={filter === "refused"} onClick={() => setFilter("refused")}
                className="rounded-[0.3125rem] px-2 py-0.5 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent">
                Refusals ({refusedCount})
              </button>
            </div>
            <label className="relative block w-48">
              <span className="sr-only">Search rules</span>
              <Search className="pointer-events-none absolute start-2.5 top-2 size-3.5 text-muted-foreground" />
              <Input className="h-7 ps-8 text-xs" placeholder="Filter rules…" value={query} onChange={(e) => setQuery(e.target.value)} />
            </label>
          </div>
        }
      >
        <Well className="grid lg:grid-cols-[minmax(0,1fr)_23rem]">
          <div className="min-w-0 overflow-x-auto">
            <Table aria-label="Finance rules" className="text-meta tabular-nums">
              <TableHeader>
                <TableRow>
                  <TableHead>Rule</TableHead>
                  <TableHead>Guards</TableHead>
                  <TableHead>Objects</TableHead>
                  <TableHead className="text-end">Status</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {filtered.map((rule) => {
                  const active = rule.id === selected.id
                  const modified = rule.param !== undefined && draftParams[rule.id] !== undefined && draftParams[rule.id] !== rule.param.value
                  const firstTwo = rule.reads.slice(0, 2).map(objectLabel).join(", ")
                  const extra = rule.reads.length - 2
                  return (
                    <TableRow
                      key={rule.id}
                      id={rule.id}
                      onClick={() => setSelectedId(rule.id)}
                      className={cn("cursor-pointer scroll-mt-4", active && "bg-muted/60")}
                    >
                      <TableCell className={cn(active && "border-s-2 border-s-foreground")}>
                        <div className="flex items-center gap-2">
                          <Shield className={cn("size-3.5 shrink-0", rule.refusal ? "text-tone-red" : "text-muted-foreground")} />
                          <span className="font-medium text-foreground">{rule.title}</span>
                          {modified && <Badge variant="secondary" className="text-[10px]">Draft</Badge>}
                        </div>
                      </TableCell>
                      <TableCell className="font-mono text-xs text-muted-foreground">{rule.guards.split(", ")[0]}{rule.guards.includes(",") ? " +1" : ""}</TableCell>
                      <TableCell className="whitespace-nowrap text-muted-foreground">{firstTwo}{extra > 0 ? ` +${extra}` : ""}</TableCell>
                      <TableCell className="whitespace-nowrap text-end">
                        {rule.refusal ? <Chip tone="red" dot>1 refused</Chip> : <Chip tone="green" dot>Passed</Chip>}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
            {filtered.length === 0 && <p className="p-6 text-center text-meta text-muted-foreground">No rules match this filter.</p>}
          </div>

          <aside aria-label="Rule inspector" className="flex flex-col border-t bg-surface-subtle/40 lg:border-s lg:border-t-0">
            <div className="flex items-start justify-between gap-2 border-b px-4 py-3">
              <div className="min-w-0">
                <p className="text-xs text-muted-foreground">Rule inspector</p>
                <h3 className="mt-0.5 text-sm font-medium text-foreground">{selected.title}</h3>
              </div>
              <Chip tone={selected.refusal ? "red" : "green"} dot>{selected.refusal ? "Refusal recorded" : "Check passed"}</Chip>
            </div>

            <div className="flex-1 divide-y px-4 text-meta">
              {selected.refusal && (
                <div className="space-y-1.5 py-3.5">
                  <div className="flex items-center gap-1.5 text-xs font-medium text-destructive-text">
                    <ShieldX className="size-3.5 shrink-0" />
                    <span>{selected.refusal.action}</span>
                  </div>
                  <p className="font-medium text-foreground">{selected.refusal.reason}</p>
                  <p className="text-xs leading-relaxed text-muted-foreground">{selected.refusal.evidence}</p>
                  <Link to={selected.refusal.href} className="inline-flex items-center gap-1 pt-0.5 text-xs font-medium text-foreground underline underline-offset-4">
                    Inspect evidence · {selected.refusal.reference}<ArrowRight className="size-3 text-muted-foreground" />
                  </Link>
                </div>
              )}

              <div className="space-y-1 py-3.5">
                <p className="text-xs text-muted-foreground">Business invariant</p>
                <p className="leading-relaxed text-foreground">{selected.condition}</p>
                {selected.id === "settlement" && (
                  <Link to="/metrics" hash="corrections" className="inline-flex items-center gap-1 pt-1 text-xs font-medium text-foreground underline underline-offset-4">
                    View settlement correction<ArrowRight className="size-3 text-muted-foreground" />
                  </Link>
                )}
              </div>

              {selected.param && draftValue !== undefined && (
                <div className="space-y-2 py-3.5">
                  <div className="flex items-center justify-between gap-2">
                    <span className="inline-flex items-center gap-1.5 text-xs font-medium text-foreground">
                      <SlidersHorizontal className="size-3 text-muted-foreground" />
                      Rule parameter
                    </span>
                    {isDraft && (
                      <button type="button" onClick={() => setDraftParams((curr) => { const copy = { ...curr }; delete copy[selected.id]; return copy })}
                        className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
                        <RotateCcw className="size-3" />Reset
                      </button>
                    )}
                  </div>
                  <label className="block space-y-1 text-xs">
                    <span className="text-muted-foreground">{selected.param.label}</span>
                    <div className="flex items-center gap-2">
                      <Input
                        type="number"
                        min={1}
                        step={selected.param.step ?? 1}
                        value={draftValue}
                        onChange={(e) => {
                          const val = Number(e.target.value)
                          if (Number.isFinite(val) && val > 0) setDraftParams((curr) => ({ ...curr, [selected.id]: val }))
                        }}
                        className="h-8 w-36 font-mono text-xs"
                      />
                      <span className="text-xs text-muted-foreground">{selected.param.unit}</span>
                    </div>
                  </label>
                  {isDraft && (
                    <p role="status" className="text-xs text-tone-amber">
                      Draft preview: {ROW_COUNT.format(draftValue)} {selected.param.unit} (live: {ROW_COUNT.format(selected.param.value)}). Evaluated before publish.
                    </p>
                  )}
                </div>
              )}

              <div className="py-3.5">
                <RuleScope rule={selected} />
              </div>

              <div className="py-3.5">
                <p className="mb-1.5 text-xs text-muted-foreground">Backing systems</p>
                <SystemDependencies systems={selected.systems} needs={selected.needs} compact />
              </div>
            </div>
          </aside>
        </Well>
      </Panel>
    </section>
  )
}

export const MARKING_FIELDS = [
  { label: "National ID", ar: "الرقم القومي", value: CLIENT.nationalId, type: "client", field: "Client.nationalId", confidence: 98.1 },
  { label: "Phone", ar: "رقم الهاتف", value: CLIENT.phone, type: "client", field: "Client.phone", confidence: 97.2 },
  { label: "Date of birth", ar: "تاريخ الميلاد", value: CLIENT.birth, type: "kyc-check", field: "KycCheck.birthDate", confidence: 97.8 },
  { label: "Address", ar: "العنوان", value: CLIENT.address, type: "kyc-check", field: "KycCheck.address", confidence: 86.3 },
] as const

export const MARKING_ROLES = ["Viewer", "Analyst", "Auditor"] as const
export type MarkingRole = typeof MARKING_ROLES[number]
export const canRevealSample = (role: MarkingRole) => role === "Auditor"

export function FinanceMarkings() {
  return (
    <section className="space-y-6">
      <PageHeader
        title="Markings"
        meta={<Chip tone="amber" icon={Shield}>Restricted · {MARKING_FIELDS.length} fields</Chip>}
        description="Field-level sensitivity classifications governing what each role can view or reveal across documents and records."
        primary={
          <Button asChild size="sm">
            <Link to="/examples/$scene" params={{ scene: "document" }}>Preview in Document mapping</Link>
          </Button>
        }
      />

      <Panel icon={Fingerprint} label="Personally identifiable information" action="Role reveal policy">
        <TableCard>
          <Table aria-label="PII role reveal matrix">
            <TableHeader>
              <TableRow>
                <TableHead scope="col">Marked field</TableHead>
                {MARKING_ROLES.map((role) => <TableHead key={role} scope="col" className="w-[18%]">{role}</TableHead>)}
              </TableRow>
            </TableHeader>
            <TableBody>
              {MARKING_FIELDS.map((field) => (
                <TableRow key={field.field}>
                  <th scope="row" className="h-cell px-3 py-1.5 text-start align-middle font-normal whitespace-nowrap">
                    <div className="flex items-center gap-2.5">
                      <TypeTile type={field.type} />
                      <div>
                        <div className="font-medium text-foreground">{field.label}</div>
                        <code className="text-xs text-muted-foreground">{field.field}</code>
                      </div>
                    </div>
                  </th>
                  {MARKING_ROLES.map((role) => (
                    <TableCell key={role}>
                      {canRevealSample(role) ? (
                        <Chip tone="green" icon={BadgeCheck}>Can reveal</Chip>
                      ) : (
                        <span className="inline-flex items-center gap-2 text-xs text-muted-foreground">
                          <span aria-hidden className="h-2.5 w-6 rounded-xs bg-foreground/80" />Hidden
                        </span>
                      )}
                    </TableCell>
                  ))}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      </Panel>
    </section>
  )
}

const FEATURED_OBJECTS: readonly FinanceType[] = ["client", "loan", "brokerage-account", "payment", "hardship-case", "regulatory-return"]

/** Calm workbench Home: one StatStrip, refusal table, ruled object directory, quiet data foundation bar. */
export function FinanceHome() {
  return (
    <section className="space-y-6">
      <PageHeader
        title="Home"
        meta="As of 22 Nov 2026"
        description="Lending, payments, and brokerage operations across 7 connected systems."
        primary={<Button asChild size="sm"><Link to="/objects/$type" params={{ type: "client" }}>Browse records</Link></Button>}
        secondary={[
          { label: "Model & aggregate graph", icon: Network, to: "/model" },
          { label: "Dataset mapping", icon: Database, to: "/examples/dataset" },
        ]}
      />

      <StatStrip>
        <Link to="/objects/$type" params={{ type: "client" }} className="block transition-colors hover:bg-muted/40">
          <KpiTile label="Business objects" value={Object.keys(TYPES).length} sub="11,872 clients resolved" />
        </Link>
        <Link to="/guarantees" hash="kyc" className="block transition-colors hover:bg-muted/40">
          <KpiTile label="Refused actions" value={<span className="text-destructive-text">{refusedCount}</span>} sub={`${RULES.length} active cross-object rules`} />
        </Link>
        <Link to="/metrics" hash="corrections" className="block transition-colors hover:bg-muted/40">
          <KpiTile label="Weekly reconciliation" value="97.50%" sub="1 correction this week (was 98.00%)" />
        </Link>
        <Link to="/workflows" className="block transition-colors hover:bg-muted/40">
          <KpiTile label="Lifecycles & automations" value="5" sub="3 state machines · 2 process managers" />
        </Link>
      </StatStrip>

      <Panel
        icon={ShieldX}
        label="Recent guardrail refusals"
        action={
          <Link to="/guarantees" className="inline-flex items-center gap-1 font-medium text-foreground hover:underline underline-offset-4">
            All {RULES.length} rules<ArrowRight className="size-3 text-muted-foreground" />
          </Link>
        }
      >
        <TableCard>
          <Table aria-label="Refused actions">
            <TableHeader>
              <TableRow>
                <TableHead>Blocked action</TableHead>
                <TableHead>Invariant</TableHead>
                <TableHead>Reason</TableHead>
                <TableHead className="text-end">Evidence</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {RULES.flatMap((rule) =>
                rule.refusal ? [
                  <TableRow key={rule.id}>
                    <TableCell className="font-medium text-foreground">{rule.refusal.action}</TableCell>
                    <TableCell className="text-muted-foreground">{rule.title}</TableCell>
                    <TableCell className="text-muted-foreground">{rule.refusal.reason}</TableCell>
                    <TableCell className="text-end">
                      <Link to="/guarantees" hash={rule.id} className="inline-flex items-center gap-1 font-medium text-foreground hover:underline underline-offset-4">
                        Inspect<ArrowRight className="size-3 text-muted-foreground" />
                      </Link>
                    </TableCell>
                  </TableRow>,
                ] : [],
              )}
            </TableBody>
          </Table>
        </TableCard>
      </Panel>

      <Panel
        icon={Layers}
        label="Core objects"
        action={
          <Link to="/model" className="inline-flex items-center gap-1 font-medium text-foreground hover:underline underline-offset-4">
            Aggregate boundaries<ArrowRight className="size-3 text-muted-foreground" />
          </Link>
        }
      >
        <TableCard>
          <Table aria-label="Core domain objects">
            <TableHeader>
              <TableRow>
                <TableHead>Object</TableHead>
                <TableHead>Role</TableHead>
                <TableHead className="text-end">Guarding rules</TableHead>
                <TableHead className="text-end">Sample records</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {FEATURED_OBJECTS.map((type) => {
                const aggRoot = AGGREGATES.some((a) => a.root === type)
                const recCount = RECORDS.filter((r) => r.type === type).length
                const ruleCount = RULES.filter((r) => r.reads.includes(type)).length
                return (
                  <TableRow key={type}>
                    <TableCell>
                      <Link to="/objects/$type" params={{ type }} className="inline-flex items-center gap-2 font-medium text-foreground hover:underline underline-offset-4">
                        <TypeTile type={type} />
                        <span>{TYPES[type].plural}</span>
                      </Link>
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {aggRoot ? <Badge variant="secondary" className="font-normal">Aggregate root</Badge> : "Entity"}
                    </TableCell>
                    <TableCell className="text-end text-muted-foreground">{ruleCount}</TableCell>
                    <TableCell className="text-end text-muted-foreground">{recCount}</TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </TableCard>
      </Panel>

      <footer className="flex flex-wrap items-center justify-between gap-3 border-t pt-4 text-meta text-muted-foreground">
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
          <span className="inline-flex items-center gap-1.5 font-medium text-foreground"><Database className="size-3.5 text-muted-foreground" />Data foundation</span>
          <span><strong className="font-medium text-foreground">{SYSTEMS.length}</strong> systems synced</span>
          <span><strong className="font-medium text-foreground">212</strong> quarantined</span>
          <Link to="/examples/$scene" params={{ scene: "dataset" }} hash="review" className="font-medium text-tone-amber hover:underline underline-offset-4">
            32 identity reviews pending
          </Link>
        </div>
        <Link to="/sources" className="inline-flex items-center gap-1 font-medium text-foreground hover:underline underline-offset-4">
          Open Builder sources<ArrowRight className="size-3.5 text-muted-foreground" />
        </Link>
      </footer>
    </section>
  )
}
