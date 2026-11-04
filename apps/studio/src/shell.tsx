import { Link, Outlet, useRouterState } from "@tanstack/react-router"
import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

export interface Section {
  readonly path: string
  readonly label: string
  readonly icon: LucideIcon
  readonly summary: string
  /** The section's own items (each concept, question or workflow), listed in the submenu island. */
  readonly useMenu?: () => readonly MenuEntry[] | undefined
}

const ROW = "flex h-9 w-full items-center gap-2.5 rounded-lg px-3 text-sm text-muted-foreground transition-colors duration-150 ease-spring outline-none hover:bg-accent/70 hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/30"
const ACTIVE = "bg-accent font-medium text-foreground"

export interface MenuEntry {
  readonly href: string
  readonly label: ReactNode
}

/** The submenu island; it collapses when the section has fewer than two items to list. */
function Submenu({ label, useMenu }: { label: string; useMenu: () => readonly MenuEntry[] | undefined }) {
  const entries = useMenu() ?? []
  if (entries.length < 2) return null
  return (
    <aside aria-label={`${label} items`} className="sticky top-2 max-h-[calc(100svh-1rem)] w-56 shrink-0 self-start overflow-y-auto rounded-2xl border bg-background p-2 max-xl:hidden">
      <div className="px-3 pt-2 pb-1.5 text-meta font-medium text-muted-foreground">{label}</div>
      {entries.map(({ href, label: entry }) => (
        <a key={href} href={href} className={ROW}><span className="min-w-0 truncate">{entry}</span></a>
      ))}
    </aside>
  )
}

/**
 * Three islands on the grey canvas: the section rail, the active section's
 * submenu, and the page. On narrow screens the rail becomes a dock.
 */
export function Shell({ sections }: { sections: readonly Section[] }) {
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const current = sections.find(({ path }) => path === pathname)
  return (
    <div className="flex min-h-svh gap-2 bg-canvas p-2 text-foreground max-md:pb-20">
      <nav aria-label="Sections" className="sticky top-2 flex w-48 shrink-0 self-start flex-col gap-0.5 px-1 py-3 max-md:hidden">
        <Link to="/questions" className="mb-4 flex h-10 items-center gap-2.5 rounded-lg px-3">
          <span className="flex size-6 items-center justify-center rounded-md bg-primary text-xs font-semibold text-primary-foreground">N</span>
          <span className="text-panel font-medium">Norn Studio</span>
        </Link>
        {sections.map(({ path, label, icon: Icon }) => (
          <Link key={path} to={path} aria-current={path === pathname ? "page" : undefined} className={cn(ROW, path === pathname && ACTIVE)}>
            <Icon className="size-4 shrink-0" /><span>{label}</span>
          </Link>
        ))}
      </nav>
      {current?.useMenu && <Submenu key={current.path} label={current.label} useMenu={current.useMenu} />}
      <div className="min-w-0 flex-1 rounded-2xl border bg-background">
        <main className="mx-auto w-full max-w-6xl p-4 md:p-8"><Outlet /></main>
      </div>
      <nav aria-label="Sections" className="fixed inset-x-0 bottom-4 mx-auto flex w-fit gap-1 rounded-full bg-foreground p-1.5 text-background md:hidden">
        {sections.map(({ path, label, icon: Icon }) => (
          <Link key={path} to={path} aria-label={label} aria-current={path === pathname ? "page" : undefined}
            className={cn("flex size-10 items-center justify-center rounded-full transition-colors duration-150 ease-spring", path === pathname ? "bg-background/20" : "opacity-70 hover:opacity-100")}>
            <Icon className="size-4" />
          </Link>
        ))}
      </nav>
    </div>
  )
}
