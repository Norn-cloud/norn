import { Link, useParams } from "@tanstack/react-router"
import { ArrowRight, BadgeCheck, CircleAlert, Database, Fingerprint, GitMerge, Shield, ShieldX } from "lucide-react"
import { Chip } from "@/components/chip"
import { Panel } from "@/components/panel"
import { cn } from "@/lib/utils"
import type { Workflow } from "../lib/workflows.ts"
import type { MenuEntry } from "../shell.tsx"
import { WorkflowCard } from "../workflows.tsx"
import { CLIENT, MARKS, ROW_COUNT, SYSTEMS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { FinanceHeading, Mark, TypeTile } from "./workspace.tsx"

type Systems = readonly [MarkId, MarkId, ...MarkId[]]
type ProcessStep = readonly [from: string, step: string, to: string, condition: string, systems: Systems, needs: string]

function SystemsNeeded({ systems, needs }: { readonly systems: Systems; readonly needs: string }) {
  return <div className="space-y-1.5">
    <div className="flex flex-wrap items-center gap-1.5" aria-label="Required systems">
      {systems.map(id => <span key={id} className="inline-flex items-center gap-1 rounded-full border pe-1.5 text-[10px] text-muted-foreground"><Mark id={id} className="size-5 rounded-full border-0 text-[7px]" />{MARKS[id].name}</span>)}
    </div>
    <p className="text-xs text-muted-foreground"><strong className="font-medium text-foreground">Needs</strong> · {needs}</p>
  </div>
}

function process(id: string, title: string, type: FinanceType, description: string, terminal: string[], steps: readonly [ProcessStep, ...ProcessStep[]]) {
  const transitions = steps.map(([from, id, to, description]) => ({ from: [from], id, to, description }))
  const workflow: Workflow = {
    objectType: TYPES[type].name,
    description,
    lifecycle: { property: id, initial: [steps[0][0]], terminal, transitions },
    steps: transitions.map(transition => ({ transition, actions: [] })),
    laws: [],
  }
  return { id, title, workflow, steps }
}

const PROCESSES = [
  process("origination", "Loan origination", "loan", "From a CRM lead to disbursement and regulatory reporting.", ["Reported to FRA"], [
    ["Applied", "Register CRM lead", "Lead matched", "An eligible CRM lead matches or creates one LMS application.", ["salesforce", "postgres"], "The CRM lead and the LMS application have to resolve to the same borrower."],
    ["Lead matched", "Verify e-KYC", "KYC verified", "Identity and liveness checks pass and the verification is current.", ["valify", "salesforce"], "Valify evidence has to agree with the lead identity held in CRM."],
    ["KYC verified", "Pull I-Score", "I-Score pulled", "The bureau inquiry is no more than 30 days old and matches the verified national ID.", ["iscore", "valify"], "The bureau report needs to match the identity verified by Valify."],
    ["I-Score pulled", "Underwrite in the LMS", "Approved", "I-Score is at least 600 or manual approval is recorded. Debt-to-income is at most 50%. Product terms and review capacity meet policy.", ["postgres", "iscore", "salesforce"], "Underwriting combines bureau evidence with application values and CRM risk flags."],
    ["Approved", "Disburse via InstaPay", "Disbursed", "LMS approval, current KYC, a fresh I-Score inquiry and no open CRM fraud or hardship flag.", ["instapay", "postgres", "valify", "iscore", "salesforce"], "Payment release depends on approval, verified identity, bureau freshness and CRM flags."],
    ["Disbursed", "File with FRA", "Reported to FRA", "The disbursed amounts appear in the reconciled monthly return.", ["fra", "postgres", "instapay"], "The return needs both the LMS disbursement and the confirmation from InstaPay."],
  ]),
  process("collections", "Repayment & collections", "installment", "Route arrears through hardship review and settlement before deciding the brokerage hold.", ["Brokerage held", "Brokerage released"], [
    ["Performing", "Read days past due", "Arrears identified", "An LMS installment is overdue after applying confirmed settlements.", ["postgres", "fawry", "instapay"], "A missed payment can only be confirmed after the bank settlement window is checked."],
    ["Arrears identified", "Check CRM hardship", "Hardship reviewed", "Check the borrower's CRM case. Open hardship pauses collections and late fees.", ["salesforce", "postgres"], "CRM owns the hardship case and the LMS controls dunning."],
    ["Hardship reviewed", "Issue Fawry payment link", "Payment link sent", "The installment is overdue and collections has not been paused for hardship.", ["fawry", "postgres", "salesforce"], "Fawry needs the LMS balance and CRM's clearance before sending a payment link."],
    ["Payment link sent", "Reconcile settlement", "Settlement checked", "A payment is matched within T+2, or an exception is opened.", ["fawry", "instapay", "postgres"], "Only a confirmed settlement matched to the installment can clear the balance."],
    ["Settlement checked", "Hold brokerage account", "Brokerage held", "The client remains at 30 or more days past due.", ["postgres", "egx", "mcdr"], "The arrears calculation is in the LMS and the hold applies to the brokerage account."],
    ["Settlement checked", "Release brokerage hold", "Brokerage released", "The matched payment clears the arrears threshold.", ["postgres", "egx", "mcdr"], "Brokerage needs the reconciled LMS position to release the hold."],
  ]),
  process("brokerage", "Brokerage onboarding", "brokerage-account", "Verify identity, confirm margin eligibility, open and fund the account, and assign its RM.", ["RM assigned"], [
    ["Application received", "Verify e-KYC", "KYC verified", "Valify verifies the applicant on the CRM onboarding case.", ["valify", "salesforce"], "The identity result has to attach to the applicant tracked by CRM."],
    ["KYC verified", "Check margin eligibility", "Margin eligibility checked", "A recent I-Score report matches the verified applicant and suitability is on file.", ["iscore", "valify", "egx"], "Brokerage suitability needs the Valify identity and matching bureau report."],
    ["Margin eligibility checked", "Open MCDR account", "Account opened at MCDR", "MCDR acknowledges the brokerage account for the verified applicant.", ["mcdr", "valify"], "MCDR opens the account using an identity verified upstream."],
    ["Account opened at MCDR", "Confirm InstaPay funding", "Funded", "InstaPay settles a deposit against the MCDR account number.", ["instapay", "mcdr"], "Payment settlement and account ownership come from different sources."],
    ["Funded", "Assign CRM relationship manager", "RM assigned", "CRM assigns an RM against the funded MCDR account holder.", ["salesforce", "instapay", "mcdr"], "CRM needs the actual funded account and its holder to record the assignment."],
  ]),
] as const

export const financeWorkflowMenu = (): readonly MenuEntry[] => PROCESSES.map(({ id, title }) => ({ href: `/workflows/${id}`, label: title }))

export function FinanceWorkflows() {
  const { process: id } = useParams({ strict: false })
  const selected = PROCESSES.find(item => item.id === id) ?? PROCESSES[0]
  return <section className="space-y-5">
    <FinanceHeading title="Workflows" description="Follow the states and conditions for lending, collections and brokerage onboarding." />
    <nav aria-label="Finance workflows" className="flex flex-wrap gap-1 rounded-xl border bg-card p-1">
      {PROCESSES.map(item => <Link key={item.id} to="/workflows/$process" params={{ process: item.id }}
        aria-current={item === selected ? "page" : undefined}
        className={cn("rounded-lg px-3 py-1.5 text-sm font-medium text-muted-foreground hover:bg-muted", item === selected && "bg-data-accent/10 text-foreground")}>
        {item.title}
      </Link>)}
    </nav>
    <WorkflowCard key={selected.id} workflow={selected.workflow} title={selected.title} illustrative annotateStep={(id, compact) => {
      const step = selected.steps.find(([, name]) => name === id)
      if (!step) return null
      const [, , , , systems, needs] = step
      return compact ? <span title={`Needs · ${needs}`} className="flex gap-0.5 ps-7 pt-1">{systems.map(system => <Mark key={system} id={system} className="size-5 rounded-md text-[7px]" />)}</span>
        : <SystemsNeeded systems={systems} needs={needs} />
    }} />
  </section>
}

interface Refusal {
  readonly action: string
  readonly reason: string
  readonly evidence: string
  readonly reference: string
  readonly href: string
}

type RuleObject = FinanceType | "lease" | "gl-entry"

const SOURCE_OBJECTS = {
  lease: { name: "Lease", tone: "teal" },
  "gl-entry": { name: "GL entry", tone: "violet" },
} as const

interface FinanceRule {
  readonly id: string
  readonly title: string
  readonly guards: string
  readonly reads: readonly [RuleObject, RuleObject, ...RuleObject[]]
  readonly path: string
  readonly condition: string
  readonly systems: Systems
  readonly needs: string
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
    refusal: {
      action: "Disbursement LN-1042-C refused", reason: "Valify KYC expired 3 Nov. LMS approved, I-Score fresh.",
      evidence: "Valify expiry 3 Nov 2026 · I-Score inquiry 22 Oct 2026 · LMS approved 4 Nov 2026 · CRM clearance recorded.",
      reference: "KYC-1042", href: "/sources/kyc-check/KYC-1042",
    },
  },
  {
    id: "exposure", title: "Total exposure per client ≤ EGP 5M",
    guards: "Loan.approve, Order.margin", reads: ["client", "loan", "brokerage-account", "lease"],
    path: "Client → holds → {Loan, Brokerage account, Lease} (sum)",
    systems: ["postgres", "egx", "mcdr", "leasing"],
    condition: "Sum outstanding loans, margin credit and leases by national ID. Refuse an increase above EGP 5,000,000.",
    needs: "Loans, margin positions and lease obligations are recorded separately and must be merged by national ID.",
  },
  {
    id: "settlement", title: "Match every settlement within T+2",
    guards: "Payment", reads: ["payment", "installment", "loan"],
    path: "Payment → settles → Installment ← has ← Loan",
    systems: ["fawry", "instapay", "postgres"],
    condition: "Match each Fawry or InstaPay settlement to an LMS installment within two business days, or open an exception.",
    needs: "Payment providers confirm settled amounts. The LMS provides the installment allocation and due date.",
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
    guards: "Order.margin, Brokerage account.withdraw", reads: ["brokerage-account", "client", "loan", "installment"],
    path: "Brokerage account ← holds ← Client → holds → Loan → has → Installment (DPD)",
    systems: ["postgres", "egx", "mcdr"],
    condition: "At 30 or more days past due in the LMS, block margin orders and brokerage cash withdrawals.",
    needs: "The outstanding loan is held by the LMS while the withdrawal or margin request arrives through brokerage.",
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
  },
  {
    id: "reporting", title: "FRA return equals source totals",
    guards: "Regulatory return.file", reads: ["regulatory-return", "loan", "brokerage-account", "gl-entry"],
    path: "Regulatory return → aggregate by period → {Loan, Brokerage account, GL entry}",
    systems: ["fra", "postgres", "egx", "mcdr", "ledger"],
    condition: "Reconcile the monthly FRA return to LMS outstanding, brokerage positions and the general ledger for the same period.",
    needs: "The filed return has to match separately maintained balances and posted journal entries.",
  },
]

const refusedCount = RULES.filter(rule => rule.refusal).length

export const financeRuleMenu = (): readonly MenuEntry[] => RULES.map(({ id, title }) => ({ href: `#${id}`, label: title }))

export function FinanceRules() {
  return <section className="space-y-4">
    <FinanceHeading title="Rules" description="Inspect finance checks and the evidence behind each recorded refusal in the synthetic sample." />
    <div className="flex flex-wrap gap-2">
      <Chip tone="green" icon={BadgeCheck} className="rounded-full">{RULES.length} checks passed in the sample</Chip>
      <Chip tone="red" icon={ShieldX} className="rounded-full">{refusedCount} actions refused</Chip>
    </div>
    <div className="grid gap-3 lg:grid-cols-2">
      {RULES.map(({ id, title, guards, reads, path, condition, systems, needs, refusal }) => <Panel key={id} id={id} label={title} icon={Shield} className="scroll-mt-4"
        action={<Chip tone="green" dot>Check passed</Chip>}>
        <div className="space-y-3 px-5 pt-2 pb-4">
          <dl className="space-y-2 border-b pb-3 text-xs">
            <div className="flex items-baseline gap-2"><dt className="w-10 shrink-0 text-muted-foreground">Guards</dt><dd><code>{guards}</code></dd></div>
            <div className="flex items-baseline gap-2"><dt className="w-10 shrink-0 text-muted-foreground">Reads</dt><dd className="flex flex-wrap gap-1">
              {reads.map(type => {
                const auxiliary = type === "lease" || type === "gl-entry"
                const item = auxiliary ? SOURCE_OBJECTS[type] : TYPES[type]
                return <Link key={type} to={auxiliary ? `/policies/${type}` : `/sources/${type}`} className="rounded-md hover:ring-1 hover:ring-border"><Chip tone={item.tone}>{item.name}</Chip></Link>
              })}
            </dd></div>
            <div className="flex items-baseline gap-2"><dt className="w-10 shrink-0 text-muted-foreground">Path</dt><dd className="font-mono leading-relaxed text-muted-foreground">{path}</dd></div>
          </dl>
          <p className="text-sm">{condition}</p>
          <SystemsNeeded systems={systems} needs={needs} />
          {id === "settlement" && <Link to="/metrics" hash="corrections" className="inline-flex items-center gap-1 text-xs font-medium text-tone-blue hover:underline">View settlement correction<ArrowRight className="size-3.5" /></Link>}
          {refusal ? <div className="space-y-2 rounded-xl border border-tone-red/25 bg-tone-red/5 p-3">
            <p className="text-xs font-semibold text-tone-red">{refusal.action}</p>
            <p className="text-sm">{refusal.reason}</p>
            <p className="text-xs text-muted-foreground">{refusal.evidence}</p>
            <Link to={refusal.href} className="inline-flex items-center gap-2 text-xs font-medium text-tone-blue hover:underline">Evidence · {refusal.reference}<ArrowRight className="size-3.5" /></Link>
          </div> : <p className="flex items-center gap-2 border-t pt-2 text-xs text-muted-foreground">
            No refusal recorded in this sample
          </p>}
        </div>
      </Panel>)}
    </div>
  </section>
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
  return <section className="space-y-4">
    <FinanceHeading title="Markings" description="See how personally identifiable information (PII) is displayed for each role in the synthetic document example." />
    <Panel label="Personally identifiable information" icon={Fingerprint} action={<Chip tone="amber" icon={Shield}>Restricted</Chip>}>
      <div className="overflow-x-auto px-5 py-3">
        <table className="w-full min-w-135 text-start text-sm">
          <thead className="text-xs text-muted-foreground"><tr>
            <th scope="col" className="pb-3 text-start font-medium">Marked field</th>
            {MARKING_ROLES.map(role => <th key={role} scope="col" className="w-[16%] pb-3 text-start font-medium">{role}</th>)}
          </tr></thead>
          <tbody>{MARKING_FIELDS.map(field => <tr key={field.field} className="border-t">
            <th scope="row" className="py-3 text-start font-medium">
              <div className="flex items-center gap-2"><TypeTile type={field.type} /><div>{field.label}<code className="block text-xs font-normal text-muted-foreground">{field.field}</code></div></div>
            </th>
            {MARKING_ROLES.map(role => <td key={role}>{canRevealSample(role)
              ? <Chip tone="green" icon={BadgeCheck}>Can reveal</Chip>
              : <span className="inline-flex items-center gap-2 text-xs text-muted-foreground"><span aria-hidden className="h-2.5 w-6 rounded-xs bg-black dark:ring-1 dark:ring-white/25" />Hidden</span>}</td>)}
          </tr>)}</tbody>
        </table>
      </div>
    </Panel>
    <Link to="/examples/$scene" params={{ scene: "document" }} className="inline-flex items-center gap-2 text-sm font-medium text-tone-blue hover:underline">
      Preview role views in Document mapping<ArrowRight className="size-4" />
    </Link>
  </section>
}

const OVERVIEW = [
  { count: SYSTEMS.length, label: "Systems synced", tone: "green", icon: Database, to: "/sources" },
  { count: 11_872, label: "Borrowers merged", tone: "blue", icon: GitMerge, to: "/sources/client" },
  { count: 32, label: "In review", tone: "amber", icon: CircleAlert, to: "/examples/dataset", hash: "review" },
  { count: 212, label: "Quarantined", tone: "amber", icon: Shield },
  { count: 1, label: "Correction this week", tone: "blue", icon: BadgeCheck, to: "/metrics", hash: "corrections" },
  { count: refusedCount, label: "Refused actions", tone: "red", icon: ShieldX, to: "/guarantees", hash: "kyc" },
] as const

export function FinanceHome() {
  return <section className="space-y-5">
    <FinanceHeading title="Home" description="Lending, payments and brokerage activity as of 22 November 2026." />
    <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
      {OVERVIEW.map(item => <Panel key={item.label} label={item.label} icon={item.icon}>
        <div className="flex items-end justify-between gap-2 px-5 pt-1 pb-4">
          <p className="text-3xl font-semibold tabular-nums" style={{ color: `var(--tone-${item.tone})` }}>{ROW_COUNT.format(item.count)}</p>
          {"to" in item && <Link to={item.to} hash={"hash" in item ? item.hash : undefined} aria-label={`View ${item.label.toLowerCase()}`}
            className="rounded-md p-1.5 text-tone-blue hover:bg-data-accent/10"><ArrowRight className="size-4" /></Link>}
        </div>
      </Panel>)}
    </div>
    <Panel label="Refused actions" icon={ShieldX}>
      <ul className="divide-y px-5 pb-3">
        {RULES.flatMap(({ id, refusal }) => refusal ? [<li key={id} className="py-2">
          <Link to="/guarantees" hash={id} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm hover:text-tone-blue">
            <strong className="font-medium">{refusal.action}</strong><span className="text-xs text-muted-foreground">{refusal.reason}</span><ArrowRight className="ms-auto size-4 text-tone-blue" />
          </Link>
        </li>] : [])}
      </ul>
    </Panel>
    <Panel label="Synced systems" icon={Database}>
      <ul className="grid divide-y px-5 pb-3 sm:grid-cols-2 sm:gap-x-6 sm:divide-none">
        {SYSTEMS.map(system => <li key={system.id} className="flex items-center justify-between gap-2 py-2">
          <div className="flex items-center gap-2">
            <div className="flex -space-x-1">{system.marks.map(mark => <Mark key={mark} id={mark} />)}</div>
            <span className="text-sm">{system.name}</span>
          </div>
          <span className="text-xs tabular-nums text-muted-foreground">{system.syncedAt} EET</span>
        </li>)}
      </ul>
    </Panel>
  </section>
}
