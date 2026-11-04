import { Link, Outlet, useRouterState } from "@tanstack/react-router"
import type { LucideIcon } from "lucide-react"
import type { ComponentType, ReactNode } from "react"
import { EmptyState } from "@/components/panel"
import { cn } from "@/lib/utils"

export interface Section {
  readonly path: string
  readonly label: string
  readonly icon: LucideIcon
  readonly summary: string
  /** The section's own items (each question, each workflow), listed in the submenu island. */
  readonly menu?: ComponentType
}

const ROW = "flex h-9 w-full items-center gap-2.5 rounded-lg px-3 text-sm text-muted-foreground transition-colors duration-150 ease-spring outline-none hover:bg-accent/70 hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/30"
const ACTIVE = "bg-accent font-medium text-foreground"

/** One row of a submenu island. */
export function MenuItem({ href, children }: { href: string; children: ReactNode }) {
  return <a href={href} className={ROW}><span className="min-w-0 truncate">{children}</span></a>
}

/**
 * Three islands on the grey canvas: the section rail, the active section's
 * submenu, and the page. On narrow screens the rail becomes a dock.
 */
export function Shell({ sections }: { sections: readonly Section[] }) {
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const current = sections.find(({ path }) => path === pathname)
  const Menu = current?.menu
  return (
    <div className="flex min-h-svh gap-2 bg-canvas p-2 text-foreground max-md:pb-20">
      <nav aria-label="Sections" className="flex w-52 shrink-0 flex-col gap-0.5 px-1 py-3 max-md:hidden">
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
      {current && (
        <aside aria-label={`${current.label} items`} className="sticky top-2 w-60 shrink-0 self-start rounded-2xl border bg-background p-2 max-lg:hidden">
          <div className="px-3 pt-2 pb-1.5 text-meta font-medium text-muted-foreground">{current.label}</div>
          {Menu ? <Menu /> : <EmptyState icon={current.icon}>Nothing to list in this section yet.</EmptyState>}
        </aside>
      )}
      <div className="min-w-0 flex-1 rounded-2xl border bg-background">
        <header className="flex h-12 items-center border-b px-6 text-meta text-muted-foreground">{current?.label}</header>
        <main className="mx-auto w-full max-w-6xl p-5 md:p-8"><Outlet /></main>
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
