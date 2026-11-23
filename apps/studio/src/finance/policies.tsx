import { Link, useParams } from "@tanstack/react-router"
import { ArrowRight, Check, ClipboardList, GitCommitHorizontal, Shield, SlidersHorizontal } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { cn } from "@/lib/utils"
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
  return (
    <article id={policy.id} className="scroll-mt-4 border-t border-border px-4 py-3 first:border-t-0">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <h3 className="text-xs font-semibold text-foreground">{policy.name}</h3>
          <p className="text-base font-semibold tabular-nums text-foreground">{showValue(version.value)}</p>
          <code className="rounded-xs bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">v{version.number}</code>
        </div>
        {declared && <Button size="sm" variant="outline" className="h-7 text-xs" aria-expanded={editing} onClick={() => setEditing(!editing)}>Edit policy</Button>}
      </div>
      <div className="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-muted-foreground">
        <span>{policy.scope}</span>
        <span>Effective {version.date}</span>
        <span className="inline-flex items-center gap-1">
          Used by:
          {policy.consumers.map(({ to, hash, label }) => (
            <Link key={label} to={to} hash={hash} className="font-medium text-tone-blue hover:underline">{label}</Link>
          ))}
        </span>
      </div>
      {declared && editing && (
        <form
          className="mt-3 flex flex-wrap items-end gap-3 rounded-md border border-border bg-surface-subtle p-3"
          onSubmit={(event) => {
            event.preventDefault()
            const entered = new FormData(event.currentTarget).get("value")
            if (typeof entered !== "string" || entered.trim() === "") return
            const number = Number(entered)
            if (!Number.isFinite(number) || number <= 0 || number === declared.number) return
            setVersion((previous) => ({ number: previous.number + 1, value: { ...declared, number }, date: "22 Nov 2026" }))
            setEditing(false)
          }}
        >
          <label className="block text-xs font-medium">
            New value ({declared.unit})
            <Input name="value" type="number" min="1" required defaultValue={declared.number} className="mt-1 h-8 w-40 bg-background font-mono text-xs" />
          </label>
          <Button size="sm" type="submit" className="h-8 text-xs"><GitCommitHorizontal className="size-3.5" />Create preview v{version.number + 1}</Button>
          <p className="w-full text-[11px] text-muted-foreground">Affects {policy.consumers.map(({ label }) => label).join(", ")}.</p>
        </form>
      )}
      {version.number > 1 && !editing && <p role="status" className="mt-2 text-xs text-tone-green">Preview version v{version.number} created.</p>}
    </article>
  )
}

export function BusinessPolicies({ type }: { readonly type: PolicyType }) {
  const policies = POLICIES.filter((policy) => policy.owner === type)
  return (
    <section className="overflow-hidden rounded-lg border border-border bg-background">
      <header className="flex items-center justify-between border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
        <span className="inline-flex items-center gap-1.5"><SlidersHorizontal className="size-3.5 text-muted-foreground" />Business policies</span>
        <span className="font-mono text-[11px] font-normal text-muted-foreground">{policies.length} policies</span>
      </header>
      <div>
        {policies.length > 0 ? (
          policies.map((policy) => <PolicyRow key={policy.id} policy={policy} />)
        ) : (
          <p className="p-4 text-xs text-muted-foreground">This type inherits limits from linked policy types.</p>
        )}
        {type === "loan" && (
          <div className="flex flex-wrap items-center gap-2 border-t border-border bg-surface-subtle/50 px-4 py-2.5 text-xs">
            <span className="text-muted-foreground">Linked policy scopes:</span>
            {(["loan-product", "branch", "credit-officer", "portfolio"] as const).map((id) => (
              <Link key={id} to="/policies/$scope" params={{ scope: id }} className="inline-flex items-center gap-1 rounded-sm border border-border bg-background px-2 py-0.5 font-medium text-tone-blue hover:border-data-accent">
                {POLICY_TYPES[id]}<ArrowRight className="size-3" />
              </Link>
            ))}
          </div>
        )}
        {type === "hardship-case" && (
          <div className="border-t border-border bg-surface-subtle/50 px-4 py-2.5 text-xs">
            <Link to="/policies/$scope" params={{ scope: "collections-agent" }} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">
              Collections agent policies<ArrowRight className="size-3" />
            </Link>
          </div>
        )}
      </div>
    </section>
  )
}

export function FinancePolicyType() {
  const { scope } = useParams({ strict: false })
  const type = (Object.keys(POLICY_TYPES) as (keyof typeof POLICY_TYPES)[]).find((key) => key === scope) ?? "loan-product"
  const auxiliary = type === "relationship-assignment" || type === "holdings" ? AUXILIARY_TYPES[type] : undefined
  return (
    <section className="space-y-4">
      <FinanceHeading
        title={POLICY_TYPES[type]}
        description={auxiliary ? `Entity in the ${TYPES[auxiliary.parent].name} aggregate.` : "Business limits and operating parameters editable without code."}
      />
      <nav aria-label="Policy types" className="flex flex-wrap gap-1 border-b border-border">
        {financePolicyMenu().map(({ href, label }) => (
          <Link
            key={href}
            to={href}
            className={cn(
              "border-b-2 border-transparent px-2.5 py-2 text-xs font-medium text-muted-foreground hover:text-foreground",
              href.endsWith(type) && "border-data-accent text-foreground",
            )}
          >
            {label}
          </Link>
        ))}
      </nav>
      {auxiliary ? (
        <section className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">Entity definition</header>
          <div className="space-y-3 p-4 text-xs">
            <div className="flex items-center gap-2"><Mark id={auxiliary.source} />{MARKS[auxiliary.source].name}</div>
            <ul className="flex flex-wrap gap-1.5">{auxiliary.fields.map((field) => <li key={field}><code className="rounded-sm border border-border bg-surface-subtle px-2 py-0.5 text-[11px]">{field}</code></li>)}</ul>
            <Link to="/sources/$concept" params={{ concept: auxiliary.parent }} search={{ tab: "links" }} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">
              Open aggregate links<ArrowRight className="size-3.5" />
            </Link>
          </div>
        </section>
      ) : type === "lease" || type === "gl-entry" ? (
        <section className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">Source object definition</header>
          <div className="space-y-3 p-4 text-xs">
            <div className="flex items-center gap-2"><Mark id={type === "lease" ? "leasing" : "ledger"} />{type === "lease" ? "Leasing ledger" : "General ledger"}</div>
            <ul className="flex flex-wrap gap-1.5">{SOURCE_FIELDS[type].map((field) => <li key={field}><code className="rounded-sm border border-border bg-surface-subtle px-2 py-0.5 text-[11px]">{field}</code></li>)}</ul>
            <Link to="/guarantees" hash={type === "lease" ? "exposure" : "reporting"} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">
              View guarding invariant<ArrowRight className="size-3.5" />
            </Link>
          </div>
        </section>
      ) : (
        <BusinessPolicies type={type} />
      )}
    </section>
  )
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
  const expectedTotal = ASSIGNMENTS.reduce((sum, row) => sum + row.recovery, 0)
  return (
    <section className="space-y-4">
      <FinanceHeading
        title="Collections allocation (next 7 days)"
        description="23–29 November 2026 · Constraint-checked optimisation preview against current policy limits."
        actions={
          <Button type="button" size="sm" className="h-8 text-xs" onClick={() => setApplied(true)} disabled={applied}>
            {applied ? "Applied to preview" : "Apply plan"}
          </Button>
        }
      />

      <div className="grid grid-cols-1 divide-y divide-border overflow-hidden rounded-lg border border-border bg-background md:grid-cols-3 md:divide-x md:divide-y-0">
        <div className="px-4 py-3">
          <p className="text-[11px] text-muted-foreground">Expected recovery</p>
          <p className="mt-1 text-2xl font-semibold tabular-nums text-tone-green">{egp(expectedTotal)}</p>
          <p className="mt-0.5 text-[11px] text-muted-foreground">vs. {egp(3_100_000)} under round-robin</p>
        </div>
        <div className="px-4 py-3">
          <p className="text-[11px] text-muted-foreground">Objective &amp; variables</p>
          <p className="mt-1 text-xs font-medium text-foreground">Maximise expected recovery in EGP</p>
          <code className="mt-0.5 block text-[11px] text-muted-foreground">x[account, agent, day]</code>
        </div>
        <div className="px-4 py-3">
          <p className="text-[11px] text-muted-foreground">Pre-write verification</p>
          <p className="mt-1 inline-flex items-center gap-1.5 text-xs font-medium text-foreground"><Chip tone="green" icon={Check}>Verified</Chip>Snapshot v412</p>
          <p className="mt-0.5 text-[11px] text-muted-foreground">All 4 policy &amp; invariant constraints checked</p>
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_22rem]">
        <section className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="flex items-center gap-2 border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
            <ClipboardList className="size-3.5 text-muted-foreground" />Solved assignment plan
          </header>
          <div className="overflow-x-auto">
            <table className="w-full min-w-120 text-start text-xs">
              <thead>
                <tr className="border-b border-border bg-surface-subtle/40 text-muted-foreground">
                  {["Day", "Agent", "Queue", "Accounts", "Expected recovery"].map((label) => (
                    <th key={label} scope="col" className="px-4 py-2 text-start font-medium">{label}</th>
                  ))}
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {ASSIGNMENTS.map(({ day, agent, queue, count, recovery }) => (
                  <tr key={day + agent} className="hover:bg-surface-subtle/60">
                    <td className="px-4 py-2.5 text-muted-foreground">{day}</td>
                    <td className="px-4 py-2.5 font-medium text-foreground">{agent}</td>
                    <td className="px-4 py-2.5 text-muted-foreground">{queue}</td>
                    <td className="px-4 py-2.5 tabular-nums">{count}</td>
                    <td className="px-4 py-2.5 font-medium tabular-nums text-foreground">{egp(recovery)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>

        <section className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="flex items-center gap-2 border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
            <Shield className="size-3.5 text-muted-foreground" />Active constraints
          </header>
          <ul className="divide-y divide-border px-4">
            {CONSTRAINTS.map((constraint) => {
              const { name, reason, to, hash, binding } = constraint
              return (
                <li key={name} className="space-y-1 py-2.5 text-xs">
                  <div className="flex items-start justify-between gap-2">
                    <Link to={to} hash={hash} search={"search" in constraint ? constraint.search : undefined} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">
                      {name}<ArrowRight className="size-3" />
                    </Link>
                    <Chip tone={binding ? "amber" : "gray"}>{binding ? "Binding" : "Slack"}</Chip>
                  </div>
                  <p className="text-[11px] text-muted-foreground">{reason}</p>
                </li>
              )
            })}
          </ul>
        </section>
      </div>

      {applied && (
        <p role="status" className="fixed inset-e-5 bottom-5 z-50 flex items-center gap-2 rounded-md border border-border bg-background px-3.5 py-2.5 text-xs font-medium shadow-md">
          <Check className="size-4 text-tone-green" />Collection assignments applied to the synthetic preview.
        </p>
      )}
    </section>
  )
}
