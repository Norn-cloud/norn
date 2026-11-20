import { Fingerprint, History, Scale, ShieldCheck, TriangleAlert } from "lucide-react"
import { Chip, Dot } from "@/components/chip"
import { cn } from "@/lib/utils"
import { BORROWER, CHANGES, FEEDS, RECORD, ROLES, RULES, SYSTEMS, type SystemId } from "./lending.ts"
import { Person, RoleRow, Slide, SourceChip } from "./slide.tsx"

const ORDER: readonly SystemId[] = ["fineract", "payments", "crm", "bureau"]

/** A rail section: a small muted heading over a soft card. */
function RailCard({ icon: Icon, label, children }: { readonly icon: typeof Scale; readonly label: string; readonly children: React.ReactNode }) {
  return (
    <section className="rounded-2xl bg-background px-4 pt-3 pb-3.5 shadow-[var(--elevation-1)] ring-1 ring-foreground/5">
      <h2 className="flex items-center gap-1.5 pb-2.5 text-meta font-medium text-muted-foreground"><Icon aria-hidden className="size-3.5" />{label}</h2>
      {children}
    </section>
  )
}

/** Slide 2: the borrower in the middle, four systems flowing in, and what governs the record beside it. */
export function BorrowerSlide() {
  return (
    <Slide eyebrow="The Weave · one live record" title="One borrower. Every system." preview="The side rail's layout is a preview."
      lead="Four systems flow into one live record. Every value shows where it came from, who may touch it, and what changed.">
      <div className="grid grid-cols-[280px_120px_minmax(0,1fr)_400px] items-center gap-0">
        {/* Sources */}
        <div className="flex flex-col gap-4">
          {ORDER.map((id) => (
            <div key={id} className="rounded-2xl bg-background px-4 py-3.5 shadow-[var(--elevation-node)] ring-1 ring-foreground/5">
              <div className="flex items-center gap-2">
                <Dot tone={SYSTEMS[id].tone} className="size-2 [&>span]:size-2" />
                <span className="text-[1.0625rem] font-semibold tracking-tight">{SYSTEMS[id].name}</span>
              </div>
              <div className="pt-0.5 text-meta text-muted-foreground">{SYSTEMS[id].role}</div>
              <div className="pt-1.5 text-xs text-muted-foreground">{FEEDS[id]}</div>
            </div>
          ))}
        </div>
        {/* Flows */}
        <svg aria-hidden viewBox="0 0 120 400" preserveAspectRatio="none" className="h-[420px] w-full overflow-visible">
          {[50, 150, 250, 350].map((y, i) => (
            <g key={y}>
              <path d={`M 0 ${y} C 60 ${y}, 60 200, 120 200`} fill="none" stroke={`var(--tone-${["blue", "teal", "violet", "amber"][i]})`} strokeOpacity="0.55" strokeWidth="2" strokeDasharray="5 5" vectorEffect="non-scaling-stroke" />
              <circle cx="0" cy={y} r="4" fill={`var(--tone-${["blue", "teal", "violet", "amber"][i]})`} />
            </g>
          ))}
          <circle cx="120" cy="200" r="5" fill="var(--action)" />
        </svg>
        {/* The record */}
        <section className="rounded-[22px] bg-background p-2.5 shadow-[var(--elevation-node-hover)] ring-1 ring-foreground/5">
          <div className="rounded-[16px] bg-[color-mix(in_oklab,var(--action)_5%,var(--background))] px-6 pt-5 pb-4 ring-1 ring-foreground/[0.04]">
            <div className="flex items-center gap-4">
              <Person initials={BORROWER.initials} tone="violet" size="lg" />
              <div className="min-w-0 flex-1">
                <div className="text-[1.5rem] font-semibold tracking-tight">{BORROWER.name}</div>
                <div className="text-meta text-muted-foreground">Borrower · {BORROWER.detail}</div>
              </div>
              <Chip tone="red" icon={TriangleAlert} className="h-6 px-2">Flagged</Chip>
            </div>
          </div>
          <dl className="divide-y divide-border/70 px-4 pt-1">
            {RECORD.map(({ label, value, from, alert, note }) => (
              <div key={label} className="flex items-center gap-3 py-2">
                <dt className="w-44 shrink-0 text-meta text-muted-foreground">{label}</dt>
                <dd className={cn("min-w-0 flex-1 truncate text-sm font-medium tabular-nums", alert && "text-tone-red")}>
                  {value}{note && <span className="font-normal text-muted-foreground"> · {note}</span>}
                </dd>
                <SourceChip system={from} />
              </div>
            ))}
          </dl>
        </section>
        {/* The rail */}
        <div className="ms-8 flex flex-col gap-3">
          <RailCard icon={Fingerprint} label="Who can see, edit or act">
            <div className="space-y-1.5">{ROLES.map(({ role, can }) => <RoleRow key={role} role={role} can={can} compact />)}</div>
          </RailCard>
          <RailCard icon={Scale} label="Rules it keeps">
            <ul className="space-y-2">
              {RULES.map((rule) => <li key={rule} className="flex gap-2 text-[0.9375rem] leading-snug"><ShieldCheck aria-hidden className="mt-0.5 size-3.5 shrink-0 text-tone-green" />{rule}</li>)}
            </ul>
          </RailCard>
          <RailCard icon={History} label="Last 3 changes">
            <ol className="space-y-2.5">
              {CHANGES.slice(0, 3).map(({ when, who, what, why }) => (
                <li key={when} className="text-[0.9375rem] leading-snug">
                  <div className="font-medium">{what}</div>
                  <div className="text-xs text-muted-foreground">{who} · {when} · {why}</div>
                </li>
              ))}
            </ol>
          </RailCard>
        </div>
      </div>
    </Slide>
  )
}
