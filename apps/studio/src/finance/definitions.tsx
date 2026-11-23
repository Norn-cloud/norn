import { Link } from "@tanstack/react-router"
import { ArrowRight, Box, Database, Fingerprint, GitBranch, KeyRound, Radio, Shield, SlidersHorizontal, TableProperties, Zap } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { cn } from "@/lib/utils"
import { AGGREGATES, AUXILIARY_TYPES, COMMANDS, EVENTS, MACHINES, VALUE_OBJECTS, type ValueObject } from "./behavior.ts"
import { FinanceStateMachine } from "./behavior-pages.tsx"
import { MARKS, MODEL_LINKS, TYPES, type FinanceType, type MarkId } from "./data.ts"
import { MARKING_FIELDS, RULES, RuleScope } from "./operations.tsx"
import { BusinessPolicies } from "./policies.tsx"
import { RECORDS } from "./records.ts"
import { Mark, TypeTile, useFinanceMode } from "./workspace.tsx"

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

/** Consolidated 5 enterprise tabs (Palantir OMA / Snowsight style), with full legacy query-param mapping. */
export const PRIMARY_TABS = [
  { id: "overview", label: "Overview" },
  { id: "schema", label: "Schema & Links" },
  { id: "behavior", label: "Behavior" },
  { id: "governance", label: "Rules & Policies" },
  { id: "records", label: "Records" },
] as const

export type PrimaryTab = typeof PRIMARY_TABS[number]["id"]

export const OBJECT_TABS = [
  { id: "overview", label: "Overview" }, { id: "schema", label: "Schema & Links" },
  { id: "properties", label: "Properties" }, { id: "links", label: "Links" },
  { id: "behavior", label: "Behavior" }, { id: "lifecycle", label: "Lifecycle" },
  { id: "commands", label: "Commands" }, { id: "events", label: "Events" },
  { id: "governance", label: "Rules & Policies" }, { id: "rules", label: "Rules" },
  { id: "policies", label: "Policies" }, { id: "records", label: "Records" },
] as const

export type ObjectTab = typeof OBJECT_TABS[number]["id"]
export const objectTab = (value: unknown): ObjectTab => OBJECT_TABS.find((tab) => tab.id === value)?.id ?? "overview"

export function primaryTabOf(tab: ObjectTab): PrimaryTab {
  switch (tab) {
    case "overview": return "overview"
    case "schema": case "properties": case "links": return "schema"
    case "behavior": case "lifecycle": case "commands": case "events": return "behavior"
    case "governance": case "rules": case "policies": return "governance"
    case "records": return "records"
  }
}

function valueObject(kind: string): kind is ValueObject {
  return Object.hasOwn(VALUE_OBJECTS, kind)
}

function Overview({ type }: { readonly type: FinanceType }) {
  const { mode, setMode } = useFinanceMode()
  const { key, fields } = DEFINITIONS[type]
  const sources = [...new Set(fields.flatMap(([field]) => fieldSources(type, field).map(([source]) => source)))]
  const aggregate = AGGREGATES.find((item) => item.root === type || item.members.some((member) => member === type))
  const root = aggregate?.root === type
  const cmdCount = Object.values(COMMANDS).filter((c) => c.on === type).length
  const ruleCount = RULES.filter((rule) => rule.reads.includes(type) || Object.values(COMMANDS).some((c) => c.on === type && c.rules.some((id) => id === rule.id))).length

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 divide-y divide-border overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-4 lg:divide-x lg:divide-y-0">
        <div className="px-4 py-3">
          <p className="text-[11px] text-muted-foreground">Identifier</p>
          <p className="mt-1 flex items-center gap-1.5 text-sm font-semibold text-foreground"><KeyRound className="size-3.5 text-muted-foreground" />{key}</p>
        </div>
        <div className="px-4 py-3">
          <p className="text-[11px] text-muted-foreground">Aggregate role</p>
          <p className="mt-1 text-sm font-semibold text-foreground">
            {root ? "Aggregate root" : aggregate ? `${TYPES[aggregate.root].name} entity` : "Reference entity"}
          </p>
        </div>
        <div className="px-4 py-3">
          <p className="text-[11px] text-muted-foreground">Properties</p>
          <p className="mt-1 text-sm font-semibold tabular-nums text-foreground">{fields.length} typed fields</p>
        </div>
        <div className="px-4 py-3">
          <p className="text-[11px] text-muted-foreground">Behavior &amp; guardrails</p>
          <p className="mt-1 text-sm font-semibold tabular-nums text-foreground">{cmdCount} commands · {ruleCount} rules</p>
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <section className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="flex items-center justify-between border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
            <span className="inline-flex items-center gap-1.5"><Box className="size-3.5 text-muted-foreground" />Aggregate boundary</span>
            <Chip tone={root ? "blue" : "gray"}>{root ? "Aggregate root" : "Entity"}</Chip>
          </header>
          <div className="space-y-2.5 p-4 text-xs">
            {root ? (
              <>
                <p className="text-foreground">{TYPES[type].name} owns transactional consistency for these child entities:</p>
                <div className="flex flex-wrap gap-1.5">
                  {aggregate.members.map((member) => (
                    <Link key={member} to="/sources/$concept" params={{ concept: member }} className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface-subtle px-2 py-1 font-medium hover:border-data-accent hover:text-tone-blue">
                      <TypeTile type={member} className="size-5" />{TYPES[member].name}
                    </Link>
                  ))}
                  {aggregate.related.map((member) => (
                    <Link key={member} to="/policies/$scope" params={{ scope: member }} className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface-subtle px-2 py-1 font-medium text-muted-foreground hover:border-data-accent hover:text-foreground">
                      {AUXILIARY_TYPES[member].name}
                    </Link>
                  ))}
                </div>
              </>
            ) : aggregate ? (
              <p className="text-muted-foreground">
                Changes are coordinated through the{" "}
                <Link to="/sources/$concept" params={{ concept: aggregate.root }} className="font-semibold text-tone-blue hover:underline">{TYPES[aggregate.root].name}</Link>{" "}
                aggregate root.
              </p>
            ) : (
              <p className="text-muted-foreground">Standalone reference entity linked by other aggregates.</p>
            )}
            {type === "client" && (
              <p className="border-t border-border pt-2.5 text-muted-foreground">
                Resolution fallback: when no national ID matches, name and date of birth route probable matches to the identity review queue.
              </p>
            )}
          </div>
        </section>

        <section className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="flex items-center justify-between border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
            <span className="inline-flex items-center gap-1.5"><Shield className="size-3.5 text-muted-foreground" />Data contract</span>
            <Chip tone="amber">Quarantine on failure</Chip>
          </header>
          <div className="space-y-3 p-4 text-xs">
            <div>
              <p className="text-[11px] text-muted-foreground">Required properties</p>
              <p className="mt-1 font-mono text-foreground">
                {fields.filter(([, , presence]) => presence === "Required").map(([k]) => k).join(", ")}
              </p>
            </div>
            {mode === "data" ? (
              <div className="border-t border-border pt-2.5">
                <p className="mb-1.5 text-[11px] text-muted-foreground">Backing source systems</p>
                <ul className="flex flex-wrap gap-1.5">
                  {sources.map((id) => (
                    <li key={id} className="inline-flex items-center gap-1.5 rounded-md border border-border bg-surface-subtle px-2 py-1 text-[11px]">
                      <Mark id={id} className="size-4" />{MARKS[id].name}
                    </li>
                  ))}
                </ul>
              </div>
            ) : (
              <div className="flex items-center justify-between border-t border-border pt-2.5 text-muted-foreground">
                <span>Normalized from {sources.length} backing {sources.length === 1 ? "system" : "systems"} · 212 rows quarantined workspace-wide</span>
                <Link to="/examples/$scene" params={{ scene: "dataset" }} onClick={() => setMode("data")} className="inline-flex items-center gap-1 font-medium text-tone-blue hover:underline">
                  Mapping<ArrowRight className="size-3" />
                </Link>
              </div>
            )}
          </div>
        </section>
      </div>
    </div>
  )
}

function SchemaAndLinks({ type }: { readonly type: FinanceType }) {
  const { mode, setMode } = useFinanceMode()
  const { fields } = DEFINITIONS[type]
  const objects = [...new Set(fields.map(([, kind]) => kind).filter(valueObject))]
  const links = MODEL_LINKS.filter(([from, , to]) => type === from || type === to)
  const aggregate = AGGREGATES.find((item) => item.root === type)

  return (
    <div className="space-y-4">
      <section className="overflow-hidden rounded-lg border border-border bg-background">
        <header className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-surface-subtle px-4 py-2">
          <span className="inline-flex items-center gap-1.5 text-xs font-semibold"><TableProperties className="size-3.5 text-muted-foreground" />Properties ({fields.length})</span>
          {mode === "domain" && (
            <button type="button" onClick={() => setMode("data")} className="inline-flex items-center gap-1 text-[11px] font-medium text-muted-foreground hover:text-tone-blue">
              <Database className="size-3" />Show raw source columns (Data Engineering)
            </button>
          )}
        </header>
        <div className="overflow-x-auto">
          <table className="w-full min-w-120 text-start text-xs">
            <thead>
              <tr className="border-b border-border bg-surface-subtle/40 text-muted-foreground">
                <th scope="col" className="px-4 py-2 text-start font-medium">Property</th>
                <th scope="col" className="px-3 py-2 text-start font-medium">Value type</th>
                <th scope="col" className="px-3 py-2 text-start font-medium">Presence</th>
                <th scope="col" className="px-4 py-2 text-start font-medium">{mode === "data" ? "Source columns" : "Classification"}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {fields.map(([key, kind, presence]) => {
                const restricted = MARKING_FIELDS.some(({ field, type: owner }) => owner === type && field.endsWith(`.${key}`))
                return (
                  <tr key={key} className="hover:bg-surface-subtle/60">
                    <th scope="row" className="px-4 py-2.5 text-start font-mono font-medium text-foreground">{key}</th>
                    <td className="px-3 py-2.5">
                      {valueObject(kind) ? (
                        <a href={`#${kind}`} className="font-mono font-medium text-tone-blue hover:underline">{kind}</a>
                      ) : (
                        <span className="text-muted-foreground">{kind}</span>
                      )}
                    </td>
                    <td className="px-3 py-2.5 text-muted-foreground">{presence}</td>
                    <td className="px-4 py-2.5">
                      {mode === "data" ? (
                        <div className="space-y-1">
                          {fieldSources(type, key).map(([source, path]) => (
                            <div key={path} className="flex items-center gap-1.5">
                              <Mark id={source} className="size-4 rounded-xs text-[7px]" />
                              <code className="text-[11px] text-muted-foreground">{path}</code>
                            </div>
                          ))}
                        </div>
                      ) : restricted ? (
                        <Chip tone="amber" icon={Fingerprint}>Restricted PII</Chip>
                      ) : (
                        <span className="text-muted-foreground">Standard</span>
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      </section>

      <div className="grid gap-4 lg:grid-cols-2">
        <section className="overflow-hidden rounded-lg border border-border bg-background">
          <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
            <span className="inline-flex items-center gap-1.5"><GitBranch className="size-3.5 text-muted-foreground" />Relationships ({links.length + (aggregate?.related.length ?? 0)})</span>
          </header>
          <ul className="divide-y divide-border px-4">
            {links.map(([from, forward, to, inverse, outgoing, incoming]) => {
              const [other, relation, reverse, count, reverseCount] = type === from ? [to, forward, inverse, outgoing, incoming] : [from, inverse, forward, incoming, outgoing]
              return (
                <li key={from + forward + to} className="flex flex-wrap items-center gap-2 py-2.5 text-xs">
                  <span className="font-medium text-foreground">{relation}</span>
                  <ArrowRight className="size-3 text-muted-foreground" />
                  <Link to="/sources/$concept" params={{ concept: other }} className="inline-flex items-center gap-1.5 font-medium text-tone-blue hover:underline">
                    <TypeTile type={other} className="size-5" />{TYPES[other].name}
                  </Link>
                  <code className="rounded-xs bg-muted px-1 text-[10px] text-muted-foreground">{count}</code>
                  <span className="ms-auto text-[11px] text-muted-foreground">inverse: {reverse} ({reverseCount})</span>
                </li>
              )
            })}
            {aggregate?.related.map((member) => (
              <li key={member} className="flex flex-wrap items-center gap-2 py-2.5 text-xs">
                <span className="font-medium text-foreground">owns</span>
                <ArrowRight className="size-3 text-muted-foreground" />
                <Link to="/policies/$scope" params={{ scope: member }} className="font-medium text-tone-blue hover:underline">{AUXILIARY_TYPES[member].name}</Link>
                <code className="rounded-xs bg-muted px-1 text-[10px] text-muted-foreground">0..*</code>
              </li>
            ))}
          </ul>
        </section>

        {objects.length > 0 && (
          <section className="overflow-hidden rounded-lg border border-border bg-background">
            <header className="border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
              <span className="inline-flex items-center gap-1.5"><Box className="size-3.5 text-muted-foreground" />Value objects</span>
            </header>
            <dl className="divide-y divide-border px-4">
              {objects.map((kind) => (
                <div key={kind} id={kind} className="py-2.5 text-xs">
                  <dt className="font-mono font-semibold text-foreground">{kind}</dt>
                  <dd className="mt-0.5 text-muted-foreground">{VALUE_OBJECTS[kind]}</dd>
                </div>
              ))}
            </dl>
          </section>
        )}
      </div>
    </div>
  )
}

function CommandsMasterDetail({ type }: { readonly type: FinanceType }) {
  const commands = Object.entries(COMMANDS).filter(([, command]) => command.on === type)
  const [selectedId, setSelectedId] = useState(commands[0]?.[0])
  if (!commands.length) {
    return <div className="rounded-lg border border-border bg-background p-6 text-xs text-muted-foreground">This object type has no declared commands in the finance model.</div>
  }
  const [id, command] = commands.find(([k]) => k === selectedId) ?? commands[0]!
  return (
    <div className="grid overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-[minmax(0,1fr)_22rem]">
      <div className="overflow-x-auto">
        <table className="w-full text-start text-xs">
          <thead>
            <tr className="border-b border-border bg-surface-subtle text-muted-foreground">
              <th className="px-4 py-2 text-start font-medium">Command</th>
              <th className="px-3 py-2 text-start font-medium">Issuer</th>
              <th className="px-4 py-2 text-end font-medium">Emits</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {commands.map(([key, item]) => {
              const active = key === id
              return (
                <tr key={key} id={key} onClick={() => setSelectedId(key)} className={cn("cursor-pointer hover:bg-surface-subtle", active && "bg-tone-blue/[0.06]")}>
                  <td className={cn("px-4 py-2.5 font-mono font-semibold text-tone-blue", active && "border-s-2 border-s-data-accent")}>{key}</td>
                  <td className="px-3 py-2.5 text-muted-foreground">{item.issuer}</td>
                  <td className="px-4 py-2.5 text-end font-mono text-[11px] text-muted-foreground">{item.emits.join(", ") || "—"}</td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
      <aside className="space-y-3.5 border-t border-border bg-surface-subtle/35 p-4 text-xs lg:border-s lg:border-t-0">
        <div>
          <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Command Inspector</p>
          <code className="mt-0.5 block text-sm font-semibold text-tone-blue">{id}</code>
        </div>
        <div className="border-t border-border pt-3">
          <p className="text-[11px] text-muted-foreground">Issued by</p>
          <p className="mt-0.5 font-medium text-foreground">{command.issuer}</p>
        </div>
        <div className="border-t border-border pt-3">
          <p className="text-[11px] text-muted-foreground">Guards</p>
          <p className="mt-0.5 leading-relaxed text-foreground">{command.guard}</p>
          {command.rules.length > 0 && (
            <div className="mt-2 flex flex-col gap-1">
              {command.rules.map((ruleId) => (
                <Link key={ruleId} to="/guarantees" hash={ruleId} className="inline-flex items-center justify-between rounded-md border border-border bg-background px-2.5 py-1.5 font-medium text-tone-blue hover:border-data-accent">
                  <span>{RULES.find((r) => r.id === ruleId)?.title ?? ruleId}</span><ArrowRight className="size-3" />
                </Link>
              ))}
            </div>
          )}
        </div>
        <div className="border-t border-border pt-3">
          <p className="text-[11px] text-muted-foreground">Emits event</p>
          <div className="mt-1 flex flex-wrap gap-1.5">
            {command.emits.length > 0 ? command.emits.map((event) => (
              <Link key={event} to="/sources/$concept" params={{ concept: EVENTS[event].on }} search={{ tab: "events" }} hash={event}
                className="rounded-sm border border-border bg-background px-2 py-0.5 font-mono text-[11px] font-medium text-tone-blue hover:border-data-accent">
                {event}
              </Link>
            )) : <span className="text-muted-foreground">No public event</span>}
          </div>
        </div>
      </aside>
    </div>
  )
}

function EventsMasterDetail({ type }: { readonly type: FinanceType }) {
  const events = Object.entries(EVENTS).filter(([name, event]) => event.on === type || (type === "loan" && name === "InstallmentMissed") ||
    (type === "client" && (name === "HardshipOpened" || name === "KycExpired")))
  const [selectedId, setSelectedId] = useState(events[0]?.[0])
  if (!events.length) {
    return <div className="rounded-lg border border-border bg-background p-6 text-xs text-muted-foreground">This object type emits no public events in the finance model.</div>
  }
  const [id, event] = events.find(([k]) => k === selectedId) ?? events[0]!
  return (
    <div className="grid overflow-hidden rounded-lg border border-border bg-background lg:grid-cols-[minmax(0,1fr)_22rem]">
      <div className="overflow-x-auto">
        <table className="w-full text-start text-xs">
          <thead>
            <tr className="border-b border-border bg-surface-subtle text-muted-foreground">
              <th className="px-4 py-2 text-start font-medium">Event</th>
              <th className="px-3 py-2 text-start font-medium">Source object</th>
              <th className="px-4 py-2 text-end font-medium">Subscribers</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {events.map(([key, item]) => {
              const active = key === id
              return (
                <tr key={key} id={key} onClick={() => setSelectedId(key)} className={cn("cursor-pointer hover:bg-surface-subtle", active && "bg-tone-blue/[0.06]")}>
                  <td className={cn("px-4 py-2.5 font-mono font-semibold text-foreground", active && "border-s-2 border-s-data-accent text-tone-blue")}>{key}</td>
                  <td className="px-3 py-2.5 text-muted-foreground">{TYPES[item.on].name}</td>
                  <td className="px-4 py-2.5 text-end tabular-nums text-muted-foreground">{item.reactions.length}</td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
      <aside className="space-y-3.5 border-t border-border bg-surface-subtle/35 p-4 text-xs lg:border-s lg:border-t-0">
        <div>
          <p className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">Event Inspector</p>
          <code className="mt-0.5 block text-sm font-semibold text-foreground">{id}</code>
        </div>
        <div className="border-t border-border pt-3">
          <p className="mb-1.5 text-[11px] text-muted-foreground">Payload schema</p>
          <dl className="divide-y divide-border rounded-md border border-border bg-background px-2.5">
            {event.payload.map(([field, kind]) => (
              <div key={field} className="flex items-center justify-between gap-2 py-1.5">
                <dt className="font-mono text-[11px] text-foreground">{field}</dt>
                <dd className="font-mono text-[10px] text-muted-foreground">{kind}</dd>
              </div>
            ))}
          </dl>
        </div>
        <div className="border-t border-border pt-3">
          <p className="mb-1 text-[11px] text-muted-foreground">Downstream reactions</p>
          <ul className="list-disc space-y-1 ps-4 text-foreground/90">
            {event.reactions.map((r) => <li key={r}>{r}</li>)}
          </ul>
        </div>
      </aside>
    </div>
  )
}

function BehaviorSection({ type, initial }: { readonly type: FinanceType; readonly initial: "lifecycle" | "commands" | "events" }) {
  const machine = MACHINES.find((item) => item.type === type)
  const [sub, setSub] = useState<"lifecycle" | "commands" | "events">(initial === "lifecycle" && !machine ? "commands" : initial)
  return (
    <div className="space-y-3">
      <div role="group" aria-label="Behavior views" className="inline-flex rounded-md border border-border bg-surface-subtle p-0.5 text-xs">
        {machine && (
          <button type="button" onClick={() => setSub("lifecycle")} className={cn("inline-flex items-center gap-1.5 rounded-sm px-2.5 py-1 font-medium", sub === "lifecycle" ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground")}>
            <GitBranch className="size-3" />Lifecycle
          </button>
        )}
        <button type="button" onClick={() => setSub("commands")} className={cn("inline-flex items-center gap-1.5 rounded-sm px-2.5 py-1 font-medium", sub === "commands" ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground")}>
          <Zap className="size-3" />Commands
        </button>
        <button type="button" onClick={() => setSub("events")} className={cn("inline-flex items-center gap-1.5 rounded-sm px-2.5 py-1 font-medium", sub === "events" ? "bg-background text-foreground shadow-xs ring-1 ring-border" : "text-muted-foreground hover:text-foreground")}>
          <Radio className="size-3" />Events
        </button>
      </div>
      {sub === "lifecycle" && machine ? <FinanceStateMachine key={type} machine={machine} />
        : sub === "events" ? <EventsMasterDetail key={type} type={type} />
          : <CommandsMasterDetail key={type} type={type} />}
    </div>
  )
}

function GovernanceSection({ type }: { readonly type: FinanceType }) {
  const rules = RULES.filter((rule) => rule.reads.includes(type) || Object.values(COMMANDS).some((command) => command.on === type && command.rules.some((id) => id === rule.id)))
  return (
    <div className="space-y-4">
      <section className="overflow-hidden rounded-lg border border-border bg-background">
        <header className="flex items-center justify-between border-b border-border bg-surface-subtle px-4 py-2 text-xs font-semibold">
          <span className="inline-flex items-center gap-1.5"><Shield className="size-3.5 text-muted-foreground" />Guarding invariants ({rules.length})</span>
        </header>
        {rules.length === 0 ? (
          <p className="p-4 text-xs text-muted-foreground">No finance rule directly reads this type.</p>
        ) : (
          <div className="divide-y divide-border">
            {rules.map((rule) => (
              <div key={rule.id} className="space-y-2 p-4">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <Link to="/guarantees" hash={rule.id} className="text-xs font-semibold text-foreground hover:text-tone-blue">{rule.title}</Link>
                  <Chip tone={rule.refusal ? "red" : "green"} dot>{rule.refusal ? "Refusal recorded" : "Check passed"}</Chip>
                </div>
                <RuleScope rule={rule} />
              </div>
            ))}
          </div>
        )}
      </section>
      <BusinessPolicies type={type} />
    </div>
  )
}

export function TypeDefinition({ type, tab }: { readonly type: FinanceType; readonly tab: Exclude<ObjectTab, "records"> }) {
  switch (primaryTabOf(tab)) {
    case "overview": return <Overview type={type} />
    case "schema": return <SchemaAndLinks type={type} />
    case "behavior": return (
      <BehaviorSection
        key={`${type}-${tab}`}
        type={type}
        initial={tab === "events" ? "events" : tab === "commands" ? "commands" : "lifecycle"}
      />
    )
    case "governance": return <GovernanceSection type={type} />
  }
}
