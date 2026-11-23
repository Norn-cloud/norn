import { Link, useLocation } from "@tanstack/react-router"
import { ArrowRight, Check, Database, GitMerge, KeyRound, Shield, Sparkles } from "lucide-react"
import { useState, type DragEvent } from "react"
import { Chip } from "@/components/chip"
import { PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Button } from "@/components/ui/button"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
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
    <section id="review" className="space-y-6">
      <PageHeader
        title="Borrower identity review"
        meta={<Chip tone="amber" dot>{pending} need review</Chip>}
        description="Four representative synthetic identity cases flagged across CRM, LMS, and bureau feeds."
        actions={
          <Button asChild size="sm" variant="outline">
            <Link to="/examples/$scene" params={{ scene: "dataset" }}>Back to field mappings</Link>
          </Button>
        }
      />

      <div className="grid gap-6 lg:grid-cols-2">
        {CASES.map(({ id, label, confidence, fields }) => (
          <Panel
            key={id}
            label={<span className="inline-flex items-baseline gap-2"><span>{label}</span><code className="text-xs font-normal text-muted-foreground">{id}</code></span>}
            action={<Chip tone="amber">{confidence} match</Chip>}
          >
            <TableCard>
              <Table aria-label={label} className="table-fixed">
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-24">Field</TableHead>
                    <TableHead>Source record A</TableHead>
                    <TableHead>Source record B</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {fields.map(([fieldLabel, a, b]) => (
                    <TableRow key={fieldLabel}>
                      <TableCell className="font-medium text-muted-foreground">{fieldLabel}</TableCell>
                      <TableCell className={cn("whitespace-normal [overflow-wrap:anywhere]", a !== b && "font-medium text-tone-amber")}><bdi>{a}</bdi></TableCell>
                      <TableCell className={cn("whitespace-normal [overflow-wrap:anywhere]", a !== b && "font-medium text-tone-amber")}><bdi>{b}</bdi></TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </TableCard>
            <div className="flex flex-wrap items-center gap-2 border-t px-4 py-2.5">
              {decisions[id] ? (
                <Chip tone="green" icon={Check}>{decisions[id]} in preview</Chip>
              ) : (
                <>
                  <Button size="sm" variant="secondary" className="h-7 text-xs" onClick={() => decide(id, "Merged")}>Merge</Button>
                  <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => decide(id, "Kept separate")}>Keep separate</Button>
                </>
              )}
            </div>
          </Panel>
        ))}
      </div>
    </section>
  )
}

const TONES = { Mapped: "gray", Key: "green", Partial: "amber", Conflict: "red", Suggested: "violet", Unmapped: "gray", Ignored: "gray" } as const

/** Calm 2-pane Field Mapper: dense mapping table with inline schema filter + hairline Field Inspector. */
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
  const pendingReview = 32 - Object.keys(decisions).length

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
    <section className="space-y-6">
      <PageHeader
        title="Dataset mapping"
        meta={`Key ${CLIENT.nationalId} · 11,872 merged`}
        description="Map source schemas into the shared financial model, resolve conflicts, and enforce data contracts."
        actions={
          <Button asChild size="sm" variant="outline">
            <Link to="/examples/$scene" params={{ scene: "dataset" }} hash="review">
              {pendingReview} need review<ArrowRight className="size-3.5" />
            </Link>
          </Button>
        }
        primary={
          <Button size="sm" onClick={() => setPublish(true)}>Publish contract</Button>
        }
      />

      <Panel
        icon={Database}
        label="Field mappings"
        action={
          <div className="flex flex-wrap items-center gap-2">
            <div role="group" aria-label="Mapping object type" className="inline-flex h-7 items-center rounded-md bg-muted p-0.5 text-xs">
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
                  className="rounded-[0.3125rem] px-2.5 py-0.5 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent"
                >
                  {item.title}
                </button>
              ))}
            </div>
            <label className="inline-flex items-center">
              <span className="sr-only">Filter by source schema</span>
              <select
                aria-label="Filter by source schema"
                value={sourceFilter}
                onChange={(e) => setSourceFilter(e.target.value as MarkId | "all")}
                className="h-7 rounded-md border bg-card px-2 text-xs text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <option value="all">All schemas ({schemaTree.length})</option>
                {schemaTree.map(({ id, name, schema }) => (
                  <option key={schema} value={id}>{name} ({schema})</option>
                ))}
              </select>
            </label>
          </div>
        }
      >
        <Well className="grid lg:grid-cols-[minmax(0,1fr)_21rem]">
          <div className="flex min-w-0 flex-col">
            <div className="space-y-2 border-b px-4 py-3 text-xs text-muted-foreground">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="inline-flex items-center gap-1.5">
                  <KeyRound className="size-3.5" />
                  <span>{wired.length} of {properties.length} properties mapped · {counted(partial, "partial mapping")} · {counted(conflicts, "conflict")}</span>
                </span>
                <span className="inline-flex items-center gap-1.5">
                  <Shield className="size-3.5" />
                  <span>Required: {model.required} · Quarantine on failure</span>
                </span>
              </div>
              <div className="h-1.5 overflow-hidden rounded-full bg-muted">
                <div className="h-full rounded-full bg-foreground/75 transition-all" style={{ width: `${(100 * wired.length) / properties.length}%` }} />
              </div>
              {publish && (
                <p role="status" className="font-medium text-foreground">
                  Preview: {counted(unresolved, "unmapped property", "unmapped properties")}, {counted(partial, "partial mapping")}, {counted(conflicts, "conflict")}. Contract quarantines records on missing required fields.
                </p>
              )}
            </div>

            <div className="overflow-x-auto">
              <Table aria-label="Field mappings" className="text-meta tabular-nums">
                <TableHeader>
                  <TableRow>
                    <TableHead>Source column</TableHead>
                    <TableHead className="w-6 text-center" aria-label="Maps to" />
                    <TableHead>Domain property</TableHead>
                    <TableHead>Type</TableHead>
                    <TableHead className="text-end">Status</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {visibleRows.map((row) => {
                    const active = selected?.path === row.path
                    return (
                      <TableRow
                        key={row.path}
                        draggable
                        onDragStart={(event) => event.dataTransfer.setData("text/plain", row.path)}
                        onDragOver={(event) => { if (row.target) event.preventDefault() }}
                        onDrop={(event) => { if (row.target) drop(event, row.target) }}
                        onClick={() => setSelectedPath(row.path)}
                        className={cn("cursor-pointer", active && "bg-muted/60")}
                      >
                        <TableCell className={cn(active && "border-s-2 border-s-foreground")}>
                          <div className="flex items-center gap-2">
                            <Mark id={row.source} className="size-4 shrink-0 rounded-xs" />
                            <code className="truncate text-xs text-foreground" title={row.path}>{row.path}</code>
                          </div>
                        </TableCell>
                        <TableCell className="text-center text-muted-foreground">→</TableCell>
                        <TableCell className="font-mono text-xs text-foreground">
                          {row.target || <span className="italic text-muted-foreground">Unassigned</span>}
                        </TableCell>
                        <TableCell className="whitespace-nowrap text-xs text-muted-foreground">{row.format}</TableCell>
                        <TableCell className="whitespace-nowrap text-end">
                          <Chip tone={TONES[row.status]} icon={row.status === "Suggested" ? Sparkles : undefined}>{row.status}</Chip>
                        </TableCell>
                      </TableRow>
                    )
                  })}
                  {model.extra.map((target) => (
                    <TableRow key={target} onDragOver={(e) => e.preventDefault()} onDrop={(e) => drop(e, target)} className="bg-muted/20">
                      <TableCell className="text-xs italic text-muted-foreground">Drop a source column…</TableCell>
                      <TableCell className="text-center text-muted-foreground">→</TableCell>
                      <TableCell className="font-mono text-xs text-foreground">{target}</TableCell>
                      <TableCell className="text-xs text-muted-foreground">missing</TableCell>
                      <TableCell className="text-end text-xs text-muted-foreground">
                        {wired.includes(target) ? "Mapped" : target.endsWith(".monthlyIncome") ? "Payslip · review" : "Needs source"}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          </div>

          <aside aria-label="Selected mapping" className="flex flex-col border-t bg-surface-subtle/40 lg:border-s lg:border-t-0">
            <div className="border-b px-4 py-3">
              <p className="text-xs text-muted-foreground">Field inspector</p>
              {selected && <code className="mt-0.5 block truncate text-xs font-medium text-foreground">{selected.path}</code>}
            </div>
            {selected ? (
              <div className="flex-1 divide-y px-4 text-meta">
                <div className="flex items-center justify-between py-3">
                  <span className="inline-flex items-center gap-1.5 font-medium text-foreground">
                    <Mark id={selected.source} className="size-5" />{MARKS[selected.source].name}
                  </span>
                  <Chip tone={TONES[selected.status]}>{selected.status}</Chip>
                </div>

                <div className="space-y-1 py-3">
                  <p className="text-xs text-muted-foreground">Target property</p>
                  <code className="block font-mono text-xs text-foreground">{selected.target || "No target assigned"}</code>
                  <p className="text-xs text-muted-foreground">Format: {selected.format}</p>
                </div>

                <div className="space-y-1 py-3">
                  <p className="text-xs text-muted-foreground">Sample value</p>
                  <p className="font-medium text-foreground">{selected.sample}</p>
                </div>

                {selected.note && (
                  <div className="space-y-1 py-3">
                    <p className="text-xs text-muted-foreground">Transform &amp; precedence rule</p>
                    <p className="text-xs leading-relaxed text-foreground/90">{selected.note}</p>
                  </div>
                )}

                {selected.status === "Suggested" && (
                  <div className="flex gap-2 py-3">
                    <Button size="sm" className="h-7 flex-1 text-xs" onClick={() => update(selected, { status: "Mapped" })}>Accept suggestion</Button>
                    <Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => update(selected, { status: "Unmapped", target: "" })}>Reject</Button>
                  </div>
                )}

                {selected.status === "Unmapped" && (
                  <form
                    className="space-y-2 py-3"
                    onSubmit={(event) => {
                      event.preventDefault()
                      const target = new FormData(event.currentTarget).get("target")
                      if (typeof target === "string" && properties.includes(target)) update(selected, { status: "Mapped", target })
                    }}
                  >
                    <label className="block space-y-1">
                      <span className="text-xs text-muted-foreground">Assign to domain property</span>
                      <select name="target" aria-label="Target property" className="h-8 w-full rounded-md border bg-card px-2 text-xs">
                        {properties.map((target) => <option key={target}>{target}</option>)}
                      </select>
                    </label>
                    <div className="flex gap-2">
                      <Button size="sm" className="h-7 flex-1 text-xs">Map field</Button>
                      <Button type="button" size="sm" variant="outline" className="h-7 text-xs" onClick={() => update(selected, { status: "Ignored" })}>Ignore</Button>
                    </div>
                  </form>
                )}

                <div className="py-3 text-xs text-muted-foreground">
                  <p className="inline-flex items-center gap-1.5"><GitMerge className="size-3.5" />Identity join key: National ID</p>
                </div>
              </div>
            ) : (
              <p className="p-4 text-meta text-muted-foreground">Select a mapping row to inspect sample data and contract rules.</p>
            )}
          </aside>
        </Well>
      </Panel>
    </section>
  )
}
