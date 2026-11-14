import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router"
import { Command, CommandEmpty, CommandGroup, CommandGroupLabel, CommandInput, CommandItem, CommandItemIcon, CommandList } from "@ui-sdk/react/command"
import { Dialog, DialogBackdrop, DialogContent, DialogTitle, DialogTrigger } from "@ui-sdk/react/dialog"
import { MenuRadioGroup, MenuTrigger } from "@ui-sdk/react/menu"
import { PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover"
import { Toolbar, ToolbarButton, ToolbarLink } from "@ui-sdk/react/toolbar"
import { Tree, TreeGroup, TreeItem, TreeItemIndicator, TreeItemLabel } from "@ui-sdk/react/tree"
import { Bell, ChevronDown, ChevronRight, ChevronsLeft, CloudOff, Keyboard, LogOut, Plus, Search, SlidersHorizontal, type LucideIcon } from "lucide-react"
import { useState, type CSSProperties, type ReactNode } from "react"
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "@/components/ui/breadcrumb"
import { Button } from "@/components/ui/button"
import { Sidebar, SidebarInset, SidebarProvider, SidebarTrigger, useSidebar } from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"
import { Hint, KitMenuContent, KitMenuItem, KitMenuLabel, KitMenuRadioItem, KitMenuSeparator, KitPopoverContent, Menu, Popover } from "@/components/kit"
import { Inspector, InspectorProvider } from "@/lib/inspector"
import { KINDS, type Kind } from "@/lib/kinds"
import { cn } from "@/lib/utils"
import { ViewProvider, useView } from "@/lib/view"
import { LOCALES, LocaleProvider, directionOf, isLocale, useLocale } from "@/lib/locale"
import { WORKSPACES, WORKSPACE_GROUPS } from "@/lib/workspaces"
import { useMode } from "@/lib/live-transport"
import { useStudioScope } from "@/lib/definitions"
import { useAccount, type Account } from "@/lib/session"
import nornMark from "@/assets/norn-mark.webp"
import { Keys } from "@/components/keys"

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
  /** The rail group the section is listed under. */
  readonly group?: string
  /** Listed at the foot of the rail instead of with the sections. */
  readonly foot?: boolean
  /** The applet's primary action, shown under its title. */
  readonly action?: { readonly label: string; readonly to: string }
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
  /** A count the server reported for the entry (records, open items). */
  readonly count?: number | string
}

const none = () => undefined

/** Whether a route is the entry's page or a page under it. */
const within = (href: string, pathname: string) => !href.startsWith("#") && (pathname === href || pathname.startsWith(`${href}/`))

/** Where an entry goes: its route, or an anchor on its section's page. */
export const target = (section: Pick<Section, "path">, href: string) =>
  href.startsWith("#") ? { to: section.path, hash: href.slice(1) } : { to: href }

/** The section a route belongs to: its own path, or a record page under it. */
export const sectionOf = (sections: readonly Section[], pathname: string) =>
  sections.find(({ path }) => within(path, pathname))

export const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((part) => part[0]?.toUpperCase()).join("") || "?"

/** A count as shown: "99+" past 99, the server's text (e.g. "500+") as sent, nothing for zero. */
const shown = (count: number | string | undefined) =>
  count === undefined || count === 0 || count === "" || count === "0" ? undefined : typeof count === "string" ? count : count > 99 ? "99+" : String(count)

/** A path segment as text; a malformed escape (e.g. `%E0%A4%A`) is shown as written instead of failing the page. */
export function decoded(segment: string): string {
  try {
    return decodeURIComponent(segment)
  } catch {
    return segment
  }
}

/** The open workspace as the server reported it; until the mode is decided it says so, never naming the recording. */
function useOpenWorkspace() {
  const mode = useMode()
  const scope = useStudioScope()
  const account = useAccount()
  const live = mode.data?.kind === "live"
  const recorded = WORKSPACES.find(({ recorded }) => recorded)
  const open = live
    ? { id: "live", name: account.data?.organization?.name ?? scope.data?.orgId ?? "Connected cell", detail: `Live cell${scope.data ? ` · ${scope.data.orgId}/${scope.data.environmentId}` : ""}` }
    : mode.data?.kind !== "recorded"
      ? { id: "pending", name: account.data?.organization?.name ?? scope.data?.orgId ?? "Workspace", detail: mode.isError || mode.failureCount > 0 ? "Can't reach the cell · retrying" : "Connecting…" }
      : recorded && { id: recorded.id, name: recorded.name, detail: recorded.detail }
  return { live, open }
}

/** The scope switcher: the open workspace, and the others with whether Studio has recorded documents for them. */
function ScopeSwitcher() {
  const { live, open } = useOpenWorkspace()
  return (
    <Menu>
      <MenuTrigger className="group/scope -mx-1.5 flex max-w-full min-w-0 items-center gap-1 rounded-md px-1.5 py-0.5 text-start text-sm text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring data-open:bg-accent">
        <span className="truncate">{open?.name}<span className="text-muted-foreground"> · {open?.detail}</span></span>
        <ChevronDown aria-hidden className="size-3.5 shrink-0 transition-transform group-data-open/scope:rotate-180" />
      </MenuTrigger>
      <KitMenuContent align="start" className="w-72">
        <MenuRadioGroup value={live ? "" : open?.id ?? ""}>
          {WORKSPACE_GROUPS.map((group, index) => (
            <div key={group} role="none">
              {index > 0 && <KitMenuSeparator />}
              <KitMenuLabel>{group}</KitMenuLabel>
              {WORKSPACES.filter((workspace) => workspace.group === group).map((workspace) => workspace.results !== undefined
                ? (
                  <KitMenuItem key={workspace.id} value={workspace.id} label={workspace.name} render={<a href={workspace.results} target="_blank" rel="noreferrer" />} className="flex-col items-start gap-0">
                    <span>{workspace.name}</span><span className="text-xs text-muted-foreground">{workspace.detail} · results ↗</span>
                  </KitMenuItem>
                )
                : (
                  <KitMenuRadioItem key={workspace.id} value={workspace.id} label={workspace.name} disabled={live || !workspace.recorded} className="flex-col items-start gap-0">
                    <span>{workspace.name}</span><span className="text-xs text-muted-foreground">{workspace.detail}</span>
                  </KitMenuRadioItem>
                ))}
            </div>
          ))}
        </MenuRadioGroup>
      </KitMenuContent>
    </Menu>
  )
}

/** The view (Business or Technical) and the language, as menu choices. */
function ViewAndLanguage() {
  const { view, setView } = useView()
  const { locale, setLocale } = useLocale()
  return <>
    <MenuRadioGroup value={view} onValueChange={(next) => { if (next === "business" || next === "technical") setView(next) }}>
      <KitMenuLabel>View</KitMenuLabel>
      <KitMenuRadioItem value="business">Business</KitMenuRadioItem>
      <KitMenuRadioItem value="technical">Technical</KitMenuRadioItem>
    </MenuRadioGroup>
    <KitMenuSeparator />
    <MenuRadioGroup value={locale} onValueChange={(next) => { if (isLocale(next)) setLocale(next) }}>
      <KitMenuLabel>Language</KitMenuLabel>
      {LOCALES.map(({ tag, name }) => <KitMenuRadioItem key={tag} value={tag} lang={tag}>{name}</KitMenuRadioItem>)}
    </MenuRadioGroup>
  </>
}

/** The signed-in user, their organization and role, and sign-out. The role is shown, never used to hide or allow anything. */
export function AccountMenu({ account, onSignOut }: { readonly account: Account; readonly onSignOut: () => void }) {
  const { user, organization } = account
  return (
    <Menu onAction={(value) => { if (value === "sign-out") onSignOut() }}>
      <Hint label={`${user.name} · ${organization ? `${organization.name} · ${organization.role}` : "No organization"}`}>
        <ToolbarButton render={<MenuTrigger />} aria-label="Account" className="flex size-9 items-center justify-center rounded-full bg-background text-xs font-medium ring-1 ring-border outline-none hover:ring-foreground/30 focus-visible:ring-2 focus-visible:ring-ring">
          {initials(user.name || user.email)}
        </ToolbarButton>
      </Hint>
      <KitMenuContent side="end" align="end" className="min-w-60">
        <div className="grid px-2 py-1.5 text-sm">
          <span className="font-medium">{user.name}</span>
          <span className="text-meta text-muted-foreground">{user.email}</span>
        </div>
        <KitMenuSeparator />
        <div className="grid px-2 py-1.5 text-sm">
          <span>{organization?.name ?? "No organization"}</span>
          {organization && <span className="text-meta text-muted-foreground">{organization.name} · {organization.role}</span>}
        </div>
        <KitMenuSeparator />
        <ViewAndLanguage />
        <KitMenuSeparator />
        <KitMenuItem value="sign-out"><LogOut aria-hidden />Sign out</KitMenuItem>
      </KitMenuContent>
    </Menu>
  )
}

/** One applet on the rail: its icon, its name as a hint, and its count. */
function RailLink({ section }: { readonly section: Section }) {
  const { setOpenMobile } = useSidebar()
  const count = shown((section.useBadge ?? none)())
  const Icon = section.icon
  return (
    <Hint label={section.label}>
      <ToolbarLink
        render={<Link to={section.path} onClick={() => setOpenMobile(false)} />}
        aria-label={count ? `${section.label}, ${count} open` : section.label}
        className="relative flex size-9 items-center justify-center rounded-lg text-muted-foreground outline-none hover:bg-sidebar-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring data-[status=active]:bg-background data-[status=active]:text-foreground data-[status=active]:shadow-sm data-[status=active]:ring-1 data-[status=active]:ring-border [&_svg]:size-[1.15rem]"
      >
        <Icon aria-hidden />
        {count && <span data-count className="absolute -end-1 -top-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-destructive px-1 text-[0.625rem] leading-none font-semibold text-destructive-foreground tabular-nums ring-2 ring-sidebar">{count}</span>}
      </ToolbarLink>
    </Hint>
  )
}

/** The thin rail of applets: the mark, every section, then the viewer. */
function Rail({ sections, account, onSignOut }: { readonly sections: readonly Section[]; readonly account: Account; readonly onSignOut: () => void }) {
  const main = sections.filter(({ foot }) => foot !== true)
  const foot = sections.filter(({ foot }) => foot === true)
  const groups = [...new Set(main.map(({ group }) => group))]
  return (
    <Toolbar orientation="vertical" aria-label="Applets" className="flex h-full w-(--sidebar-width-icon) shrink-0 flex-col items-center gap-1 py-3">
      <Link to="/" aria-label="Norn Studio home" className="mb-2 flex size-9 items-center justify-center rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring">
        <img src={nornMark} alt="" width={28} height={28} className="size-7 object-contain" />
      </Link>
      {groups.map((group, index) => (
        <div key={group ?? ""} role="group" aria-label={group} className="flex flex-col items-center gap-1">
          {index > 0 && <div aria-hidden className="my-1.5 h-px w-5 bg-sidebar-border" />}
          {main.filter((section) => section.group === group).map((section) => <RailLink key={section.path} section={section} />)}
        </div>
      ))}
      <div className="mt-auto flex flex-col items-center gap-2">
        {foot.map((section) => <RailLink key={section.path} section={section} />)}
        <AccountMenu account={account} onSignOut={onSignOut} />
      </div>
    </Toolbar>
  )
}

const OVERVIEW = "__overview"

// Focus stays on the tree item; its row is the label inside.
const focusRow = "outline-none focus-visible:[&>[data-slot=tree-label]]:ring-2 focus-visible:[&>[data-slot=tree-label]]:ring-ring"

/** The selected applet's sidebar: its title, the scope, its primary action, and its pages grouped with their counts. */
function AppletPanel({ section, pathname }: { readonly section: Section; readonly pathname: string }) {
  const { setOpenMobile, toggleSidebar } = useSidebar()
  const entries = (section.useMenu ?? none)() ?? []
  const badge = shown((section.useBadge ?? none)())
  const groups = [...new Set(entries.map(({ group }) => group ?? ""))]
  const current = [...entries].filter(({ href }) => within(href, pathname)).sort((a, b) => b.href.length - a.href.length)[0]?.href ?? (pathname === section.path ? OVERVIEW : null)
  const [expanded, setExpanded] = useState<string[]>(() => groups.map((group) => `group:${group}`))
  const close = () => setOpenMobile(false)
  const label = "flex h-8 w-full min-w-0 items-center gap-2 rounded-lg px-2 text-sm text-sidebar-foreground/85 outline-none hover:bg-sidebar-accent hover:text-sidebar-foreground data-current:bg-sidebar-accent data-current:font-medium data-current:text-sidebar-foreground [&_svg]:size-4 [&_svg]:shrink-0"
  return (
    <div className="flex h-full min-w-0 flex-1 flex-col">
      <div className="space-y-3 px-4 pt-5 pb-3">
        <div className="space-y-1">
          <h2 className="truncate text-xl font-semibold tracking-tight">{section.label}</h2>
          <ScopeSwitcher />
        </div>
        {section.action && (
          <Button asChild className="w-full rounded-full">
            <Link to={section.action.to} onClick={close}><Plus aria-hidden />{section.action.label}</Link>
          </Button>
        )}
      </div>
      <nav aria-label={section.label} className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        <Tree aria-label={`${section.label} pages`} mode="none" current={current} expanded={[...new Set([...expanded, ...groups.map((heading) => `group:${heading}`).filter((value) => holds(value, current, entries))])]} onExpandedChange={setExpanded}>
          <TreeItem value={OVERVIEW} label="Overview" className={focusRow}>
            <TreeItemLabel render={<Link to={section.path} onClick={close} />} className={cn(label, section.kind && KINDS[section.kind].svg)}>
              <section.icon aria-hidden /><span className="truncate">Overview</span>
              {badge && <span className="ms-auto rounded-full bg-destructive px-1.5 text-[0.6875rem] leading-4 font-semibold text-destructive-foreground tabular-nums">{badge}</span>}
            </TreeItemLabel>
          </TreeItem>
          {groups.map((heading) => {
            const members = entries.filter((entry) => (entry.group ?? "") === heading)
            const items = members.map((entry) => {
              const Icon = entry.icon ?? (entry.kind ? KINDS[entry.kind].icon : undefined)
              const text = typeof entry.label === "string" ? entry.label : undefined
              const count = shown(entry.count)
              return (
                <TreeItem key={entry.href} value={entry.href} label={text} className={focusRow}>
                  <TreeItemLabel render={<Link {...target(section, entry.href)} onClick={close} />} className={cn(label, entry.kind && KINDS[entry.kind].svg)}>
                    {Icon && <Icon aria-hidden />}<span className="truncate">{entry.label}</span>
                    {count && <span className="ms-auto text-xs text-muted-foreground tabular-nums">{count}</span>}
                  </TreeItemLabel>
                </TreeItem>
              )
            })
            if (heading === "") return items
            return (
              <TreeItem key={heading} value={`group:${heading}`} label={heading} className={cn("mt-3", focusRow)}>
                <TreeItemLabel className="group/heading flex h-7 w-full items-center gap-1 rounded-md px-2 text-xs font-medium text-muted-foreground outline-none hover:text-foreground">
                  <span className="truncate">{heading}</span>
                  <span className="text-muted-foreground/70 tabular-nums">{members.length}</span>
                  <TreeItemIndicator className="ms-auto flex opacity-0 transition-[opacity,rotate] group-hover/heading:opacity-100 data-expanded:rotate-90"><ChevronRight aria-hidden className="size-3.5" /></TreeItemIndicator>
                </TreeItemLabel>
                <TreeGroup className="mt-0.5 space-y-0.5">{items}</TreeGroup>
              </TreeItem>
            )
          })}
        </Tree>
      </nav>
      <div className="flex items-center px-3 pb-3">
        <Hint label={<>Collapse the sidebar <Keys className="opacity-70">⌘B</Keys></>} side="top">
          <button type="button" onClick={toggleSidebar} aria-label="Collapse the sidebar" className="flex size-8 items-center justify-center rounded-lg text-muted-foreground outline-none hover:bg-sidebar-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring rtl:rotate-180"><ChevronsLeft aria-hidden className="size-4" /></button>
        </Hint>
      </div>
    </div>
  )
}

/** The current page's group stays open so its row is never hidden. */
const holds = (value: string, current: string | null, entries: readonly MenuEntry[]) =>
  current !== null && entries.some(({ href, group: heading }) => href === current && `group:${heading ?? ""}` === value)

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

/** "Search… ⌘K": every applet and every page the applets list. */
function CommandMenu({ sections }: { readonly sections: readonly Section[] }) {
  const [open, setOpen] = useState(false)
  const navigate = useNavigate()
  const lists = sections.map((section) => ({ section, entries: (section.useMenu ?? none)() ?? [] }))
  const routes = new Map<string, { to: string; hash?: string }>([
    ...sections.map(({ path }) => [`section:${path}`, { to: path }] as const),
    ...lists.flatMap(({ section, entries }) => entries.map(({ href }) => [`entry:${section.path}:${href}`, target(section, href)] as const)),
  ])
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger shortcut="mod+k" aria-label="Search" className="flex h-9 w-full max-w-md min-w-0 items-center gap-2 rounded-full bg-muted/60 px-3 text-sm text-muted-foreground ring-1 ring-border outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring max-md:w-9 max-md:justify-center max-md:px-0">
        <Search aria-hidden className="size-4 shrink-0" /><span className="hidden truncate md:inline">Search…</span><Keys className="ms-auto hidden text-meta md:inline">⌘K</Keys>
      </DialogTrigger>
      <DialogBackdrop className="fixed inset-0 bg-black/40 transition-opacity duration-150 data-starting-style:opacity-0 data-ending-style:opacity-0" />
      <DialogContent className="fixed top-[12vh] left-1/2 m-0 w-[min(36rem,calc(100vw-2rem))] -translate-x-1/2 overflow-hidden rounded-2xl bg-popover p-0 text-popover-foreground shadow-2xl ring-1 ring-border outline-none transition-[opacity,scale] duration-150 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0">
        <DialogTitle className="sr-only">Search Studio</DialogTitle>
        <Command onAction={(value) => { const to = routes.get(value); setOpen(false); if (to) void navigate(to) }}>
          <div className="flex items-center gap-2 border-b px-4">
            <Search aria-hidden className="size-4 text-muted-foreground" />
            <CommandInput aria-label="Search applets, object types, questions" placeholder="Search applets, object types, questions…" className="h-12 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground" />
          </div>
          <CommandEmpty className="px-4 py-8 text-center text-sm text-muted-foreground">Nothing matches.</CommandEmpty>
          <CommandList className="max-h-[min(24rem,60vh)] overflow-y-auto p-1.5">
            <CommandGroup>
              <CommandGroupLabel className="px-2 pt-2 pb-1 text-xs text-muted-foreground">Applets</CommandGroupLabel>
              {sections.map(({ path, label, icon: Icon }) => (
                <CommandItem key={path} value={`section:${path}`} label={label} className="flex cursor-default items-center gap-2 rounded-lg px-2 py-1.5 text-sm data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Icon /></CommandItemIcon>{label}
                </CommandItem>
              ))}
            </CommandGroup>
            {lists.filter(({ entries }) => entries.length > 0).map(({ section, entries }) => (
              <CommandGroup key={section.path}>
                <CommandGroupLabel className="px-2 pt-3 pb-1 text-xs text-muted-foreground">{section.label}</CommandGroupLabel>
                {entries.map((entry) => (
                  <CommandItem key={entry.href} value={`entry:${section.path}:${entry.href}`} label={typeof entry.label === "string" ? entry.label : entry.href} keywords={entry.group ? [entry.group] : undefined} className="flex cursor-default items-center gap-2 rounded-lg px-2 py-1.5 text-sm data-highlighted:bg-accent">
                    <span className="truncate">{entry.label}</span>{entry.group && <span className="ms-auto truncate text-xs text-muted-foreground">{entry.group}</span>}
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
          </CommandList>
        </Command>
      </DialogContent>
    </Dialog>
  )
}

/**
 * The cell did not answer the mode read (a network error or a 5xx): say so above the page and keep
 * retrying; nothing is shown from the recording meanwhile.
 */
function ConnectionBanner() {
  const mode = useMode()
  if (mode.data !== undefined || (mode.failureCount === 0 && !mode.isError)) return null
  return (
    <div role="status" data-connection="retrying" className="flex flex-wrap items-center gap-2 border-b border-tone-amber/30 bg-tone-amber/10 px-4 py-2 text-sm text-tone-amber md:px-8">
      <CloudOff aria-hidden className="size-4 shrink-0" />
      <span className="font-medium">Couldn't reach the cell.</span>
      <span className="text-foreground/80">{mode.isError ? "It didn't answer after several tries." : "Retrying…"}</span>
      <Button type="button" variant="outline" size="xs" className="ms-auto" disabled={mode.isFetching} onClick={() => void mode.refetch()}>Retry now</Button>
    </div>
  )
}

const iconButton = "flex size-9 items-center justify-center rounded-full text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring data-open:bg-accent [&_svg]:size-[1.15rem]"

/** The page bar's right-hand buttons: the page's own tools, then shortcuts and notifications. */
function BarButtons({ tools }: { readonly tools?: ReactNode }) {
  const { view } = useView()
  return (
    <div className="flex items-center gap-0.5">
      {view === "technical" && <span className="hidden rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground lg:inline">Technical view</span>}
      {/* On a phone the rail's account menu is behind the sheet, so one small menu holds the view and language. */}
      <Menu>
        <MenuTrigger aria-label="Language and view" className={cn(iconButton, "md:hidden")}><SlidersHorizontal aria-hidden /></MenuTrigger>
        <KitMenuContent align="end" className="w-52"><ViewAndLanguage /></KitMenuContent>
      </Menu>
      {tools}
      <Popover>
        <PopoverTrigger aria-label="Keyboard shortcuts" className={cn(iconButton, "max-md:hidden")}><Keyboard aria-hidden /></PopoverTrigger>
        <KitPopoverContent align="end" className="w-60">
          <PopoverTitle className="mb-2 text-sm font-medium">Keyboard shortcuts</PopoverTitle>
          <dl className="grid grid-cols-[1fr_auto] gap-y-1.5 text-sm">
            <dt className="text-muted-foreground">Search</dt><dd><Keys>⌘K</Keys></dd>
            <dt className="text-muted-foreground">Collapse the sidebar</dt><dd><Keys>⌘B</Keys></dd>
            <dt className="text-muted-foreground">Move between applets</dt><dd><Keys>↑ ↓</Keys></dd>
            <dt className="text-muted-foreground">Jump to a page</dt><dd className="text-muted-foreground">type its name</dd>
          </dl>
        </KitPopoverContent>
      </Popover>
      <Popover>
        <PopoverTrigger aria-label="Notifications" className={iconButton}><Bell aria-hidden /></PopoverTrigger>
        <KitPopoverContent align="end">
          <PopoverTitle className="mb-1 text-sm font-medium">Notifications</PopoverTitle>
          <p className="text-sm text-muted-foreground">Nothing yet.</p>
        </KitPopoverContent>
      </Popover>
    </div>
  )
}

/**
 * The app rail and the selected applet's sidebar (shadcn's sidebar-09 layout), the page in a
 * rounded card beside them, and the inspector docked on its right.
 */
export function Shell(props: { readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly account: Account; readonly onSignOut: () => void }) {
  return <LocaleProvider><LocalizedShell {...props} /></LocaleProvider>
}

/** The shell in the reader's direction: in Arabic the rail docks on the right. */
function LocalizedShell({ sections, tools, account, onSignOut }: { readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly account: Account; readonly onSignOut: () => void }) {
  const { locale } = useLocale()
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const current = sectionOf(sections, pathname)
  return (
    <ViewProvider><InspectorProvider><TooltipProvider>
      <SidebarProvider data-shell="rail" className="bg-shell has-data-[variant=inset]:bg-shell" style={{ "--sidebar-width": "20rem", "--sidebar-width-icon": "3.5rem" } as CSSProperties}>
        <Sidebar collapsible="icon" variant="inset" side={directionOf(locale) === "rtl" ? "right" : "left"}
          className="overflow-hidden p-0! group-data-[collapsible=icon]:w-(--sidebar-width-icon)!">
          <div className="flex size-full min-w-0 flex-row bg-shell">
          <Rail sections={sections} account={account} onSignOut={onSignOut} />
          <div className="flex min-w-0 flex-1 py-2 max-md:pe-2">
            <div className="flex min-w-0 flex-1 overflow-hidden rounded-2xl bg-sidebar ring-1 ring-sidebar-border">
              {current
                ? <AppletPanel key={current.path} section={current} pathname={pathname} />
                : <div className="px-4 pt-5"><h2 className="text-xl font-semibold tracking-tight">Studio</h2><ScopeSwitcher /></div>}
            </div>
          </div>
          </div>
        </Sidebar>
        <SidebarInset className="min-w-0 flex-row overflow-hidden ring-1 ring-border md:m-2! md:peer-data-[state=expanded]:ms-2! md:peer-data-[side=left]:peer-data-[state=collapsed]:ms-0!">
          <div className="flex min-w-0 flex-1 flex-col">
            <header className="grid h-14 shrink-0 grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-2 px-3 md:px-4">
              <div className="flex min-w-0 items-center gap-1.5">
                <SidebarTrigger className={cn("-ms-1 text-muted-foreground")} />
                <div className="min-w-0">{current && <Crumbs key={current.path} section={current} pathname={pathname} />}</div>
              </div>
              <div className="flex justify-center md:w-[min(28rem,40vw)]"><CommandMenu sections={sections} /></div>
              <div className="flex justify-end"><BarButtons tools={tools} /></div>
            </header>
            <ConnectionBanner />
            <div className="mx-auto w-full max-w-6xl px-4 py-6 md:px-8"><Outlet /></div>
          </div>
          <Inspector />
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider></InspectorProvider></ViewProvider>
  )
}
