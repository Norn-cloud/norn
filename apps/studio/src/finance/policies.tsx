import { Link, useParams } from "@tanstack/react-router"
import { ArrowRight, CalendarDays, Check, ClipboardList, GitCommitHorizontal, Shield, SlidersHorizontal } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { Panel } from "@/components/panel"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import type { MenuEntry } from "../shell.tsx"
import { AUXILIARY_TYPES } from "./behavior.ts"
import { MARKS, ROW_COUNT, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { egp } from "./records.ts"
import { FinanceHeading, Mark } from "./workspace.tsx"

const POLICY_TYPES = {
  "loan-product": "Loan product",
  branch: "Branch",
  "credit-officer": "Credit officer",
  "collections-agent": "Collections agent",
  portfolio: "Portfolio",
  lease: "Lease",
  "gl-entry": "GL entry",
  "relationship-assignment": AUXILIARY_TYPES["relationship-assignment"].name,
  holdings: AUXILIARY_TYPES.holdings.name,
} as const

type PolicyType = FinanceType | keyof typeof POLICY_TYPES
type PolicyValue =
  | { readonly kind: "limit"; readonly number: number; readonly unit: "EGP" | "%" | "% per sector" | "months" | "approvals/day" | "files/day" | "calls/day" | "score"; readonly op: "≤" | "≥" }
  | { readonly kind: "range"; readonly low: number; readonly high: number; readonly unit: "%" }
  | { readonly kind: "time"; readonly time: string }
  | { readonly kind: "languages"; readonly names: readonly string[] }

type Consumer = { readonly label: string; readonly to: string; readonly hash?: string }
type BusinessPolicy = {
  readonly id: string
  readonly owner: PolicyType
  readonly name: string
  readonly value: PolicyValue
  readonly scope: string
  readonly source: MarkId
  readonly consumers: readonly Consumer[]
}

const underwriting = { label: "Loan underwriting", to: "/workflows/origination" }
const allocation = { label: "Collections allocation", to: "/optimisations/collections" }
const settlement = { label: "Settlement matching", to: "/guarantees", hash: "settlement" }
const reconciliation = { label: "Repayment reconciliation", to: "/metrics" }
const portfolioReport = { label: "Regulatory reconciliation", to: "/guarantees", hash: "reporting" }

const limit = (number: number, unit: Extract<PolicyValue, { kind: "limit" }>["unit"], op: "≤" | "≥" = "≤"): PolicyValue =>
  ({ kind: "limit", number, unit, op })

const POLICIES: readonly BusinessPolicy[] = [
  { id: "tenor", owner: "loan-product", name: "Max tenor", value: limit(60, "months"), scope: "Term loan product", source: "postgres", consumers: [underwriting] },
  { id: "rate-range", owner: "loan-product", name: "Rate range", value: { kind: "range", low: 22, high: 28, unit: "%" }, scope: "Term loan product", source: "postgres", consumers: [underwriting] },
  { id: "min-score", owner: "loan-product", name: "Min I-Score", value: limit(600, "score", "≥"), scope: "Term loan product", source: "norn", consumers: [underwriting] },
  { id: "max-dti", owner: "loan-product", name: "Max DTI", value: limit(50, "%"), scope: "Term loan product", source: "norn", consumers: [underwriting] },
  { id: "total-exposure", owner: "client", name: "Max total exposure", value: limit(5_000_000, "EGP"), scope: "Client type", source: "norn", consumers: [{ label: "Total exposure rule", to: "/guarantees", hash: "exposure" }] },
  { id: "branch-approvals", owner: "branch", name: "Max approvals per day", value: limit(40, "approvals/day"), scope: "Cairo branch", source: "postgres", consumers: [underwriting] },
  { id: "officer-capacity", owner: "credit-officer", name: "Review capacity", value: limit(25, "files/day"), scope: "Credit officer type", source: "salesforce", consumers: [underwriting] },
  { id: "agent-capacity", owner: "collections-agent", name: "Call capacity", value: limit(60, "calls/day"), scope: "Collections agent type", source: "salesforce", consumers: [allocation] },
  { id: "agent-languages", owner: "collections-agent", name: "Languages", value: { kind: "languages", names: ["Arabic", "English"] }, scope: "Bilingual collections desk", source: "salesforce", consumers: [allocation] },
  { id: "sector", owner: "portfolio", name: "Sector concentration", value: limit(20, "% per sector"), scope: "Consumer lending portfolio", source: "norn", consumers: [portfolioReport] },
  { id: "liquidity", owner: "portfolio", name: "Liquidity buffer", value: limit(50_000_000, "EGP", "≥"), scope: "Consumer lending portfolio", source: "ledger", consumers: [portfolioReport] },
  { id: "fawry-cutoff", owner: "payment", name: "Fawry settlement cut-off", value: { kind: "time", time: "16:00" }, scope: "Fawry payment rail", source: "fawry", consumers: [settlement, reconciliation, allocation] },
  { id: "instapay-limit", owner: "payment", name: "InstaPay per-transfer limit", value: limit(70_000, "EGP"), scope: "InstaPay payment rail", source: "instapay", consumers: [settlement] },
  { id: "margin-ratio", owner: "brokerage-account", name: "Margin ratio", value: limit(50, "%"), scope: "Brokerage account type", source: "egx", consumers: [{ label: "Margin decision", to: "/guarantees", hash: "margin" }] },
]

export const financePolicyMenu = (): readonly MenuEntry[] =>
  Object.entries(POLICY_TYPES).map(([id, label]) => ({ href: `/policies/${id}`, label, group: "Policy types", kind: "objectType" }))

function showValue(value: PolicyValue) {
  switch (value.kind) {
    case "limit": return `${value.op} ${value.unit === "EGP" ? egp(value.number) : `${ROW_COUNT.format(value.number)}${value.unit.startsWith("%") ? "" : " "}${value.unit}`}`
    case "range": return `${value.low}–${value.high}%`
    case "time": return `${value.time} EET`
    case "languages": return value.names.join(", ")
  }
}

function PolicyRow({ policy }: { readonly policy: BusinessPolicy }) {
  const [version, setVersion] = useState({ number: 1, value: policy.value, date: "1 Nov 2026" })
  const [editing, setEditing] = useState(false)
  const declared = policy.source === "norn" && version.value.kind === "limit" ? version.value : undefined
  return <article id={policy.id} className="scroll-mt-4 border-t py-3">
    <div className="flex flex-wrap items-start justify-between gap-2">
      <div className="space-y-1">
        <h3 className="text-sm font-semibold">{policy.name}</h3>
        <p className="text-xl font-semibold tabular-nums tracking-tight">{showValue(version.value)}</p>
      </div>
      {declared && <Button size="sm" variant="outline" aria-expanded={editing} onClick={() => setEditing(!editing)}>Edit policy</Button>}
    </div>
    <dl className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
      <div><dt className="sr-only">Scope</dt><dd>{policy.scope}</dd></div>
      <div><dt className="sr-only">Source</dt><dd className="flex items-center gap-1.5"><Mark id={policy.source} className="size-5 rounded-md" />{MARKS[policy.source].name}</dd></div>
      <div><dt className="sr-only">Effective date</dt><dd>Effective {version.date} · Open-ended</dd></div>
      <div><dt className="sr-only">Version</dt><dd><Chip mono>v{version.number}</Chip></dd></div>
    </dl>
    <div className="mt-2 flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
      <span className="me-1">Used by</span>
      {policy.consumers.map(({ to, hash, label }) => <Link key={label} to={to} hash={hash} className="rounded-md focus-visible:ring-2 focus-visible:ring-ring">
        <Chip tone="blue" icon={ArrowRight}>{label}</Chip>
      </Link>)}
    </div>
    {declared && editing && <form className="mt-3 space-y-2 rounded-lg border bg-surface-subtle p-3" onSubmit={event => {
      event.preventDefault()
      const entered = new FormData(event.currentTarget).get("value")
      if (typeof entered !== "string" || entered.trim() === "") return
      const number = Number(entered)
      if (!Number.isFinite(number) || number <= 0 || number === declared.number) return
      setVersion(previous => ({ number: previous.number + 1, value: { ...declared, number }, date: "22 Nov 2026" }))
      setEditing(false)
    }}>
      <label className="block text-xs font-medium">New value ({declared.unit})<Input name="value" type="number" min="1" required defaultValue={declared.number} className="mt-1 max-w-44" /></label>
      <p className="text-xs text-muted-foreground">Preview version v{version.number + 1} affects {policy.consumers.map(({ label }) => label).join(", ")}.</p>
      <Button size="sm" type="submit"><GitCommitHorizontal className="size-4" />Create preview version</Button>
    </form>}
    {version.number > 1 && !editing && <p role="status" className="mt-2 text-xs text-tone-green">Preview version v{version.number} created.</p>}
  </article>
}

export function BusinessPolicies({ type }: { readonly type: PolicyType }) {
  const policies = POLICIES.filter(policy => policy.owner === type)
  return <Panel label="Policies" icon={SlidersHorizontal} action={<Chip mono>{policies.length} policies</Chip>}>
    <div className="px-5 pb-1">
      {policies.length > 0 ? policies.map(policy => <PolicyRow key={policy.id} policy={policy} />)
        : <p className="pb-4 text-sm text-muted-foreground">This type inherits limits from linked policy types.</p>}
      {type === "loan" && <div className="flex flex-wrap gap-2 pb-3">
        {(["loan-product", "branch", "credit-officer", "portfolio"] as const).map(id =>
          <Link key={id} to="/policies/$scope" params={{ scope: id }}><Chip tone="blue" icon={ArrowRight}>{POLICY_TYPES[id]}</Chip></Link>)}
      </div>}
      {type === "hardship-case" && <Link to="/policies/$scope" params={{ scope: "collections-agent" }} className="mb-3 inline-flex"><Chip tone="blue" icon={ArrowRight}>Collections agent</Chip></Link>}
    </div>
  </Panel>
}

export function FinancePolicyType() {
  const { scope } = useParams({ strict: false })
  const type = (Object.keys(POLICY_TYPES) as (keyof typeof POLICY_TYPES)[]).find(key => key === scope) ?? "loan-product"
  const auxiliary = type === "relationship-assignment" || type === "holdings" ? AUXILIARY_TYPES[type] : undefined
  return <section className="space-y-4">
    <FinanceHeading title={POLICY_TYPES[type]} description={auxiliary ? `Entity in the ${TYPES[auxiliary.parent].name} aggregate.` : "Business limits and operating parameters used across the finance model."} />
    <nav aria-label="Policy types" className="flex flex-wrap gap-1.5">
      {financePolicyMenu().map(({ href, label }) => <Link key={href} to={href}><Chip tone={href.endsWith(type) ? "blue" : "gray"}>{label}</Chip></Link>)}
    </nav>
    {auxiliary ? <Panel label="Entity definition" icon={ClipboardList}>
      <div className="space-y-2 px-5 pt-1 pb-4">
        <div className="flex items-center gap-2 text-xs"><Mark id={auxiliary.source} />{MARKS[auxiliary.source].name}</div>
        <ul className="flex flex-wrap gap-1.5">{auxiliary.fields.map(field => <li key={field}><Chip mono>{field}</Chip></li>)}</ul>
        <Link to="/sources/$concept" params={{ concept: auxiliary.parent }} search={{ tab: "links" }} className="inline-flex items-center gap-1 text-sm text-tone-blue hover:underline">Open aggregate links<ArrowRight className="size-4" /></Link>
      </div>
    </Panel> : type === "lease" || type === "gl-entry" ? <Panel label="Source object definition" icon={ClipboardList}>
      <div className="space-y-2 px-5 pt-1 pb-4">
        <div className="flex items-center gap-2 text-xs"><Mark id={type === "lease" ? "leasing" : "ledger"} />{type === "lease" ? "Leasing ledger" : "General ledger"}</div>
        <ul className="flex flex-wrap gap-1.5">{SOURCE_FIELDS[type].map(field => <li key={field}><Chip mono>{field}</Chip></li>)}</ul>
        <Link to="/guarantees" hash={type === "lease" ? "exposure" : "reporting"} className="inline-flex items-center gap-1 text-sm text-tone-blue hover:underline">View the rule that reads this source<ArrowRight className="size-4" /></Link>
      </div>
    </Panel> : <BusinessPolicies type={type} />}
  </section>
}

const SOURCE_FIELDS = {
  lease: ["Lease reference", "Client national ID", "Outstanding principal (EGP)", "As-of date"],
  "gl-entry": ["Journal reference", "Posting period", "Debit (EGP)", "Credit (EGP)"],
} as const

const CONSTRAINTS = [
  { name: "60 calls per agent per day", reason: "Each assignment consumes one unit of daily capacity.", to: "/policies/collections-agent", hash: "agent-capacity", binding: true },
  { name: "Match agent and borrower languages", reason: "Assign only an agent who speaks the borrower's preferred language.", to: "/policies/collections-agent", hash: "agent-languages", binding: true },
  { name: "Pause collections for open hardship", reason: "Exclude accounts with an open CRM hardship case.", to: "/guarantees", hash: "hardship", binding: true },
  { name: "Fawry cut-off at 16:00 EET", reason: "Move promises received after the cut-off to the next settlement day.", to: "/sources/payment", search: { tab: "policies" }, hash: "fawry-cutoff", binding: false },
] as const

const ASSIGNMENTS = [
  { day: "23 Nov", agent: "Dina Helmy", queue: "Arabic · early arrears", count: 60, recovery: 1_180_000 },
  { day: "23 Nov", agent: "Rana Osman", queue: "English · early arrears", count: 42, recovery: 610_000 },
  { day: "24 Nov", agent: "Dina Helmy", queue: "Arabic · promises due", count: 58, recovery: 950_000 },
  { day: "25 Nov", agent: "Rana Osman", queue: "English · promises due", count: 60, recovery: 740_000 },
  { day: "26–29 Nov", agent: "Bilingual desk", queue: "Mixed · follow-up", count: 56, recovery: 720_000 },
] as const

export function FinanceOptimisation() {
  const [applied, setApplied] = useState(false)
  return <section className="space-y-4">
    <FinanceHeading title="Collections allocation (next 7 days)" description="23–29 November 2026 · Synthetic optimisation preview" />
    <div className="grid gap-3 md:grid-cols-2">
      <Panel label="Model" icon={SlidersHorizontal}>
        <dl className="space-y-2 px-5 pt-1 pb-4 text-sm">
          <dt className="text-xs text-muted-foreground">Variables</dt><dd><code>x[account, agent, day]</code> · assign one account to an agent on a given day</dd>
          <dt className="text-xs text-muted-foreground">Objective</dt><dd>Maximise expected recovery in EGP.</dd>
        </dl>
      </Panel>
      <Panel label="Expected recovery" icon={CalendarDays}>
        <div className="px-5 pt-1 pb-4">
          <p className="text-2xl font-semibold text-tone-green">{egp(ASSIGNMENTS.reduce((sum, row) => sum + row.recovery, 0))}</p>
          <p className="mt-1 text-sm text-muted-foreground">{egp(3_100_000)} under current round-robin</p>
        </div>
      </Panel>
    </div>
    <Panel label="Constraints" icon={Shield}>
      <ul className="divide-y px-5 pb-2">{CONSTRAINTS.map(constraint => { const { name, reason, to, hash, binding } = constraint; return <li key={name} className="flex flex-wrap items-start justify-between gap-2 py-2">
        <div><Link to={to} hash={hash} search={"search" in constraint ? constraint.search : undefined} className="flex items-center gap-1 text-sm font-medium text-tone-blue hover:underline">{name}<ArrowRight className="size-3.5" /></Link>
          <p className="text-xs text-muted-foreground">{reason}</p></div>
        <Chip tone={binding ? "amber" : "gray"}>{binding ? "Binding" : "Within limit"}</Chip>
      </li>})}</ul>
    </Panel>
    <Panel label="Solved plan" icon={ClipboardList}>
      <div className="overflow-x-auto px-5 pb-3">
        <table className="w-full min-w-128 text-start text-sm">
          <thead className="text-xs text-muted-foreground"><tr>{["Day", "Agent", "Queue", "Accounts", "Expected recovery"].map(label => <th key={label} scope="col" className="py-2 text-start font-medium">{label}</th>)}</tr></thead>
          <tbody>{ASSIGNMENTS.map(({ day, agent, queue, count, recovery }) => <tr key={day + agent} className="border-t">
            <td className="py-2">{day}</td><td>{agent}</td><td>{queue}</td><td>{count}</td><td className="tabular-nums">{egp(recovery)}</td>
          </tr>)}</tbody>
        </table>
      </div>
    </Panel>
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-card p-3">
      <div className="flex flex-wrap items-center gap-2 text-sm"><Chip tone="green" icon={Check}>OK</Chip>Checked before write-back · Plan re-verified against current snapshot v412</div>
      <Button type="button" onClick={() => setApplied(true)} disabled={applied}>{applied ? "Applied to sample" : "Apply"}</Button>
    </div>
    {applied && <p role="status" className="fixed inset-e-5 bottom-5 z-50 flex gap-2 rounded-xl border bg-card p-3 text-sm shadow-lg"><Check className="size-4 text-tone-green" />Collection assignments applied to the synthetic preview.</p>}
  </section>
}
