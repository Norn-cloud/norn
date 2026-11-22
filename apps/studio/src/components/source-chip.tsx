import { Database } from "lucide-react"
import { siErpnext, siOdoo, siPostgresql, siShopify } from "simple-icons"
import { Chip, IconTile } from "@/components/chip"
import { systemKey, systemName, systemTone } from "@/lib/catalog"
import { cn } from "@/lib/utils"

const MARKS: Readonly<Record<string, string>> = {
  erpnext: siErpnext.path, shopify: siShopify.path, odoo: siOdoo.path, postgresql: siPostgresql.path,
}
const EXCEL_MARK = "https://res.cdn.office.net/files/fabric-cdn-prod_20260113.001/assets/item-types/24/xlsx.svg"

export function SourceMark({ system }: { readonly system: string }) {
  const key = systemKey(system)
  const path = Object.hasOwn(MARKS, key) ? MARKS[key] : undefined
  if (key === "excel") return <img alt="" referrerPolicy="no-referrer" src={EXCEL_MARK} width={16} height={16} />
  return path
    ? <svg aria-hidden focusable="false" viewBox="0 0 24 24" fill="currentColor"><path d={path} /></svg>
    : <Database aria-hidden />
}

export function SourceTile({ system }: { readonly system: string }) {
  return <IconTile tone={systemTone(system)}><SourceMark system={system} /></IconTile>
}

/** The source system a value was read from, as a chip in that system's tone ("ERPNext"). */
export function SourceChip({ system, className, title }: { readonly system: string; readonly className?: string; readonly title?: string }) {
  const name = systemName(system)
  return <Chip tone={systemTone(system)} data-source={name} className={cn("rounded-full px-2 bg-[color-mix(in_oklab,currentColor_15%,transparent)] [&_svg]:size-3 [&_img]:size-3", className)} title={title ?? `Read from ${name}`}><SourceMark system={system} />{name}</Chip>
}
