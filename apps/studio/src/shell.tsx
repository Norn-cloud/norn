import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router"
import { ChevronsUpDown, Search, type LucideIcon } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command"
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import {
  Sidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarHeader, SidebarInput, SidebarInset,
  SidebarMenu, SidebarMenuButton, SidebarMenuItem, SidebarProvider, SidebarTrigger, useSidebar,
} from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"

export interface Section {
  readonly path: string
  readonly label: string
  readonly icon: LucideIcon
  readonly summary: string
  /** The section's own items (each object type, question or workflow), listed in its submenu. */
  readonly useMenu?: () => readonly MenuEntry[] | undefined
  /** A count that needs attention, shown on the rail. */
  readonly useBadge?: () => number | undefined
}

export interface MenuEntry {
  /** The entry's route, e.g. `/sources/canon.Holding`, or `#anchor` on the section's own page. */
  readonly href: string
  readonly label: ReactNode
  /** The submenu group this entry is listed under. */
  readonly group?: string
}

const none = () => undefined

function RailItem({ section, active }: { readonly section: Section; readonly active: boolean }) {
  const badge = (section.useBadge ?? none)()
  const Icon = section.icon
  const { setOpenMobile } = useSidebar()
  return (
    <SidebarMenuItem>
      <SidebarMenuButton asChild isActive={active} tooltip={{ children: section.label, hidden: false }} className="relative px-2.5 md:px-2">
        <Link to={section.path} onClick={() => setOpenMobile(false)}>
          <Icon />
          <span className="sr-only">{section.label}</span>
          {badge !== undefined && badge > 0 && <span className="absolute -top-0.5 -right-0.5 flex size-4 items-center justify-center rounded-full bg-destructive text-[10px] font-medium text-white tabular-nums">{badge}</span>}
        </Link>
      </SidebarMenuButton>
    </SidebarMenuItem>
  )
}

/**
 * The section's submenu: its title and data context, then its grouped entries.
 * No server document declares a section action yet, so none is shown.
 */
export function Submenu({ section }: { readonly section: Section }) {
  // Navigating closes the phone sheet the submenu sits in.
  const { setOpenMobile } = useSidebar()
  const [search, setSearch] = useState("")
  const all = (section.useMenu ?? none)() ?? []
  const query = search.trim().toLowerCase()
  const entries = all.filter(({ label }) => typeof label !== "string" || label.toLowerCase().includes(query))
  const groups = [...new Set(entries.map(({ group }) => group ?? section.label))]
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  return (
    <Sidebar collapsible="none" className="flex-1">
      <SidebarHeader className="gap-3 px-4 pt-5 pb-3">
        <div className="text-title font-medium">{section.label}</div>
        <DropdownMenu>
          <DropdownMenuTrigger className="flex w-fit items-center gap-1 rounded-md text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/30">
            Recorded · synthetic world <ChevronsUpDown className="size-3.5" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            <DropdownMenuLabel>Data</DropdownMenuLabel>
            <DropdownMenuCheckboxItem checked>Recorded · synthetic world</DropdownMenuCheckboxItem>
            <DropdownMenuItem disabled>Live, once the read host is up</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        {all.length > 0 && (
          <label className="relative block">
            <span className="sr-only">Find in {section.label}</span>
            <Search aria-hidden className="pointer-events-none absolute start-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <SidebarInput type="search" placeholder={`Find in ${section.label}`} value={search} onChange={(event) => setSearch(event.target.value)} className="ps-8" />
          </label>
        )}
      </SidebarHeader>
      <SidebarContent>
        {all.length > 0 && entries.length === 0 && <p className="px-4 py-2 text-sm text-muted-foreground">Nothing matches “{search.trim()}”.</p>}
        {groups.map((group) => (
          <SidebarGroup key={group}>
            <SidebarGroupLabel>{group}</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {entries.filter((entry) => (entry.group ?? section.label) === group).map((entry) => (
                  <SidebarMenuItem key={entry.href}><SidebarMenuButton asChild isActive={entry.href === pathname || pathname.startsWith(`${entry.href}/`)}><Link {...target(section, entry.href)} onClick={() => setOpenMobile(false)}>{entry.label}</Link></SidebarMenuButton></SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        ))}
      </SidebarContent>
    </Sidebar>
  )
}

/** "Search or add… ⌘K": every section and every entry the sections list. */
function CommandMenu({ sections }: { readonly sections: readonly Section[] }) {
  const [open, setOpen] = useState(false)
  const navigate = useNavigate()
  const entries = sections.map((section) => ({ section, entries: (section.useMenu ?? none)() ?? [] }))
  useEffect(() => {
    const toggle = (event: KeyboardEvent) => {
      if (event.key === "k" && (event.metaKey || event.ctrlKey)) { event.preventDefault(); setOpen((current) => !current) }
    }
    document.addEventListener("keydown", toggle)
    return () => document.removeEventListener("keydown", toggle)
  }, [])
  const go = (to: { to: string; hash?: string }) => { setOpen(false); void navigate(to) }
  return (
    <>
      <Button variant="outline" className="w-full max-w-md justify-start text-muted-foreground" onClick={() => setOpen(true)}>
        <Search /> Search or add… <kbd className="ms-auto text-meta">⌘K</kbd>
      </Button>
      <CommandDialog open={open} onOpenChange={setOpen} title="Search Studio" description="Go to a section or an item">
        <Command>
        <CommandInput placeholder="Search sections, object types, questions…" />
        <CommandList>
          <CommandEmpty>Nothing matches.</CommandEmpty>
          <CommandGroup heading="Sections">
            {sections.map(({ path, label, icon: Icon }) => <CommandItem key={path} onSelect={() => go({ to: path })}><Icon />{label}</CommandItem>)}
          </CommandGroup>
          {entries.filter(({ entries: list }) => list.length > 0).map(({ section, entries: list }) => (
            <CommandGroup key={section.path} heading={section.label}>
              {list.map((entry) => <CommandItem key={entry.href} onSelect={() => go(target(section, entry.href))}>{entry.label}</CommandItem>)}
            </CommandGroup>
          ))}
        </CommandList>
        </Command>
      </CommandDialog>
    </>
  )
}

/** Where an entry goes: its route, or an anchor on its section's page. */
export const target = (section: Pick<Section, "path">, href: string) =>
  href.startsWith("#") ? { to: section.path, hash: href.slice(1) } : { to: href }

/** The section a route belongs to: its own path, or a record page under it. */
export const sectionOf = (sections: readonly Section[], pathname: string) =>
  sections.find(({ path }) => pathname === path || pathname.startsWith(`${path}/`))

/** A thin icon rail, the active section's submenu beside it, and the page card. */
export function Shell({ sections }: { sections: readonly Section[] }) {
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const current = sectionOf(sections, pathname)
  return (
    <TooltipProvider>
      <SidebarProvider style={{ "--sidebar-width": "21rem", "--sidebar-width-icon": "4rem" } as React.CSSProperties}>
        <Sidebar variant="inset" collapsible="icon" className="overflow-hidden">
          {/* One row on every screen: the phone sheet does not take the outer sidebar's classes. */}
          <div className="flex size-full min-h-0 flex-row">
          <Sidebar collapsible="none" className="w-[calc(4rem+1px)]! shrink-0">
            <SidebarHeader>
              <SidebarMenu><SidebarMenuItem>
                <SidebarMenuButton size="lg" asChild className="md:h-8 md:p-0">
                  <Link to="/questions"><span className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary text-sm font-semibold text-primary-foreground">N</span><span className="sr-only">Norn Studio</span></Link>
                </SidebarMenuButton>
              </SidebarMenuItem></SidebarMenu>
            </SidebarHeader>
            <SidebarContent>
              <SidebarGroup><SidebarGroupContent className="px-1.5 md:px-0">
                <SidebarMenu>{sections.map((section) => <RailItem key={section.path} section={section} active={section === current} />)}</SidebarMenu>
              </SidebarGroupContent></SidebarGroup>
            </SidebarContent>
            <SidebarFooter>
              <SidebarMenu><SidebarMenuItem>
                <SidebarMenuButton tooltip={{ children: "Not signed in: recorded demo", hidden: false }} className="md:h-8 md:p-0">
                  <span className="flex aspect-square size-8 items-center justify-center rounded-full bg-muted text-xs font-medium">RD</span><span className="sr-only">Recorded demo</span>
                </SidebarMenuButton>
              </SidebarMenuItem></SidebarMenu>
            </SidebarFooter>
          </Sidebar>
          {current && <Submenu key={current.path} section={current} />}
          </div>
        </Sidebar>
        <SidebarInset>
          <header className="flex h-14 items-center gap-3 border-b px-4">
            <SidebarTrigger />
            <div className="flex flex-1 justify-center"><CommandMenu sections={sections} /></div>
          </header>
          <div className="mx-auto w-full max-w-6xl p-4 md:p-8"><Outlet /></div>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  )
}
