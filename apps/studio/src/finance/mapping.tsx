import { Link, useLocation } from "@tanstack/react-router"
import { ArrowRight, Check, GitMerge, KeyRound, Shield, Sparkles } from "lucide-react"
import { useState, type DragEvent } from "react"
import { Chip } from "@/components/chip"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { CLIENT, MARKS, SYSTEMS, type MarkId } from "./data.ts"
import { egp } from "./records.ts"
import { Mark } from "./workspace.tsx"

const counted = (count: number, singular: string, plural = `${singular}s`) => `${count} ${count === 1 ? singular : plural}`

type MappingStatus = "Mapped" | "Key" | "Partial" | "Conflict" | "Suggested" | "Unmapped" | "Ignored"
type Field = {
  readonly source: MarkId
  readonly path: string
  readonly target: string
  readonly format: string
  readonly status: MappingStatus
  readonly sample: string
  readonly note?: string
}

function field(source: MarkId, path: string, target: string, format: string, status: MappingStatus, sample: string, note?: string): Field {
  return { source, path, target, format, status, sample, ...(note ? { note } : {}) }
}

const MODEL = [
  { title: "Borrower", required: "National ID and client name", extra: ["Client.employer", "Client.monthlyIncome"], rows: [
    field("postgres", "lms.loans.national_id", "Client.nationalId", "text", "Key", CLIENT.nationalId),
    field("iscore", "$.subject.national_id", "Client.nationalId", "text", "Key", CLIENT.nationalId),
    field("salesforce", "Client__c.National_Id__c", "Client.nationalId", "text", "Key", CLIENT.nationalId),
    field("salesforce", "Client__c.cust_nm", "Client.name", "text", "Partial", CLIENT.name, "Splits names into first and last for 94% of records. Arabic-only entries without Latin spelling go to review."),
    field("salesforce", "Client__c.Phone", "Client.phone", "text", "Partial", CLIENT.phone, "Normalises valid numbers to +20. 87% are valid and 13% are held back."),
    field("iscore", "$.subject.birth_date", "Client.birthDate", "date", "Partial", CLIENT.birth, "Present for 71% of reports. A further 2% of reports have a date that conflicts with KYC."),
    field("fawry", "$.collection.payer_ref", "Client.payerRef", "text", "Partial", CLIENT.payerRef, "96% of payer references resolve to a borrower. Third-party payers go to review."),
    field("postgres", "lms.borrowers.address", "Client.address", "text", "Conflict", CLIENT.address, "KYC takes precedence over CRM, then LMS. 128 addresses require review."),
    field("salesforce", "Client__c.Address", "Client.address", "text", "Conflict", "10 Sample Avenue, Cairo", "KYC takes precedence over CRM, then LMS. 128 addresses require review."),
    field("valify", "$.checks.identity.address", "Client.address", "text", "Mapped", CLIENT.address),
    field("salesforce", "Client__c.Lead_Source__c", "", "text", "Unmapped", "Referral"),
    field("postgres", "lms.borrowers.legacy_branch_code", "", "text", "Unmapped", "CAI-01"),
  ] },
  { title: "Loan", required: "Loan reference, client key and principal", extra: ["Loan.productCode"], rows: [
    field("postgres", "lms.loans.loan_id", "Loan.reference", "text", "Key", "LN-1042-A"),
    field("postgres", "lms.loans.client_id", "Loan.clientId", "reference", "Mapped", "LMS-1042"),
    field("postgres", "lms.loans.principal_egp", "Loan.principal", "money · EGP", "Mapped", egp(1_250_000)),
    field("postgres", "lms.loans.installment_egp", "Loan.installmentAmount", "money · EGP", "Mapped", egp(CLIENT.regularPayment)),
    field("fra", "$.return.reporting_period", "RegulatoryReturn.period", "month", "Mapped", "Nov 2026", "The regulatory return is a linked reporting record."),
  ] },
  { title: "Payment", required: "Payment reference, amount and settlement date", extra: ["Payment.channel"], rows: [
    field("fawry", "$.collection.payment_ref", "Payment.reference", "text", "Key", "FWR-1042-A"),
    field("fawry", "$.settlement.amount_egp", "Payment.amount", "money · EGP", "Mapped", egp(CLIENT.regularPayment)),
    field("fawry", "$.settlement.posted_on", "Payment.settledOn", "date", "Mapped", "20 Nov 2026"),
    field("fawry", "$.collection.terminal_id", "", "text", "Unmapped", "KIOSK-017"),
    field("instapay", "$.deposit.reference", "FundingTransaction.reference", "text", "Mapped", CLIENT.depositRef),
    field("egx", "$.oms.execution.order_id", "Order.executionReference", "text", "Mapped", CLIENT.brokerOrder, "The order management system confirms execution against the brokerage account."),
  ] },
  { title: "Credit report", required: "Report reference, national ID and issued date", extra: [], rows: [
    field("iscore", "$.report.reference", "CreditReport.reference", "text", "Key", CLIENT.report),
    field("iscore", "$.subject.national_id", "CreditReport.clientId", "reference", "Mapped", CLIENT.nationalId),
    field("iscore", "$.subject.score", "CreditReport.score", "integer · 400–850", "Mapped", "724"),
    field("iscore", "$.report.issued_on", "CreditReport.issuedOn", "date", "Mapped", "22 Nov 2026"),
    field("model", "$.inference.probability_of_default", "CreditRiskAssessment.probabilityOfDefault", "decimal · 0–1", "Suggested", "0.047", "Map the risk-model output to its assessment record. Preserve the bureau report separately."),
  ] },
] as const

const CASES = [
  { id: "REV-018", label: "Arabic and Latin spellings", confidence: "94%", fields: [
    ["Name", "محمد علي", "Mohamed Ali / Mohammed Ali"], ["National ID", "DEMO-NID-1182", "DEMO-NID-1182"],
  ] },
  { id: "REV-022", label: "National ID differs by one digit", confidence: "84%", fields: [
    ["Name", "Hana Kamal", "Hana Kamal"], ["National ID", "DEMO-NID-2137", "DEMO-NID-2131"],
  ] },
  { id: "REV-027", label: "Shared phone number", confidence: "67%", fields: [
    ["Name", "Omar Fares", "Mustafa Faris"], ["Phone", "+20 1X XXX X0027", "+20 1X XXX X0027"],
  ] },
  { id: "REV-032", label: "Duplicate CRM contact", confidence: "98%", fields: [
    ["CRM contact", "CRM-34018", "CRM-35672"], ["Name", "Leila Amin", "Leila Amin"], ["National ID", "DEMO-NID-1842", "DEMO-NID-1842"],
  ] },
] as const

type ReviewDecision = "Merged" | "Kept separate"

function IdentityReview({ decisions, decide }: {
  readonly decisions: Readonly<Record<string, ReviewDecision>>
  readonly decide: (id: string, decision: ReviewDecision) => void
}) {
  const pending = 32 - Object.keys(decisions).length
  return <section id="review" className="space-y-3">
    <header className="flex flex-wrap items-start justify-between gap-2">
      <div><h2 className="text-section font-semibold">Borrower identity review</h2><p className="text-xs text-muted-foreground">{pending} need review · Four cases from the synthetic queue</p></div>
      <Link to="/examples/$scene" params={{ scene: "dataset" }} className="text-sm font-medium text-tone-blue hover:underline">Back to mappings</Link>
    </header>
    {CASES.map(({ id, label, confidence, fields }) => <article key={id} className="space-y-3 rounded-xl border bg-card p-4">
      <div className="flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-semibold">{label} <span className="ms-1 font-mono text-xs font-normal text-muted-foreground">{id}</span></h3>
        <Chip tone="amber">{confidence} confidence</Chip></div>
      <table aria-label={label} className="w-full table-fixed text-sm">
        <thead className="text-xs text-muted-foreground"><tr><th scope="col" className="pb-1 text-start">Field</th><th scope="col" className="pb-1 text-start">Source record A</th><th scope="col" className="pb-1 text-start">Source record B</th></tr></thead>
        <tbody>{fields.map(([label, a, b]) => <tr key={label} className="border-t">
          <th scope="row" className="py-1.5 text-start font-medium">{label}</th>
          <td className={cn("overflow-wrap-anywhere pe-2 text-start", a !== b && "text-tone-amber")}><bdi>{a}</bdi></td>
          <td className={cn("overflow-wrap-anywhere text-start", a !== b && "text-tone-amber")}><bdi>{b}</bdi></td>
        </tr>)}</tbody>
      </table>
      <div className="flex flex-wrap gap-1.5">
        {decisions[id] ? <Chip tone="green" icon={Check}>{decisions[id]} in preview</Chip> : <>
          <Button size="sm" onClick={() => decide(id, "Merged")}>Merge</Button>
          <Button size="sm" variant="outline" onClick={() => decide(id, "Kept separate")}>Keep separate</Button>
        </>}
      </div>
    </article>)}
  </section>
}

const TONES = { Mapped: "blue", Key: "green", Partial: "amber", Conflict: "red", Suggested: "violet", Unmapped: "gray", Ignored: "gray" } as const

export function DatasetMapping() {
  const review = useLocation({ select: location => location.hash === "review" })
  const [model, setModel] = useState<(typeof MODEL)[number]>(MODEL[0])
  const [selected, setSelected] = useState<Field>()
  const [overrides, setOverrides] = useState<Readonly<Record<string, Field>>>({})
  const [decisions, setDecisions] = useState<Readonly<Record<string, ReviewDecision>>>({})
  const [publish, setPublish] = useState(false)
  const rowKey = (path: string) => `${model.title}:${path}`
  const rows = model.rows.map(row => overrides[rowKey(row.path)] ?? row)
  const properties = [...new Set([...rows.filter(row => row.target).map(row => row.target), ...model.extra])]
  const wired = properties.filter(target => rows.some(row => row.target === target && row.status !== "Suggested" && row.status !== "Ignored"))
  const partial = rows.filter(row => row.status === "Partial").length
  const conflicts = new Set(rows.filter(row => row.status === "Conflict").map(row => row.target)).size
  const unresolved = properties.length - wired.length
  const update = (row: Field, change: Partial<Pick<Field, "status" | "target">>) => {
    const next = { ...row, ...change }
    setOverrides(current => ({ ...current, [rowKey(row.path)]: next }))
    setSelected(next)
    setPublish(false)
  }
  const drop = (event: DragEvent, target: string) => {
    event.preventDefault()
    const row = rows.find(item => item.path === event.dataTransfer.getData("text/plain"))
    if (row) update(row, { target, status: "Mapped" })
  }
  if (review) return <IdentityReview decisions={decisions} decide={(id, decision) => setDecisions(current => ({ ...current, [id]: decision }))} />

  return <section className="grid gap-3 lg:grid-cols-[14rem_minmax(0,1fr)]">
    <aside className="space-y-2 rounded-2xl border bg-card p-3.5">
      <h2 className="text-sm font-semibold">Source schemas</h2>
      {SYSTEMS.map(({ id, name, marks, schema }) => <div key={id} className="flex items-center gap-2 rounded-lg bg-muted/45 p-1.5">
        <div className="flex -space-x-1">{marks.map(mark => <Mark key={mark} id={mark} className="size-7" />)}</div>
        <div className="min-w-0"><div className="text-xs font-semibold">{name}</div><code className="text-[10px] text-muted-foreground">{schema}</code></div>
      </div>)}
      <div className="flex items-center gap-2 rounded-lg bg-muted/45 p-1.5"><Mark id="model" className="size-7" /><div><p className="text-xs font-semibold">MLflow model output</p><code className="text-[10px] text-muted-foreground">risk.inference.v2</code></div></div>
    </aside>
    <section className="min-w-0 overflow-hidden rounded-2xl border bg-card">
      <header className="space-y-2 border-b bg-surface-subtle p-3.5">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2"><h2 className="text-sm font-semibold">Field mappings</h2><Chip tone="blue" icon={Shield}>Data contract</Chip></div>
          <Link to="/examples/$scene" params={{ scene: "dataset" }} hash="review"><Chip tone="amber" icon={ArrowRight}>{32 - Object.keys(decisions).length} need review</Chip></Link>
        </div>
        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground"><KeyRound className="size-3.5 text-tone-blue" />National ID · <code>{CLIENT.nationalId}</code> · {CLIENT.name} · {CLIENT.account}
          <Chip tone="blue" icon={GitMerge}>11,872 merged</Chip>
        </div>
        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground"><span>Required for {model.title.toLowerCase()} · {model.required}</span><Chip tone="amber">Quarantine on failure</Chip></div>
        <div role="group" aria-label="Mapping object type" className="flex flex-wrap gap-1">
          {MODEL.map(item => <Button key={item.title} size="sm" variant={item === model ? "outline" : "ghost"} aria-pressed={item === model} onClick={() => { setModel(item); setSelected(undefined); setPublish(false) }}>{item.title}</Button>)}
        </div>
        <div className="space-y-1">
          <div className="h-1.5 overflow-hidden rounded-full bg-muted"><div className="h-full rounded-full bg-data-accent" style={{ width: `${100 * wired.length / properties.length}%` }} /></div>
          <p className="text-xs text-muted-foreground">{wired.length} of {properties.length} properties mapped · {counted(partial, "partial mapping")} · {counted(conflicts, "conflict")}</p>
        </div>
      </header>
      <div className="space-y-1.5 p-3">
        {rows.filter(row => row.status !== "Ignored").map(row => <div key={row.path} className="grid min-w-0 grid-cols-[minmax(0,1fr)_2rem_minmax(0,1fr)] items-center gap-1">
          <button type="button" draggable aria-pressed={selected?.path === row.path} onClick={() => setSelected(row)}
            onDragStart={event => event.dataTransfer.setData("text/plain", row.path)}
            className="flex min-h-10 min-w-0 cursor-grab items-center gap-1.5 rounded-lg border bg-surface-subtle px-2 text-start hover:border-data-accent">
            <Mark id={row.source} className="size-5 rounded-sm text-[7px]" /><code className="truncate text-[10px]" title={row.path}>{row.path}</code>
          </button>
          <button type="button" aria-label={`Inspect mapping for ${row.path}`} onClick={() => setSelected(row)} className={cn("rounded hover:bg-muted", row.status === "Partial" ? "text-tone-amber" : row.status === "Conflict" ? "text-tone-red" : row.status === "Unmapped" ? "text-muted-foreground" : "text-tone-blue")}>
            <svg aria-hidden className="h-8 w-full" viewBox="0 0 40 32" preserveAspectRatio="none">
              <path d="M3 12C18 12 22 20 37 20" fill="none" stroke="currentColor" strokeWidth="1.5" strokeDasharray={row.status === "Unmapped" ? "3 3" : undefined} />
              <circle cx="3" cy="12" r="2" fill="currentColor" /><circle cx="37" cy="20" r="2" fill="currentColor" />
            </svg>
          </button>
          <div onDragOver={event => { if (row.target) event.preventDefault() }} onDrop={event => { if (row.target) drop(event, row.target) }}
            className="flex min-h-10 min-w-0 items-center justify-between gap-1 rounded-lg border px-2">
            <div className="min-w-0"><div className="truncate font-mono text-[10px]" title={row.target}>{row.target || "No target assigned"}</div><p className="text-[9px] text-muted-foreground">{row.format}</p></div>
            <Chip tone={TONES[row.status]} icon={row.status === "Suggested" ? Sparkles : undefined} className="text-[9px]">{row.status}</Chip>
          </div>
        </div>)}
        {model.extra.map(target => <div key={target} onDragOver={event => event.preventDefault()} onDrop={event => drop(event, target)} className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-dashed px-2 py-1.5 text-xs">
          <code>{target}</code><span className="text-muted-foreground">{wired.includes(target) ? "Mapped" : target.endsWith(".monthlyIncome") ? "Scanned payslip · Needs review" : "Needs a source"}</span>
        </div>)}
        {selected && <aside aria-label="Selected mapping" className="space-y-2 rounded-lg border border-data-accent/40 bg-data-accent/5 p-3 text-sm">
          <p className="text-xs text-muted-foreground">{MARKS[selected.source].name} · <code>{selected.path}</code></p>
          <p><strong>Sample value</strong> · {selected.sample}</p>
          {selected.note && <p className="text-xs text-muted-foreground">{selected.note}</p>}
          {selected.status === "Suggested" && <div className="flex gap-1.5"><Button size="sm" onClick={() => update(selected, { status: "Mapped" })}>Accept suggestion</Button><Button size="sm" variant="outline" onClick={() => update(selected, { status: "Unmapped", target: "" })}>Reject</Button></div>}
          {selected.status === "Unmapped" && <form className="flex flex-wrap items-center gap-1.5" onSubmit={event => {
            event.preventDefault()
            const target = new FormData(event.currentTarget).get("target")
            if (typeof target === "string" && properties.includes(target)) update(selected, { status: "Mapped", target })
          }}>
            <select name="target" aria-label="Target property" className="max-w-full rounded-md border bg-card px-2 py-1.5 text-xs">{properties.map(target => <option key={target}>{target}</option>)}</select>
            <Button size="sm">Map</Button><Button type="button" size="sm" variant="outline" onClick={() => update(selected, { status: "Ignored" })}>Ignore</Button>
          </form>}
        </aside>}
        <div className="flex flex-wrap items-center gap-2 border-t pt-2">
          <Button size="sm" variant="outline" onClick={() => setPublish(true)}>Publish model</Button>
          {publish && <p role="status" className="text-xs text-muted-foreground">Preview · {counted(unresolved, "unmapped property", "unmapped properties")}, {counted(partial, "partial mapping")}, {counted(conflicts, "conflict")}. The data contract quarantines records missing required fields.</p>}
        </div>
      </div>
    </section>
  </section>
}
