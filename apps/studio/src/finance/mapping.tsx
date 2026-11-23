import { Link, useLocation } from "@tanstack/react-router"
import { ArrowRight, Check, Database, GitMerge, KeyRound, Shield, Sparkles } from "lucide-react"
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
  return (
    <section id="review" className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border bg-surface-subtle px-4 py-2.5">
        <div>
          <h2 className="text-sm font-semibold text-foreground">Borrower identity review queue</h2>
          <p className="text-xs text-muted-foreground">{pending} pending review · 4 representative synthetic cases shown</p>
        </div>
        <Link to="/examples/$scene" params={{ scene: "dataset" }} className="inline-flex items-center gap-1 rounded-md border border-border bg-background px-3 py-1.5 text-xs font-medium text-tone-blue hover:border-data-accent">
          Back to field mappings
        </Link>
      </div>
      <div className="grid gap-3 lg:grid-cols-2">
        {CASES.map(({ id, label, confidence, fields }) => (
          <article key={id} className="overflow-hidden rounded-lg border border-border bg-background">
            <header className="flex items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-2.5">
              <div className="flex items-baseline gap-2">
                <h3 className="text-xs font-semibold text-foreground">{label}</h3>
                <code className="text-[11px] text-muted-foreground">{id}</code>
              </div>
              <Chip tone="amber">{confidence} match</Chip>
            </header>
            <div className="p-4 space-y-3">
              <table aria-label={label} className="w-full table-fixed text-xs">
                <thead>
                  <tr className="border-b border-border text-muted-foreground">
                    <th scope="col" className="w-24 pb-1.5 text-start font-medium">Field</th>
                    <th scope="col" className="pb-1.5 text-start font-medium">Source record A</th>
                    <th scope="col" className="pb-1.5 text-start font-medium">Source record B</th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border">
                  {fields.map(([fieldLabel, a, b]) => (
                    <tr key={fieldLabel}>
                      <th scope="row" className="py-2 text-start font-medium text-muted-foreground">{fieldLabel}</th>
                      <td className={cn("overflow-wrap-anywhere pe-2 py-2 text-start", a !== b && "font-medium text-tone-amber")}><bdi>{a}</bdi></td>
                      <td className={cn("overflow-wrap-anywhere py-2 text-start", a !== b && "font-medium text-tone-amber")}><bdi>{b}</bdi></td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <div className="flex flex-wrap items-center gap-2 border-t border-border pt-3">
                {decisions[id] ? (
                  <Chip tone="green" icon={Check}>{decisions[id]} in preview</Chip>
                ) : (
                  <>
                    <Button size="sm" className="h-7 text-xs" onClick={() => decide(id, "Merged")}>Merge</Button>
                    <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => decide(id, "Kept separate")}>Keep separate</Button>
                  </>
                )}
              </div>
            </div>
          </article>
        ))}
      </div>
    </section>
  )
}

const TONES = { Mapped: "blue", Key: "green", Partial: "amber", Conflict: "red", Suggested: "violet", Unmapped: "gray", Ignored: "gray" } as const

/** Snowflake / Retool 3-pane Field Mapper: Source Schema Tree | Dense Mapping Grid | Docked Field Inspector. */
export function DatasetMapping() {
  const review = useLocation({ select: (location) => location.hash === "review" })
  const [model, setModel] = useState<(typeof MODEL)[number]>(MODEL[0])
  const [sourceFilter, setSourceFilter] = useState<MarkId | "all">("all")
  const [selectedPath, setSelectedPath] = useState<string>(MODEL[0].rows[3]!.path)
  const [overrides, setOverrides] = useState<Readonly<Record<string, Field>>>({})
  const [decisions, setDecisions] = useState<Readonly<Record<string, ReviewDecision>>>({})
  const [publish, setPublish] = useState(false)

  const rowKey = (path: string) => `${model.title}:${path}`
  const rows = model.rows.map((row) => overrides[rowKey(row.path)] ?? row)
  const visibleRows = rows.filter((row) => row.status !== "Ignored" && (sourceFilter === "all" || row.source === sourceFilter))
  const selected = rows.find((r) => r.path === selectedPath) ?? visibleRows[0] ?? rows[0]

  const properties = [...new Set([...rows.filter((row) => row.target).map((row) => row.target), ...model.extra])]
  const wired = properties.filter((target) => rows.some((row) => row.target === target && row.status !== "Suggested" && row.status !== "Ignored"))
  const partial = rows.filter((row) => row.status === "Partial").length
  const conflicts = new Set(rows.filter((row) => row.status === "Conflict").map((row) => row.target)).size
  const unresolved = properties.length - wired.length

  const update = (row: Field, change: Partial<Pick<Field, "status" | "target">>) => {
    const next = { ...row, ...change }
    setOverrides((current) => ({ ...current, [rowKey(row.path)]: next }))
    setSelectedPath(next.path)
    setPublish(false)
  }

  const drop = (event: DragEvent, target: string) => {
    event.preventDefault()
    const row = rows.find((item) => item.path === event.dataTransfer.getData("text/plain"))
    if (row) update(row, { target, status: "Mapped" })
  }

  if (review) {
    return <IdentityReview decisions={decisions} decide={(id, decision) => setDecisions((current) => ({ ...current, [id]: decision }))} />
  }

  const schemaTree: readonly { readonly id: MarkId; readonly name: string; readonly schema: string }[] = [
    ...SYSTEMS.map((s) => ({ id: s.marks[0]!, name: s.name, schema: s.schema })),
    { id: "model", name: "MLflow risk model", schema: "risk.inference.v2" },
  ]

  return (
    <section className="grid overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-[12.5rem_minmax(0,1fr)_19rem]">
      {/* Pane 1: Source Schema Explorer */}
      <aside aria-label="Source schemas" className="flex flex-col border-b border-border bg-surface-subtle/50 lg:border-e lg:border-b-0">
        <div className="border-b border-border px-3 py-2.5">
          <p className="text-[10px] font-semibold tracking-wider uppercase text-muted-foreground">Source schemas</p>
        </div>
        <ul className="flex-1 divide-y divide-border/60 overflow-y-auto p-1.5 text-xs">
          <li>
            <button
              type="button"
              onClick={() => setSourceFilter("all")}
              className={cn("flex w-full items-center justify-between rounded-sm px-2 py-1.5 text-start font-medium", sourceFilter === "all" ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground")}
            >
              <span className="inline-flex items-center gap-1.5"><Database className="size-3.5" />All schemas</span>
              <span className="text-[10px] tabular-nums">{schemaTree.length}</span>
            </button>
          </li>
          {schemaTree.map(({ id, name, schema }) => {
            const active = sourceFilter === id
            return (
              <li key={schema} className="pt-0.5">
                <button
                  type="button"
                  onClick={() => setSourceFilter(active ? "all" : id)}
                  className={cn("flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-start transition-colors", active ? "bg-tone-blue/[0.08] text-foreground" : "text-foreground/80 hover:bg-background")}
                >
                  <Mark id={id} className="size-5 shrink-0" />
                  <div className="min-w-0">
                    <div className="truncate text-[11px] font-medium">{name}</div>
                    <code className="block truncate text-[10px] text-muted-foreground">{schema}</code>
                  </div>
                </button>
              </li>
            )
          })}
        </ul>
      </aside>

      {/* Pane 2: Dense Mapping Grid */}
      <div className="flex min-w-0 flex-col">
        <header className="space-y-2.5 border-b border-border bg-surface-subtle px-4 py-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div role="group" aria-label="Mapping object type" className="inline-flex rounded-md border border-border bg-background p-0.5 text-xs">
              {MODEL.map((item) => (
                <button
                  key={item.title}
                  type="button"
                  aria-pressed={item === model}
                  onClick={() => {
                    setModel(item)
                    setSourceFilter("all")
                    setSelectedPath(item.rows[0]!.path)
                    setPublish(false)
                  }}
                  className={cn("rounded-sm px-2.5 py-1 font-medium transition-colors", item === model ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground")}
                >
                  {item.title}
                </button>
              ))}
            </div>
            <div className="flex items-center gap-2">
              <Link to="/examples/$scene" params={{ scene: "dataset" }} hash="review" className="inline-flex h-7 items-center gap-1 rounded-md border border-tone-amber/40 bg-tone-amber/10 px-2.5 text-xs font-medium text-tone-amber hover:bg-tone-amber/15">
                {32 - Object.keys(decisions).length} need review<ArrowRight className="size-3" />
              </Link>
              <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => setPublish(true)}>Publish contract</Button>
            </div>
          </div>

          <div className="flex flex-wrap items-center justify-between gap-2 text-[11px] text-muted-foreground">
            <span className="inline-flex items-center gap-1.5">
              <KeyRound className="size-3 text-tone-blue" />
              <span>Key: <code className="text-foreground">{CLIENT.nationalId}</code> · 11,872 merged</span>
            </span>
            <span className="inline-flex items-center gap-1.5">
              <Shield className="size-3 text-tone-amber" />
              <span>Required: {model.required} · Quarantine on failure</span>
            </span>
          </div>

          <div className="space-y-1">
            <div className="h-1.5 overflow-hidden rounded-full bg-muted">
              <div className="h-full rounded-full bg-data-accent transition-all" style={{ width: `${(100 * wired.length) / properties.length}%` }} />
            </div>
            <div className="flex flex-wrap items-center justify-between gap-2 text-[11px] text-muted-foreground">
              <span>{wired.length} of {properties.length} properties mapped · {counted(partial, "partial mapping")} · {counted(conflicts, "conflict")}</span>
              {publish && <span role="status" className="font-medium text-tone-blue">Preview: {counted(unresolved, "unmapped property", "unmapped properties")}, {counted(partial, "partial mapping")}, {counted(conflicts, "conflict")}.</span>}
            </div>
          </div>
        </header>

        <div className="overflow-x-auto">
          <table className="w-full text-start text-xs" aria-label="Field mappings">
            <thead>
              <tr className="border-b border-border bg-surface-subtle/40 text-muted-foreground">
                <th className="px-3.5 py-2 text-start font-medium">Source column</th>
                <th className="w-6 py-2 text-center font-medium" aria-label="Maps to" />
                <th className="px-3 py-2 text-start font-medium">Domain property</th>
                <th className="px-3 py-2 text-start font-medium">Type</th>
                <th className="px-3.5 py-2 text-end font-medium">Status</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {visibleRows.map((row) => {
                const active = selected?.path === row.path
                return (
                  <tr
                    key={row.path}
                    draggable
                    onDragStart={(event) => event.dataTransfer.setData("text/plain", row.path)}
                    onDragOver={(event) => { if (row.target) event.preventDefault() }}
                    onDrop={(event) => { if (row.target) drop(event, row.target) }}
                    onClick={() => setSelectedPath(row.path)}
                    className={cn("cursor-pointer transition-colors hover:bg-surface-subtle", active && "bg-tone-blue/[0.06]")}
                  >
                    <td className={cn("px-3.5 py-2", active && "border-s-2 border-s-data-accent")}>
                      <div className="flex items-center gap-2">
                        <Mark id={row.source} className="size-4 shrink-0 rounded-xs" />
                        <code className="truncate text-[11px] text-foreground" title={row.path}>{row.path}</code>
                      </div>
                    </td>
                    <td className="py-2 text-center text-muted-foreground">→</td>
                    <td className="px-3 py-2 font-mono text-[11px] text-foreground">
                      {row.target || <span className="italic text-muted-foreground">Unassigned</span>}
                    </td>
                    <td className="px-3 py-2 text-[11px] text-muted-foreground whitespace-nowrap">{row.format}</td>
                    <td className="px-3.5 py-2 text-end whitespace-nowrap">
                      <Chip tone={TONES[row.status]} icon={row.status === "Suggested" ? Sparkles : undefined}>{row.status}</Chip>
                    </td>
                  </tr>
                )
              })}
              {model.extra.map((target) => (
                <tr key={target} onDragOver={(e) => e.preventDefault()} onDrop={(e) => drop(e, target)} className="bg-surface-subtle/30">
                  <td className="px-3.5 py-2 text-[11px] italic text-muted-foreground">Drop a source column…</td>
                  <td className="py-2 text-center text-muted-foreground">→</td>
                  <td className="px-3 py-2 font-mono text-[11px] text-foreground">{target}</td>
                  <td className="px-3 py-2 text-[11px] text-muted-foreground">missing</td>
                  <td className="px-3.5 py-2 text-end">
                    <span className="text-[11px] text-muted-foreground">{wired.includes(target) ? "Mapped" : target.endsWith(".monthlyIncome") ? "Payslip · review" : "Needs source"}</span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>

      {/* Pane 3: Docked Field Inspector */}
      <aside aria-label="Selected mapping" className="flex flex-col border-t border-border bg-surface-subtle/35 lg:border-s lg:border-t-0">
        <div className="border-b border-border bg-surface-subtle px-4 py-2.5">
          <p className="text-[10px] font-semibold tracking-wider uppercase text-muted-foreground">Field Inspector</p>
          {selected && <code className="mt-0.5 block truncate text-xs font-semibold text-foreground">{selected.path}</code>}
        </div>
        {selected ? (
          <div className="flex-1 space-y-4 p-4 text-xs">
            <div className="flex items-center justify-between">
              <span className="inline-flex items-center gap-1.5 font-medium text-foreground">
                <Mark id={selected.source} className="size-5" />{MARKS[selected.source].name}
              </span>
              <Chip tone={TONES[selected.status]}>{selected.status}</Chip>
            </div>

            <div className="space-y-1 border-t border-border pt-3">
              <p className="text-[11px] text-muted-foreground">Target property</p>
              <code className="block rounded-md border border-border bg-background px-2.5 py-1.5 font-mono text-xs text-foreground">
                {selected.target || "No target assigned"}
              </code>
              <p className="text-[11px] text-muted-foreground">Format: {selected.format}</p>
            </div>

            <div className="space-y-1 border-t border-border pt-3">
              <p className="text-[11px] text-muted-foreground">Sample value</p>
              <p className="rounded-md border border-border bg-background px-2.5 py-1.5 font-medium text-foreground">{selected.sample}</p>
            </div>

            {selected.note && (
              <div className="space-y-1 border-t border-border pt-3">
                <p className="text-[11px] text-muted-foreground">Transform &amp; precedence rule</p>
                <p className="leading-relaxed text-foreground/90">{selected.note}</p>
              </div>
            )}

            {selected.status === "Suggested" && (
              <div className="flex gap-2 border-t border-border pt-3">
                <Button size="sm" className="h-7 flex-1 text-xs" onClick={() => update(selected, { status: "Mapped" })}>Accept suggestion</Button>
                <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => update(selected, { status: "Unmapped", target: "" })}>Reject</Button>
              </div>
            )}

            {selected.status === "Unmapped" && (
              <form
                className="space-y-2 border-t border-border pt-3"
                onSubmit={(event) => {
                  event.preventDefault()
                  const target = new FormData(event.currentTarget).get("target")
                  if (typeof target === "string" && properties.includes(target)) update(selected, { status: "Mapped", target })
                }}
              >
                <label className="block space-y-1">
                  <span className="text-[11px] text-muted-foreground">Assign to domain property</span>
                  <select name="target" aria-label="Target property" className="h-8 w-full rounded-md border border-border bg-background px-2 text-xs">
                    {properties.map((target) => <option key={target}>{target}</option>)}
                  </select>
                </label>
                <div className="flex gap-2">
                  <Button size="sm" className="h-7 flex-1 text-xs">Map field</Button>
                  <Button type="button" size="sm" variant="outline" className="h-7 text-xs" onClick={() => update(selected, { status: "Ignored" })}>Ignore</Button>
                </div>
              </form>
            )}

            <div className="border-t border-border pt-3 text-[11px] text-muted-foreground">
              <p className="inline-flex items-center gap-1.5"><GitMerge className="size-3.5 text-tone-blue" />Identity join key: National ID</p>
            </div>
          </div>
        ) : (
          <p className="p-4 text-xs text-muted-foreground">Select a mapping row to inspect sample data and contract rules.</p>
        )}
      </aside>
    </section>
  )
}
