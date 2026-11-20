import { ArrowRight, Check, CircleDot, FileCheck2, GitBranch, Play, ShieldCheck, Sparkles, Users, Zap, type LucideIcon } from "lucide-react"
import { Chip, type Tone } from "@/components/chip"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { BOOK, BORROWER, RULES } from "./lending.ts"
import { Person, Slide, WASH } from "./slide.tsx"

/** One block of the rule row: a small tinted heading over the value it was given. */
function Block({ icon: Icon, label, tone, children, detail }: { readonly icon: LucideIcon; readonly label: string; readonly tone: Tone; readonly children: React.ReactNode; readonly detail: string }) {
  return (
    <div className={cn("flex min-w-0 flex-1 flex-col rounded-2xl px-5 pt-4 pb-5 ring-1 ring-foreground/5", WASH[tone])}>
      <Chip tone={tone} icon={Icon} className="self-start">{label}</Chip>
      <div className="mt-3.5 rounded-xl bg-background px-3.5 py-3 text-[1.0625rem] leading-snug font-semibold tracking-tight shadow-[var(--elevation-1)] ring-1 ring-foreground/5">{children}</div>
      <div className="mt-2.5 text-meta text-muted-foreground">{detail}</div>
    </div>
  )
}

const Joint = () => <ArrowRight aria-hidden className="size-5 shrink-0 self-center text-muted-foreground" />

/** Slide 3: the rule an analyst writes, as one row a reader follows left to right. */
export function WorkflowSlide() {
  return (
    <Slide eyebrow="The Weave · rules in plain words" title="Write the rule once. Norn runs it safely." preview="The visual rule builder is a preview."
      lead="An analyst turns the insight into a rule in minutes. Norn checks it against every rule and permission before it can run.">
      <section className="rounded-[22px] bg-background p-2.5 shadow-[var(--elevation-node-hover)] ring-1 ring-foreground/5">
        <header className="flex items-center gap-3 px-4 pt-2 pb-4">
          <span className="flex size-10 items-center justify-center rounded-xl bg-tone-violet/10 text-tone-violet"><GitBranch aria-hidden className="size-5" /></span>
          <div className="min-w-0 flex-1">
            <div className="text-[1.25rem] font-semibold tracking-tight">Early arrears response</div>
            <div className="text-meta text-muted-foreground">Written by Nour Hassan, Data team · reviewed by Risk</div>
          </div>
          <Chip tone="green" icon={Check}>All checks pass</Chip>
          <Button variant="outline" className="h-10 rounded-xl px-4"><Play aria-hidden /> Test on today's book</Button>
          <Button className="h-10 rounded-xl px-4"><Zap aria-hidden /> Publish</Button>
        </header>
        <div className="flex items-stretch gap-3 rounded-[16px] bg-surface-subtle p-4 ring-1 ring-foreground/[0.04]">
          <Block icon={CircleDot} label="When" tone="red" detail="Read from Payments, as it happens">Missed payments ≥ 2 in 60 days</Block>
          <Joint />
          <Block icon={GitBranch} label="If" tone="blue" detail="Read from Fineract">Loan balance &gt; EGP 50,000</Block>
          <Joint />
          <Block icon={Zap} label="Then" tone="violet" detail="Two steps, one decision">Freeze limit + assign to Collections</Block>
          <Joint />
          <Block icon={ShieldCheck} label="Check" tone="green" detail="Rules and permissions, before it runs">3 rules pass ✓</Block>
          <Joint />
          <Block icon={FileCheck2} label="Record" tone="gray" detail="Kept with the record">Who · when · why</Block>
        </div>
        <div className="grid grid-cols-[1.1fr_1fr] gap-3 px-2 pt-4 pb-2">
          <div className="rounded-2xl px-4 py-3.5 ring-1 ring-foreground/5">
            <div className="flex items-center gap-1.5 pb-2.5 text-meta font-medium text-muted-foreground"><ShieldCheck aria-hidden className="size-3.5" />Checked against</div>
            <ul className="space-y-2">
              {RULES.map((rule) => <li key={rule} className="flex gap-2 text-sm"><Check aria-hidden className="mt-0.5 size-4 shrink-0 text-tone-green" />{rule}</li>)}
            </ul>
          </div>
          <div className="rounded-2xl px-4 py-3.5 ring-1 ring-foreground/5">
            <div className="flex items-center gap-1.5 pb-2.5 text-meta font-medium text-muted-foreground"><Sparkles aria-hidden className="size-3.5" />If it ran today</div>
            <div className="flex items-end gap-8">
              <div><div className="text-[2.25rem] leading-none font-light tracking-[-0.04em] tabular-nums">{BOOK.live.loans}</div><div className="pt-1.5 text-meta text-muted-foreground">loans flagged of {BOOK.activeLoans}</div></div>
              <div><div className="text-[2.25rem] leading-none font-light tracking-[-0.04em] tabular-nums">{BOOK.live.exposure}</div><div className="pt-1.5 text-meta text-muted-foreground">balance at risk</div></div>
            </div>
            <div className="mt-3.5 flex items-center gap-2.5 text-sm">
              <Person initials={BORROWER.initials} tone="violet" size="sm" />
              <span><span className="font-medium">{BORROWER.name}</span> · {BORROWER.loan} is one of them</span>
              <Chip tone="teal" icon={Users} className="ms-auto">Goes to Collections</Chip>
            </div>
          </div>
        </div>
      </section>
    </Slide>
  )
}
