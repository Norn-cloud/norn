import { Check, GitFork, Radio, Trash2, TrendingDown, UploadCloud, X } from "lucide-react"
import { Chip } from "@/components/chip"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { BOOK, BORROWER } from "./lending.ts"
import { Person, Slide } from "./slide.tsx"

type Side = (typeof BOOK)["live"] | (typeof BOOK)["fork"]

/** One side of the comparison: the rule, the three figures, and whether the sample borrower is caught. */
function Column({ side, forked }: { readonly side: Side; readonly forked: boolean }) {
  return (
    <section className={cn("flex flex-col rounded-[22px] bg-background p-2.5 shadow-[var(--elevation-node)] ring-1", forked ? "ring-[color-mix(in_oklab,var(--action)_40%,transparent)]" : "ring-foreground/5")}>
      <div className={cn("rounded-[16px] px-6 pt-4 pb-5 ring-1 ring-foreground/[0.04]", forked ? "bg-[color-mix(in_oklab,var(--action)_6%,var(--background))]" : "bg-surface-subtle")}>
        <div className="flex items-center justify-between">
          {forked ? <Chip tone="violet" icon={GitFork}>Fork · what if</Chip> : <Chip tone="green" icon={Radio} dot>Live</Chip>}
          <span className="text-meta text-muted-foreground">{forked ? "Nothing here touches a real loan" : "Running on the book today"}</span>
        </div>
        <div className="pt-4 text-[1.25rem] leading-snug font-semibold tracking-tight">
          Freeze after <span className={cn("rounded-md px-1.5", forked ? "bg-[var(--action)] text-[var(--action-foreground)]" : "bg-foreground/[0.07]")}>{side.threshold}</span> missed payments in 60 days
        </div>
        <div className="text-meta text-muted-foreground">on loans over EGP 50,000</div>
      </div>
      <dl className="grid grid-cols-3 gap-3 px-3 pt-5 pb-3">
        <div><dt className="text-meta text-muted-foreground">Loans flagged</dt><dd className="pt-1 text-[2.75rem] leading-none font-light tracking-[-0.04em] tabular-nums">{side.loans}</dd></div>
        <div className="col-span-2"><dt className="text-meta text-muted-foreground">Balance at risk</dt><dd className="pt-1 text-[2.75rem] leading-none font-light tracking-[-0.04em] tabular-nums">{side.exposure}</dd></div>
      </dl>
      <div className="mx-3 mt-2 mb-2 flex items-center gap-3 rounded-xl px-3.5 py-3 ring-1 ring-foreground/5">
        <Person initials={BORROWER.initials} tone="violet" size="sm" />
        <span className="min-w-0 flex-1 text-sm"><span className="font-medium">{BORROWER.name}</span> · 2 missed in 60 days</span>
        {side.mona ? <Chip tone="red" icon={Check}>Flagged</Chip> : <Chip tone="gray" icon={X}>Not flagged</Chip>}
      </div>
      <div className="mx-3 mb-2 flex items-center justify-between text-meta text-muted-foreground"><span>Collections calls this week</span><span className="font-medium text-foreground tabular-nums">{side.calls}</span></div>
    </section>
  )
}

/** Slide 5: fork the live model, change one number, compare, then promote or discard. */
export function ForkSlide() {
  return (
    <Slide eyebrow="The Weave · forks" title="Try a change before it's real." preview="The fork comparison is a preview."
      lead="Fork the live model, change one rule, and see what it would do to the whole book. Then promote it or throw it away.">
      <div className="grid grid-cols-[1fr_260px_1fr] items-stretch gap-5">
        <Column side={BOOK.live} forked={false} />
        <div className="flex flex-col items-center justify-center gap-3 text-center">
          <span className="flex size-11 items-center justify-center rounded-full bg-background text-muted-foreground shadow-[var(--elevation-float)] ring-1 ring-foreground/5"><TrendingDown aria-hidden className="size-5" /></span>
          <div className="text-meta text-muted-foreground">If the fork went live</div>
          <div className="text-[1.6rem] leading-none font-semibold tracking-tight tabular-nums text-balance">{BOOK.difference.loans} fewer loans watched</div>
          <div className="text-sm font-medium tabular-nums text-balance">{BOOK.difference.exposure} no longer flagged</div>
        </div>
        <Column side={BOOK.fork} forked />
      </div>
      <div className="mt-6 flex items-center justify-center gap-3">
        <Button variant="outline" className="h-11 rounded-xl px-5 text-[0.9375rem]"><Trash2 aria-hidden /> Discard</Button>
        <Button className="h-11 rounded-xl px-5 text-[0.9375rem]"><UploadCloud aria-hidden /> Promote to live</Button>
        <span className="ms-2 text-meta text-muted-foreground">Promoting needs Risk's approval, and every rule is checked again first.</span>
      </div>
    </Slide>
  )
}
