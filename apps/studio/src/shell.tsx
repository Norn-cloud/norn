import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router"
import { ChevronsUpDown, Search, type LucideIcon } from "lucide-react"
import { useEffect, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command"
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import {
  Sidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarHeader, SidebarInset,
  SidebarMenu, SidebarMenuButton, SidebarMenuItem, SidebarProvider, SidebarTrigger,
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
  readonly href: string
  readonly label: ReactNode
  /** The submenu group this entry is listed under. */
  readonly group?: string
}

const none = () => undefined

function RailItem({ section, active }: { readonly section: Section; readonly active: boolean }) {
  const badge = (section.useBadge ?? none)()
  const Icon = section.icon
  return (
    <SidebarMenuItem>
      <SidebarMenuButton asChild isActive={active} tooltip={{ children: section.label, hidden: false }} className="relative px-2.5 md:px-2">
        <Link to={section.path}>
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
  const entries = (section.useMenu ?? none)() ?? []
  const groups = [...new Set(entries.map(({ group }) => group ?? section.label))]
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
      </SidebarHeader>
      <SidebarContent>
        {groups.map((group) => (
          <SidebarGroup key={group}>
            <SidebarGroupLabel>{group}</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {entries.filter((entry) => (entry.group ?? section.label) === group).map((entry) => (
                  <SidebarMenuItem key={entry.href}><SidebarMenuButton asChild><a href={entry.href}>{entry.label}</a></SidebarMenuButton></SidebarMenuItem>
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
  const go = (path: string, hash?: string) => { setOpen(false); void navigate({ to: path, hash }) }
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
            {sections.map(({ path, label, icon: Icon }) => <CommandItem key={path} onSelect={() => go(path)}><Icon />{label}</CommandItem>)}
          </CommandGroup>
          {entries.filter(({ entries: list }) => list.length > 0).map(({ section, entries: list }) => (
            <CommandGroup key={section.path} heading={section.label}>
              {list.map((entry) => <CommandItem key={entry.href} onSelect={() => go(section.path, entry.href.replace(/^#/, ""))}>{entry.label}</CommandItem>)}
            </CommandGroup>
          ))}
        </CommandList>
        </Command>
      </CommandDialog>
    </>
  )
}

/** A thin icon rail, the active section's submenu beside it, and the page card. */
export function Shell({ sections }: { sections: readonly Section[] }) {
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const current = sections.find(({ path }) => path === pathname)
  return (
    <TooltipProvider>
      <SidebarProvider style={{ "--sidebar-width": "21rem", "--sidebar-width-icon": "4rem" } as React.CSSProperties}>
        <Sidebar variant="inset" collapsible="icon" className="overflow-hidden *:data-[sidebar=sidebar]:flex-row">
          <Sidebar collapsible="none" className="w-[calc(var(--sidebar-width-icon)+1px)]!">
            <SidebarHeader>
              <SidebarMenu><SidebarMenuItem>
                <SidebarMenuButton size="lg" asChild className="md:h-8 md:p-0">
                  <Link to="/questions"><span className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary text-sm font-semibold text-primary-foreground">N</span><span className="sr-only">Norn Studio</span></Link>
                </SidebarMenuButton>
              </SidebarMenuItem></SidebarMenu>
            </SidebarHeader>
            <SidebarContent>
              <SidebarGroup><SidebarGroupContent className="px-1.5 md:px-0">
                <SidebarMenu>{sections.map((section) => <RailItem key={section.path} section={section} active={section.path === pathname} />)}</SidebarMenu>
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
