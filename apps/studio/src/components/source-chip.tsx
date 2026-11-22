import { Building2, Database, Landmark } from "lucide-react"
import { useState } from "react"
import { siApacheairflow, siApachekafka, siApachespark, siErpnext, siFastapi, siGitlab, siGrafana, siKubernetes, siLanggraph, siMlflow, siOdoo, siOllama, siPostgresql, siPython, siRedhatopenshift, siScikitlearn, siShopify } from "simple-icons"
import { Chip, IconTile } from "@/components/chip"
import { systemKey, systemName, systemTone } from "@/lib/catalog"
import { cn } from "@/lib/utils"

const MARKS: Readonly<Record<string, string>> = {
  erpnext: siErpnext.path, shopify: siShopify.path, odoo: siOdoo.path, postgresql: siPostgresql.path,
  kafka: siApachekafka.path, spark: siApachespark.path, airflow: siApacheairflow.path, mlflow: siMlflow.path,
  "scikit-learn": siScikitlearn.path, gitlab: siGitlab.path, kubernetes: siKubernetes.path,
  openshift: siRedhatopenshift.path, ollama: siOllama.path,
  python: siPython.path, fastapi: siFastapi.path, langgraph: siLanggraph.path, grafana: siGrafana.path,
}
const OFFICIAL_MARKS: Readonly<Record<string, string>> = {
  excel: "https://res.cdn.office.net/files/fabric-cdn-prod_20260113.001/assets/item-types/24/xlsx.svg",
  iceberg: "https://iceberg.apache.org/assets/images/favicon-96x96.png",
  aws: "https://a0.awsstatic.com/libra-css/images/site/fav/favicon.ico",
  salesforce: "https://wp.sfdcdigital.com/en-us/wp-content/uploads/sites/4/2026/05/logo-salesforce-no-type.svg?w=1024",
  instapay: "https://www.instapay.eg/wp-content/uploads/2022/03/cropped-logo-192x192.png",
  fawry: "https://www.fawry.com/wp-content/uploads/2023/05/fawry-Logo.png",
  fra: "https://fra.gov.eg/wp-content/uploads/2024/04/Logo-88-1.png",
  "i-score": "https://www.i-score.com.eg/_next/static/media/logo.f159ad9c.webp",
  egx: "https://beta.egx.com.eg/api/media/uploads/egx_logo_light_245x45_6dd9431853.png",
  mcdr: "https://www.mcsd.com.eg/favicon.svg",
}

function SourceMark({ system, monogram }: { readonly system: string; readonly monogram?: boolean }) {
  const key = systemKey(system)
  const path = Object.hasOwn(MARKS, key) ? MARKS[key] : undefined
  const image = Object.hasOwn(OFFICIAL_MARKS, key) ? OFFICIAL_MARKS[key] : undefined
  const [failed, setFailed] = useState<string>()
  if (image && failed !== image) return <img alt="" referrerPolicy="no-referrer" src={image} width={16} height={16} className="object-contain" onError={() => setFailed(image)} />
  if (key === "lms") return <Building2 aria-hidden />
  if (key === "bank-settlement") return <Landmark aria-hidden />
  if (path) return <svg aria-hidden focusable="false" viewBox="0 0 24 24" fill="currentColor"><path d={path} /></svg>
  if (monogram) return <span aria-hidden className="text-[8px] font-bold uppercase">{systemName(system).slice(0, 4)}</span>
  return <Database aria-hidden />
}

export function SourceTile({ system }: { readonly system: string }) {
  return <IconTile tone={systemTone(system)}><SourceMark system={system} monogram /></IconTile>
}

/** The source system a value was read from, as a chip in that system's tone ("ERPNext"). */
export function SourceChip({ system, className, title }: { readonly system: string; readonly className?: string; readonly title?: string }) {
  const name = systemName(system)
  return <Chip tone={systemTone(system)} data-source={name} className={cn("rounded-full px-2 bg-[color-mix(in_oklab,currentColor_15%,transparent)] [&_svg]:size-3 [&_img]:size-3", className)} title={title ?? `Read from ${name}`}><SourceMark system={system} />{name}</Chip>
}
