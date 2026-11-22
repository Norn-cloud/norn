import { Link } from "@tanstack/react-router"
import { ArrowRight, Fingerprint, GitBranch, KeyRound, Shield, TableProperties } from "lucide-react"
import { Chip } from "@/components/chip"
import { Panel } from "@/components/panel"
import { MODEL_LINKS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { MARKING_FIELDS, RULES } from "./operations.tsx"
import { BusinessPolicies } from "./policies.tsx"
import { RECORDS } from "./records.ts"
import { Mark, TypeTile } from "./workspace.tsx"

type FieldKind = "Text" | "Date" | "EGP money" | "Score, 400–850" | "Enum" | "Reference"
type Property = readonly [key: string, type: FieldKind, presence: "Required" | "Optional"]
type Definition = { readonly key: string; readonly fields: readonly Property[] }

const DEFINITIONS: Record<FinanceType, Definition> = {
  client: { key: "National ID", fields: [
    ["nationalId", "Text", "Required"], ["name", "Text", "Required"], ["lmsClientId", "Text", "Optional"], ["payerRef", "Text", "Optional"], ["phone", "Text", "Optional"],
  ] },
  loan: { key: "Loan reference", fields: [
    ["principal", "EGP money", "Required"], ["clientId", "Reference", "Required"], ["installmentAmount", "EGP money", "Required"], ["disbursedOn", "Date", "Optional"],
  ] },
  installment: { key: "Installment reference", fields: [
    ["amount", "EGP money", "Required"], ["dueDate", "Date", "Required"], ["loanId", "Reference", "Required"],
  ] },
  payment: { key: "Settlement reference", fields: [
    ["amount", "EGP money", "Required"], ["currency", "Enum", "Required"], ["payerRef", "Text", "Optional"], ["settledOn", "Date", "Required"], ["reportingWeek", "Date", "Optional"],
  ] },
  "credit-report": { key: "Bureau report reference", fields: [
    ["score", "Score, 400–850", "Required"], ["riskBand", "Enum", "Optional"], ["issuedOn", "Date", "Required"],
  ] },
  "kyc-check": { key: "Identity check reference", fields: [
    ["status", "Enum", "Required"], ["client", "Reference", "Required"], ["verifiedOn", "Date", "Optional"], ["previousExpiresOn", "Date", "Optional"], ["birthDate", "Date", "Optional"], ["address", "Text", "Optional"],
  ] },
  "relationship-manager": { key: "CRM user ID", fields: [
    ["name", "Text", "Required"], ["client", "Reference", "Optional"],
  ] },
  "hardship-case": { key: "CRM case reference", fields: [
    ["category", "Enum", "Required"], ["openedOn", "Date", "Required"], ["client", "Reference", "Required"],
  ] },
  "brokerage-account": { key: "MCDR account number", fields: [
    ["number", "Text", "Required"], ["balance", "EGP money", "Optional"], ["client", "Reference", "Required"],
  ] },
  order: { key: "Brokerage order reference", fields: [
    ["value", "EGP money", "Required"], ["accountNumber", "Reference", "Required"],
  ] },
  "regulatory-return": { key: "Return reference", fields: [
    ["period", "Date", "Required"], ["portfolio", "Enum", "Required"],
  ] },
}

const PROPERTY_MAPPINGS = {
  nationalId: [
    ["postgres", "lms.loans.national_id"],
    ["iscore", "$.subject.national_id"],
    ["salesforce", "Client__c.National_Id__c"],
  ],
  phone: [["valify", "$.checks.identity.phone"]],
  birthDate: [["valify", "$.checks.identity.birth_date"]],
  address: [["valify", "$.checks.identity.address"]],
} satisfies Record<string, readonly (readonly [MarkId, string])[]>

function fieldSources(type: FinanceType, key: string): readonly (readonly [MarkId, string])[] {
  if (type === "client" && key === "nationalId") return PROPERTY_MAPPINGS.nationalId
  if (type === "client" && key === "phone") return PROPERTY_MAPPINGS.phone
  if (type === "kyc-check" && key === "birthDate") return PROPERTY_MAPPINGS.birthDate
  if (type === "kyc-check" && key === "address") return PROPERTY_MAPPINGS.address
  const distinct = new Map<string, readonly [MarkId, string]>()
  for (const record of RECORDS) {
    if (record.type !== type) continue
    for (const fact of record.facts) if (fact.target.endsWith(`.${key}`)) distinct.set(fact.path, [fact.source, fact.path])
  }
  return [...distinct.values()]
}

export function TypeDefinition({ type }: { readonly type: FinanceType }) {
  const { key, fields } = DEFINITIONS[type]
  const links = MODEL_LINKS.filter(([from, , to]) => type === from || type === to)
  const rules = RULES.filter(rule => rule.reads.includes(type))
  return <div className="space-y-3">
    <div className="grid gap-3 xl:grid-cols-2">
      <Panel label="Identity" icon={KeyRound}>
        <div className="space-y-1.5 px-5 pt-1 pb-4 text-sm"><p className="font-semibold">{key}</p>
          {type === "client" && <p className="text-muted-foreground">If no national ID can be matched, compare name and date of birth. Send probable matches to review.</p>}
        </div>
      </Panel>
      <Panel label="Data contract" icon={Shield} action={<Chip tone="amber">Quarantine on failure</Chip>}>
        <div className="space-y-1.5 px-5 pt-1 pb-4 text-sm">
          <p><span className="text-muted-foreground">Required </span>{fields.filter(([, , presence]) => presence === "Required").map(([key]) => key).join(", ")}</p>
          <p className="text-muted-foreground">212 records quarantined across the workspace.</p>
        </div>
      </Panel>
    </div>
    <BusinessPolicies type={type} />
    <Panel label="Properties" icon={TableProperties}>
      <div className="overflow-x-auto px-5 pb-4">
        <table className="w-full min-w-134 text-start text-sm">
          <thead className="text-xs text-muted-foreground"><tr>{["Property", "Type", "Presence", "Source fields"].map(label => <th scope="col" key={label} className="py-2 pe-3 text-start font-medium">{label}</th>)}</tr></thead>
          <tbody>{fields.map(([key, kind, presence]) => {
            const restricted = MARKING_FIELDS.some(({ field, type: owner }) => owner === type && field.endsWith(`.${key}`))
            return <tr key={key} className="border-t align-top">
              <th scope="row" className="py-2 pe-3 text-start"><code className="text-xs font-medium">{key}</code>
                {restricted && <Chip tone="amber" icon={Fingerprint} className="mt-1 block w-fit">Restricted</Chip>}
              </th>
              <td className="py-2 pe-3 text-xs">{kind}</td>
              <td className="py-2 pe-3 text-xs">{presence}</td>
              <td className="space-y-1 py-2">{fieldSources(type, key).map(([source, path]) => <div key={path} className="flex items-center gap-2">
                <Mark id={source} className="size-5 rounded-sm text-[7px]" /><code className="text-xs text-muted-foreground">{path}</code>
              </div>)}</td>
            </tr>
          })}</tbody>
        </table>
      </div>
    </Panel>
    <Panel label="Links" icon={GitBranch}>
      <ul className="divide-y px-5 pb-4">{links.map(([from, forward, to, inverse, outgoing, incoming]) => {
        const [other, relation, reverse, count, reverseCount] = type === from ? [to, forward, inverse, outgoing, incoming] : [from, inverse, forward, incoming, outgoing]
        return <li key={from + forward + to} className="flex flex-wrap items-center gap-2 py-2 text-xs">
          <span className="font-semibold">{relation}</span><ArrowRight className="size-3 text-tone-blue" />
          <Link to="/sources/$concept" params={{ concept: other }} className="inline-flex items-center gap-1.5 font-medium hover:text-tone-blue"><TypeTile type={other} />{TYPES[other].name}</Link>
          <Chip mono>{count}</Chip><span className="text-muted-foreground">Inverse · {reverse}</span><Chip mono>{reverseCount}</Chip>
        </li>
      })}</ul>
    </Panel>
    {(type === "loan" || type === "brokerage-account") && <Panel label="Lifecycle" icon={GitBranch}>
      <div className="space-y-2 px-5 pt-1 pb-4 text-sm">
        <p>{type === "loan" ? "Applied → KYC verified → I-Score pulled → Approved → Disbursed → Reported to FRA" : "e-KYC → Suitability → Account opened at MCDR → Funded → RM assigned"}</p>
        <Link to="/workflows/$process" params={{ process: type === "loan" ? "origination" : "brokerage" }} className="inline-flex items-center gap-1.5 text-xs font-semibold text-tone-blue hover:underline">View workflow conditions<ArrowRight className="size-3.5" /></Link>
        {type === "loan" && <p><Link to="/workflows/$process" params={{ process: "collections" }} className="inline-flex items-center gap-1.5 text-xs font-semibold text-tone-blue hover:underline">View repayment and collections<ArrowRight className="size-3.5" /></Link></p>}
      </div>
    </Panel>}
    {rules.length > 0 && <Panel label="Related rules" icon={Shield}>
      <ul className="divide-y px-5 pb-4">{rules.map(rule => <li key={rule.id} className="flex items-center justify-between gap-2 py-2 text-sm">
        <Link to="/guarantees" hash={rule.id} className="font-medium hover:text-tone-blue">{rule.title}</Link><Chip tone="green" dot>Check passed</Chip>
      </li>)}</ul>
    </Panel>}
  </div>
}
