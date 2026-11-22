import { Link, useParams } from "@tanstack/react-router"
import { ArrowLeft, Check, FileScan, Network, ScanLine, Shield, Sparkles } from "lucide-react"
import { useState } from "react"
import { Chip, IconTile, type Tone } from "@/components/chip"
import { ObjectTile, objectAppearance } from "@/components/object-glyph"
import { PageHeader } from "@/components/panel"
import { SourceChip, SourceTile } from "@/components/source-chip"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

const SCENES = [
  { id: "explorer", title: "Object explorer", description: "Follow the relationships around a client, account, and its evidence." },
  { id: "document", title: "Document mapping", description: "Trace OCR evidence to the model and compare what each example role can read." },
  { id: "dataset", title: "Dataset mapping", description: "Read the same customer and transaction model across events, tables, and a model output." },
] as const

export function ExamplePage() {
  const { scene } = useParams({ strict: false })
  const active = SCENES.find(({ id }) => id === scene) ?? SCENES[0]
  return (
    <section className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <Link to="/metrics" hash="corrections" className="inline-flex items-center gap-1.5 text-sm font-medium text-muted-foreground hover:text-foreground"><ArrowLeft className="size-3.5" />Metrics</Link>
        <Chip tone="gray" className="rounded-full">Synthetic example</Chip>
      </div>
      <PageHeader title={active.title} description={active.description} />
      <nav aria-label="Sample screens" className="flex flex-wrap gap-1.5 rounded-xl bg-muted/70 p-1.5">
        {SCENES.map(({ id, title }) => (
          <Button key={id} asChild size="sm" variant="ghost" className={cn("rounded-lg", active.id === id && "bg-card font-semibold shadow-sm ring-1 ring-border")}>
            <Link to="/examples/$scene" params={{ scene: id }} aria-current={active.id === id ? "page" : undefined}>{title}</Link>
          </Button>
        ))}
      </nav>
      {active.id === "document" ? <DocumentMapping /> : active.id === "dataset" ? <DatasetMapping /> : <ObjectExplorer />}
    </section>
  )
}

const RECORDS = [
  { id: "DEMO-CLIENT-017", type: "Client", title: "Sample client 017", detail: "Client identity shared by onboarding, account activity, and risk review." },
  { id: "DEMO-ACCT-017", type: "Account", title: "Brokerage account", detail: "The account holds the client’s deposits and points to its transaction history." },
  { id: "DEMO-KYC-004", type: "KYC", title: "Identity evidence", detail: "Submitted identity evidence, with each extracted field traced to its document." },
  { id: "DEMO-TXN-028", type: "Transaction", title: "Account funding", detail: "A synthetic USD 12,500.00 deposit posted to the brokerage account." },
  { id: "DEMO-POST-028", type: "Posting", title: "Ledger posting", detail: "The paired ledger entry that records the funding transaction." },
  { id: "DEMO-RISK-017", type: "RiskProfile", title: "Investment profile", detail: "The simulated profile carries the client’s risk level and its source evidence." },
] as const

const LINKS = [
  [0, 1, "holds"], [0, 2, "verified by"], [0, 3, "funded by"], [0, 4, "recorded in"], [0, 5, "profiled by"],
  [1, 3, "receives"], [3, 4, "posts as"],
] as const

const POSITIONS = [[50, 50], [16, 26], [16, 74], [84, 17], [84, 50], [84, 83]] as const
const labelOf = (type: string) => type === "RiskProfile" ? "Risk profile" : type === "KYC" ? "KYC record" : type

function ObjectExplorer() {
  const [focus, setFocus] = useState(0)
  const selected = RECORDS[focus] ?? RECORDS[0]
  const neighbors = LINKS.filter(([from, to]) => from === focus || to === focus)
  const order = [focus, ...RECORDS.map((_, index) => index).filter((index) => index !== focus)]
  const slots = new Map(order.map((index, position) => [index, POSITIONS[position] ?? POSITIONS[0]]))
  return (
    <div className="grid gap-3 xl:grid-cols-[minmax(0,1fr)_15rem]">
      <section aria-label="Sample record graph" className="relative grid gap-2 rounded-2xl border bg-surface-subtle p-3 sm:block sm:h-[31rem] sm:bg-[radial-gradient(var(--border)_1px,transparent_1px)] sm:bg-size-[15px_15px]">
        <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="pointer-events-none absolute inset-0 hidden size-full overflow-visible sm:block">
          {neighbors.map(([from, to, label]) => {
            const [x, y] = slots.get(from) ?? POSITIONS[0]
            const [endX, endY] = slots.get(to) ?? POSITIONS[0]
            const mid = (x + endX) / 2
            return <path key={label} d={`M ${x} ${y} C ${mid} ${y}, ${mid} ${endY}, ${endX} ${endY}`} fill="none" stroke="var(--data-accent)" strokeWidth={1.75} strokeOpacity={0.8} vectorEffect="non-scaling-stroke" />
          })}
        </svg>
        {neighbors.map(([from, to, label]) => {
          const [x, y] = slots.get(from) ?? POSITIONS[0]
          const [endX, endY] = slots.get(to) ?? POSITIONS[0]
          return <span key={label} className="pointer-events-none absolute z-10 hidden -translate-x-1/2 -translate-y-1/2 rounded-full border bg-background px-2 py-0.5 text-[10px] whitespace-nowrap text-muted-foreground sm:block" style={{ left: `${(x + endX) / 2}%`, top: `${(y + endY) / 2}%` }}>{label}</span>
        })}
        {order.map((index) => {
          const record = RECORDS[index]!
          const [x, y] = slots.get(index) ?? POSITIONS[0]
          const current = index === focus
          return (
            <button key={record.id} type="button" aria-pressed={current} onClick={() => setFocus(index)} className={cn("z-20 flex min-w-0 items-center gap-2 rounded-xl border bg-card p-3 text-start shadow-sm outline-none transition-shadow hover:shadow-md focus-visible:ring-2 focus-visible:ring-data-accent sm:absolute sm:w-[29%] sm:max-w-52 sm:-translate-x-1/2 sm:-translate-y-1/2", current && "border-data-accent bg-[color-mix(in_oklab,var(--card)_92%,var(--data-accent))] ring-4 ring-data-accent/15")} style={{ left: `${x}%`, top: `${y}%` }}>
              <ObjectTile name={record.type} />
              <span className="min-w-0"><span className="block text-sm font-semibold">{labelOf(record.type)}</span><span className="block truncate font-mono text-[10px] text-muted-foreground">{record.id}</span></span>
            </button>
          )
        })}
      </section>
      <aside className="flex flex-col items-start gap-3 rounded-2xl border bg-card p-4">
        <ObjectTile name={selected.type} className="size-10 [&_svg]:size-5" />
        <div><h2 className="text-section font-semibold">{selected.title}</h2><p className="font-mono text-xs text-muted-foreground">{selected.id}</p></div>
        <p className="text-sm text-muted-foreground">{selected.detail}</p>
        <div className="mt-1 w-full space-y-2 border-t pt-3">
          <span className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground"><Network className="size-3.5" />Linked records</span>
          {neighbors.map(([from, to, verb]) => {
            const index = from === focus ? to : from
            const record = RECORDS[index]
            return <button key={record.id} type="button" onClick={() => setFocus(index)} className="flex w-full items-center justify-between rounded-lg bg-muted/70 px-2 py-1.5 text-start text-xs hover:bg-muted"><span>{labelOf(record.type)}</span><span className="text-muted-foreground">{verb}</span></button>
          })}
        </div>
      </aside>
    </div>
  )
}

const RECEIPT = [
  { label: "Client name", ar: "اسم العميل", value: "Sample Client 017", type: "Client", field: "displayName", confidence: 99.2 },
  { label: "Account number", ar: "رقم الحساب", value: "DEMO-ACCT-017", type: "Account", field: "number", confidence: 98.5 },
  { label: "Funding amount", ar: "مبلغ التمويل", value: "USD 12,500.00", type: "Transaction", field: "amount", confidence: 96.4 },
  { label: "Value date", ar: "تاريخ القيمة", value: "22 Nov 2026", type: "Transaction", field: "valueDate", confidence: 91.2 },
  { label: "National ID", ar: "الرقم القومي", value: "DEMO-ID-017", type: "Client", field: "nationalId", confidence: 98.1, marked: true },
  { label: "Phone", ar: "رقم الهاتف", value: "+000 000 0017", type: "Client", field: "phone", confidence: 95.3, marked: true },
  { label: "Date of birth", ar: "تاريخ الميلاد", value: "17 Jan 1990", type: "KYC", field: "dateOfBirth", confidence: 93.7, marked: true },
  { label: "Address", ar: "العنوان", value: "17 Sample Lane, Demo City", type: "KYC", field: "address", confidence: 86.3, marked: true },
] as const

const confidenceFloor = 92

function MappingCurve({ tone }: { readonly tone: Tone }) {
  return (
    <svg aria-hidden viewBox="0 0 100 50" preserveAspectRatio="none" className="h-12 w-full">
      <path d="M 3 17 C 50 17, 50 33, 97 33" fill="none" stroke={`var(--tone-${tone})`} strokeOpacity={0.65} strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
      <circle cx={3} cy={17} r={2.5} fill={`var(--tone-${tone})`} />
      <circle cx={97} cy={33} r={2.5} fill={`var(--tone-${tone})`} />
    </svg>
  )
}

function DocumentMapping() {
  const [auditor, setAuditor] = useState(false)
  return (
    <section aria-label="OCR receipt and markings" className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-card px-3 py-2">
        <div className="flex flex-wrap items-center gap-2">
          <Chip tone={auditor ? "blue" : "gray"} icon={Shield} className="rounded-full">{auditor ? "Restricted fields shown for Auditor" : "Restricted: hidden for Viewer"}</Chip>
          <span className="text-xs text-muted-foreground">Role simulation</span>
        </div>
        <div role="group" aria-label="Preview role" className="inline-flex gap-1 rounded-lg bg-muted p-1">
          <Button size="sm" className="h-7" variant={auditor ? "ghost" : "outline"} aria-pressed={!auditor} onClick={() => setAuditor(false)}>Viewer</Button>
          <Button size="sm" className="h-7" variant={auditor ? "outline" : "ghost"} aria-pressed={auditor} onClick={() => setAuditor(true)}>Auditor</Button>
        </div>
      </div>
      <div className="grid gap-y-3 rounded-2xl border bg-surface-subtle p-3 md:grid-cols-[minmax(0,1.07fr)_4rem_minmax(0,1fr)]">
        <article className="overflow-hidden rounded-xl border bg-card shadow-sm">
          <header className="flex h-24 items-start justify-between gap-2 border-b border-dashed px-4 pt-4">
            <div>
              <p className="mb-1 flex items-center gap-1.5 font-mono text-[10px] font-medium tracking-wider text-muted-foreground uppercase"><ScanLine className="size-3.5" />Synthetic scan · FUND-028</p>
              <h2 className="text-lg font-semibold">Brokerage funding receipt</h2>
              <p lang="ar" dir="rtl" className="text-start text-sm text-muted-foreground">إيصال تمويل حساب الوساطة</p>
            </div>
            <IconTile tone="blue"><FileScan /></IconTile>
          </header>
          <dl className="px-3">
            {RECEIPT.map((field) => {
              const hidden = "marked" in field && !auditor
              const { tone } = objectAppearance(field.type)
              return (
                <div key={field.field} className="grid h-12 grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center gap-2 border-b border-dashed px-1.5 last:border-0">
                  <dt className="text-xs font-medium"><span className="block truncate">{field.label}</span><span lang="ar" dir="auto" className="text-[10px] text-muted-foreground">{field.ar}</span></dt>
                  <dd className="min-w-0 text-end">
                    {hidden
                      ? <span data-redacted role="img" aria-label="Restricted value" className="inline-block h-3.5 w-4/5 rounded-xs bg-black align-middle" />
                      : <span className="inline-block max-w-full truncate rounded-sm px-1.5 py-1 align-middle font-mono text-[10px] font-medium" style={{ background: `color-mix(in oklab, var(--tone-${tone}) 13%, transparent)`, color: `var(--tone-${tone})` }}>{field.value}</span>}
                  </dd>
                </div>
              )
            })}
          </dl>
        </article>
        <div className="hidden pt-24 md:block">
          {RECEIPT.map((field) => <MappingCurve key={field.field} tone={objectAppearance(field.type).tone} />)}
        </div>
        <article className="overflow-hidden rounded-xl border bg-card">
          <header className="flex h-24 flex-col justify-center gap-1 border-b px-4">
            <h2 className="text-lg font-semibold">Extracted fields</h2>
            <p className="text-xs text-muted-foreground">Values below {confidenceFloor}% need review.</p>
          </header>
          <div className="px-3">
            {RECEIPT.map((field) => (
              <div key={field.field} className="flex h-12 items-center gap-2 border-b px-1 last:border-0">
                <ObjectTile name={field.type} className="size-6 rounded-md [&_svg]:size-3.5" />
                <div className="min-w-0 flex-1">
                  <div className="truncate font-mono text-[11px] font-medium" title={`${field.type}.${field.field}`}>{field.type}.{field.field}</div>
                  <div className="text-[10px] text-muted-foreground">{field.label}</div>
                </div>
                {auditor || !("marked" in field)
                  ? <Chip tone={field.confidence < confidenceFloor ? "amber" : "green"} className="rounded-full tabular-nums">{field.confidence.toFixed(1)}%{field.confidence < confidenceFloor ? " · Needs review" : ""}</Chip>
                  : <Chip tone="gray" className="rounded-full">Restricted</Chip>}
              </div>
            ))}
          </div>
        </article>
      </div>
    </section>
  )
}

const BUSINESS = [
  { id: "lending", title: "Loan system", subtitle: "Core lending", tone: "blue", mark: "LS" },
  { id: "bureau", title: "Credit bureau", subtitle: "I-Score", tone: "violet", system: "i-score" },
  { id: "crm", title: "CRM", subtitle: "Salesforce", tone: "blue", system: "salesforce" },
  { id: "payments", title: "Payments", subtitle: "InstaPay · Fawry · bank statements", tone: "teal", system: "instapay", other: "fawry" },
  { id: "brokerage", title: "Brokerage / trading", subtitle: "EGX orders · MCDR custody", tone: "orange", system: "egx", other: "mcdr" },
  { id: "identity", title: "e-KYC", subtitle: "Identity verification", tone: "pink", mark: "ID" },
  { id: "regulatory", title: "Regulatory reporting", subtitle: "FRA returns", tone: "green", system: "fra" },
] as const

const DATA_INPUTS = [
  { system: "kafka", title: "Kafka topic", locator: "transactions.events", type: "JSON events" },
  { system: "postgresql", title: "Postgres table", locator: "core.clients", type: "SQL rows" },
  { system: "iceberg", title: "Iceberg table", locator: "lakehouse.repayments", type: "Table snapshots" },
  { system: "mlflow", title: "MLflow model output", locator: "risk_score", type: "Model output" },
] as const

const MAPPINGS = [
  { source: "Loan system", field: "borrower.national_id", target: "Client", property: "nationalId", type: "text", key: true },
  { source: "I-Score", field: "$.subject.national_id", target: "Client", property: "nationalId", type: "text", key: true },
  { source: "CRM", field: "National_Id__c", target: "Client", property: "nationalId", type: "text", key: true },
  { source: "Loan system", field: "loan.loan_no", target: "Loan", property: "id", type: "text" },
  { source: "Payments", field: "settlement.amount", target: "Repayment", property: "amount", type: "decimal" },
  { source: "Brokerage", field: "$.order.reference", target: "Transaction", property: "reference", type: "text" },
  { source: "e-KYC", field: "checks.verified_at", target: "KYC", property: "verifiedAt", type: "timestamp" },
  { source: "FRA report", field: "return.account_no", target: "Account", property: "number", type: "text" },
  { source: "MLflow", field: "$.predictions[0].score", target: "Client", property: "riskScore", type: "decimal", suggested: true },
] as const

function DatasetMapping() {
  return (
    <div className="grid gap-3 xl:grid-cols-[15rem_minmax(0,1fr)]">
      <aside className="space-y-4 rounded-2xl border bg-card p-3.5" aria-label="Example sources">
        <div className="space-y-2">
          <h2 className="text-sm font-semibold">Business systems</h2>
          {BUSINESS.map((source) => (
            <div key={source.id} className="flex items-center gap-2 rounded-lg bg-muted/45 p-1.5">
              {"system" in source ? <SourceTile system={source.system} /> : <IconTile tone={source.tone}><span className="text-[9px] font-bold">{source.mark}</span></IconTile>}
              <div className="min-w-0"><div className="text-xs font-semibold">{source.title}</div><div className="text-[10px] text-muted-foreground">{source.subtitle}</div></div>
              {"other" in source && <span className="ms-auto"><SourceTile system={source.other} /></span>}
            </div>
          ))}
        </div>
        <div className="space-y-1.5 border-t pt-3" aria-label="Data feeds">
          {DATA_INPUTS.map(({ system, title, locator, type }) => (
            <div key={system} className="flex items-center gap-2" title={`${title} · ${type}`}>
              <SourceChip system={system} />
              <div className="min-w-0 text-[9px] text-muted-foreground">
                <div>{type}</div>
                <code className="block truncate">{locator}</code>
              </div>
            </div>
          ))}
        </div>
      </aside>
      <section className="overflow-hidden rounded-2xl border bg-card" aria-label="Example field mappings">
        <header className="border-b bg-surface-subtle px-4 py-3">
          <div className="flex flex-wrap items-center gap-2"><h2 className="text-sm font-semibold">Client and account model</h2><Chip tone="blue" icon={Shield} className="rounded-full">Data contract</Chip></div>
          <p className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground"><span>Required: national ID, transaction reference</span><span>On failure</span><Chip tone="amber" className="rounded-full">Quarantine</Chip></p>
        </header>
        <div className="grid grid-cols-[minmax(0,1fr)_2.75rem_minmax(0,1fr)] gap-x-1 px-3 pt-2 text-xs font-medium text-muted-foreground"><span>Source field</span><span aria-hidden /><span>Ontology field</span></div>
        <div className="space-y-1.5 p-3">
          {MAPPINGS.map((row) => {
            const tone = objectAppearance(row.target).tone
            return (
              <div key={row.field} className="grid min-w-0 grid-cols-[minmax(0,1fr)_2.75rem_minmax(0,1fr)] items-center">
                <div className="flex h-12 min-w-0 flex-col justify-center rounded-lg border bg-surface-subtle px-2.5">
                  <span className="truncate text-[10px] text-muted-foreground">{row.source}</span>
                  <code className="truncate text-[11px]" title={row.field}>{row.field}</code>
                </div>
                <MappingCurve tone={tone} />
                <div className="flex h-12 min-w-0 items-center gap-1.5 rounded-lg border bg-card px-2">
                  <ObjectTile name={row.target} className="size-6 rounded-md [&_svg]:size-3.5" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-mono text-[10px] font-medium" title={`${row.target}.${row.property}`}>{row.target}.{row.property}</div>
                    <span className="inline-flex rounded bg-muted px-1 py-0.5 text-[9px] leading-none text-muted-foreground">{row.type}</span>
                  </div>
                  {"key" in row && <Chip tone="green" icon={Check} className="hidden rounded-full sm:inline-flex">key</Chip>}
                  {"suggested" in row && <Chip tone="violet" icon={Sparkles} className="hidden rounded-full sm:inline-flex">Suggested</Chip>}
                </div>
              </div>
            )
          })}
        </div>
      </section>
    </div>
  )
}
