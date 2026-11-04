import { Link } from "@tanstack/react-router"
import { Search } from "lucide-react"
import { useState, type ReactNode } from "react"
import { EmptyState } from "@/components/panel"
import { Input } from "@/components/ui/input"
import { cn } from "@/lib/utils"

export interface ListItem {
  readonly id: string
  readonly to: string
  readonly title: string
  readonly subtitle?: string
  readonly group: string
}

/** A searchable list island beside the selected item's detail; the list's groups keep their given order. */
export function ListDetail({ title, items, selected, placeholder, children }: {
  readonly title: string
  readonly items: readonly ListItem[]
  readonly selected: string | undefined
  readonly placeholder: string
  readonly children: ReactNode
}) {
  const [search, setSearch] = useState("")
  const query = search.trim().toLowerCase()
  const shown = items.filter(({ title: name, subtitle }) => `${name} ${subtitle ?? ""}`.toLowerCase().includes(query))
  const groups = [...new Set(shown.map(({ group }) => group))]
  return (
    <div className="grid gap-4 lg:grid-cols-[17rem_minmax(0,1fr)]">
      <nav aria-label={title} className="self-start rounded-2xl border bg-card lg:sticky lg:top-4">
        <div className="space-y-3 border-b p-3">
          <div className="px-1 text-panel font-medium">{title}</div>
          <label className="relative block">
            <span className="sr-only">{placeholder}</span>
            <Search aria-hidden className="pointer-events-none absolute start-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input type="search" placeholder={placeholder} value={search} onChange={(event) => setSearch(event.target.value)} className="border-border bg-background ps-9 dark:bg-background" />
          </label>
        </div>
        <div className="max-h-[70svh] overflow-y-auto p-2">
          {groups.length === 0 && <EmptyState icon={Search}>Nothing matches “{search.trim()}”.</EmptyState>}
          {groups.map((group) => (
            <div key={group} className="pb-2">
              <div className="px-2 pt-2 pb-1 text-meta font-medium text-muted-foreground">{group}</div>
              {shown.filter((item) => item.group === group).map((item) => (
                <Link key={item.id} to={item.to} aria-current={item.id === selected ? "page" : undefined}
                  className={cn("block rounded-lg px-2 py-1.5 text-sm outline-none hover:bg-accent focus-visible:ring-3 focus-visible:ring-ring/30", item.id === selected && "bg-accent font-medium")}>
                  <div className="truncate">{item.title}</div>
                  {item.subtitle && <div className="truncate text-meta text-muted-foreground">{item.subtitle}</div>}
                </Link>
              ))}
            </div>
          ))}
        </div>
      </nav>
      <div className="min-w-0">{children}</div>
    </div>
  )
}
