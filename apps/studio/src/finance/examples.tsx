import { Link, useParams } from "@tanstack/react-router"
import { ArrowRight, FileScan, ScanLine, Shield } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { Button } from "@/components/ui/button"
import type { MenuEntry } from "../shell.tsx"
import { cn } from "@/lib/utils"
import { CLIENT, TYPES, type FinanceType } from "./data.ts"
import { DatasetMapping } from "./mapping.tsx"
import { MARKING_FIELDS, MARKING_ROLES, canRevealSample, type MarkingRole } from "./operations.tsx"
import { BY_ID, egp } from "./records.ts"
import { Neighbourhood, RecordLink } from "./record-pages.tsx"
import { FinanceHeading, TypeTile } from "./workspace.tsx"

const scenes = [
  { id: "explorer", title: "Object explorer", detail: "Follow a client to their loans, payments, and supporting evidence." },
  { id: "document", title: "Document mapping", detail: "Trace extracted receipt fields and see how markings affect each view." },
  { id: "dataset", title: "Dataset mapping", detail: "See how the source records resolve to a shared client and payment model." },
] as const

export const financeExampleMenu = (): readonly MenuEntry[] =>
  scenes.map((scene) => ({ href: `/examples/${scene.id}`, label: scene.title, group: "Examples" }))

export function FinanceExamples() {
  const { scene } = useParams({ strict: false })
  const selected = scenes.find(({ id }) => id === scene) ?? scenes[0]
  return <section className="space-y-4">
    <FinanceHeading title={selected.title} description={selected.detail} />
    <nav aria-label="Finance examples" className="flex flex-wrap gap-1 rounded-xl border bg-card p-1">
      {scenes.map(({ id, title }) => <Link key={id} to="/examples/$scene" params={{scene:id}} aria-current={selected.id===id?"page":undefined} className={cn("rounded-lg px-3 py-1.5 text-sm font-medium text-muted-foreground hover:bg-muted", selected.id===id && "bg-data-accent/10 text-foreground")}>{title}</Link>)}
    </nav>
    {selected.id==="explorer" ? <Explorer /> : selected.id==="document" ? <DocumentMapping /> : <DatasetMapping />}
  </section>
}

function Explorer() {
  const record=BY_ID.get(CLIENT.id)!
  return <div className="space-y-3">
    <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border bg-card p-3">
      <div className="flex items-center gap-2"><TypeTile type="client" /><div><p className="text-sm font-semibold">{CLIENT.name}</p><code className="text-xs text-muted-foreground">{CLIENT.id}</code></div></div>
      <RecordLink record={record} className="text-sm font-medium text-tone-blue hover:underline">Open client record</RecordLink>
    </div>
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 rounded-xl border bg-card px-3 py-2 text-sm">
      <span>Deposit amount / <span lang="ar">مبلغ الإيداع</span></span><strong className="tabular-nums text-tone-blue">{egp(CLIENT.depositAmount)}.00</strong>
    </div>
    <section aria-label="Brokerage funding path" className="grid items-center gap-2 rounded-xl border bg-surface-subtle p-3 lg:grid-cols-[1fr_auto_1fr_auto_1fr_auto_1fr]">
      {[
        { label: "Client", value: CLIENT.id, relation: "holds" },
        { label: "Brokerage account", value: CLIENT.account, relation: "receives" },
        { label: "Funding transaction", value: CLIENT.depositRef, relation: "posts to" },
        { label: "GL journal", value: "GL-1042" },
      ].map(({ label, value, relation }) => <div key={label} className="contents">
        <div className="rounded-lg border bg-card p-2"><div className="text-xs font-semibold">{label}</div><code className="text-[10px] text-muted-foreground">{value}</code></div>
        {relation && <span className="flex items-center gap-1 text-[10px] text-muted-foreground">{relation}<ArrowRight className="size-3 text-tone-blue" /></span>}
      </div>)}
    </section>
    <Neighbourhood record={record} />
  </div>
}

const RECEIPT = [
  { label:"Account number", ar:"رقم الحساب", value:CLIENT.account, type:"brokerage-account", field:"BrokerageAccount.number", confidence:98.5 },
  { label:"Payment reference", ar:"مرجع الدفع", value:CLIENT.depositRef, type:"brokerage-account", field:"FundingTransaction.reference", confidence:99.2 },
  { label:"Deposit amount", ar:"مبلغ الإيداع", value:`${egp(CLIENT.depositAmount)}.00`, type:"brokerage-account", field:"FundingTransaction.amount", confidence:96.4 },
  { label:"Value date", ar:"تاريخ القيمة", value:CLIENT.depositDate, type:"brokerage-account", field:"FundingTransaction.valueDate", confidence:91.2 },
] as const

const KYC_EXTRACT = [
  { label:"Client name", ar:"اسم العميل", value:CLIENT.name, type:"client", field:"Client.name", confidence:99.2 },
  ...MARKING_FIELDS.map(field => ({ ...field, restricted: true })),
] as const

function Curve({ type }: { readonly type: FinanceType }) {
  const color=`var(--tone-${TYPES[type].tone})`
  return <svg aria-hidden viewBox="0 0 100 50" preserveAspectRatio="none" className="h-12 w-full">
    <path d="M 3 17 C 50 17, 50 33, 97 33" fill="none" stroke={color} strokeOpacity={.65} strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
    <circle cx={3} cy={17} r={2.5} fill={color} /><circle cx={97} cy={33} r={2.5} fill={color} />
  </svg>
}

function DocumentMapping() {
  const [document, setDocument] = useState<"receipt" | "kyc">("receipt")
  const [role, setRole] = useState<MarkingRole>("Viewer")
  const isKyc = document === "kyc"
  const fields = isKyc ? KYC_EXTRACT : RECEIPT
  const hidden = (field: typeof fields[number]) => "restricted" in field && !canRevealSample(role)
  return <section aria-label="OCR receipt and markings" className="space-y-3">
    <div role="group" aria-label="Sample document" className="flex flex-wrap gap-1">
      <Button size="sm" variant={isKyc ? "ghost" : "outline"} aria-pressed={!isKyc} onClick={() => setDocument("receipt")}>Deposit receipt</Button>
      <Button size="sm" variant={isKyc ? "outline" : "ghost"} aria-pressed={isKyc} onClick={() => setDocument("kyc")}>Synthetic e-KYC extract</Button>
    </div>
    {isKyc && <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-card px-3 py-2">
      <div className="flex flex-wrap items-center gap-2"><Chip tone={canRevealSample(role) ? "blue" : "gray"} icon={Shield} className="rounded-full">{canRevealSample(role) ? "Restricted fields shown for Auditor" : `Restricted: hidden for ${role}`}</Chip><span className="text-xs text-muted-foreground">Role simulation</span></div>
      <div aria-label="Preview role" role="group" className="flex gap-1 rounded-lg bg-muted p-1">
        {MARKING_ROLES.map(item => <Button key={item} size="sm" variant={role === item ? "outline" : "ghost"} aria-pressed={role === item} onClick={() => setRole(item)}>{item}</Button>)}
      </div>
    </div>}
    <div className="grid gap-y-3 rounded-2xl border bg-surface-subtle p-3 md:grid-cols-[minmax(0,1fr)_3.5rem_minmax(0,1fr)]">
      <article className="overflow-hidden rounded-xl border bg-card shadow-sm">
        <header className="h-24 border-b border-dashed px-4 pt-3">
          <p className="flex items-center gap-1.5 font-mono text-[10px] uppercase text-muted-foreground"><ScanLine className="size-3.5" />Synthetic scan · {isKyc ? "KYC-1042" : CLIENT.document}</p>
          <h2 className="mt-1 text-lg font-semibold">{isKyc ? "Synthetic e-KYC extract" : "Deposit receipt"}</h2><p lang="ar" dir="rtl" className="w-fit text-sm text-muted-foreground">{isKyc ? "مستخرج التحقق من الهوية" : "إيصال إيداع"}</p>
        </header>
        <dl className="px-3">{fields.map((field)=><div key={field.field} className="grid h-12 grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center gap-2 border-b border-dashed last:border-0">
          <dt className="text-xs font-medium"><span className="block truncate">{field.label}</span><span lang="ar" dir="auto" className="text-[10px] text-muted-foreground">{field.ar}</span></dt>
          <dd className="min-w-0 text-end">{hidden(field)?<span aria-label="Restricted value" data-redacted className="inline-block h-3.5 w-4/5 rounded-xs bg-black align-middle" />:<Chip tone={TYPES[field.type].tone} mono className="max-w-full truncate">{field.value}</Chip>}</dd>
        </div>)}</dl>
      </article>
      <div className="hidden pt-24 md:block">{fields.map(({type,field})=><Curve key={field} type={type}/>)}</div>
      <article className="overflow-hidden rounded-xl border bg-card">
        <header className="flex h-24 flex-col justify-center gap-1 border-b px-4"><h2 className="flex items-center gap-2 text-lg font-semibold"><FileScan className="size-4 text-tone-blue"/>Extracted fields</h2><p className="text-xs text-muted-foreground">Values below 92% need review.</p></header>
        <div className="px-3">{fields.map((field)=><div key={field.field} className="flex h-12 items-center gap-2 border-b last:border-0">
          <TypeTile type={field.type}/><div className="min-w-0 flex-1"><div className="truncate font-mono text-xs" title={field.field}>{field.field}</div><div className="text-[10px] text-muted-foreground">{field.label}</div></div>
          {hidden(field)?<Chip className="rounded-full">Restricted</Chip>:<Chip tone={field.confidence<92?"amber":"green"} className="rounded-full tabular-nums">{field.confidence.toFixed(1)}%{field.confidence<92?" · Needs review":""}</Chip>}
        </div>)}</div>
      </article>
    </div>
  </section>
}
