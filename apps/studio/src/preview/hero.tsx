import { Activity, Check, Fingerprint, PhoneCall, ShieldCheck, Snowflake, TriangleAlert, X } from "lucide-react"
import { Chip } from "@/components/chip"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { ACTION, ACTOR, BLOCKED, BORROWER, FACTS, PAYMENTS, ROLES, RULE_CHECK } from "./lending.ts"
import { Person, RoleRow, Slide, SourceChip, StepArrow, StepCard } from "./slide.tsx"

/** 01 · The signal: seven monthly installments, the last two missed. */
function Signal() {
  return (
    <>
      <div className="flex items-start justify-between gap-3">
        <div>
          <div className="text-sm font-medium">Monthly installments</div>
          <div className="text-meta text-muted-foreground">{BORROWER.loan} · {BORROWER.installment} due on the 15th</div>
        </div>
        <Chip tone="red" icon={TriangleAlert} className="h-6 px-2">2 missed in 60 days</Chip>
      </div>
      <div className="flex min-h-32 flex-1 items-stretch gap-2.5 pt-5 pb-1" aria-label="Installments, May to November: five paid, October and November missed">
        {PAYMENTS.map(({ month, paid }) => (
          <div key={month} className="flex flex-1 flex-col items-center gap-2">
            <div className={cn("w-full flex-1 rounded-lg", paid
              ? "bg-tone-teal/45"
              : "flex items-center justify-center border-2 border-dashed border-tone-red/45 bg-tone-red/[0.06] text-tone-red")}>
              {!paid && <X aria-hidden className="size-4" />}
            </div>
            <span className={cn("text-xs tabular-nums", paid ? "text-muted-foreground" : "font-semibold text-tone-red")}>{month}</span>
          </div>
        ))}
      </div>
      <div className="mt-4 flex items-center gap-3 rounded-xl bg-background px-3.5 py-3 shadow-[var(--elevation-1)] ring-1 ring-foreground/5">
        <span className="flex size-8 items-center justify-center rounded-lg bg-tone-red/10 text-tone-red"><TriangleAlert aria-hidden className="size-4" /></span>
        <div className="min-w-0 flex-1">
          <div className="text-sm font-medium">Installment missed</div>
          <div className="text-xs text-muted-foreground tabular-nums">{BORROWER.installment} · due 15 Nov</div>
        </div>
        <SourceChip system="payments" />
      </div>
    </>
  )
}

/** 02 · The borrower: one record, every value with its source, and who may do what. */
function Borrower() {
  return (
    <>
      <div className="flex items-center gap-3.5">
        <Person initials={BORROWER.initials} tone="violet" size="lg" />
        <div className="min-w-0">
          <div className="text-[1.1875rem] font-semibold tracking-tight">{BORROWER.name}</div>
          <div className="text-meta text-muted-foreground">{BORROWER.detail}</div>
        </div>
      </div>
      <dl className="mt-3.5 divide-y divide-border/70 rounded-xl bg-background px-3.5 shadow-[var(--elevation-1)] ring-1 ring-foreground/5">
        {FACTS.map(({ label, value, from, alert }) => (
          <div key={label} className="flex items-center gap-3 py-1.5">
            <dt className="w-32 shrink-0 text-meta text-muted-foreground">{label}</dt>
            <dd className={cn("min-w-0 flex-1 truncate text-sm font-medium tabular-nums", alert && "text-tone-red")}>{value}</dd>
            <SourceChip system={from} />
          </div>
        ))}
      </dl>
      <div className="mt-3 space-y-1.5 rounded-xl border border-dashed border-border px-3.5 py-2.5">
        <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground"><Fingerprint aria-hidden className="size-3.5" />Who can see, edit or act</div>
        {ROLES.slice(0, 3).map((role) => <RoleRow key={role.role} {...role} compact />)}
      </div>
    </>
  )
}

/** 03 · The action: checked against the actor's authority before it runs, and recorded. */
function Act() {
  return (
    <>
      <div className="flex items-center gap-3">
        <span className="flex size-11 items-center justify-center rounded-xl bg-background text-tone-green shadow-[var(--elevation-1)] ring-1 ring-foreground/5"><Snowflake aria-hidden className="size-5" /></span>
        <div className="min-w-0">
          <div className="text-[1.1875rem] font-semibold tracking-tight">{ACTION.title}</div>
          <div className="text-meta text-muted-foreground">{BORROWER.loan} · outstanding {BORROWER.outstanding}</div>
        </div>
      </div>
      <div className="mt-4 space-y-2">
        <div className="flex items-center gap-2.5 rounded-xl bg-background px-3.5 py-2.5 text-sm font-medium shadow-[var(--elevation-1)] ring-1 ring-foreground/5">
          <span className="flex size-7 shrink-0 items-center justify-center rounded-full bg-tone-green/12 text-tone-green"><Check aria-hidden className="size-4" /></span>
          <span className="min-w-0 flex-1">{RULE_CHECK}</span>
        </div>
        <div className="flex items-center gap-3 rounded-xl bg-background px-3.5 py-2.5 shadow-[var(--elevation-1)] ring-1 ring-foreground/5">
          <Person initials={ACTOR.initials} tone="teal" size="sm" />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-medium">{ACTOR.name}</div>
            <div className="text-xs text-muted-foreground">{ACTOR.role}</div>
          </div>
          <Chip tone="green" icon={Check} className="h-6 px-2">Within your authority</Chip>
        </div>
        <div className="flex items-center gap-3 rounded-xl bg-background/70 px-3.5 py-2.5 ring-1 ring-foreground/5">
          <Person initials={BLOCKED.initials} tone="amber" size="sm" />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-medium">{BLOCKED.name}</div>
            <div className="text-xs text-muted-foreground">{BLOCKED.role}</div>
          </div>
          <Chip tone="red" icon={X} className="h-6 px-2">Sales can't do this</Chip>
        </div>
      </div>
      <Button className="mt-3.5 h-11 w-full rounded-xl text-[0.9375rem]"><PhoneCall aria-hidden /> {ACTION.title}</Button>
      <div className="mt-auto pt-4">
        <div className="flex items-center gap-1.5 pb-1.5 text-xs font-medium text-muted-foreground"><ShieldCheck aria-hidden className="size-3.5" />Recorded: who · when · why</div>
        <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-0.5 rounded-xl bg-background/70 px-3.5 py-2 text-meta ring-1 ring-foreground/5">
          <span className="text-muted-foreground">Who</span><span className="font-medium">{ACTOR.name}</span>
          <span className="text-muted-foreground">When</span><span className="font-medium tabular-nums">{ACTION.when}</span>
          <span className="text-muted-foreground">Why</span><span className="font-medium">{ACTION.why}</span>
        </div>
      </div>
    </>
  )
}

/** Slide 1: spot it, see everything, act safely. */
export function HeroSlide() {
  return (
    <Slide eyebrow="The Weave" title="One live model of the business." accent="Insight to action in hours."
      preview="The role strip is a preview." lead="Data, operations and developers share one live record: a signal becomes a checked action in hours, not a ticket in weeks.">
      <div className="grid grid-cols-[1fr_auto_1fr_auto_1fr] items-stretch gap-5">
        <StepCard number="01" icon={Activity} kicker="Payments signal" tone="red" title="Spot it." sentence="Norn watches every payment and flags the borrower the moment two installments are missed.">
          <Signal />
        </StepCard>
        <StepArrow />
        <StepCard number="02" icon={Fingerprint} kicker="One live record" tone="blue" title="See everything." sentence="Core lending, payments, CRM and the bureau in one record, each value with its source.">
          <Borrower />
        </StepCard>
        <StepArrow />
        <StepCard number="03" icon={ShieldCheck} kicker="Checked action" tone="green" title="Act safely." sentence="The action checks the rule and who is asking before it runs, then records who, when and why.">
          <Act />
        </StepCard>
      </div>
    </Slide>
  )
}
