import { useParams } from "@tanstack/react-router"
import { ArrowRight, FileScan, ScanLine, Shield, SlidersHorizontal } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { PageHeader, Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Button } from "@/components/ui/button"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
import type { MenuEntry } from "../shell.tsx"
import { CLIENT, type FinanceType } from "./data.ts"
import { DatasetMapping } from "./mapping.tsx"
import { MARKING_FIELDS, MARKING_ROLES, canRevealSample, type MarkingRole } from "./operations.tsx"
import { BY_ID, egp } from "./records.ts"
import { Neighbourhood, RecordLink } from "./record-pages.tsx"
import { TypeTile } from "./workspace.tsx"

const scenes = [
  { id: "dataset", title: "Dataset mapping", detail: "Map source schemas into the shared financial model, resolve conflicts and enforce data contracts." },
  { id: "document", title: "Document mapping", detail: "Configure bilingual extraction templates, confidence review thresholds and role-based PII redaction." },
  { id: "explorer", title: "Record lineage", detail: "Follow a unified client record across loans, payments, brokerage funding and source evidence." },
] as const

export const financeExampleMenu = (): readonly MenuEntry[] => [
  { href: "/examples/dataset", label: "Dataset mapping", count: 32 },
  { href: "/examples/document", label: "Document mapping" },
  { href: "/examples/explorer", label: "Record lineage" },
]

export function FinanceExamples() {
  const { scene } = useParams({ strict: false })
  const selected = scenes.find(({ id }) => id === scene) ?? scenes[0]
  if (selected.id === "dataset") return <DatasetMapping />
  if (selected.id === "document") return <DocumentMapping />
  return <Explorer />
}

function Explorer() {
  const record = BY_ID.get(CLIENT.id)!
  return (
    <section className="space-y-6">
      <PageHeader
        title="Record lineage"
        meta={<><code>{CLIENT.id}</code><span>{CLIENT.name}</span></>}
        description="Follow a unified client record across loans, payments, brokerage funding, and source evidence."
        primary={<Button asChild size="sm"><RecordLink record={record}>Open client record</RecordLink></Button>}
      />

      <Panel
        label="Brokerage funding path"
        action={
          <span className="tabular-nums">
            Deposit / <span lang="ar">مبلغ الإيداع</span> <strong className="font-medium text-foreground">{egp(CLIENT.depositAmount)}.00</strong>
          </span>
        }
      >
        <Well>
          <div aria-label="Brokerage funding path" className="grid items-center gap-3 p-4 lg:grid-cols-[1fr_auto_1fr_auto_1fr_auto_1fr]">
            {[
              { label: "Client", value: CLIENT.id, relation: "holds" },
              { label: "Brokerage account", value: CLIENT.account, relation: "receives" },
              { label: "Funding transaction", value: CLIENT.depositRef, relation: "posts to" },
              { label: "GL journal", value: "GL-1042" },
            ].map(({ label, value, relation }) => (
              <div key={label} className="contents">
                <div className="rounded-md border bg-muted/25 px-3 py-2">
                  <div className="text-meta font-medium text-foreground">{label}</div>
                  <code className="text-xs text-muted-foreground">{value}</code>
                </div>
                {relation && (
                  <span className="flex items-center gap-1 text-xs text-muted-foreground">
                    {relation}<ArrowRight className="size-3" />
                  </span>
                )}
              </div>
            ))}
          </div>
        </Well>
      </Panel>

      <Neighbourhood record={record} />
    </section>
  )
}

interface DocField {
  readonly label: string
  readonly ar: string
  readonly value: string
  readonly type: FinanceType
  readonly field: string
  readonly confidence: number
  readonly restricted?: boolean
}

const DOC_TYPES: Readonly<Record<"receipt" | "kyc" | "payslip", {
  readonly id: "receipt" | "kyc" | "payslip"
  readonly name: string
  readonly arTitle: string
  readonly docId: string
  readonly targetObject: string
  readonly fields: readonly DocField[]
}>> = {
  receipt: {
    id: "receipt",
    name: "Deposit receipt",
    arTitle: "إيصال إيداع",
    docId: CLIENT.document,
    targetObject: "FundingTransaction",
    fields: [
      { label: "Account number", ar: "رقم الحساب", value: CLIENT.account, type: "brokerage-account", field: "BrokerageAccount.number", confidence: 98.5 },
      { label: "Payment reference", ar: "مرجع الدفع", value: CLIENT.depositRef, type: "brokerage-account", field: "FundingTransaction.reference", confidence: 99.2 },
      { label: "Deposit amount", ar: "مبلغ الإيداع", value: `${egp(CLIENT.depositAmount)}.00`, type: "brokerage-account", field: "FundingTransaction.amount", confidence: 96.4 },
      { label: "Value date", ar: "تاريخ القيمة", value: CLIENT.depositDate, type: "brokerage-account", field: "FundingTransaction.valueDate", confidence: 91.2 },
    ],
  },
  kyc: {
    id: "kyc",
    name: "e-KYC extract",
    arTitle: "مستخرج التحقق من الهوية",
    docId: "KYC-1042",
    targetObject: "Client / KycCheck",
    fields: [
      { label: "Client name", ar: "اسم العميل", value: CLIENT.name, type: "client", field: "Client.name", confidence: 99.2 },
      ...MARKING_FIELDS.map((field) => ({ ...field, restricted: true })),
    ],
  },
  payslip: {
    id: "payslip",
    name: "Income payslip",
    arTitle: "بيان مفردات مرتب",
    docId: "PAYSLIP-1042",
    targetObject: "Client",
    fields: [
      { label: "Employee name", ar: "اسم الموظف", value: CLIENT.name, type: "client", field: "Client.name", confidence: 98.7 },
      { label: "Employer", ar: "جهة العمل", value: "Cairo Logistics SAE", type: "client", field: "Client.employer", confidence: 95.1 },
      { label: "Net monthly income", ar: "صافي الدخل الشهري", value: "EGP 68,500.00", type: "client", field: "Client.monthlyIncome", confidence: 89.4, restricted: true },
      { label: "National ID", ar: "الرقم القومي", value: CLIENT.nationalId, type: "client", field: "Client.nationalId", confidence: 97.6, restricted: true },
    ],
  },
}

function DocumentMapping() {
  const [docId, setDocId] = useState<"receipt" | "kyc" | "payslip">("kyc")
  const [role, setRole] = useState<MarkingRole>("Viewer")
  const [threshold, setThreshold] = useState(92)
  const [restrictedOverrides, setRestrictedOverrides] = useState<Readonly<Record<string, boolean>>>({})

  const doc = DOC_TYPES[docId]
  const isRestricted = (field: DocField) => restrictedOverrides[`${doc.id}:${field.field}`] ?? Boolean(field.restricted)
  const hidden = (field: DocField) => isRestricted(field) && !canRevealSample(role)
  const needsReviewCount = doc.fields.filter((f) => f.confidence < threshold).length

  return (
    <section aria-label="OCR receipt and markings" className="space-y-6">
      <PageHeader
        title="Document mapping"
        meta={`${Object.keys(DOC_TYPES).length} templates`}
        description="Configure bilingual extraction templates, confidence review thresholds, and role-based PII redaction."
      />

      <div className="flex flex-wrap items-center justify-between gap-3 border-b pb-3">
        <div role="group" aria-label="Document template" className="inline-flex h-8 items-center rounded-md bg-muted p-0.5 text-xs">
          {(["kyc", "receipt", "payslip"] as const).map((id) => (
            <button
              key={id}
              type="button"
              aria-pressed={docId === id}
              onClick={() => setDocId(id)}
              className="rounded-[0.3125rem] px-2.5 py-1 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent"
            >
              {DOC_TYPES[id].name}
            </button>
          ))}
        </div>

        <div className="flex flex-wrap items-center gap-4 text-xs">
          <label className="inline-flex items-center gap-2 text-muted-foreground">
            <SlidersHorizontal className="size-3.5" />
            <span>Review threshold</span>
            <input
              type="range"
              min={85}
              max={98}
              value={threshold}
              onChange={(e) => setThreshold(Number(e.target.value))}
              className="h-1.5 w-20 accent-[var(--foreground)]"
            />
            <code className="font-medium text-foreground tabular-nums">{threshold}%</code>
          </label>

          <div className="flex items-center gap-2">
            <span className="text-muted-foreground">Preview role</span>
            <div aria-label="Preview role" role="group" className="inline-flex h-8 items-center rounded-md bg-muted p-0.5">
              {MARKING_ROLES.map((item) => (
                <button
                  key={item}
                  type="button"
                  aria-pressed={role === item}
                  onClick={() => setRole(item)}
                  className="rounded-[0.3125rem] px-2 py-1 text-xs text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent"
                >
                  {item}
                </button>
              ))}
            </div>
          </div>
        </div>
      </div>

      <div className="grid gap-6 lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
        <Panel
          icon={ScanLine}
          label={
            <span className="inline-flex flex-wrap items-baseline gap-2">
              <span>{doc.name}</span>
              <code className="text-xs font-normal text-muted-foreground">{doc.docId}</code>
              <span lang="ar" dir="rtl" className="text-xs font-normal text-muted-foreground">{doc.arTitle}</span>
            </span>
          }
          action={
            <Chip tone={canRevealSample(role) ? "green" : "gray"} icon={Shield}>
              {canRevealSample(role) ? "Auditor · unredacted" : `${role} · PII masked`}
            </Chip>
          }
        >
          <Well>
            <dl className="divide-y px-4">
              {doc.fields.map((field) => (
                <div key={field.field} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center gap-3 py-3">
                  <dt className="text-meta">
                    <span className="block font-medium text-foreground">{field.label}</span>
                    <span lang="ar" dir="auto" className="text-xs text-muted-foreground">{field.ar}</span>
                  </dt>
                  <dd className="min-w-0 text-end">
                    {hidden(field) ? (
                      <span aria-label="Restricted value" data-redacted className="inline-block h-3.5 w-4/5 rounded-xs bg-foreground/85 align-middle" />
                    ) : (
                      <code className="inline-block max-w-full truncate rounded-sm bg-muted px-2 py-0.5 text-xs font-medium text-foreground">
                        {field.value}
                      </code>
                    )}
                  </dd>
                </div>
              ))}
            </dl>
          </Well>
        </Panel>

        <Panel
          icon={FileScan}
          label={`Extraction schema · ${doc.targetObject}`}
          action={`${needsReviewCount} below ${threshold}%`}
        >
          <TableCard>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Target property</TableHead>
                  <TableHead className="text-end">Confidence</TableHead>
                  <TableHead className="text-center">PII mask</TableHead>
                  <TableHead className="text-end">Routing</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {doc.fields.map((field) => {
                  const restricted = isRestricted(field)
                  const low = field.confidence < threshold
                  return (
                    <TableRow key={field.field}>
                      <TableCell>
                        <div className="flex items-center gap-2">
                          <TypeTile type={field.type} />
                          <div className="min-w-0">
                            <code className="block truncate font-medium text-foreground">{field.field}</code>
                            <span className="text-xs text-muted-foreground">{field.label}</span>
                          </div>
                        </div>
                      </TableCell>
                      <TableCell className="text-end font-mono text-foreground">{field.confidence.toFixed(1)}%</TableCell>
                      <TableCell className="text-center">
                        <button
                          type="button"
                          aria-pressed={restricted}
                          onClick={() => setRestrictedOverrides((c) => ({ ...c, [`${doc.id}:${field.field}`]: !restricted }))}
                          className={cn(
                            "rounded-sm border px-2 py-0.5 text-[11px] font-medium transition-colors",
                            restricted ? "border-tone-amber/40 bg-tone-amber/10 text-tone-amber" : "border-border bg-muted/30 text-muted-foreground hover:text-foreground",
                          )}
                        >
                          {restricted ? "Restricted" : "Standard"}
                        </button>
                      </TableCell>
                      <TableCell className="whitespace-nowrap text-end">
                        {hidden(field) ? (
                          <Chip tone="gray">Masked</Chip>
                        ) : low ? (
                          <Chip tone="amber" dot>Needs review</Chip>
                        ) : (
                          <Chip tone="green" dot>Auto-accept</Chip>
                        )}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
          </TableCard>
        </Panel>
      </div>
    </section>
  )
}
