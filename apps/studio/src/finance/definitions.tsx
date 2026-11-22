import { Link } from "@tanstack/react-router"
import { ArrowRight, Box, Fingerprint, GitBranch, KeyRound, Shield, TableProperties, Zap, Radio } from "lucide-react"
import { Chip } from "@/components/chip"
import { Panel } from "@/components/panel"
import { AGGREGATES, AUXILIARY_TYPES, COMMANDS, EVENTS, MACHINES, VALUE_OBJECTS, type ValueObject } from "./behavior.ts"
import { FinanceStateMachine } from "./behavior-pages.tsx"
import { MARKS, MODEL_LINKS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { MARKING_FIELDS, RULES, RuleScope } from "./operations.tsx"
import { BusinessPolicies } from "./policies.tsx"
import { RECORDS } from "./records.ts"
import { Mark, TypeTile } from "./workspace.tsx"

type FieldKind = ValueObject | "Text" | "Date" | "Enum" | "Reference"
type Property = readonly [key: string, type: FieldKind, presence: "Required" | "Optional"]
type Definition = { readonly key: string; readonly fields: readonly Property[] }

const DEFINITIONS: Record<FinanceType, Definition> = {
  client: { key: "National ID", fields: [
    ["nationalId", "NationalId", "Required"], ["name", "Text", "Required"], ["lmsClientId", "Text", "Optional"], ["payerRef", "Text", "Optional"], ["phone", "Text", "Optional"],
  ] },
  loan: { key: "Loan reference", fields: [
    ["status", "Enum", "Required"], ["dti", "Percent", "Optional"], ["principal", "Money", "Required"], ["clientId", "Reference", "Required"], ["installmentAmount", "Money", "Required"], ["disbursedOn", "Date", "Optional"],
  ] },
  installment: { key: "Installment reference", fields: [
    ["amount", "Money", "Required"], ["dpd", "DPD", "Optional"], ["dueDate", "Date", "Required"], ["loanId", "Reference", "Required"],
  ] },
  payment: { key: "Settlement reference", fields: [
    ["status", "Enum", "Required"], ["amount", "Money", "Required"], ["currency", "Enum", "Required"], ["payerRef", "Text", "Optional"], ["settledOn", "Date", "Required"], ["reportingWeek", "Date", "Optional"],
  ] },
  "credit-report": { key: "Bureau report reference", fields: [
    ["score", "IScore", "Required"], ["riskBand", "Enum", "Optional"], ["issuedOn", "Date", "Required"],
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
    ["status", "Enum", "Required"], ["marginRatio", "Percent", "Optional"], ["number", "Text", "Required"], ["balance", "Money", "Optional"], ["client", "Reference", "Required"],
  ] },
  order: { key: "Brokerage order reference", fields: [
    ["value", "Money", "Required"], ["accountNumber", "Reference", "Required"],
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
  if (type === "loan" && key === "dti") return [["postgres", "lms.underwriting.dti_pct"]]
  if (type === "installment" && key === "dpd") return [["postgres", "lms.installments.days_past_due"]]
  if (type === "brokerage-account" && key === "marginRatio") return [["egx", "$.account.margin_ratio_pct"]]
  if (key === "status" && (type === "loan" || type === "payment" || type === "brokerage-account")) {
    const sources = { loan: ["postgres", "lms.loans.status"], payment: ["fawry", "$.settlement.status"], "brokerage-account": ["mcdr", "$.account.status"] } as const
    return [sources[type]]
  }
  const distinct = new Map<string, readonly [MarkId, string]>()
  for (const record of RECORDS) {
    if (record.type !== type) continue
    for (const fact of record.facts) if (fact.target.endsWith(`.${key}`)) distinct.set(fact.path, [fact.source, fact.path])
  }
  return [...distinct.values()]
}

export const OBJECT_TABS = [
  { id: "overview", label: "Overview" }, { id: "properties", label: "Properties" }, { id: "links", label: "Links" },
  { id: "lifecycle", label: "Lifecycle" }, { id: "commands", label: "Commands" }, { id: "events", label: "Events" },
  { id: "rules", label: "Rules" }, { id: "policies", label: "Policies" }, { id: "records", label: "Records" },
] as const

export type ObjectTab = typeof OBJECT_TABS[number]["id"]
export const objectTab = (value: unknown): ObjectTab => OBJECT_TABS.find(tab => tab.id === value)?.id ?? "overview"

function valueObject(kind: string): kind is ValueObject {
  return Object.hasOwn(VALUE_OBJECTS, kind)
}

function Overview({ type }: { readonly type: FinanceType }) {
  const { key, fields } = DEFINITIONS[type]
  const sources = [...new Set(fields.flatMap(([field]) => fieldSources(type, field).map(([source]) => source)))]
  const aggregate = AGGREGATES.find(item => item.root === type || item.members.some(member => member === type))
  const root = aggregate?.root === type
  return <div className="grid gap-3 lg:grid-cols-2">
    <Panel label="Identity" icon={KeyRound}>
      <div className="space-y-1.5 px-5 pt-1 pb-4 text-sm"><p className="font-semibold">{key}</p>
        {type === "client" && <p className="text-muted-foreground">If no national ID can be matched, compare name and date of birth. Send probable matches to review.</p>}
      </div>
    </Panel>
    <Panel label="Aggregate boundary" icon={Box} action={<Chip tone={root ? "blue" : "gray"}>{root ? "Aggregate root" : "Entity"}</Chip>}>
      <div className="space-y-2 px-5 pt-1 pb-4 text-sm">
        {root ? <><p>{TYPES[type].name} owns changes to these entities.</p>
          <div className="flex flex-wrap gap-1.5">
            {aggregate.members.map(member => <Link key={member} to="/sources/$concept" params={{ concept: member }}><Chip tone={TYPES[member].tone}>{TYPES[member].name}</Chip></Link>)}
            {aggregate.related.map(member => <Link key={member} to="/policies/$scope" params={{ scope: member }}><Chip tone="gray">{AUXILIARY_TYPES[member].name}</Chip></Link>)}
          </div></>
          : aggregate ? <p>Changes belong to the <Link to="/sources/$concept" params={{ concept: aggregate.root }} className="font-semibold text-tone-blue hover:underline">{TYPES[aggregate.root].name}</Link> aggregate.</p>
            : <p>References other aggregates through links.</p>}
      </div>
    </Panel>
    <Panel label="Source systems" icon={TableProperties}>
      <ul className="flex flex-wrap gap-2 px-5 pt-1 pb-4">{sources.map(id => <li key={id} className="inline-flex items-center gap-1.5 rounded-lg border px-2 py-1 text-xs">
        <Mark id={id} className="size-6" />{MARKS[id].name}
      </li>)}</ul>
    </Panel>
    <Panel label="Data contract" icon={Shield} action={<Chip tone="amber">Quarantine on failure</Chip>}>
      <div className="space-y-1.5 px-5 pt-1 pb-4 text-sm">
        <p><span className="text-muted-foreground">Required </span>{fields.filter(([, , presence]) => presence === "Required").map(([key]) => key).join(", ")}</p>
        <p className="text-muted-foreground">212 records quarantined across the workspace.</p>
      </div>
    </Panel>
  </div>
}

function Properties({ type }: { readonly type: FinanceType }) {
  const { fields } = DEFINITIONS[type]
  const objects = [...new Set(fields.map(([, kind]) => kind).filter(valueObject))]
  return <div className="space-y-3">
    <Panel label="Properties" icon={TableProperties}>
      <div className="overflow-x-auto px-5 pb-4">
        <table className="w-full min-w-134 text-start text-sm">
          <thead className="text-xs text-muted-foreground"><tr>{["Property", "Type", "Presence", "Source fields"].map(label => <th scope="col" key={label} className="py-2 pe-3 text-start font-medium">{label}</th>)}</tr></thead>
          <tbody>{fields.map(([key, kind, presence]) => {
            const restricted = MARKING_FIELDS.some(({ field, type: owner }) => owner === type && field.endsWith(`.${key}`))
            return <tr key={key} className="border-t align-top">
              <th scope="row" className="py-2 pe-3 text-start"><code className="text-xs font-medium">{key}</code>
                {restricted && <Chip tone="amber" icon={Fingerprint} className="mt-1 w-fit">Restricted</Chip>}
              </th>
              <td className="py-2 pe-3 text-xs">{valueObject(kind) ? <Link to="/sources/$concept" params={{ concept: type }} search={{ tab: "properties" }} hash={kind} className="font-medium text-tone-blue hover:underline">{kind}</Link> : kind}</td>
              <td className="py-2 pe-3 text-xs">{presence}</td>
              <td className="space-y-1 py-2">{fieldSources(type, key).map(([source, path]) => <div key={path} className="flex items-center gap-2">
                <Mark id={source} className="size-5 rounded-sm text-[7px]" /><code className="text-xs text-muted-foreground">{path}</code>
              </div>)}</td>
            </tr>
          })}</tbody>
        </table>
      </div>
    </Panel>
    {objects.length > 0 && <Panel label="Value objects" icon={Box}>
      <dl className="divide-y px-5 pb-4">{objects.map(kind => <div key={kind} id={kind} className="py-2 text-sm">
        <dt className="font-mono font-semibold">{kind}</dt><dd className="text-muted-foreground">{VALUE_OBJECTS[kind]}</dd>
      </div>)}</dl>
    </Panel>}
  </div>
}

function TypeLinks({ type }: { readonly type: FinanceType }) {
  const links = MODEL_LINKS.filter(([from, , to]) => type === from || type === to)
  const aggregate = AGGREGATES.find(item => item.root === type)
  return <Panel label="Links" icon={GitBranch}>
    <ul className="divide-y px-5 pb-4">
      {links.map(([from, forward, to, inverse, outgoing, incoming]) => {
        const [other, relation, reverse, count, reverseCount] = type === from ? [to, forward, inverse, outgoing, incoming] : [from, inverse, forward, incoming, outgoing]
        return <li key={from + forward + to} className="flex flex-wrap items-center gap-2 py-2 text-xs">
          <span className="font-semibold">{relation}</span><ArrowRight className="size-3 text-tone-blue" />
          <Link to="/sources/$concept" params={{ concept: other }} className="inline-flex items-center gap-1.5 font-medium hover:text-tone-blue"><TypeTile type={other} />{TYPES[other].name}</Link>
          <Chip mono>{count}</Chip><span className="text-muted-foreground">Inverse · {reverse}</span><Chip mono>{reverseCount}</Chip>
        </li>
      })}
      {aggregate?.related.map(member => <li key={member} className="flex flex-wrap items-center gap-2 py-2 text-xs">
        <span className="font-semibold">owns</span><ArrowRight className="size-3 text-tone-blue" />
        <Link to="/policies/$scope" params={{ scope: member }} className="font-medium hover:text-tone-blue">{AUXILIARY_TYPES[member].name}</Link>
        <Chip mono>0..*</Chip><span className="text-muted-foreground">Inverse · belongs to</span><Chip mono>1</Chip>
      </li>)}
    </ul>
  </Panel>
}

function Commands({ type }: { readonly type: FinanceType }) {
  const commands = Object.entries(COMMANDS).filter(([, command]) => command.on === type)
  if (!commands.length) return <Panel label="Commands" icon={Zap}><p className="px-5 pb-4 text-sm text-muted-foreground">This type has no commands in the finance model.</p></Panel>
  return <div className="grid gap-3 lg:grid-cols-2">{commands.map(([id, command]) => <Panel key={id} id={id} label={<code>{id}</code>} icon={Zap}>
    <dl className="space-y-2 px-5 pt-1 pb-4 text-sm">
      <div><dt className="text-xs font-medium text-muted-foreground">Issued by</dt><dd>{command.issuer}</dd></div>
      <div><dt className="text-xs font-medium text-muted-foreground">Guards</dt><dd>{command.guard}</dd></div>
      {command.rules.length > 0 && <div><dt className="sr-only">Linked rules</dt><dd className="flex flex-wrap gap-1.5">
        {command.rules.map(id => <Link key={id} to="/guarantees" hash={id}><Chip tone="blue" icon={Shield}>{RULES.find(rule => rule.id === id)?.title ?? id}</Chip></Link>)}
      </dd></div>}
      <div><dt className="text-xs font-medium text-muted-foreground">Emits</dt><dd className="mt-0.5 flex flex-wrap gap-1.5">
        {command.emits.length > 0 ? command.emits.map(event => <Link key={event} to="/sources/$concept" params={{ concept: EVENTS[event].on }} search={{ tab: "events" }} hash={event}><Chip tone="violet" mono>{event}</Chip></Link>) : <span className="text-xs text-muted-foreground">No public event</span>}
      </dd></div>
    </dl>
  </Panel>)}</div>
}

function Events({ type }: { readonly type: FinanceType }) {
  const events = Object.entries(EVENTS).filter(([name, event]) => event.on === type || type === "loan" && name === "InstallmentMissed" ||
    type === "client" && (name === "HardshipOpened" || name === "KycExpired"))
  if (!events.length) return <Panel label="Events" icon={Radio}><p className="px-5 pb-4 text-sm text-muted-foreground">This type has no public events in the finance model.</p></Panel>
  return <div className="grid gap-3 lg:grid-cols-2">{events.map(([id, event]) => <Panel key={id} id={id} label={<code>{id}</code>} icon={Radio} action={<Chip tone="violet">{TYPES[event.on].name}</Chip>}>
    <div className="space-y-3 px-5 pt-1 pb-4 text-sm">
      <div><h3 className="text-xs font-medium text-muted-foreground">Payload</h3>
        <dl className="mt-1 divide-y rounded-lg border px-2">{event.payload.map(([field, kind]) => <div key={field} className="flex items-center justify-between gap-2 py-1">
          <dt className="font-mono text-xs">{field}</dt><dd><Chip mono tone={valueObject(kind) ? "blue" : "gray"}>{kind}</Chip></dd>
        </div>)}</dl>
      </div>
      <div><h3 className="text-xs font-medium text-muted-foreground">Who reacts</h3><ul className="mt-1 list-disc space-y-1 ps-4 text-xs">{event.reactions.map(reaction => <li key={reaction}>{reaction}</li>)}</ul></div>
    </div>
  </Panel>)}</div>
}

function TypeRules({ type }: { readonly type: FinanceType }) {
  const rules = RULES.filter(rule => rule.reads.includes(type) || Object.values(COMMANDS).some(command => command.on === type && command.rules.some(id => id === rule.id)))
  if (!rules.length) return <Panel label="Rules" icon={Shield}><p className="px-5 pb-4 text-sm text-muted-foreground">No finance rule directly reads this type.</p></Panel>
  return <div className="grid gap-3 lg:grid-cols-2">{rules.map(rule => <Panel key={rule.id} label={rule.title} icon={Shield} action={<Chip tone="green" dot>Check passed</Chip>}>
    <div className="space-y-2 px-5 pt-1 pb-4">
      <RuleScope rule={rule} />
      <Link to="/guarantees" hash={rule.id} className="inline-flex items-center gap-1.5 text-xs font-medium text-tone-blue hover:underline">Inspect rule and evidence<ArrowRight className="size-3.5" /></Link>
    </div>
  </Panel>)}</div>
}

export function TypeDefinition({ type, tab }: { readonly type: FinanceType; readonly tab: Exclude<ObjectTab, "records"> }) {
  switch (tab) {
    case "overview": return <Overview type={type} />
    case "properties": return <Properties type={type} />
    case "links": return <TypeLinks type={type} />
    case "lifecycle": {
      const machine = MACHINES.find(item => item.type === type)
      return machine ? <FinanceStateMachine key={type} machine={machine} />
        : <Panel label="Lifecycle" icon={GitBranch}><p className="px-5 pb-4 text-sm text-muted-foreground">This type has no independent state machine in the finance model.</p></Panel>
    }
    case "commands": return <Commands type={type} />
    case "events": return <Events type={type} />
    case "rules": return <TypeRules type={type} />
    case "policies": return <BusinessPolicies type={type} />
  }
}
