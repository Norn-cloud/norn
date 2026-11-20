import { Link, useParams } from "@tanstack/react-router"
import type { ComponentType } from "react"
import { BorrowerSlide } from "./borrower.tsx"
import { ForkSlide } from "./fork.tsx"
import { HeroSlide } from "./hero.tsx"
import { TimelineSlide } from "./timeline.tsx"
import { WorkflowSlide } from "./workflow.tsx"

/**
 * `/preview/$screen`: the finale pitch's slides, outside the product nav and the session. Each is a
 * static composition of Studio's own components over the synthetic sample in lending.ts; it reads
 * nothing from a cell and writes nothing.
 */
export const SCREENS: Readonly<Record<string, ComponentType>> = {
  hero: HeroSlide,
  borrower: BorrowerSlide,
  workflow: WorkflowSlide,
  timeline: TimelineSlide,
  fork: ForkSlide,
}

export function PreviewPage() {
  const { screen = "" } = useParams({ strict: false })
  const Screen = SCREENS[screen]
  if (Screen !== undefined) return <Screen />
  return (
    <main className="flex min-h-svh flex-col items-center justify-center gap-3 p-8 text-sm text-muted-foreground">
      <p>No preview named “{screen}”.</p>
      <ul className="flex gap-3">{Object.keys(SCREENS).map((name) => <li key={name}><Link to="/preview/$screen" params={{ screen: name }} className="underline underline-offset-4">{name}</Link></li>)}</ul>
    </main>
  )
}
