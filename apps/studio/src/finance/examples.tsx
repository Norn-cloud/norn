import { Link, useParams } from "@tanstack/react-router"
import { Check, FileScan, KeyRound, ScanLine, Shield, Sparkles } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { Button } from "@/components/ui/button"
import type { MenuEntry } from "../shell.tsx"
import { cn } from "@/lib/utils"
import { CLIENT, SYSTEMS, TYPES, type FinanceType } from "./data.ts"
import { BY_ID, egp } from "./records.ts"
import { Neighbourhood, RecordLink } from "./record-pages.tsx"
import { FinanceHeading, Mark, TypeTile } from "./workspace.tsx"

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
    <Neighbourhood record={record} />
  </div>
}

const RECEIPT = [
  { label:"Client name", ar:"اسم العميل", value:CLIENT.name, type:"client", field:"Client.name", confidence:99.2 },
  { label:"Account number", ar:"رقم الحساب", value:CLIENT.account, type:"brokerage-account", field:"BrokerageAccount.number", confidence:98.5 },
  { label:"Repayment amount", ar:"مبلغ السداد", value:egp(CLIENT.regularPayment), type:"payment", field:"Payment.amount", confidence:96.4 },
  { label:"Value date", ar:"تاريخ القيمة", value:"20 Nov 2026", type:"payment", field:"Payment.settledOn", confidence:91.2 },
  { label:"National ID", ar:"الرقم القومي", value:CLIENT.nationalId, type:"client", field:"Client.nationalId", confidence:98.1, restricted:true },
  { label:"Phone", ar:"رقم الهاتف", value:CLIENT.phone, type:"client", field:"Client.phone", confidence:97.2, restricted:true },
  { label:"Date of birth", ar:"تاريخ الميلاد", value:CLIENT.birth, type:"kyc-check", field:"KycCheck.birthDate", confidence:97.8, restricted:true },
  { label:"Address", ar:"العنوان", value:CLIENT.address, type:"kyc-check", field:"KycCheck.address", confidence:86.3, restricted:true },
] as const

function Curve({ type }: { readonly type: FinanceType }) {
  const color=`var(--tone-${TYPES[type].tone})`
  return <svg aria-hidden viewBox="0 0 100 50" preserveAspectRatio="none" className="h-12 w-full">
    <path d="M 3 17 C 50 17, 50 33, 97 33" fill="none" stroke={color} strokeOpacity={.65} strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
    <circle cx={3} cy={17} r={2.5} fill={color} /><circle cx={97} cy={33} r={2.5} fill={color} />
  </svg>
}

function DocumentMapping() {
  const [auditor,setAuditor]=useState(false)
  const hidden=(field:typeof RECEIPT[number])=>"restricted" in field && !auditor
  return <section aria-label="OCR receipt and markings" className="space-y-3">
    <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-card px-3 py-2">
      <div className="flex flex-wrap items-center gap-2"><Chip tone={auditor?"blue":"gray"} icon={Shield} className="rounded-full">{auditor?"Restricted fields shown for Auditor":"Restricted: hidden for Viewer"}</Chip><span className="text-xs text-muted-foreground">Role simulation</span></div>
      <div aria-label="Preview role" role="group" className="flex gap-1 rounded-lg bg-muted p-1">
        <Button size="sm" variant={auditor?"ghost":"outline"} aria-pressed={!auditor} onClick={()=>setAuditor(false)}>Viewer</Button>
        <Button size="sm" variant={auditor?"outline":"ghost"} aria-pressed={auditor} onClick={()=>setAuditor(true)}>Auditor</Button>
      </div>
    </div>
    <div className="grid gap-y-3 rounded-2xl border bg-surface-subtle p-3 md:grid-cols-[minmax(0,1fr)_3.5rem_minmax(0,1fr)]">
      <article className="overflow-hidden rounded-xl border bg-card shadow-sm">
        <header className="h-24 border-b border-dashed px-4 pt-3">
          <p className="flex items-center gap-1.5 font-mono text-[10px] uppercase text-muted-foreground"><ScanLine className="size-3.5" />Synthetic scan · {CLIENT.document}</p>
          <h2 className="mt-1 text-lg font-semibold">Repayment receipt</h2><p lang="ar" dir="rtl" className="w-fit text-sm text-muted-foreground">إيصال سداد القسط</p>
        </header>
        <dl className="px-3">{RECEIPT.map((field)=><div key={field.field} className="grid h-12 grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center gap-2 border-b border-dashed last:border-0">
          <dt className="text-xs font-medium"><span className="block truncate">{field.label}</span><span lang="ar" dir="auto" className="text-[10px] text-muted-foreground">{field.ar}</span></dt>
          <dd className="min-w-0 text-end">{hidden(field)?<span aria-label="Restricted value" data-redacted className="inline-block h-3.5 w-4/5 rounded-xs bg-black align-middle" />:<Chip tone={TYPES[field.type].tone} mono className="max-w-full truncate">{field.value}</Chip>}</dd>
        </div>)}</dl>
      </article>
      <div className="hidden pt-24 md:block">{RECEIPT.map(({type,field})=><Curve key={field} type={type}/>)}</div>
      <article className="overflow-hidden rounded-xl border bg-card">
        <header className="flex h-24 flex-col justify-center gap-1 border-b px-4"><h2 className="flex items-center gap-2 text-lg font-semibold"><FileScan className="size-4 text-tone-blue"/>Extracted fields</h2><p className="text-xs text-muted-foreground">Values below 92% need review.</p></header>
        <div className="px-3">{RECEIPT.map((field)=><div key={field.field} className="flex h-12 items-center gap-2 border-b last:border-0">
          <TypeTile type={field.type}/><div className="min-w-0 flex-1"><div className="truncate font-mono text-xs" title={field.field}>{field.field}</div><div className="text-[10px] text-muted-foreground">{field.label}</div></div>
          {hidden(field)?<Chip className="rounded-full">Restricted</Chip>:<Chip tone={field.confidence<92?"amber":"green"} className="rounded-full tabular-nums">{field.confidence.toFixed(1)}%{field.confidence<92?" · Needs review":""}</Chip>}
        </div>)}</div>
      </article>
    </div>
  </section>
}

const MAPPINGS = [
  { source:"Core lending", path:"lms.clients.national_id", target:"Client.nationalId", type:"client", format:"text", key:true },
  { source:"I-Score", path:"$.subject.national_id", target:"Client.nationalId", type:"client", format:"text", key:true },
  { source:"Salesforce", path:"Client__c.National_Id__c", target:"Client.nationalId", type:"client", format:"text", key:true },
  { source:"Salesforce", path:"Client__c.cust_nm", target:"Client.name", type:"client", format:"text" },
  { source:"Payments", path:"$.collection.payer_ref", target:"Client.payerRef", type:"client", format:"text" },
  { source:"I-Score", path:"$.subject.score", target:"CreditReport.score", type:"credit-report", format:"integer", suggested:true },
  { source:"Payments", path:"$.settlement.amount_egp", target:"Payment.amount", type:"payment", format:"EGP" },
  { source:"Valify", path:"$.checks.identity.status", target:"KycCheck.status", type:"kyc-check", format:"enum" },
] as const

function DatasetMapping() {
  return <section className="grid gap-3 lg:grid-cols-[15rem_minmax(0,1fr)]">
    <aside className="space-y-2 rounded-2xl border bg-card p-3.5">
      <h2 className="text-sm font-semibold">Source schemas</h2>
      {SYSTEMS.map(({id,name,marks,schema})=><div key={id} className="flex items-center gap-2 rounded-lg bg-muted/45 p-1.5">
        <div className="flex -space-x-1">{marks.map(mark=><Mark key={mark} id={mark} className="size-7"/>)}</div>
        <div className="min-w-0"><div className="text-xs font-semibold">{name}</div><code className="text-[10px] text-muted-foreground">{schema}</code></div>
      </div>)}
    </aside>
    <section className="overflow-hidden rounded-2xl border bg-card">
      <header className="space-y-2 border-b bg-surface-subtle p-3.5">
        <div className="flex flex-wrap items-center gap-2"><h2 className="text-sm font-semibold">{CLIENT.name} · {CLIENT.id}</h2><Chip tone="blue" icon={Shield} className="rounded-full">Data contract</Chip></div>
        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground"><KeyRound className="size-3.5 text-tone-blue"/>Matching key <code>{CLIENT.nationalId}</code><span>· Required national ID and payment reference · On failure</span><Chip tone="amber" className="rounded-full">Quarantine</Chip></div>
      </header>
      <div className="space-y-1.5 p-3">{MAPPINGS.map(row=><div key={row.path} className="grid min-w-0 grid-cols-[minmax(0,1fr)_2.5rem_minmax(0,1fr)] items-center">
        <div className="flex h-12 min-w-0 flex-col justify-center rounded-lg border bg-surface-subtle px-2"><span className="truncate text-[10px] text-muted-foreground">{row.source}</span><code className="truncate text-[11px]" title={row.path}>{row.path}</code></div>
        <Curve type={row.type}/>
        <div className="flex h-12 min-w-0 items-center gap-1.5 rounded-lg border px-2"><TypeTile type={row.type}/><div className="min-w-0 flex-1"><code className="block truncate text-[11px]" title={row.target}>{row.target}</code><span className="text-[9px] text-muted-foreground">{row.format}</span></div>
          {"key" in row&&<Chip tone="green" icon={Check} className="hidden rounded-full sm:inline-flex">key</Chip>}
          {"suggested" in row&&<Chip tone="violet" icon={Sparkles} className="hidden rounded-full sm:inline-flex">Suggested</Chip>}
        </div>
      </div>)}</div>
    </section>
  </section>
}
