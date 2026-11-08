import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router"
import { Bell, ChevronRight, ChevronsUpDown, Keyboard, LogOut, Search, type LucideIcon } from "lucide-react"
import { Fragment, useEffect, useState, type ReactNode } from "react"
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "@/components/ui/breadcrumb"
import { Button } from "@/components/ui/button"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command"
import { DropdownMenu, DropdownMenuCheckboxItem, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuShortcut, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Separator } from "@/components/ui/separator"
import {
  Sidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupLabel, SidebarHeader, SidebarInset, SidebarMenu, SidebarMenuAction,
  SidebarMenuButton, SidebarMenuItem, SidebarMenuSub, SidebarMenuSubButton, SidebarMenuSubItem, SidebarProvider, SidebarRail, SidebarTrigger, useSidebar,
} from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"
import { Inspector, InspectorProvider } from "@/lib/inspector"
import { KINDS, type Kind } from "@/lib/kinds"
import type { Account } from "@/lib/session"
import { cn } from "@/lib/utils"
import { ViewProvider, ViewToggle, useView } from "@/lib/view"
import { WORKSPACES, WORKSPACE_GROUPS } from "@/lib/workspaces"
import nornMark from "@/assets/norn-mark.webp"

export interface Section {
  readonly path: string
  readonly label: string
  readonly icon: LucideIcon
  readonly summary: string
  /** The kind of artefact the section lists, which colours its icon. */
  readonly kind?: Kind
  /** The section's own items (each object type, question or workflow), listed under it. */
  readonly useMenu?: () => readonly MenuEntry[] | undefined
  /** A count that needs attention, shown on the section; text (e.g. "500+") is shown as the server's lower bound. */
  readonly useBadge?: () => number | string | undefined
  /** The sidebar group the section is listed under. */
  readonly group?: string
  /** Listed at the foot of the sidebar instead of with the sections. */
  readonly foot?: boolean
}

export interface MenuEntry {
  /** The entry's route, e.g. `/sources/canon.Holding`, or `#anchor` on the section's own page. */
  readonly href: string
  readonly label: ReactNode
  /** The heading this entry is listed under. */
  readonly group?: string
  /** The entry's leading icon; its kind's icon when absent. */
  readonly icon?: LucideIcon
  /** The kind of artefact the entry is, which colours its icon. */
  readonly kind?: Kind
}

const none = () => undefined

/** Entries listed under a section before "Show more". */
const SHOWN = 6

/** Whether a route is the entry's page or a page under it. */
const within = (href: string, pathname: string) => !href.startsWith("#") && (pathname === href || pathname.startsWith(`${href}/`))

/** Where an entry goes: its route, or an anchor on its section's page. */
export const target = (section: Pick<Section, "path">, href: string) =>
  href.startsWith("#") ? { to: section.path, hash: href.slice(1) } : { to: href }

/** The section a route belongs to: its own path, or a record page under it. */
export const sectionOf = (sections: readonly Section[], pathname: string) =>
  sections.find(({ path }) => within(path, pathname))

/** The project switcher: the open workspace, and the others with whether Studio has recorded documents for them. */
function WorkspaceSwitcher() {
  const { isMobile } = useSidebar()
  const open = WORKSPACES.find(({ recorded }) => recorded)
  return (
    <SidebarMenu><SidebarMenuItem>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <SidebarMenuButton size="lg" className="data-[state=open]:bg-sidebar-accent">
            <img src={nornMark} alt="" width={32} height={32} className="size-8 shrink-0 object-contain" />
            <span className="grid flex-1 text-start text-sm leading-tight"><span className="truncate font-medium">{open?.name}</span><span className="truncate text-xs text-muted-foreground">{open?.detail}</span></span>
            <ChevronsUpDown className="ms-auto" />
          </SidebarMenuButton>
        </DropdownMenuTrigger>
        <DropdownMenuContent className="w-72" align="start" side={isMobile ? "bottom" : "right"} sideOffset={4}>
          {WORKSPACE_GROUPS.map((group, index) => (
            <DropdownMenuGroup key={group}>
              {index > 0 && <DropdownMenuSeparator />}
              <DropdownMenuLabel className="text-xs text-muted-foreground">{group}</DropdownMenuLabel>
              {WORKSPACES.filter((workspace) => workspace.group === group).map((workspace) => workspace.results !== undefined
                ? (
                  <DropdownMenuItem key={workspace.id} asChild className="flex-col items-start gap-0">
                    <a href={workspace.results} target="_blank" rel="noreferrer"><span>{workspace.name}</span><span className="text-xs text-muted-foreground">{workspace.detail} · results ↗</span></a>
                  </DropdownMenuItem>
                )
                : (
                  <DropdownMenuCheckboxItem key={workspace.id} checked={workspace === open} disabled={!workspace.recorded} className="flex-col items-start gap-0">
                    <span>{workspace.name}</span><span className="text-xs text-muted-foreground">{workspace.detail}</span>
                  </DropdownMenuCheckboxItem>
                ))}
            </DropdownMenuGroup>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </SidebarMenuItem></SidebarMenu>
  )
}

/** One section and, opened under it, its own entries. */
function NavSection({ section, pathname }: { readonly section: Section; readonly pathname: string }) {
  const { setOpenMobile } = useSidebar()
  const entries = (section.useMenu ?? none)() ?? []
  const badge = (section.useBadge ?? none)()
  const active = within(section.path, pathname)
  const [open, setOpen] = useState(active)
  const [more, setMore] = useState(false)
  useEffect(() => { if (active) setOpen(true) }, [active])
  const shown = more || entries.length <= SHOWN + 1 ? entries : entries.filter((entry, index) => index < SHOWN || within(entry.href, pathname))
  const Icon = section.icon
  return (
    <Collapsible asChild open={open} onOpenChange={setOpen}>
      <SidebarMenuItem>
        <SidebarMenuButton asChild isActive={active && pathname === section.path} tooltip={section.label} className={section.kind && KINDS[section.kind].svg}>
          <Link to={section.path} onClick={() => setOpenMobile(false)}>
            <Icon /><span>{section.label}</span>
            {badge !== undefined && badge !== 0 && badge !== "" && badge !== "0" && <span className="ms-auto me-5 flex h-4 min-w-4 items-center justify-center rounded-full bg-destructive px-1 text-[0.625rem] leading-none font-semibold text-destructive-foreground tabular-nums">{typeof badge === "string" ? badge : badge > 99 ? "99+" : badge}</span>}
          </Link>
        </SidebarMenuButton>
        {entries.length > 0 && <>
          <CollapsibleTrigger asChild>
            <SidebarMenuAction className="data-[state=open]:rotate-90"><ChevronRight /><span className="sr-only">{open ? "Hide" : "Show"} {section.label}</span></SidebarMenuAction>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <SidebarMenuSub>
              {shown.map((entry, index) => {
                const EntryIcon = entry.icon ?? (entry.kind ? KINDS[entry.kind].icon : undefined)
                const heading = entry.group !== undefined && entry.group !== shown[index - 1]?.group
                return (
                  <Fragment key={entry.href}>
                    {heading && <li className="px-2 pt-2 pb-1 text-xs text-muted-foreground">{entry.group}</li>}
                    <SidebarMenuSubItem>
                      <SidebarMenuSubButton asChild isActive={within(entry.href, pathname)} className={entry.kind && KINDS[entry.kind].svg}>
                        <Link {...target(section, entry.href)} onClick={() => setOpenMobile(false)}>{EntryIcon && <EntryIcon />}<span>{entry.label}</span></Link>
                      </SidebarMenuSubButton>
                    </SidebarMenuSubItem>
                  </Fragment>
                )
              })}
              {entries.length > SHOWN + 1 && (
                <SidebarMenuSubItem>
                  <SidebarMenuSubButton asChild><button type="button" onClick={() => setMore((current) => !current)} className="w-full text-muted-foreground">{more ? "Show less" : `Show ${entries.length - shown.length} more`}</button></SidebarMenuSubButton>
                </SidebarMenuSubItem>
              )}
            </SidebarMenuSub>
          </CollapsibleContent>
        </>}
      </SidebarMenuItem>
    </Collapsible>
  )
}

export const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((part) => part[0]?.toUpperCase()).join("") || "?"

/** The signed-in user, their organization and role, and sign-out. The role is shown, never used to hide or allow anything. */
export function AccountMenu({ account, onSignOut }: { readonly account: Account; readonly onSignOut: () => void }) {
  const { user, organization } = account
  return (
    <SidebarMenu><SidebarMenuItem>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <SidebarMenuButton size="lg" tooltip={`${user.name} · ${organization?.name ?? "No organization"}`}>
            <span className="flex size-8 shrink-0 items-center justify-center rounded-full bg-muted text-xs font-medium ring-1 ring-border">{initials(user.name || user.email)}</span>
            <span className="grid flex-1 text-start text-sm leading-tight"><span className="truncate font-medium">{user.name}</span><span className="truncate text-xs text-muted-foreground">{organization ? `${organization.name} · ${organization.role}` : "No organization"}</span></span>
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
    </SidebarMenuItem></SidebarMenu>
  )
}

/** A path segment as text; a malformed escape (e.g. `%E0%A4%A`) is shown as written instead of failing the page. */
export function decoded(segment: string): string {
  try {
    return decodeURIComponent(segment)
  } catch {
    return segment
  }
}

/** Section, item, then the record under it. */
function Crumbs({ section, pathname }: { readonly section: Section; readonly pathname: string }) {
  const entry = ((section.useMenu ?? none)() ?? []).find(({ href }) => within(href, pathname))
  const record = entry && pathname !== entry.href ? decoded(pathname.slice(entry.href.length + 1)) : undefined
  const last = entry === undefined ? "section" : record === undefined ? "entry" : "record"
  return (
    <Breadcrumb className="min-w-0">
      <BreadcrumbList className="flex-nowrap">
        <BreadcrumbItem className={cn(last !== "section" && "hidden md:inline-flex")}>
          {last === "section" ? <BreadcrumbPage>{section.label}</BreadcrumbPage> : <BreadcrumbLink asChild><Link to={section.path}>{section.label}</Link></BreadcrumbLink>}
        </BreadcrumbItem>
        {entry && <>
          <BreadcrumbSeparator className="hidden md:block" />
          <BreadcrumbItem className={cn("min-w-0", last === "record" && "hidden md:inline-flex")}>
            {last === "entry" ? <BreadcrumbPage className="truncate">{entry.label}</BreadcrumbPage> : <BreadcrumbLink asChild><Link to={entry.href} className="truncate">{entry.label}</Link></BreadcrumbLink>}
          </BreadcrumbItem>
        </>}
        {record !== undefined && <>
          <BreadcrumbSeparator className="hidden md:block" />
          <BreadcrumbItem className="min-w-0"><BreadcrumbPage className="truncate font-mono text-xs">{record}</BreadcrumbPage></BreadcrumbItem>
        </>}
      </BreadcrumbList>
    </Breadcrumb>
  )
}

/** "Search… ⌘K": every section and every entry the sections list. */
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
      <Button variant="outline" aria-label="Search" className="h-9 justify-start gap-2 px-2.5 font-normal text-muted-foreground shadow-none md:w-56 md:px-3" onClick={() => setOpen(true)}>
        <Search /><span className="hidden md:inline">Search…</span><kbd className="ms-auto hidden font-sans text-meta md:inline">⌘K</kbd>
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

/** The page bar's right-hand buttons: the view, the page's own tools, then shortcuts and notifications. */
function BarButtons({ tools }: { readonly tools?: ReactNode }) {
  const { view, setView } = useView()
  return (
    <div className="flex items-center gap-1">
      <ViewToggle view={view} onChange={setView} label="Business or technical view" />
      {tools}
      <DropdownMenu>
        <DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="hidden text-muted-foreground md:inline-flex"><Keyboard /><span className="sr-only">Keyboard shortcuts</span></Button></DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuLabel>Keyboard shortcuts</DropdownMenuLabel>
          <DropdownMenuItem disabled>Search<DropdownMenuShortcut>⌘K</DropdownMenuShortcut></DropdownMenuItem>
          <DropdownMenuItem disabled>Collapse the sidebar<DropdownMenuShortcut>⌘B</DropdownMenuShortcut></DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <DropdownMenu>
        <DropdownMenuTrigger asChild><Button variant="ghost" size="icon" className="text-muted-foreground"><Bell /><span className="sr-only">Notifications</span></Button></DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-64">
          <DropdownMenuLabel>Notifications</DropdownMenuLabel>
          <p className="px-2 pb-2 text-sm text-muted-foreground">Nothing yet. A recorded demo has no one to notify.</p>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

/**
 * A sidebar that collapses to icons (workspace, sections with their items,
 * who is viewing), a page bar with breadcrumbs, search and the view, the
 * page, and the inspector docked on its right.
 */
export function Shell({ sections, tools, account, onSignOut }: { readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly account: Account; readonly onSignOut: () => void }) {
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const current = sectionOf(sections, pathname)
  const main = sections.filter(({ foot }) => foot !== true)
  const foot = sections.filter(({ foot }) => foot === true)
  const groups = [...new Set(main.map(({ group }) => group))]
  return (
    <ViewProvider><InspectorProvider><TooltipProvider>
      <SidebarProvider>
        <Sidebar collapsible="icon">
          <SidebarHeader><WorkspaceSwitcher /></SidebarHeader>
          <SidebarContent>
            {groups.map((group) => (
              <SidebarGroup key={group ?? ""}>
                {group !== undefined && <SidebarGroupLabel>{group}</SidebarGroupLabel>}
                <SidebarMenu>{main.filter((section) => section.group === group).map((section) => <NavSection key={section.path} section={section} pathname={pathname} />)}</SidebarMenu>
              </SidebarGroup>
            ))}
            {foot.length > 0 && <SidebarGroup className="mt-auto"><SidebarMenu>{foot.map((section) => <NavSection key={section.path} section={section} pathname={pathname} />)}</SidebarMenu></SidebarGroup>}
          </SidebarContent>
          <SidebarFooter><AccountMenu account={account} onSignOut={onSignOut} /></SidebarFooter>
          <SidebarRail />
        </Sidebar>
        <SidebarInset className="min-w-0 flex-row">
          <div className="flex min-w-0 flex-1 flex-col">
            <header className="flex h-14 shrink-0 items-center gap-2 border-b px-3 md:px-4">
              <SidebarTrigger className="-ms-1" />
              <Separator orientation="vertical" className="me-1 h-4!" />
              <div className="min-w-0 flex-1">{current && <Crumbs key={current.path} section={current} pathname={pathname} />}</div>
              <CommandMenu sections={sections} />
              <BarButtons tools={tools} />
            </header>
            <div className="mx-auto w-full max-w-6xl px-4 py-6 md:px-8"><Outlet /></div>
          </div>
          <Inspector />
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider></InspectorProvider></ViewProvider>
  )
}
