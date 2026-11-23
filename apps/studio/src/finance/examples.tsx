import { useParams } from "@tanstack/react-router"
import { ArrowRight, FileScan, ScanLine, Shield, SlidersHorizontal } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import type { MenuEntry } from "../shell.tsx"
import { cn } from "@/lib/utils"
import { CLIENT, type FinanceType } from "./data.ts"
import { DatasetMapping } from "./mapping.tsx"
import { MARKING_FIELDS, MARKING_ROLES, canRevealSample, type MarkingRole } from "./operations.tsx"
import { BY_ID, egp } from "./records.ts"
import { Neighbourhood, RecordLink } from "./record-pages.tsx"
import { FinanceHeading, TypeTile } from "./workspace.tsx"

const scenes = [
  { id: "explorer", title: "Record Lineage", detail: "Follow a unified client record across loans, payments, brokerage funding and source evidence." },
  { id: "document", title: "DocTypes", detail: "Configure document extraction templates, confidence review thresholds and role-based PII redaction." },
  { id: "dataset", title: "Dataset Mapping", detail: "Map source schemas into the shared financial ontology, resolve conflicts and enforce data contracts." },
] as const

export const financeExampleMenu = (): readonly MenuEntry[] =>
  scenes.map((scene) => ({ href: `/examples/${scene.id}`, label: scene.title, group: "Examples" }))

export function FinanceExamples() {
  const { scene } = useParams({ strict: false })
  const selected = scenes.find(({ id }) => id === scene) ?? scenes[0]
  return (
    <section className="space-y-4">
      <FinanceHeading title={selected.title} description={selected.detail} />
      {selected.id === "explorer" ? <Explorer /> : selected.id === "document" ? <DocumentMapping /> : <DatasetMapping />}
    </section>
  )
}

function Explorer() {
  const record = BY_ID.get(CLIENT.id)!
  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-background px-4 py-3">
        <div className="flex items-center gap-2.5">
          <TypeTile type="client" className="border-data-accent/40 text-tone-blue" />
          <div>
            <p className="text-sm font-semibold">{CLIENT.name}</p>
            <p className="text-xs text-muted-foreground"><code>{CLIENT.id}</code> · Deposit amount / <span lang="ar">مبلغ الإيداع</span> <strong className="tabular-nums text-foreground">{egp(CLIENT.depositAmount)}.00</strong></p>
          </div>
        </div>
        <RecordLink record={record} className="inline-flex items-center gap-1 rounded-md border border-border bg-surface-subtle px-3 py-1.5 text-xs font-medium text-tone-blue hover:border-data-accent">
          Open client record<ArrowRight className="size-3.5" />
        </RecordLink>
      </div>

      <section aria-label="Brokerage funding path" className="overflow-hidden rounded-lg border border-border bg-background">
        <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">Brokerage funding path</header>
        <div className="grid items-center gap-2 p-4 lg:grid-cols-[1fr_auto_1fr_auto_1fr_auto_1fr]">
          {[
            { label: "Client", value: CLIENT.id, relation: "holds" },
            { label: "Brokerage account", value: CLIENT.account, relation: "receives" },
            { label: "Funding transaction", value: CLIENT.depositRef, relation: "posts to" },
            { label: "GL journal", value: "GL-1042" },
          ].map(({ label, value, relation }) => (
            <div key={label} className="contents">
              <div className="rounded-md border border-border bg-surface-subtle px-3 py-2">
                <div className="text-xs font-semibold text-foreground">{label}</div>
                <code className="text-[11px] text-muted-foreground">{value}</code>
              </div>
              {relation && (
                <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
                  {relation}<ArrowRight className="size-3 text-tone-blue" />
                </span>
              )}
            </div>
          ))}
        </div>
      </section>

      <Neighbourhood record={record} />
    </div>
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

/** High-level Domain Expert DocType Customizer: templates, confidence gate, and per-field PII redaction. */
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
    <section aria-label="OCR receipt and markings" className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-background px-4 py-2.5">
        <div role="group" aria-label="Document template" className="inline-flex rounded-md border border-border bg-surface-subtle p-0.5 text-xs">
          {(["kyc", "receipt", "payslip"] as const).map((id) => (
            <button
              key={id}
              type="button"
              aria-pressed={docId === id}
              onClick={() => setDocId(id)}
              className={cn("rounded-sm px-2.5 py-1 font-medium transition-colors", docId === id ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground")}
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
              className="h-1.5 w-20 accent-[var(--data-accent)]"
            />
            <code className="font-semibold text-foreground tabular-nums">{threshold}%</code>
          </label>

          <div className="flex items-center gap-2">
            <span className="text-muted-foreground">Preview role</span>
            <div aria-label="Preview role" role="group" className="inline-flex rounded-md border border-border bg-surface-subtle p-0.5">
              {MARKING_ROLES.map((item) => (
                <button
                  key={item}
                  type="button"
                  aria-pressed={role === item}
                  onClick={() => setRole(item)}
                  className={cn("rounded-sm px-2 py-0.5 text-xs font-medium transition-colors", role === item ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground")}
                >
                  {item}
                </button>
              ))}
            </div>
          </div>
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
        {/* Left: Synthetic document scan preview */}
        <article className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="flex items-start justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-3">
            <div>
              <p className="flex items-center gap-1.5 font-mono text-[10px] uppercase text-muted-foreground">
                <ScanLine className="size-3.5" />Synthetic scan · {doc.docId}
              </p>
              <h2 className="mt-1 text-base font-semibold text-foreground">{doc.name}</h2>
              <p lang="ar" dir="rtl" className="w-fit text-xs text-muted-foreground">{doc.arTitle}</p>
            </div>
            <Chip tone={canRevealSample(role) ? "blue" : "gray"} icon={Shield}>
              {canRevealSample(role) ? "Auditor · unredacted" : `${role} · PII masked`}
            </Chip>
          </header>
          <dl className="divide-y divide-border px-4">
            {doc.fields.map((field) => (
              <div key={field.field} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center gap-3 py-3">
                <dt className="text-xs">
                  <span className="block font-medium text-foreground">{field.label}</span>
                  <span lang="ar" dir="auto" className="text-[11px] text-muted-foreground">{field.ar}</span>
                </dt>
                <dd className="min-w-0 text-end">
                  {hidden(field) ? (
                    <span aria-label="Restricted value" data-redacted className="inline-block h-3.5 w-4/5 rounded-xs bg-foreground/90 align-middle" />
                  ) : (
                    <code className="inline-block max-w-full truncate rounded-sm border border-border bg-surface-subtle px-2 py-0.5 text-xs font-medium text-foreground">
                      {field.value}
                    </code>
                  )}
                </dd>
              </div>
            ))}
          </dl>
        </article>

        {/* Right: Structured extraction schema & no-code PII / review rules */}
        <article className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-3">
            <div>
              <h2 className="flex items-center gap-1.5 text-sm font-semibold text-foreground">
                <FileScan className="size-4 text-tone-blue" />Extraction schema · {doc.targetObject}
              </h2>
              <p className="text-xs text-muted-foreground">
                Fields below {threshold}% route to manual review ({needsReviewCount} flagged). Toggle PII marking to test role redaction.
              </p>
            </div>
          </header>
          <div className="overflow-x-auto">
            <table className="w-full text-start text-xs">
              <thead>
                <tr className="border-b border-border bg-surface-subtle/40 text-muted-foreground">
                  <th className="px-4 py-2 text-start font-medium">Target property</th>
                  <th className="px-3 py-2 text-end font-medium">Confidence</th>
                  <th className="px-3 py-2 text-center font-medium">PII mask</th>
                  <th className="px-4 py-2 text-end font-medium">Routing</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {doc.fields.map((field) => {
                  const restricted = isRestricted(field)
                  const low = field.confidence < threshold
                  return (
                    <tr key={field.field} className="hover:bg-surface-subtle/60">
                      <td className="px-4 py-2.5">
                        <div className="flex items-center gap-2">
                          <TypeTile type={field.type} />
                          <div className="min-w-0">
                            <code className="block truncate font-medium text-foreground">{field.field}</code>
                            <span className="text-[11px] text-muted-foreground">{field.label}</span>
                          </div>
                        </div>
                      </td>
                      <td className="px-3 py-2.5 text-end font-mono tabular-nums text-foreground">{field.confidence.toFixed(1)}%</td>
                      <td className="px-3 py-2.5 text-center">
                        <button
                          type="button"
                          aria-pressed={restricted}
                          onClick={() => setRestrictedOverrides((c) => ({ ...c, [`${doc.id}:${field.field}`]: !restricted }))}
                          className={cn(
                            "rounded-sm border px-2 py-0.5 text-[10px] font-medium transition-colors",
                            restricted ? "border-tone-amber/40 bg-tone-amber/10 text-tone-amber" : "border-border bg-surface-subtle text-muted-foreground hover:text-foreground",
                          )}
                        >
                          {restricted ? "Restricted" : "Standard"}
                        </button>
                      </td>
                      <td className="px-4 py-2.5 text-end whitespace-nowrap">
                        {hidden(field) ? (
                          <Chip tone="gray">Masked</Chip>
                        ) : low ? (
                          <Chip tone="amber" dot>Needs review</Chip>
                        ) : (
                          <Chip tone="green" dot>Auto-accept</Chip>
                        )}
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        </article>
      </div>
    </section>
  )
}
