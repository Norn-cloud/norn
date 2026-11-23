import { Link } from "@tanstack/react-router"
import { ArrowRight, Box, Database, Fingerprint, GitBranch, KeyRound, Radio, Shield, TableProperties, Zap } from "lucide-react"
import { useState } from "react"
import { Chip } from "@/components/chip"
import { KpiTile, StatStrip } from "@/components/kpi-tile"
import { Panel, Well } from "@/components/panel"
import { TableCard } from "@/components/table-card"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table"
import { cn } from "@/lib/utils"
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
  const { key, fields } = DEFINITIONS[type]
  const sources = [...new Set(fields.flatMap(([field]) => fieldSources(type, field).map(([source]) => source)))]
  const aggregate = AGGREGATES.find((item) => item.root === type || item.members.some((member) => member === type))
  const root = aggregate?.root === type
  const cmdCount = Object.values(COMMANDS).filter((c) => c.on === type).length
  const ruleCount = RULES.filter((rule) => rule.reads.includes(type) || Object.values(COMMANDS).some((c) => c.on === type && c.rules.some((id) => id === rule.id))).length

  return (
    <div className="space-y-6">
      <StatStrip>
        <KpiTile icon={KeyRound} label="Identifier" value={<span className="text-base font-medium">{key}</span>} />
        <KpiTile label="Aggregate role" value={<span className="text-base font-medium">{root ? "Aggregate root" : aggregate ? `${TYPES[aggregate.root].name} entity` : "Reference entity"}</span>} />
        <KpiTile label="Properties" value={fields.length} sub="typed fields" />
        <KpiTile label="Behavior" value={`${cmdCount} · ${ruleCount}`} sub="commands · guarding rules" />
      </StatStrip>

      <div className="grid gap-6 lg:grid-cols-2">
        <Panel icon={Box} label="Aggregate boundary" action={<Badge variant="secondary" className="font-normal">{root ? "Aggregate root" : "Entity"}</Badge>}>
          <Well className="space-y-3 p-4 text-meta">
            {root ? (
              <>
                <p className="text-muted-foreground">{TYPES[type].name} owns transactional consistency for these child entities:</p>
                <div className="flex flex-wrap gap-2">
                  {aggregate.members.map((member) => (
                    <Link key={member} to="/sources/$concept" params={{ concept: member }} className="inline-flex items-center gap-1.5 rounded-md border bg-muted/30 px-2 py-1 font-medium text-foreground hover:border-foreground/40">
                      <TypeTile type={member} className="size-5" />{TYPES[member].name}
                    </Link>
                  ))}
                  {aggregate.related.map((member) => (
                    <Link key={member} to="/policies/$scope" params={{ scope: member }} className="inline-flex items-center gap-1.5 rounded-md border bg-muted/30 px-2 py-1 text-muted-foreground hover:border-foreground/40 hover:text-foreground">
                      {AUXILIARY_TYPES[member].name}
                    </Link>
                  ))}
                </div>
              </>
            ) : aggregate ? (
              <p className="text-muted-foreground">
                Changes are coordinated through the{" "}
                <Link to="/sources/$concept" params={{ concept: aggregate.root }} className="font-medium text-foreground underline underline-offset-4">{TYPES[aggregate.root].name}</Link>{" "}
                aggregate root.
              </p>
            ) : (
              <p className="text-muted-foreground">Standalone reference entity linked by other aggregates.</p>
            )}
            {type === "client" && (
              <p className="border-t pt-3 text-xs text-muted-foreground">
                Resolution fallback: when no national ID matches, name and date of birth route probable matches to the identity review queue.
              </p>
            )}
          </Well>
        </Panel>

        <Panel icon={Shield} label="Data contract" action={<Chip tone="amber">Quarantine on failure</Chip>}>
          <Well className="space-y-3 p-4 text-meta">
            <div>
              <p className="text-xs text-muted-foreground">Required properties</p>
              <p className="mt-1 font-mono text-xs text-foreground">
                {fields.filter(([, , presence]) => presence === "Required").map(([k]) => k).join(", ")}
              </p>
            </div>
            <div className="border-t pt-3">
              <p className="mb-2 text-xs text-muted-foreground">Backing systems ({sources.length})</p>
              <ul className="flex flex-wrap gap-1.5">
                {sources.map((id) => (
                  <li key={id} className="inline-flex items-center gap-1.5 rounded-md border bg-muted/30 px-2 py-1 text-xs text-muted-foreground">
                    <Mark id={id} className="size-4 border-0 bg-transparent" />{MARKS[id].name}
                  </li>
                ))}
              </ul>
            </div>
            <div className="flex items-center justify-between border-t pt-3 text-xs text-muted-foreground">
              <span>212 rows quarantined workspace-wide</span>
              <Link to="/examples/$scene" params={{ scene: "dataset" }} className="inline-flex items-center gap-1 font-medium text-foreground hover:underline underline-offset-4">
                Dataset mapping<ArrowRight className="size-3 text-muted-foreground" />
              </Link>
            </div>
          </Well>
        </Panel>
      </div>
    </div>
  )
}

function SchemaAndLinks({ type }: { readonly type: FinanceType }) {
  const [showRaw, setShowRaw] = useState(false)
  const { fields } = DEFINITIONS[type]
  const objects = [...new Set(fields.map(([, kind]) => kind).filter(valueObject))]
  const links = MODEL_LINKS.filter(([from, , to]) => type === from || type === to)
  const aggregate = AGGREGATES.find((item) => item.root === type)

  return (
    <div className="space-y-6">
      <Panel
        icon={TableProperties}
        label={`Properties (${fields.length})`}
        action={
          <Button type="button" size="sm" variant="ghost" className="h-7 text-xs" onClick={() => setShowRaw((v) => !v)}>
            <Database className="size-3.5" />{showRaw ? "Hide raw source columns" : "Show raw source columns"}
          </Button>
        }
      >
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Property</TableHead>
                <TableHead>Value type</TableHead>
                <TableHead>Presence</TableHead>
                <TableHead>{showRaw ? "Source columns" : "Classification"}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {fields.map(([key, kind, presence]) => {
                const restricted = MARKING_FIELDS.some(({ field, type: owner }) => owner === type && field.endsWith(`.${key}`))
                return (
                  <TableRow key={key}>
                    <TableCell className="font-mono font-medium text-foreground">{key}</TableCell>
                    <TableCell>
                      {valueObject(kind) ? (
                        <a href={`#${kind}`} className="font-mono font-medium text-foreground underline underline-offset-4">{kind}</a>
                      ) : (
                        <span className="text-muted-foreground">{kind}</span>
                      )}
                    </TableCell>
                    <TableCell className="text-muted-foreground">{presence}</TableCell>
                    <TableCell>
                      {showRaw ? (
                        <div className="space-y-1">
                          {fieldSources(type, key).map(([source, path]) => (
                            <div key={path} className="flex items-center gap-1.5">
                              <Mark id={source} className="size-4 rounded-xs text-[7px]" />
                              <code className="text-xs text-muted-foreground">{path}</code>
                            </div>
                          ))}
                        </div>
                      ) : restricted ? (
                        <Chip tone="amber" icon={Fingerprint}>Restricted PII</Chip>
                      ) : (
                        <span className="text-muted-foreground">Standard</span>
                      )}
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </TableCard>
      </Panel>

      <div className="grid gap-6 lg:grid-cols-2">
        <Panel icon={GitBranch} label={`Relationships (${links.length + (aggregate?.related.length ?? 0)})`}>
          <Well>
            <ul className="divide-y px-4">
              {links.map(([from, forward, to, inverse, outgoing, incoming]) => {
                const [other, relation, reverse, count, reverseCount] = type === from ? [to, forward, inverse, outgoing, incoming] : [from, inverse, forward, incoming, outgoing]
                return (
                  <li key={from + forward + to} className="flex flex-wrap items-center gap-2 py-2.5 text-meta">
                    <span className="font-medium text-foreground">{relation}</span>
                    <ArrowRight className="size-3 text-muted-foreground" />
                    <Link to="/sources/$concept" params={{ concept: other }} className="inline-flex items-center gap-1.5 font-medium text-foreground hover:underline underline-offset-4">
                      <TypeTile type={other} className="size-5" />{TYPES[other].name}
                    </Link>
                    <code className="rounded-sm bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">{count}</code>
                    <span className="ms-auto text-xs text-muted-foreground">inverse: {reverse} ({reverseCount})</span>
                  </li>
                )
              })}
              {aggregate?.related.map((member) => (
                <li key={member} className="flex flex-wrap items-center gap-2 py-2.5 text-meta">
                  <span className="font-medium text-foreground">owns</span>
                  <ArrowRight className="size-3 text-muted-foreground" />
                  <Link to="/policies/$scope" params={{ scope: member }} className="font-medium text-foreground hover:underline underline-offset-4">{AUXILIARY_TYPES[member].name}</Link>
                  <code className="rounded-sm bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">0..*</code>
                </li>
              ))}
            </ul>
          </Well>
        </Panel>

        {objects.length > 0 && (
          <Panel icon={Box} label="Value objects">
            <Well>
              <dl className="divide-y px-4">
                {objects.map((kind) => (
                  <div key={kind} id={kind} className="py-2.5 text-meta">
                    <dt className="font-mono font-medium text-foreground">{kind}</dt>
                    <dd className="mt-0.5 text-xs text-muted-foreground">{VALUE_OBJECTS[kind]}</dd>
                  </div>
                ))}
              </dl>
            </Well>
          </Panel>
        )}
      </div>
    </div>
  )
}

function CommandsMasterDetail({ type }: { readonly type: FinanceType }) {
  const commands = Object.entries(COMMANDS).filter(([, command]) => command.on === type)
  const [selectedId, setSelectedId] = useState(commands[0]?.[0])
  if (!commands.length) {
    return <Panel icon={Zap} label="Commands"><Well className="p-6 text-meta text-muted-foreground">This object type has no declared commands in the finance model.</Well></Panel>
  }
  const [id, command] = commands.find(([k]) => k === selectedId) ?? commands[0]!
  return (
    <Panel icon={Zap} label="Commands" action={`${commands.length} declared`}>
      <Well className="grid lg:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="overflow-x-auto">
          <Table className="text-meta tabular-nums">
            <TableHeader>
              <TableRow>
                <TableHead>Command</TableHead>
                <TableHead>Issuer</TableHead>
                <TableHead className="text-end">Emits</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {commands.map(([key, item]) => {
                const active = key === id
                return (
                  <TableRow key={key} id={key} onClick={() => setSelectedId(key)} className={cn("cursor-pointer", active && "bg-muted/60")}>
                    <TableCell className={cn("font-mono font-medium text-foreground", active && "border-s-2 border-s-foreground")}>{key}</TableCell>
                    <TableCell className="text-muted-foreground">{item.issuer}</TableCell>
                    <TableCell className="text-end font-mono text-xs text-muted-foreground">{item.emits.join(", ") || "—"}</TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
        <aside className="divide-y border-t bg-surface-subtle/40 px-4 text-meta lg:border-s lg:border-t-0">
          <div className="py-3">
            <p className="text-xs text-muted-foreground">Command inspector</p>
            <code className="mt-1 block text-sm font-medium text-foreground">{id}</code>
          </div>
          <div className="py-3">
            <p className="text-xs text-muted-foreground">Issued by</p>
            <p className="mt-1 text-foreground">{command.issuer}</p>
          </div>
          <div className="space-y-2 py-3">
            <p className="text-xs text-muted-foreground">Guards</p>
            <p className="leading-relaxed text-foreground">{command.guard}</p>
            {command.rules.length > 0 && (
              <ul className="space-y-1 pt-1">
                {command.rules.map((ruleId) => (
                  <li key={ruleId}>
                    <Link to="/guarantees" hash={ruleId} className="inline-flex items-center gap-1.5 text-xs font-medium text-foreground hover:underline underline-offset-4">
                      <span>{RULES.find((r) => r.id === ruleId)?.title ?? ruleId}</span><ArrowRight className="size-3 text-muted-foreground" />
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div className="py-3">
            <p className="text-xs text-muted-foreground">Emits event</p>
            <div className="mt-1.5 flex flex-wrap gap-1.5">
              {command.emits.length > 0 ? command.emits.map((event) => (
                <Link key={event} to="/sources/$concept" params={{ concept: EVENTS[event].on }} search={{ tab: "events" }} hash={event}
                  className="rounded-sm border bg-card px-2 py-0.5 font-mono text-xs text-foreground hover:border-foreground/40">
                  {event}
                </Link>
              )) : <span className="text-xs text-muted-foreground">No public event</span>}
            </div>
          </div>
        </aside>
      </Well>
    </Panel>
  )
}

function EventsMasterDetail({ type }: { readonly type: FinanceType }) {
  const events = Object.entries(EVENTS).filter(([name, event]) => event.on === type || (type === "loan" && name === "InstallmentMissed") ||
    (type === "client" && (name === "HardshipOpened" || name === "KycExpired")))
  const [selectedId, setSelectedId] = useState(events[0]?.[0])
  if (!events.length) {
    return <Panel icon={Radio} label="Events"><Well className="p-6 text-meta text-muted-foreground">This object type emits no public events in the finance model.</Well></Panel>
  }
  const [id, event] = events.find(([k]) => k === selectedId) ?? events[0]!
  return (
    <Panel icon={Radio} label="Events" action={`${events.length} emitted`}>
      <Well className="grid lg:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="overflow-x-auto">
          <Table className="text-meta tabular-nums">
            <TableHeader>
              <TableRow>
                <TableHead>Event</TableHead>
                <TableHead>Source object</TableHead>
                <TableHead className="text-end">Subscribers</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {events.map(([key, item]) => {
                const active = key === id
                return (
                  <TableRow key={key} id={key} onClick={() => setSelectedId(key)} className={cn("cursor-pointer", active && "bg-muted/60")}>
                    <TableCell className={cn("font-mono font-medium text-foreground", active && "border-s-2 border-s-foreground")}>{key}</TableCell>
                    <TableCell className="text-muted-foreground">{TYPES[item.on].name}</TableCell>
                    <TableCell className="text-end text-muted-foreground">{item.reactions.length}</TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
        <aside className="divide-y border-t bg-surface-subtle/40 px-4 text-meta lg:border-s lg:border-t-0">
          <div className="py-3">
            <p className="text-xs text-muted-foreground">Event inspector</p>
            <code className="mt-1 block text-sm font-medium text-foreground">{id}</code>
          </div>
          <div className="py-3">
            <p className="mb-1.5 text-xs text-muted-foreground">Payload schema</p>
            <dl className="divide-y">
              {event.payload.map(([field, kind]) => (
                <div key={field} className="flex items-center justify-between gap-2 py-1.5">
                  <dt className="font-mono text-xs text-foreground">{field}</dt>
                  <dd className="font-mono text-xs text-muted-foreground">{kind}</dd>
                </div>
              ))}
            </dl>
          </div>
          <div className="py-3">
            <p className="mb-1.5 text-xs text-muted-foreground">Downstream reactions</p>
            <ul className="list-disc space-y-1 ps-4 text-xs text-foreground/90">
              {event.reactions.map((r) => <li key={r}>{r}</li>)}
            </ul>
          </div>
        </aside>
      </Well>
    </Panel>
  )
}

function BehaviorSection({ type, initial }: { readonly type: FinanceType; readonly initial: "lifecycle" | "commands" | "events" }) {
  const machine = MACHINES.find((item) => item.type === type)
  const [sub, setSub] = useState<"lifecycle" | "commands" | "events">(initial === "lifecycle" && !machine ? "commands" : initial)
  return (
    <div className="space-y-4">
      <div role="group" aria-label="Behavior views" className="inline-flex h-8 items-center rounded-md bg-muted p-0.5 text-xs">
        {machine && (
          <button type="button" aria-pressed={sub === "lifecycle"} onClick={() => setSub("lifecycle")}
            className="inline-flex h-7 items-center gap-1.5 rounded-[0.3125rem] px-2.5 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent">
            <GitBranch className="size-3.5" />Lifecycle
          </button>
        )}
        <button type="button" aria-pressed={sub === "commands"} onClick={() => setSub("commands")}
          className="inline-flex h-7 items-center gap-1.5 rounded-[0.3125rem] px-2.5 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent">
          <Zap className="size-3.5" />Commands
        </button>
        <button type="button" aria-pressed={sub === "events"} onClick={() => setSub("events")}
          className="inline-flex h-7 items-center gap-1.5 rounded-[0.3125rem] px-2.5 text-muted-foreground hover:text-foreground aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-xs dark:aria-pressed:bg-accent">
          <Radio className="size-3.5" />Events
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
    <div className="space-y-6">
      <Panel icon={Shield} label={`Guarding invariants (${rules.length})`}>
        <Well>
          {rules.length === 0 ? (
            <p className="p-4 text-meta text-muted-foreground">No finance rule directly reads this type.</p>
          ) : (
            <div className="divide-y">
              {rules.map((rule) => (
                <div key={rule.id} className="space-y-2.5 p-4">
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <Link to="/guarantees" hash={rule.id} className="text-meta font-medium text-foreground hover:underline underline-offset-4">{rule.title}</Link>
                    <Chip tone={rule.refusal ? "red" : "green"} dot>{rule.refusal ? "Refusal recorded" : "Check passed"}</Chip>
                  </div>
                  <RuleScope rule={rule} />
                </div>
              ))}
            </div>
          )}
        </Well>
      </Panel>
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
