import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router"
import { Bell, ChevronDown, Keyboard, LogOut, Search, type LucideIcon } from "lucide-react"
import { Fragment, useEffect, useState, type ReactNode } from "react"
import { Button } from "@/components/ui/button"
import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command"
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuShortcut, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Separator } from "@/components/ui/separator"
import {
  Sidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarHeader, SidebarInput, SidebarInset,
  SidebarMenu, SidebarMenuButton, SidebarMenuItem, SidebarProvider, SidebarTrigger, useSidebar,
} from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"
import type { Account } from "@/lib/session"
import { cn } from "@/lib/utils"

export interface Section {
  readonly path: string
  readonly label: string
  readonly icon: LucideIcon
  readonly summary: string
  /** The section's own items (each object type, question or workflow), listed in its submenu. */
  readonly useMenu?: () => readonly MenuEntry[] | undefined
  /** A count that needs attention, shown on the rail. */
  readonly useBadge?: () => number | undefined
  /** The rail group the section belongs to; a thin divider separates groups. */
  readonly group?: string
  /** Listed at the foot of the rail, above the avatar, instead of with the sections. */
  readonly foot?: boolean
}

export interface MenuEntry {
  /** The entry's route, e.g. `/sources/canon.Holding`, or `#anchor` on the section's own page. */
  readonly href: string
  readonly label: ReactNode
  /** The submenu group this entry is listed under. */
  readonly group?: string
  /** The entry's leading icon; the section's icon when absent. */
  readonly icon?: LucideIcon
  /** The chart colour (1–5) that tints the icon, one per family of entries. */
  readonly tint?: 1 | 2 | 3 | 4 | 5
}

const none = () => undefined

/** Entries listed per group before "Show more". */
const SHOWN = 6

/** Icon tints, one chart colour per family of entries; untinted icons are muted. */
const TINT = ["[&>svg]:text-muted-foreground", "[&>svg]:text-chart-1", "[&>svg]:text-chart-2", "[&>svg]:text-chart-3", "[&>svg]:text-chart-4", "[&>svg]:text-chart-5"] as const

function RailItem({ section, active }: { readonly section: Section; readonly active: boolean }) {
  const badge = (section.useBadge ?? none)()
  const Icon = section.icon
  const { setOpenMobile } = useSidebar()
  return (
    <SidebarMenuItem>
      <SidebarMenuButton asChild isActive={active} tooltip={{ children: section.label, hidden: false }}
        className="relative mx-auto size-10! justify-center rounded-[11px]! p-0! text-muted-foreground hover:bg-sidebar-accent data-[active=true]:bg-background data-[active=true]:text-foreground data-[active=true]:shadow-sm [&>svg]:size-5!">
        <Link to={section.path} onClick={() => setOpenMobile(false)}>
          <Icon />
          <span className="sr-only">{section.label}</span>
          {/* Inside the tile: the button clips anything outside it. A count replaces the active dot. */}
          {badge !== undefined && badge > 0
            ? <span className="absolute top-0.5 right-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-destructive px-1 text-[10px] leading-none font-semibold text-destructive-foreground tabular-nums">{badge > 99 ? "99+" : badge}</span>
            : active && <span aria-hidden className="absolute top-1 right-1 size-2 rounded-full bg-ring ring-2 ring-background" />}
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
  const [closed, setClosed] = useState<ReadonlySet<string>>(new Set())
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set())
  const toggle = (set: ReadonlySet<string>, group: string) => { const next = new Set(set); if (!next.delete(group)) next.add(group); return next }
  return (
    <Sidebar collapsible="none" className="min-w-0 flex-1 rounded-2xl bg-background shadow-sm">
      <SidebarHeader className="gap-2 px-5 pt-6 pb-3">
        <div className="text-[1.75rem] leading-tight font-semibold tracking-tight">{section.label}</div>
        <DropdownMenu>
          <DropdownMenuTrigger className="flex w-fit max-w-full items-center gap-1 rounded-md text-start text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/30">
            <span className="truncate" title="Recorded · synthetic world">Recorded · synthetic world</span> <ChevronDown className="size-4 shrink-0" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            <DropdownMenuLabel>Data</DropdownMenuLabel>
            <DropdownMenuCheckboxItem checked>Recorded · synthetic world</DropdownMenuCheckboxItem>
            <DropdownMenuItem disabled>Live, once the read host is up</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        {all.length > 0 && (
          <label className="relative mt-3 block">
            <span className="sr-only">Find in {section.label}</span>
            <Search aria-hidden className="pointer-events-none absolute start-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <SidebarInput type="search" placeholder={`Find in ${section.label}`} value={search} onChange={(event) => setSearch(event.target.value)} className="h-9 border-transparent bg-muted ps-9 shadow-none dark:bg-muted" />
          </label>
        )}
      </SidebarHeader>
      <SidebarContent className="px-2 pb-3">
        {all.length > 0 && entries.length === 0 && <p className="px-3 py-2 text-sm text-muted-foreground">Nothing matches “{search.trim()}”.</p>}
        {groups.map((group) => (
          <SidebarGroup key={group}>
            <SidebarGroupLabel asChild className="text-xs font-normal text-muted-foreground">
              <button type="button" aria-expanded={groupOpen(closed, group, query)} onClick={() => setClosed((set) => toggle(set, group))} className="w-full gap-1 hover:text-foreground">
                {group}<ChevronDown className={cn("size-3.5! transition-transform", !groupOpen(closed, group, query) && "-rotate-90")} />
              </button>
            </SidebarGroupLabel>
            {groupOpen(closed, group, query) && <SidebarGroupContent>
              <SidebarMenu>
                {entries.filter((entry) => (entry.group ?? section.label) === group).filter((entry, index, list) => {
                  const active = entry.href === pathname || pathname.startsWith(`${entry.href}/`)
                  return query !== "" || expanded.has(group) || index < SHOWN || active || list.length <= SHOWN + 1
                }).map((entry) => {
                  const Icon = entry.icon ?? section.icon
                  return (
                    <SidebarMenuItem key={entry.href}>
                      <SidebarMenuButton asChild isActive={entry.href === pathname || pathname.startsWith(`${entry.href}/`)}
                        className={cn("h-10 gap-3 rounded-lg px-3 text-[0.9375rem] hover:bg-muted data-[active=true]:bg-muted data-[active=true]:font-medium [&>svg]:size-[18px]", TINT[entry.tint ?? 0])}>
                        <Link {...target(section, entry.href)} onClick={() => setOpenMobile(false)}><Icon /><span className="truncate">{entry.label}</span></Link>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  )
                })}
                {query === "" && entries.filter((entry) => (entry.group ?? section.label) === group).length > SHOWN + 1 && (
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => setExpanded((set) => toggle(set, group))} className="h-9 rounded-lg px-3 text-sm text-muted-foreground hover:bg-muted">
                      {expanded.has(group) ? "Show less" : "Show more"}
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                )}
              </SidebarMenu>
            </SidebarGroupContent>}
          </SidebarGroup>
        ))}
      </SidebarContent>
    </Sidebar>
  )
}

/** A group shows its entries unless the user closed it; a search opens every group so its matches stay visible. */
export const groupOpen = (closed: ReadonlySet<string>, group: string, query: string) => query !== "" || !closed.has(group)

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
      <Button variant="outline" className="h-10 w-full min-w-0 justify-start rounded-full bg-background px-4 font-normal text-muted-foreground shadow-none dark:bg-background" onClick={() => setOpen(true)}>
        <Search /> <span className="min-w-0 truncate">Search or add…</span> <kbd className="ms-auto hidden font-sans text-meta sm:inline">⌘K</kbd>
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

export const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((part) => part[0]?.toUpperCase()).join("") || "?"

/** The signed-in user, their organization and role, and sign-out. The role is shown, never used to hide or allow anything. */
export function AccountMenu({ account, onSignOut }: { readonly account: Account; readonly onSignOut: () => void }) {
  const { user, organization } = account
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <SidebarMenuButton tooltip={{ children: `${user.name} · ${organization?.name ?? "No organization"}`, hidden: false }} className="size-10! justify-center rounded-full p-0!">
          <span className="flex size-9 items-center justify-center rounded-full bg-muted text-xs font-medium ring-1 ring-border">{initials(user.name || user.email)}</span>
          <span className="sr-only">Account</span>
        </SidebarMenuButton>
      </DropdownMenuTrigger>
      <DropdownMenuContent side="right" align="end" className="min-w-56">
        <DropdownMenuLabel className="grid">
          <span className="font-medium">{user.name}</span>
          <span className="text-meta font-normal text-muted-foreground">{user.email}</span>
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuLabel className="grid font-normal">
          <span>{organization?.name ?? "No organization"}</span>
          {organization && <span className="text-meta text-muted-foreground">Role: {organization.role}</span>}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={onSignOut}><LogOut />Sign out</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/** The page bar's right-hand buttons: notifications and keyboard shortcuts. */
function BarButtons() {
  return (
    <div className="flex items-center justify-end gap-1">
      <DropdownMenu>
        <DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="rounded-full text-muted-foreground"><Keyboard className="size-5" /><span className="sr-only">Keyboard shortcuts</span></Button></DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuLabel>Keyboard shortcuts</DropdownMenuLabel>
          <DropdownMenuItem disabled>Search or add<DropdownMenuShortcut>⌘K</DropdownMenuShortcut></DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <DropdownMenu>
        <DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="rounded-full text-muted-foreground"><Bell className="size-5" /><span className="sr-only">Notifications</span></Button></DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-64">
          <DropdownMenuLabel>Notifications</DropdownMenuLabel>
          <p className="px-2 pb-2 text-sm text-muted-foreground">Nothing yet. A recorded demo has no one to notify.</p>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

/**
 * Two islands on a grey page: the active section's submenu card and the page
 * card, with a thin rail of square tiles on the grey to their left.
 */
export function Shell({ sections, account, onSignOut }: { sections: readonly Section[]; account: Account; onSignOut: () => void }) {
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const current = sectionOf(sections, pathname)
  const main = sections.filter(({ foot }) => foot !== true)
  const foot = sections.filter(({ foot }) => foot === true)
  return (
    <TooltipProvider>
      <SidebarProvider style={{ "--sidebar-width": "23rem", "--sidebar-width-icon": "4rem" } as React.CSSProperties}>
        <Sidebar variant="inset" collapsible="icon" className="overflow-hidden ps-0">
          {/* One row on every screen: the mobile sheet does not take the outer sidebar's classes. */}
          <div className="flex size-full min-h-0 flex-row">
          <Sidebar collapsible="none" className="w-16! shrink-0 bg-transparent">
            <SidebarHeader className="items-center px-0 pt-1">
              <Link to="/questions" className="flex size-10 items-center justify-center rounded-[11px] bg-primary text-base font-semibold text-primary-foreground outline-none focus-visible:ring-3 focus-visible:ring-ring/30">
                N<span className="sr-only">Norn Studio</span>
              </Link>
            </SidebarHeader>
            <SidebarContent className="px-0 pt-3">
              <SidebarMenu className="items-center gap-2">
                {main.map((section, index) => (
                  <Fragment key={section.path}>
                    {index > 0 && section.group !== main[index - 1]?.group && <Separator className="my-1 w-6!" />}
                    <RailItem section={section} active={section === current} />
                  </Fragment>
                ))}
              </SidebarMenu>
            </SidebarContent>
            <SidebarFooter className="items-center px-0 pb-1">
              {foot.length > 0 && <SidebarMenu className="items-center gap-2">{foot.map((section) => <RailItem key={section.path} section={section} active={section === current} />)}</SidebarMenu>}
              <SidebarMenu><SidebarMenuItem className="flex justify-center"><AccountMenu account={account} onSignOut={onSignOut} /></SidebarMenuItem></SidebarMenu>
            </SidebarFooter>
          </Sidebar>
          {current && <Submenu key={current.path} section={current} />}
          </div>
        </Sidebar>
        <SidebarInset>
          <header className="grid h-16 grid-cols-[1fr_minmax(0,28rem)_1fr] items-center gap-3 px-4">
            <div><SidebarTrigger /></div>
            <CommandMenu sections={sections} />
            <BarButtons />
          </header>
          <div className="mx-auto w-full max-w-6xl px-4 pb-8 md:px-8"><Outlet /></div>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  )
}
