import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router"
import { Command, CommandEmpty, CommandGroup, CommandGroupLabel, CommandInput, CommandItem, CommandItemIcon, CommandList } from "@ui-sdk/react/command"
import { Dialog, DialogBackdrop, DialogContent, DialogTitle, DialogTrigger } from "@ui-sdk/react/dialog"
import { MenuGroup, MenuRadioGroup, MenuTrigger } from "@ui-sdk/react/menu"
import { PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover"
import { Toolbar, ToolbarButton, ToolbarLink, ToolbarSeparator } from "@ui-sdk/react/toolbar"
import { Tree, TreeGroup, TreeItem, TreeItemIndicator, TreeItemLabel } from "@ui-sdk/react/tree"
import { AssistantPanel, AssistantProvider, useAssistant, type AssistantContent } from "@/assistant-slot"
import { Bell, ChevronDown, ChevronLeft, ChevronRight, ChevronsUpDown, CircleEllipsis, CloudOff, Keyboard, LayoutGrid, LogOut, MessageCircle, Palette, PanelBottom, PanelLeft, Search, SlidersHorizontal, Sparkles, Wrench, type LucideIcon } from "lucide-react"
import { useEffect, useRef, useState, type CSSProperties, type ReactNode, type RefObject } from "react"
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "@/components/ui/breadcrumb"
import { Button } from "@/components/ui/button"
import { ChromeDock, ChromeDockOption, ChromeDockTray, ChromeDockTrigger } from "@/components/ui/chrome-dock"
import { AppearanceProvider, SHELLS, SKINS, isShellChrome, isSkin, useAppearance, type ShellChrome, type Skin } from "@/lib/appearance"
import { PERSPECTIVES, PerspectiveProvider, isPerspective, usePerspective, type Perspective } from "@/lib/perspective"
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
  /** The perspective that lists it; a global destination (Home, Inbox) sits on the rail in both. */
  readonly perspective: Perspective | "global"
  /** The sidebar section (a space) it is listed under: purely organisational, collapsible, never a wall. */
  readonly group?: string
  /** The kind of artefact the destination lists, which colours its icon. */
  readonly kind?: Kind
  /** The destination's own items (each object type, metric or workflow), listed under it. */
  readonly useMenu?: () => readonly MenuEntry[] | undefined
  /** A count that needs attention, shown on the destination; text (e.g. "500+") is shown as the server's lower bound. */
  readonly useBadge?: () => number | string | undefined
  /** Other route prefixes that belong to this destination, e.g. `/objects` (a type's records) under Sources. */
  readonly paths?: readonly string[]
  /** False when the destination has no page of its own and only groups its items (Records). */
  readonly index?: false
  /** True when menu entries carrying `group` pin directly into that Space in the sidebar. */
  readonly pinned?: boolean
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
  /** Another route prefix that is this entry too, e.g. a type's record pages under `/sources/<type>`. */
  readonly also?: string
}

const none = () => undefined

/** Whether a route is the entry's page or a page under it. */
const within = (href: string, pathname: string) => !href.startsWith("#") && (pathname === href || pathname.startsWith(`${href}/`))

/** The prefix of `pathname` an entry claims: its own route, or the other route it also is; none when it claims neither. */
const claim = (entry: Pick<MenuEntry, "href" | "also">, pathname: string) =>
  within(entry.href, pathname) ? entry.href : entry.also !== undefined && within(entry.also, pathname) ? entry.also : undefined

/** Where an entry goes: its route, or an anchor on its section's page. */
export const target = (section: Pick<Section, "path">, href: string) =>
  href.startsWith("#") ? { to: section.path, hash: href.slice(1) } : { to: href }

/** The section a route belongs to: its own path, or a record page under it. */
export const sectionOf = (sections: readonly Section[], pathname: string) =>
  sections.find(({ path }) => within(path, pathname)) ?? sections.find(({ paths }) => paths?.some((prefix) => within(prefix, pathname)))

/** The destination a route belongs to: by its own routes, else the destination one of whose items claims it. */
export const destinationOf = (sections: readonly Section[], pathname: string, menus: ReadonlyMap<string, readonly MenuEntry[]>) =>
  sectionOf(sections, pathname) ?? sections.find(({ path }) => (menus.get(path) ?? []).some((entry) => claim(entry, pathname) !== undefined))

/**
 * The perspective a page is shown in: the reader's own while the page belongs to it (or to both, like
 * Home), else the one that lists it, so the sidebar always shows where the reader is.
 */
export function perspectiveOf(sections: readonly Section[], pathname: string, chosen: Perspective, menus: ReadonlyMap<string, readonly MenuEntry[]> = new Map()): Perspective {
  if (destinationOf(sections.filter(({ perspective }) => perspective === chosen || perspective === "global"), pathname, menus) !== undefined) return chosen
  const other = destinationOf(sections, pathname, menus)?.perspective
  return other === undefined || other === "global" ? chosen : other
}

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

/** A workspace entry that does something here: published results, or the recording while no cell is connected. */
const real = (workspace: (typeof WORKSPACES)[number], live: boolean) => workspace.results !== undefined || (workspace.recorded && !live)

/** The scope switcher: the open workspace, and the others that open from here. */
function ScopeSwitcher({ variant = "sidebar" }: { readonly variant?: "sidebar" | "pill" }) {
  const { live, open } = useOpenWorkspace()
  return (
    <Menu>
      {variant === "pill"
        ? (
          <MenuTrigger data-scope className="group/scope inline-flex h-8 max-w-60 min-w-0 items-center gap-2 rounded-full border border-border bg-card ps-1.5 pe-2.5 text-sm text-foreground transition-[color,background-color] duration-150 outline-none hover:bg-(--well) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-open:bg-(--well)">
            <span aria-hidden className="flex size-5 shrink-0 items-center justify-center rounded-full bg-(--well) text-[0.625rem] font-medium text-foreground ring-1 ring-border">{initials(open?.name ?? "")}</span>
            <span className="truncate font-normal tracking-[-0.01em]">{open?.name}</span>
            <ChevronDown aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
          </MenuTrigger>
        )
        : (
          <MenuTrigger data-scope className="group/scope flex w-full min-w-0 items-center gap-2.5 rounded-lg px-2 py-1.5 text-start text-sm transition-[color,background-color] duration-150 outline-none hover:bg-sidebar-accent focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring data-open:bg-sidebar-accent">
            <span aria-hidden className="flex size-6 shrink-0 items-center justify-center rounded-md bg-background text-[0.6875rem] font-semibold text-foreground shadow-control ring-1 ring-border/90 dark:bg-card">{initials(open?.name ?? "")}</span>
            <span className="grid min-w-0 flex-1 leading-tight" title={`${open?.name ?? ""} · ${open?.detail ?? ""}`}>
              <span className="truncate font-semibold tracking-[-0.01em] text-sidebar-foreground">{open?.name}</span>
              <span className="truncate text-xs text-muted-foreground">{open?.detail}</span>
            </span>
            <ChevronsUpDown aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
          </MenuTrigger>
        )}
      <KitMenuContent align="start" className="w-72">
        <MenuRadioGroup value={live ? "live" : open?.id ?? ""}>
          {live && open && (
            <KitMenuRadioItem value="live" className="flex-col items-start gap-0">
              <span>{open.name}</span><span className="text-xs text-muted-foreground">{open.detail}</span>
            </KitMenuRadioItem>
          )}
          {/* Only what opens: the recorded workspace and published results; nothing greyed out or "coming soon". */}
          {WORKSPACE_GROUPS.filter((group) => WORKSPACES.some((workspace) => workspace.group === group && real(workspace, live))).map((group, index) => (
            <div key={group} role="none">
              {(index > 0 || live) && <KitMenuSeparator />}
              <KitMenuLabel>{group}</KitMenuLabel>
              {WORKSPACES.filter((workspace) => workspace.group === group && real(workspace, live)).map((workspace) => workspace.results !== undefined
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

/** The view (Business or Technical), language, skin and chrome layout as menu choices. */
function ViewAndLanguage() {
  const { view, setView } = useView()
  const { locale, setLocale } = useLocale()
  const { skin, setSkin, shell, setShell } = useAppearance()
  return <>
    <MenuRadioGroup value={view} onValueChange={(next) => { if (next === "business" || next === "technical") setView(next) }}>
      <KitMenuLabel>View</KitMenuLabel>
      <KitMenuRadioItem value="business" closeOnSelect>Business</KitMenuRadioItem>
      <KitMenuRadioItem value="technical" closeOnSelect>Technical</KitMenuRadioItem>
    </MenuRadioGroup>
    <KitMenuSeparator />
    <MenuRadioGroup value={locale} onValueChange={(next) => { if (isLocale(next)) setLocale(next) }}>
      <KitMenuLabel>Language</KitMenuLabel>
      {LOCALES.map(({ tag, name }) => <KitMenuRadioItem key={tag} value={tag} lang={tag} closeOnSelect>{name}</KitMenuRadioItem>)}
    </MenuRadioGroup>
    <KitMenuSeparator />
    <MenuRadioGroup value={skin} onValueChange={(next) => { if (isSkin(next)) setSkin(next) }}>
      <KitMenuLabel>Skin</KitMenuLabel>
      {SKINS.map(({ id, label }) => <KitMenuRadioItem key={id} value={id} closeOnSelect>{label}</KitMenuRadioItem>)}
    </MenuRadioGroup>
    <KitMenuSeparator />
    <MenuRadioGroup value={shell} onValueChange={(next) => { if (isShellChrome(next)) setShell(next) }}>
      <KitMenuLabel>Chrome</KitMenuLabel>
      {SHELLS.map(({ id, label }) => <KitMenuRadioItem key={id} value={id} closeOnSelect>{label}</KitMenuRadioItem>)}
    </MenuRadioGroup>
  </>
}

/** Interactive skin & shell chrome tray powered by the first-party `@ui-sdk/core` + `@ui-sdk/react` ChromeDock primitive. */
function AppearanceTray({ side = "bottom", variant = "bar" }: { readonly side?: "top" | "bottom"; readonly variant?: "bar" | "dock" }) {
  const { skin, setSkin, shell, setShell } = useAppearance()
  return (
    <ChromeDock
      value={skin}
      items={SKINS.map(({ id }) => id)}
      onValueChange={(next) => { if (isSkin(next)) setSkin(next) }}
      className="relative inline-flex"
    >
      <ChromeDockTrigger
        aria-label="Appearance"
        className={variant === "dock"
          ? "relative grid size-10 place-items-center rounded-full text-(--dock-muted) transition-colors outline-none hover:text-(--dock-foreground) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-open:bg-(--dock-well) data-open:text-(--dock-foreground) [&_svg]:size-[18px]"
          : cn(iconButton, "hidden @xl:flex")}
      >
        <Palette aria-hidden strokeWidth={1.7} />
      </ChromeDockTrigger>
      <ChromeDockTray
        side={side}
        align="end"
        sideOffset={8}
        aria-label="Appearance settings"
        className="w-72 origin-(--transform-origin) rounded-2xl bg-popover p-3.5 text-popover-foreground shadow-float ring-1 ring-border outline-none transition-[opacity,scale] duration-150 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0"
      >
        <div className="space-y-3">
          <div className="space-y-1.5">
            <div className="flex items-center justify-between text-xs font-medium text-muted-foreground">
              <span>Skin</span>
              <span className="capitalize text-foreground">{skin}</span>
            </div>
            <div role="group" aria-label="Skin" className="grid grid-cols-2 gap-1.5">
              {SKINS.map((option) => (
                <ChromeDockOption
                  key={option.id}
                  value={option.id}
                  data-skin-option={option.id}
                  className="flex items-center gap-2 rounded-lg border border-border/80 bg-background/70 px-2.5 py-2 text-start text-xs font-medium text-foreground transition-colors outline-none hover:bg-surface-subtle focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-[state=selected]:border-action data-[state=selected]:bg-action/10 data-[state=selected]:text-foreground"
                >
                  <span aria-hidden className="size-3.5 shrink-0 rounded-full border border-border/90 shadow-xs" style={{ backgroundColor: option.swatch }} />
                  <span className="truncate">{option.label}</span>
                </ChromeDockOption>
              ))}
            </div>
          </div>
          <div className="h-px bg-border/80" />
          <div className="space-y-1.5">
            <div className="text-xs font-medium text-muted-foreground">Shell chrome</div>
            <div role="group" aria-label="Shell chrome" className="grid grid-cols-2 gap-1.5">
              {SHELLS.map((option) => {
                const active = shell === option.id
                return (
                  <button
                    key={option.id}
                    type="button"
                    aria-pressed={active}
                    data-shell-option={option.id}
                    data-state={active ? "selected" : "idle"}
                    onClick={() => setShell(option.id)}
                    className="flex items-center gap-1.5 rounded-lg border border-border/80 bg-background/70 px-2.5 py-2 text-xs font-medium text-foreground transition-colors outline-none hover:bg-surface-subtle focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring aria-pressed:border-action aria-pressed:bg-action/10"
                  >
                    {option.id === "sidebar" ? <PanelLeft aria-hidden className="size-3.5 shrink-0" /> : <PanelBottom aria-hidden className="size-3.5 shrink-0" />}
                    <span className="truncate">{option.label}</span>
                  </button>
                )
              })}
            </div>
          </div>
        </div>
      </ChromeDockTray>
    </ChromeDock>
  )
}

/** The signed-in user, their organization and role, and sign-out. The role is shown, never used to hide or allow anything. */
export function AccountMenu({ account, onSignOut, inToolbar = true, variant = "rail" }: {
  readonly account: Account
  readonly onSignOut: () => void
  readonly inToolbar?: boolean
  readonly variant?: "rail" | "topbar" | "dock"
}) {
  const { user, organization } = account
  const triggerClass = variant === "topbar"
    ? "grid size-9 place-items-center rounded-full bg-[linear-gradient(135deg,#fde2e4,#fbe8cf)] text-[0.8125rem] font-medium text-[#8a3b2c] ring-1 ring-black/5 outline-none transition-[box-shadow,transform] hover:ring-foreground/20 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring dark:bg-[linear-gradient(135deg,#33252a,#332c22)] dark:text-[#f6d6c9] dark:ring-white/10"
    : variant === "dock"
      ? "flex size-8 items-center justify-center rounded-full bg-(--dock-active) text-[0.6875rem] font-medium text-(--dock-foreground) ring-1 ring-(--dock-border) outline-none hover:ring-(--dock-foreground)/40 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
      : "flex size-8 items-center justify-center rounded-full bg-background text-[0.6875rem] font-medium ring-1 ring-border outline-none hover:ring-foreground/30 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
  const trigger = inToolbar
    ? <ToolbarButton render={<MenuTrigger />} aria-label="Account" className={triggerClass}>{initials(user.name || user.email)}</ToolbarButton>
    : <MenuTrigger aria-label="Account" className={triggerClass}>{initials(user.name || user.email)}</MenuTrigger>
  return (
    <Menu onAction={(value) => { if (value === "sign-out") onSignOut() }}>
      <Hint side={variant === "topbar" ? "bottom" : variant === "dock" ? "top" : "right"} label={`${user.name} · ${organization ? `${organization.name} · ${organization.role}` : "No organization"}`}>
        {trigger}
      </Hint>
      <KitMenuContent side={variant === "topbar" ? "bottom" : variant === "dock" ? "top" : "end"} align="end" className="min-w-60">
        <div className="grid px-2 py-1.5 text-sm">
          <span className="font-medium">{user.name}</span>
          <span className="text-meta text-muted-foreground">{user.email}</span>
        </div>
        <KitMenuSeparator />
        <div className="grid px-2 py-1.5 text-sm">
          <span>{organization?.name ?? "No organization"}</span>
          {organization && <span className="text-meta text-muted-foreground">Role: {organization.role}</span>}
        </div>
        <KitMenuSeparator />
        <ViewAndLanguage />
        <KitMenuSeparator />
        <KitMenuItem value="sign-out"><LogOut aria-hidden />Sign out</KitMenuItem>
      </KitMenuContent>
    </Menu>
  )
}

/** Every destination's items and count, read once for the rail, the sidebar, the breadcrumbs and search. */
function useNavigation(sections: readonly Section[]) {
  // Every section's hooks run on every render, in one order, whichever perspective is shown.
  const menus = new Map(sections.map((section) => [section.path, (section.useMenu ?? none)() ?? []] as const))
  const badges = new Map(sections.map((section) => [section.path, shown((section.useBadge ?? none)())] as const))
  return { menus, badges }
}

type Navigation = ReturnType<typeof useNavigation>

const railButton = "relative flex size-8 items-center justify-center rounded-lg text-muted-foreground transition-[color,background-color] duration-150 outline-none hover:bg-sidebar-accent hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring [&_svg]:size-[1.0625rem]"
const railActive = "bg-action/12 text-action ring-1 ring-inset ring-action/20 hover:bg-action/15 hover:text-action dark:bg-action/18 dark:ring-action/30"

/** One destination on the rail: its icon, its name as a hint, and its count. */
function RailLink({ section, count, firstHref, active, onExpandSection }: {
  readonly section: Section; readonly count: string | undefined; readonly firstHref?: string; readonly active?: boolean; readonly onExpandSection?: (path: string) => void
}) {
  const { setOpen, setOpenMobile } = useSidebar()
  const Icon = section.icon
  const label = count ? `${section.label}, ${count} open` : section.label
  const badge = count && <span data-count className="absolute -end-1 -top-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-destructive px-1 text-[0.625rem] leading-none font-semibold text-destructive-foreground tabular-nums ring-2 ring-shell">{count}</span>
  // A destination without its own index page (Records) opens its first item, or expands the sidebar when none is loaded yet.
  const to = section.index === false ? firstHref : section.path
  if (to === undefined) {
    return (
      <Hint label={section.label}>
        <ToolbarButton aria-label={label} data-active={active || undefined} onClick={() => { onExpandSection?.(section.path); setOpen(true) }}
          className={cn(railButton, active && railActive)}>
          <Icon aria-hidden />{badge}
        </ToolbarButton>
      </Hint>
    )
  }
  return (
    <Hint label={section.label}>
      <ToolbarLink
        render={<Link to={to} activeOptions={{ exact: to === "/" }} onClick={() => setOpenMobile(false)} />}
        aria-label={label}
        data-active={active || undefined}
        className={cn(railButton, "data-[status=active]:bg-action/12 data-[status=active]:text-action data-[status=active]:ring-1 data-[status=active]:ring-inset data-[status=active]:ring-action/20 dark:data-[status=active]:bg-action/18 dark:data-[status=active]:ring-action/30", active && railActive)}
      >
        <Icon aria-hidden />{badge}
      </ToolbarLink>
    </Hint>
  )
}

/** The rail's assistant button: opens the panel beside the page (also ⌘J). */
function AssistantToggle({ title }: { readonly title: string }) {
  const { open, toggle } = useAssistant()
  return (
    <Hint label={<>{title} <Keys className="opacity-70">⌘J</Keys></>}>
      <ToolbarButton aria-label={title} aria-pressed={open} onClick={toggle} className={cn(railButton, "aria-pressed:bg-action/12 aria-pressed:text-action aria-pressed:ring-1 aria-pressed:ring-inset aria-pressed:ring-action/20 dark:aria-pressed:bg-action/18")}>
        <Sparkles aria-hidden />
      </ToolbarButton>
    </Hint>
  )
}

/**
 * The slim rail: the mark, the destinations every perspective shares (Home, Inbox), then the
 * assistant and the viewer. With the sidebar collapsed it also carries the perspective's
 * destinations, so nothing is out of reach.
 */
function Rail({ sections, perspective, navigation, current, account, onSignOut, assistant, railRef, onExpandSection }: {
  readonly sections: readonly Section[]; readonly perspective: Perspective; readonly navigation: Navigation
  readonly current: Section | undefined; readonly account: Account; readonly onSignOut: () => void; readonly assistant?: AssistantContent
  readonly railRef: RefObject<HTMLDivElement | null>; readonly onExpandSection: (path: string) => void
}) {
  const { state, isMobile } = useSidebar()
  const global = sections.filter((section) => section.perspective === "global")
  const own = state === "collapsed" && !isMobile ? sections.filter((section) => section.perspective === perspective) : []
  const groups = [...new Set(own.map(({ group }) => group))]
  const firstHref = (section: Section) => navigation.menus.get(section.path)?.find(({ href }) => !href.startsWith("#"))?.href
  return (
    <div ref={railRef} className="h-full shrink-0 border-e border-sidebar-border/75 group-data-[collapsible=icon]:border-e-0">
      <Toolbar orientation="vertical" aria-label="Studio" className="flex h-full w-(--sidebar-width-icon) flex-col items-center gap-1 py-2.5">
        <Link to="/" aria-label="Norn Studio home" className="mb-2 flex size-8 items-center justify-center rounded-lg outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring">
          <img src={nornMark} alt="" width={24} height={24} className="size-6 object-contain" />
        </Link>
        {global.map((section) => <RailLink key={section.path} section={section} count={navigation.badges.get(section.path)} />)}
        {groups.map((group) => (
          <div key={group ?? ""} role="group" aria-label={group} className="flex flex-col items-center gap-1">
            <div aria-hidden className="my-1.5 h-px w-4 bg-sidebar-border" />
            {own.filter((section) => section.group === group).map((section) => (
              <RailLink key={section.path} section={section} count={navigation.badges.get(section.path)}
                firstHref={firstHref(section)} active={section.index === false && current?.path === section.path}
                onExpandSection={onExpandSection} />
            ))}
          </div>
        ))}
        <div className="mt-auto flex flex-col items-center gap-2">
          {assistant && <AssistantToggle title={assistant.title} />}
          <AccountMenu account={account} onSignOut={onSignOut} />
        </div>
      </Toolbar>
    </div>
  )
}

// Focus stays on the tree item; its row is the label inside.
const focusRow = "outline-none focus-visible:[&>[data-slot=tree-label]]:relative focus-visible:[&>[data-slot=tree-label]]:z-10 focus-visible:[&>[data-slot=tree-label]]:outline-2 focus-visible:[&>[data-slot=tree-label]]:outline-solid focus-visible:[&>[data-slot=tree-label]]:outline-offset-1 focus-visible:[&>[data-slot=tree-label]]:outline-ring"
const row = "relative flex h-7.5 w-full min-w-0 items-center gap-2 rounded-lg px-2 text-sm text-sidebar-foreground/80 transition-[color,background-color] duration-150 outline-none hover:bg-sidebar-accent hover:text-sidebar-foreground data-current:bg-background data-current:font-medium data-current:text-foreground data-current:shadow-control data-current:ring-1 data-current:ring-border/80 dark:data-current:bg-card dark:data-current:ring-border data-current:[&_svg]:text-action data-current:before:absolute data-current:before:inset-y-1.5 data-current:before:start-0 data-current:before:w-[2.5px] data-current:before:rounded-full data-current:before:bg-action [&_svg]:size-4 [&_svg]:shrink-0"

/** The current page's tree value: the item that claims the longest prefix of it, else its destination. */
function currentOf(sections: readonly Section[], navigation: Navigation, pathname: string): string | null {
  const claimed = sections.flatMap((section) => (navigation.menus.get(section.path) ?? []).map((entry) => ({ entry, at: claim(entry, pathname) })))
    .filter(({ at }) => at !== undefined).sort((a, b) => (b.at?.length ?? 0) - (a.at?.length ?? 0))[0]
  if (claimed) return claimed.entry.href
  const section = destinationOf(sections, pathname, navigation.menus)
  return section ? `dest:${section.path}` : null
}

/** Record only the tree branches the reader actually toggled, leaving untouched branches free to follow defaults. */
export function diffExpanded(
  previous: readonly string[],
  next: readonly string[],
  chosen: Readonly<Record<string, boolean>>,
): Readonly<Record<string, boolean>> {
  const prevSet = new Set(previous)
  const nextSet = new Set(next)
  const updated: Record<string, boolean> = { ...chosen }
  for (const id of prevSet) if (!nextSet.has(id)) updated[id] = false
  for (const id of nextSet) if (!prevSet.has(id)) updated[id] = true
  return updated
}

/**
 * The perspective's sidebar: the workspace switcher, then its spaces (collapsible headings, never
 * walls), each listing its destinations; the open destination lists its items under it.
 */
function SectionSidebar({ sections, perspective, navigation, pathname, chosen, onChosenChange }: {
  readonly sections: readonly Section[]; readonly perspective: Perspective; readonly navigation: Navigation; readonly pathname: string
  readonly chosen: Readonly<Record<string, boolean>>
  readonly onChosenChange: (update: (previous: Readonly<Record<string, boolean>>) => Readonly<Record<string, boolean>>) => void
}) {
  const { setOpenMobile } = useSidebar()
  const own = sections.filter((section) => section.perspective === perspective)
  const open = destinationOf(own, pathname, navigation.menus)
  const current = currentOf(own, navigation, pathname)
  const spaces = [
    ...new Set([
      ...own.map(({ group }) => group ?? ""),
      ...own.filter((section) => section.pinned).flatMap((section) => (navigation.menus.get(section.path) ?? []).map((entry) => entry.group ?? "").filter(Boolean)),
    ]),
  ]
  // Spaces start open, the open destination's items show, and a destination's first group of items starts open;
  // the reader's own choices win, except that the current page's row is never hidden.
  const defaults = new Map<string, boolean>()
  const holding = new Set<string>()
  for (const space of spaces) defaults.set(`space:${space}`, true)
  for (const section of own) {
    const entries = navigation.menus.get(section.path) ?? []
    const destEntries = section.pinned ? entries.filter((entry) => !entry.group) : entries
    const active = section === open
    if (destEntries.length > 0) defaults.set(`dest:${section.path}`, active || section.index === false)
    const headings = [...new Set(destEntries.map(({ group }) => group ?? ""))].filter((heading) => heading !== "")
    headings.forEach((heading, index) => defaults.set(`group:${section.path}:${heading}`, index === 0))
    if (active) holding.add(`space:${section.group ?? ""}`)
    const at = entries.find(({ href }) => href === current)
    if (at) {
      if (section.pinned && at.group) {
        holding.add(`space:${at.group}`)
      } else {
        holding.add(`space:${section.group ?? ""}`).add(`dest:${section.path}`)
        if (at.group) holding.add(`group:${section.path}:${at.group}`)
      }
    }
  }
  const values = [...defaults.keys()]
  const expanded = values.filter((value) => holding.has(value) || (chosen[value] ?? defaults.get(value)))
  const close = () => setOpenMobile(false)
  const label = PERSPECTIVES.find(({ id }) => id === perspective)?.label ?? ""
  return (
    <div className="flex h-full min-w-0 flex-1 flex-col">
      <div className="px-2 pt-2 pb-1"><ScopeSwitcher /></div>
      <nav aria-label={label} className="min-h-0 flex-1 overflow-y-auto px-2 pb-4">
        <Tree aria-label={`${label} pages`} mode="none" expandOnPress current={current} expanded={expanded}
          onExpandedChange={(next) => onChosenChange((previous) => diffExpanded(expanded, next, previous))}>
          {spaces.map((space) => {
            const members = own.filter((section) => (section.group ?? "") === space)
            const pinnedItems = own.filter((section) => section.pinned).flatMap((section) =>
              (navigation.menus.get(section.path) ?? [])
                .filter((entry) => entry.group === space)
                .map((entry) => {
                  const Icon = entry.icon ?? section.icon
                  return (
                    <TreeItem key={entry.href} value={entry.href} label={typeof entry.label === "string" ? entry.label : undefined} className={focusRow}>
                      <TreeItemLabel render={<Link {...target(section, entry.href)} onClick={close} />} className={row}>
                        <Icon aria-hidden /><span className="truncate">{entry.label}</span>
                      </TreeItemLabel>
                    </TreeItem>
                  )
                }),
            )
            const items = [
              ...members.map((section) => {
                const entries = navigation.menus.get(section.path) ?? []
                const shownEntries = section.pinned ? entries.filter((entry) => !entry.group) : entries
                return <Destination key={section.path} section={section} entries={shownEntries} badge={navigation.badges.get(section.path)} close={close} />
              }),
              ...pinnedItems,
            ]
            if (space === "") return items
            return (
              <TreeItem key={space} value={`space:${space}`} label={space} className={cn("mt-4 first:mt-2", focusRow)}>
                <TreeItemLabel className="group/heading flex h-6 w-full items-center gap-1 rounded-md px-2 text-[0.6875rem] font-semibold tracking-[0.04em] text-muted-foreground uppercase outline-none hover:text-foreground">
                  <span className="truncate">{space}</span>
                  <TreeItemIndicator className="ms-auto flex opacity-0 transition-[opacity,rotate] group-hover/heading:opacity-100 data-expanded:rotate-90"><ChevronRight aria-hidden className="size-3.5" /></TreeItemIndicator>
                </TreeItemLabel>
                <TreeGroup className="mt-1 space-y-0.5">{items}</TreeGroup>
              </TreeItem>
            )
          })}
        </Tree>
      </nav>
    </div>
  )
}

/** One destination in the sidebar: its row (a link, or a plain heading when it has no page), then its items. */
function Destination({ section, entries, badge, close }: { readonly section: Section; readonly entries: readonly MenuEntry[]; readonly badge: string | undefined; readonly close: () => void }) {
  const headings = [...new Set(entries.map(({ group }) => group ?? ""))]
  // Items carry no icon: under one destination they are all one kind, so an icon would repeat, not help scanning.
  const rows = (members: readonly MenuEntry[]) => members.map((entry) => {
    const count = shown(entry.count)
    return (
      <TreeItem key={entry.href} value={entry.href} label={typeof entry.label === "string" ? entry.label : undefined} className={focusRow}>
        <TreeItemLabel render={<Link {...target(section, entry.href)} onClick={close} />} className={cn(row, "ps-8", entry.kind && KINDS[entry.kind].svg)}>
          <span className="truncate">{entry.label}</span>
          {count && <span className="ms-auto text-xs text-muted-foreground tabular-nums">{count}</span>}
        </TreeItemLabel>
      </TreeItem>
    )
  })
  const children = headings.map((heading) => {
    const members = entries.filter((entry) => (entry.group ?? "") === heading)
    if (heading === "") return rows(members)
    return (
      <TreeItem key={heading} value={`group:${section.path}:${heading}`} label={heading} className={cn("mt-1", focusRow)}>
        <TreeItemLabel className="group/heading flex h-6 w-full items-center gap-1 rounded-md ps-8 pe-2 text-xs text-muted-foreground outline-none hover:text-foreground">
          <span className="truncate">{heading}</span>
          <span className="text-muted-foreground/70 tabular-nums">{members.length}</span>
          <TreeItemIndicator className="ms-auto flex opacity-0 transition-[opacity,rotate] group-hover/heading:opacity-100 data-expanded:rotate-90"><ChevronRight aria-hidden className="size-3.5" /></TreeItemIndicator>
        </TreeItemLabel>
        <TreeGroup className="space-y-px">{rows(members)}</TreeGroup>
      </TreeItem>
    )
  })
  const Icon = section.icon
  const head = <>
    <Icon aria-hidden /><span className="truncate">{section.label}</span>
    {badge && <span className="ms-auto rounded-full bg-destructive px-1.5 text-[0.6875rem] leading-4 font-semibold text-destructive-foreground tabular-nums">{badge}</span>}
  </>
  return (
    <TreeItem value={`dest:${section.path}`} data-section-path={section.path} label={section.label} className={focusRow}>
      {section.index === false
        ? <TreeItemLabel className={cn(row, "cursor-default")}>{head}</TreeItemLabel>
        : <TreeItemLabel render={<Link to={section.path} onClick={close} />} className={cn(row, section.kind && KINDS[section.kind].svg)}>{head}</TreeItemLabel>}
      {entries.length > 0 && <TreeGroup className="mt-px space-y-px">{children}</TreeGroup>}
    </TreeItem>
  )
}

/** Destination, item, then the record under it. Collapses to the immediate parent link at narrow container widths so no link is crushed to 0px. */
function Crumbs({ section, entries, pathname }: { readonly section: Section; readonly entries: readonly MenuEntry[]; readonly pathname: string }) {
  const found = entries.map((entry) => ({ entry, at: claim(entry, pathname) })).filter(({ at }) => at !== undefined).sort((a, b) => (b.at?.length ?? 0) - (a.at?.length ?? 0))[0]
  const entry = found?.entry
  const record = found?.at !== undefined && pathname !== found.at ? decoded(pathname.slice(found.at.length + 1)) : undefined
  const last = entry === undefined ? "section" : record === undefined ? "entry" : "record"
  const sectionClass = last === "record" ? "hidden @md:inline-flex" : last === "entry" && section.index === false ? "hidden @sm:inline-flex" : ""
  const entryClass = last === "entry" && section.index !== false ? "hidden @sm:inline-flex" : ""
  const sep1Class = last === "record" ? "hidden @md:block" : "hidden @sm:block"
  return (
    <Breadcrumb className="min-w-0">
      <BreadcrumbList className="min-w-0 flex-nowrap overflow-hidden">
        <BreadcrumbItem className={cn("min-w-0", sectionClass)}>
          {last === "section" || section.index === false
            ? <BreadcrumbPage className={cn("truncate", last !== "section" && "font-normal text-muted-foreground")}>{section.label}</BreadcrumbPage>
            : <BreadcrumbLink asChild><Link to={section.path} className="inline-flex min-w-0 items-center gap-0.5"><ChevronLeft aria-hidden className={cn("size-3.5 shrink-0 rtl:rotate-180", last === "entry" ? "@sm:hidden" : "hidden")} /><span className="truncate">{section.label}</span></Link></BreadcrumbLink>}
        </BreadcrumbItem>
        {entry && <>
          <BreadcrumbSeparator className={sep1Class} />
          <BreadcrumbItem className={cn("min-w-0", entryClass)}>
            {last === "entry"
              ? <BreadcrumbPage className="truncate">{entry.label}</BreadcrumbPage>
              : <BreadcrumbLink asChild><Link to={entry.href} className="inline-flex min-w-0 items-center gap-0.5"><ChevronLeft aria-hidden className="size-3.5 shrink-0 rtl:rotate-180 @md:hidden" /><span className="truncate">{entry.label}</span></Link></BreadcrumbLink>}
          </BreadcrumbItem>
        </>}
        {record !== undefined && <>
          <BreadcrumbSeparator className="hidden @md:block" />
          <BreadcrumbItem className="hidden min-w-0 @md:inline-flex"><BreadcrumbPage className="truncate font-mono text-xs">{record}</BreadcrumbPage></BreadcrumbItem>
        </>}
      </BreadcrumbList>
    </Breadcrumb>
  )
}

/**
 * The top-level perspective switch. It changes what the navigation lists and the default density,
 * never what the reader may do; a page the other perspective lists opens its home instead.
 */
function PerspectiveSwitch({ perspective, onChange }: { readonly perspective: Perspective; readonly onChange: (next: Perspective) => void }) {
  return (
    <div role="group" aria-label="Perspective" data-perspective={perspective} className="flex h-8 shrink-0 items-center rounded-lg border border-border/80 bg-surface-subtle p-0.5 dark:bg-shell">
      {PERSPECTIVES.map(({ id, label }) => (
        <button key={id} type="button" aria-pressed={perspective === id} onClick={() => onChange(id)}
          className="flex h-[1.625rem] items-center gap-1.5 rounded-md px-1.5 text-sm text-muted-foreground transition-[color,background-color,box-shadow] duration-150 outline-none hover:text-foreground focus-visible:z-10 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-control aria-pressed:ring-1 aria-pressed:ring-border/70 aria-pressed:[&_svg]:text-action dark:aria-pressed:bg-accent dark:aria-pressed:ring-border @sm:px-2 @lg:px-2.5 [&_svg]:size-3.5">
          {id === "workspace" ? <LayoutGrid aria-hidden /> : <Wrench aria-hidden />}<span className="sr-only @lg:not-sr-only">{label}</span>
        </button>
      ))}
    </div>
  )
}

/** "Search… ⌘K", the universal entry: every destination in both perspectives, every page they list, and the perspective switch. */
function CommandMenu({ sections, navigation, onPerspective }: { readonly sections: readonly Section[]; readonly navigation: Navigation; readonly onPerspective: (next: Perspective) => void }) {
  const [open, setOpen] = useState(false)
  const navigate = useNavigate()
  const { setSkin, setShell } = useAppearance()
  const lists = sections.map((section) => ({ section, entries: navigation.menus.get(section.path) ?? [] }))
  const routes = new Map<string, { to: string; hash?: string }>([
    ...sections.map(({ path }) => [`section:${path}`, { to: path }] as const),
    ...lists.flatMap(({ section, entries }) => entries.map(({ href }) => [`entry:${section.path}:${href}`, target(section, href)] as const)),
  ])
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger shortcut="mod+k" aria-label="Search" className="flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground transition-[color,background-color,border-color] duration-150 outline-none hover:bg-surface-subtle hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring @2xl:w-full @2xl:justify-start @2xl:gap-2 @2xl:border @2xl:border-border/90 @2xl:bg-surface-subtle/70 @2xl:px-2.5 @2xl:hover:border-input @2xl:hover:bg-surface-subtle">
        <Search aria-hidden className="size-4 shrink-0" /><span className="hidden truncate @2xl:inline">Search…</span><Keys className="ms-auto hidden rounded border border-border/90 bg-background px-1.5 py-0.5 text-[0.6875rem] leading-none text-muted-foreground @2xl:inline">⌘K</Keys>
      </DialogTrigger>
      <DialogBackdrop className="fixed inset-0 bg-black/40 backdrop-blur-[2px] transition-opacity duration-150 data-starting-style:opacity-0 data-ending-style:opacity-0" />
      <DialogContent className="fixed top-[12vh] left-1/2 m-0 w-[min(36rem,calc(100vw-2rem))] -translate-x-1/2 overflow-hidden rounded-2xl bg-popover p-0 text-popover-foreground shadow-float ring-1 ring-border outline-none transition-[opacity,scale] duration-150 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0">
        <DialogTitle className="sr-only">Search Studio</DialogTitle>
        <Command onAction={(value) => {
          setOpen(false)
          if (value.startsWith("skin:")) {
            const next = value.slice("skin:".length)
            if (isSkin(next)) setSkin(next)
            return
          }
          if (value.startsWith("shell:")) {
            const next = value.slice("shell:".length)
            if (isShellChrome(next)) setShell(next)
            return
          }
          const perspective = value.startsWith("perspective:") ? value.slice("perspective:".length) : undefined
          if (isPerspective(perspective)) return onPerspective(perspective)
          const to = routes.get(value)
          if (to) void navigate(to)
        }}>
          <div className="flex items-center gap-2 border-b px-4">
            <Search aria-hidden className="size-4 text-muted-foreground" />
            <CommandInput aria-label="Search pages, object types, metrics" placeholder="Search pages, object types, metrics…" className="h-12 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground" />
          </div>
          <CommandEmpty className="hidden px-4 py-8 text-center text-sm text-muted-foreground data-empty:block">Nothing matches.</CommandEmpty>
          <CommandList className="max-h-[min(24rem,60vh)] overflow-y-auto p-1.5">
            <CommandGroup>
              <CommandGroupLabel className="px-2 pt-2 pb-1 text-xs text-muted-foreground">Go to</CommandGroupLabel>
              {sections.filter((section) => section.index !== false).map(({ path, label, icon: Icon, perspective }) => (
                <CommandItem key={path} value={`section:${path}`} label={label} className="flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Icon /></CommandItemIcon>{label}
                  {perspective !== "global" && <span className="ms-auto text-xs text-muted-foreground">{PERSPECTIVES.find(({ id }) => id === perspective)?.label}</span>}
                </CommandItem>
              ))}
            </CommandGroup>
            <CommandGroup>
              <CommandGroupLabel className="px-2 pt-3 pb-1 text-xs text-muted-foreground">Perspective</CommandGroupLabel>
              {PERSPECTIVES.map(({ id, label }) => (
                <CommandItem key={id} value={`perspective:${id}`} label={`Switch to ${label}`} className="flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4">{id === "workspace" ? <LayoutGrid /> : <Wrench />}</CommandItemIcon>Switch to {label}
                </CommandItem>
              ))}
            </CommandGroup>
            <CommandGroup>
              <CommandGroupLabel className="px-2 pt-3 pb-1 text-xs text-muted-foreground">Appearance</CommandGroupLabel>
              {SKINS.map(({ id, label, detail }) => (
                <CommandItem key={id} value={`skin:${id}`} label={`Skin: ${label}`} keywords={[detail]} className="flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Palette /></CommandItemIcon>Skin: {label}
                  <span className="ms-auto text-xs text-muted-foreground">{detail}</span>
                </CommandItem>
              ))}
              {SHELLS.map(({ id, label, detail }) => (
                <CommandItem key={id} value={`shell:${id}`} label={`Chrome: ${label}`} keywords={[detail]} className="flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4">{id === "sidebar" ? <PanelLeft /> : <PanelBottom />}</CommandItemIcon>Chrome: {label}
                  <span className="ms-auto text-xs text-muted-foreground">{detail}</span>
                </CommandItem>
              ))}
            </CommandGroup>
            {lists.filter(({ entries }) => entries.length > 0).map(({ section, entries }) => (
              <CommandGroup key={section.path}>
                <CommandGroupLabel className="px-2 pt-3 pb-1 text-xs text-muted-foreground">{section.label}</CommandGroupLabel>
                {entries.map((entry) => (
                  <CommandItem key={entry.href} value={`entry:${section.path}:${entry.href}`} label={typeof entry.label === "string" ? entry.label : entry.href} keywords={entry.group ? [entry.group] : undefined} className="flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-sm data-highlighted:bg-accent">
                    {(() => { const Icon = entry.icon ?? (entry.kind ? KINDS[entry.kind].icon : section.icon); return <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Icon /></CommandItemIcon> })()}
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

const iconButton = "flex size-8 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-open:bg-accent [&_svg]:size-4"

/** The page bar's right-hand buttons: the page's own tools, then shortcuts and notifications. */
function BarButtons({ tools }: { readonly tools?: ReactNode }) {
  const { view } = useView()
  const { isMobile } = useSidebar()
  return (
    <div className="flex items-center gap-0.5">
      {view === "technical" && <span className="hidden rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground lg:inline">Technical view</span>}
      {/* On a phone the rail's account menu is behind the sheet, so one small menu holds the view and language. */}
      {/* Rendered only on a phone, so its choices never shadow the account menu's. */}
      {isMobile && <Menu>
        <MenuTrigger aria-label="Language and view" className={iconButton}><SlidersHorizontal aria-hidden /></MenuTrigger>
        <KitMenuContent align="end" className="w-52"><ViewAndLanguage /></KitMenuContent>
      </Menu>}
      {tools}
      <AppearanceTray />
      <Popover>
        <PopoverTrigger aria-label="Keyboard shortcuts" className={cn(iconButton, "hidden @xl:flex")}><Keyboard aria-hidden /></PopoverTrigger>
        <KitPopoverContent align="end" className="w-60">
          <PopoverTitle className="mb-2 text-sm font-medium">Keyboard shortcuts</PopoverTitle>
          <dl className="grid grid-cols-[1fr_auto] gap-y-1.5 text-sm">
            <dt className="text-muted-foreground">Search</dt><dd><Keys>⌘K</Keys></dd>
            <dt className="text-muted-foreground">Collapse the sidebar</dt><dd><Keys>⌘B</Keys></dd>
            <dt className="text-muted-foreground">Move between rail items</dt><dd><Keys>↑ ↓</Keys></dd>
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

/** Speech-bubble "AI" glyph from v6/shell.tsx, rendered in the accent colour. */
function AiGlyph({ className }: { readonly className?: string }) {
  return (
    <span aria-hidden className={cn("relative grid size-[18px] place-items-center text-action", className)}>
      <MessageCircle className="absolute inset-0 size-full" strokeWidth={1.8} />
      <span className="relative text-[7px] leading-none font-bold tracking-tight">AI</span>
    </span>
  )
}

/**
 * Centered icon-only floating dark pill dock rendered in `dock` shell mode (`data-shell="dock"`),
 * following norn-mockups v6/shell.tsx craft: rounded-[26px] dark pill, size-10 circular buttons,
 * soft #34343a active well, hover tooltips, AI glyph, appearance tray, and overflow destinations.
 */
function FloatingDock({ sections, perspective, navigation, current, assistant }: {
  readonly sections: readonly Section[]
  readonly perspective: Perspective
  readonly navigation: Navigation
  readonly current: Section | undefined
  readonly assistant?: AssistantContent
}) {
  const { open, toggle } = useAssistant()
  const global = sections.filter((section) => section.perspective === "global")
  const firstHref = (section: Section) => navigation.menus.get(section.path)?.find(({ href }) => !href.startsWith("#"))?.href
  const resolvedOwn = sections
    .filter((section) => section.perspective === perspective)
    .map((section) => ({
      section,
      to: section.index === false ? firstHref(section) : section.path,
      count: navigation.badges.get(section.path),
    }))
    .filter((entry): entry is { readonly section: Section; readonly to: string; readonly count: string | undefined } => entry.to !== undefined)
  const primary = resolvedOwn.slice(0, 5)
  const overflow = resolvedOwn.slice(5)
  const dockBtn = "relative grid size-10 place-items-center rounded-full text-(--dock-muted) outline-none transition-colors hover:text-(--dock-foreground) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-(--dock-ring) data-[active=true]:bg-(--dock-well) data-[active=true]:text-(--dock-foreground) data-open:bg-(--dock-well) data-open:text-(--dock-foreground)"

  return (
    <nav
      data-slot="floating-dock"
      aria-label="Floating dock"
      className="pointer-events-none fixed inset-x-0 bottom-6 z-40 flex justify-center px-4"
    >
      <Toolbar
        aria-label="Dock navigation"
        className="pointer-events-auto flex max-w-full items-center gap-1 rounded-[26px] bg-(--dock-surface) p-1.5 text-(--dock-foreground) shadow-(--dock-shadow) ring-1 ring-(--dock-border) backdrop-blur-xl"
      >
        {global.map((section) => {
          const Icon = section.icon
          const count = navigation.badges.get(section.path)
          const active = current?.path === section.path
          const label = count ? `${section.label}, ${count} open` : section.label
          return (
            <Hint key={section.path} side="top" label={label}>
              <ToolbarLink
                render={<Link to={section.path} />}
                aria-label={label}
                aria-current={active ? "page" : undefined}
                data-active={active || undefined}
                className={dockBtn}
              >
                <Icon aria-hidden className="size-[18px]" strokeWidth={1.7} />
                {count && <span aria-hidden className="absolute top-[7px] right-[7px] size-[6px] rounded-full bg-(--dock-indicator) ring-2 ring-(--dock-surface)" />}
              </ToolbarLink>
            </Hint>
          )
        })}
        {primary.map(({ section, to, count }) => {
          const Icon = section.icon
          const active = current?.path === section.path
          const label = count ? `${section.label}, ${count} open` : section.label
          return (
            <Hint key={section.path} side="top" label={label}>
              <ToolbarLink
                render={<Link to={to} />}
                aria-label={label}
                aria-current={active ? "page" : undefined}
                data-active={active || undefined}
                className={dockBtn}
              >
                <Icon aria-hidden className="size-[18px]" strokeWidth={1.7} />
                {count && <span aria-hidden className="absolute top-[7px] right-[7px] size-[6px] rounded-full bg-(--dock-indicator) ring-2 ring-(--dock-surface)" />}
              </ToolbarLink>
            </Hint>
          )
        })}
        <ToolbarSeparator className="mx-1 h-5 w-px bg-white/10" />
        {assistant && (
          <Hint side="top" label={assistant.title}>
            <ToolbarButton
              aria-label={assistant.title}
              aria-pressed={open}
              data-active={open || undefined}
              onClick={toggle}
              className={dockBtn}
            >
              <AiGlyph />
            </ToolbarButton>
          </Hint>
        )}
        <AppearanceTray side="top" variant="dock" />
        {overflow.length > 0 && (
          <Menu>
            <Hint side="top" label="More destinations">
              <MenuTrigger aria-label="More destinations" className={dockBtn}>
                <CircleEllipsis aria-hidden className="size-[18px]" strokeWidth={1.7} />
              </MenuTrigger>
            </Hint>
            <KitMenuContent side="top" align="end" className="min-w-48">
              <MenuGroup>
                <KitMenuLabel>More destinations</KitMenuLabel>
                {overflow.map(({ section, to, count }) => {
                  const Icon = section.icon
                  const overflowLabel = count ? `${section.label}, ${count} open` : section.label
                  return (
                    <KitMenuItem key={section.path} value={section.path} label={section.label} aria-label={overflowLabel} render={<Link to={to} aria-label={overflowLabel} />}>
                      <Icon aria-hidden />
                      <span className="truncate">{section.label}</span>
                      {count && <span aria-hidden className="ms-auto rounded-full bg-destructive px-1.5 text-[0.625rem] leading-4 font-semibold text-destructive-foreground tabular-nums">{count}</span>}
                    </KitMenuItem>
                  )
                })}
              </MenuGroup>
            </KitMenuContent>
          </Menu>
        )}
      </Toolbar>
    </nav>
  )
}

/**
 * The workbench shell: a slim rail and the perspective's sidebar on one quiet tint, the page flat on
 * the background beside them under one bar (breadcrumbs, search, the perspective switch), and the
 * inspector and assistant docked on the end side.
 */
export function Shell(props: {
  readonly sections: readonly Section[]
  readonly tools?: ReactNode
  readonly assistant?: AssistantContent
  readonly account: Account
  readonly onSignOut: () => void
  readonly initialSkin?: Skin
  readonly initialShell?: ShellChrome
}) {
  return (
    <AppearanceProvider initialSkin={props.initialSkin} initialShell={props.initialShell}>
      <LocaleProvider><PerspectiveProvider><AssistantProvider available={props.assistant !== undefined}><LocalizedShell {...props} /></AssistantProvider></PerspectiveProvider></LocaleProvider>
    </AppearanceProvider>
  )
}

/**
 * An entry that is an anchor on its applet's page (a workflow, a guarantee) scrolls to it on every
 * hash change, also when it renders only after its data loads: the router scrolls only to an
 * element that is already there.
 */
function useHashScroll() {
  const hash = useRouterState({ select: (state) => state.location.hash })
  const at = useRouterState({ select: (state) => state.location.state })
  useEffect(() => {
    if (hash === "") return
    let frame = 0
    const until = performance.now() + 2500
    const find = () => {
      const element = document.getElementById(decodeURIComponent(hash))
      if (element) element.scrollIntoView({ block: "start" })
      else if (performance.now() < until) frame = requestAnimationFrame(find)
    }
    find()
    return () => cancelAnimationFrame(frame)
  }, [hash, at])
}

/** The sidebar, top bar and page frame inside SidebarProvider, preserving keyboard focus across collapse/expand. */
function WorkbenchFrame({ sections, tools, assistant, account, onSignOut, locale, pathname, perspective, navigation, current, switchTo }: {
  readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly assistant?: AssistantContent
  readonly account: Account; readonly onSignOut: () => void; readonly locale: ReturnType<typeof useLocale>["locale"]
  readonly pathname: string; readonly perspective: Perspective; readonly navigation: Navigation
  readonly current: Section | undefined; readonly switchTo: (next: Perspective) => void
}) {
  const { open, isMobile } = useSidebar()
  const sidebarPanelRef = useRef<HTMLDivElement>(null)
  const railRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const wasOpenRef = useRef(open)
  const focusWasInPanelRef = useRef(false)
  const pendingFocusSectionRef = useRef<string | null>(null)
  const [chosenByPerspective, setChosenByPerspective] = useState<Readonly<Record<Perspective, Readonly<Record<string, boolean>>>>>({ workspace: {}, builder: {} })
  const updateChosen = (update: (previous: Readonly<Record<string, boolean>>) => Readonly<Record<string, boolean>>) =>
    setChosenByPerspective((all) => ({ ...all, [perspective]: update(all[perspective]) }))
  const expandSection = (path: string) => {
    pendingFocusSectionRef.current = path
    const section = sections.find((candidate) => candidate.path === path && candidate.perspective === perspective)
    if (section?.group) updateChosen((previous) => ({ ...previous, [`space:${section.group}`]: true }))
  }

  useEffect(() => {
    const onFocusIn = (event: FocusEvent) => {
      const target = event.target
      focusWasInPanelRef.current = target instanceof Node && Boolean(sidebarPanelRef.current?.contains(target))
    }
    document.addEventListener("focusin", onFocusIn)
    return () => document.removeEventListener("focusin", onFocusIn)
  }, [])

  useEffect(() => {
    const wasOpen = wasOpenRef.current
    wasOpenRef.current = open
    if (isMobile) return
    if (wasOpen && !open) {
      const active = document.activeElement
      const lost = sidebarPanelRef.current?.contains(active) || (active === document.body && focusWasInPanelRef.current)
      focusWasInPanelRef.current = false
      if (lost) {
        const railTarget = railRef.current?.querySelector<HTMLElement>('[data-status="active"], [data-active="true"], [aria-current="page"]')
        ;(railTarget ?? triggerRef.current)?.focus()
      }
      return
    }
    if (!wasOpen && open && pendingFocusSectionRef.current !== null) {
      const path = pendingFocusSectionRef.current
      pendingFocusSectionRef.current = null
      const panel = sidebarPanelRef.current
      const sectionItem = Array.from(panel?.querySelectorAll<HTMLElement>("[data-section-path]") ?? [])
        .find((element) => element.dataset.sectionPath === path)
      sectionItem?.focus()
      if (panel && !panel.contains(document.activeElement)) {
        panel.querySelector<HTMLElement>('[role="treeitem"], a[href], button')?.focus()
      }
    }
  }, [open, isMobile])

  const { shell } = useAppearance()
  const isDock = shell === "dock"

  return (
    <>
      {!isDock && (
        <Sidebar collapsible="icon" side={directionOf(locale) === "rtl" ? "right" : "left"} className="overflow-hidden border-none!">
          <div className="flex size-full min-w-0 flex-row bg-shell">
            <Rail sections={sections} perspective={perspective} navigation={navigation} current={current} account={account}
              onSignOut={onSignOut} assistant={assistant} railRef={railRef}
              onExpandSection={expandSection} />
            <div ref={sidebarPanelRef} className="flex min-w-0 flex-1 group-data-[collapsible=icon]:hidden">
              <SectionSidebar key={perspective} sections={sections} perspective={perspective} navigation={navigation} pathname={pathname}
                chosen={chosenByPerspective[perspective]} onChosenChange={updateChosen} />
            </div>
          </div>
        </Sidebar>
      )}
      <SidebarInset className={cn(
        "min-w-0 flex-row overflow-clip bg-background",
        !isDock && "md:my-2 md:me-2 md:ms-0 md:rounded-shell md:border md:border-border/90 md:shadow-shell",
      )}>
        <div className="@container flex min-w-0 flex-1 flex-col">
          {isDock
            ? (
              <header data-slot="dock-topbar" className="flex h-[4.4rem] shrink-0 items-center gap-3 bg-background px-6 md:px-8">
                <Link to="/" aria-label="Norn Studio home" className="flex size-8 shrink-0 items-center justify-center rounded-lg outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring">
                  <img src={nornMark} alt="" width={28} height={28} className="size-7 object-contain" />
                </Link>
                <ScopeSwitcher variant="pill" />
                <div className="hidden min-w-0 overflow-hidden @[36rem]:block">{current && current.path !== "/" && <Crumbs key={current.path} section={current} entries={navigation.menus.get(current.path) ?? []} pathname={pathname} />}</div>
                <div className="ms-auto flex shrink-0 items-center gap-2">
                  <div className="flex shrink-0 justify-center @3xl:w-52"><CommandMenu sections={sections} navigation={navigation} onPerspective={switchTo} /></div>
                  <PerspectiveSwitch perspective={perspective} onChange={switchTo} />
                  {tools}
                  <Popover>
                    <PopoverTrigger aria-label="Notifications" className={iconButton}><Bell aria-hidden /></PopoverTrigger>
                    <KitPopoverContent align="end">
                      <PopoverTitle className="mb-1 text-sm font-medium">Notifications</PopoverTitle>
                      <p className="text-sm text-muted-foreground">Nothing yet.</p>
                    </KitPopoverContent>
                  </Popover>
                  <AccountMenu account={account} onSignOut={onSignOut} inToolbar={false} variant="topbar" />
                </div>
              </header>
            )
            : (
              <header className="flex h-12 shrink-0 items-center justify-between gap-1 border-b border-border/80 bg-background/95 px-2 @sm:gap-2 @sm:px-3 @2xl:gap-3 @2xl:px-4 @4xl:grid @4xl:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]">
                <div className="flex min-w-0 flex-1 items-center gap-1.5">
                  <SidebarTrigger ref={triggerRef} className="-ms-1 size-8 shrink-0 rounded-lg text-muted-foreground" />
                  <div className="hidden min-w-0 overflow-hidden @[17rem]:block">{current && <Crumbs key={current.path} section={current} entries={navigation.menus.get(current.path) ?? []} pathname={pathname} />}</div>
                </div>
                <div className="flex shrink-0 justify-center @2xl:w-48 @4xl:w-72 @5xl:w-96"><CommandMenu sections={sections} navigation={navigation} onPerspective={switchTo} /></div>
                <div className="flex shrink-0 items-center justify-end gap-1 @xl:gap-2">
                  <PerspectiveSwitch perspective={perspective} onChange={switchTo} />
                  <BarButtons tools={tools} />
                </div>
              </header>
            )}
          <ConnectionBanner />
          <div className={isDock ? "mx-auto w-full max-w-[1220px] px-6 pt-4 pb-32 md:px-8 md:pt-5 md:pb-32" : "mx-auto w-full max-w-7xl px-5 py-7 md:px-9 md:py-8"}><Outlet /></div>
        </div>
        <Inspector />
        <AssistantPanel assistant={assistant} />
      </SidebarInset>
      {isDock && <FloatingDock sections={sections} perspective={perspective} navigation={navigation} current={current} assistant={assistant} />}
    </>
  )
}

/** The shell in the reader's direction: in Arabic the rail docks on the right. */
function LocalizedShell({ sections, tools, assistant, account, onSignOut }: { readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly assistant?: AssistantContent; readonly account: Account; readonly onSignOut: () => void }) {
  const { locale } = useLocale()
  const { shell } = useAppearance()
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const navigate = useNavigate()
  const chosen = usePerspective()
  const navigation = useNavigation(sections)
  const perspective = perspectiveOf(sections, pathname, chosen.perspective, navigation.menus)
  const density = PERSPECTIVES.find(({ id }) => id === perspective)?.density ?? "comfortable"
  const shown = sections.filter((section) => section.perspective === perspective || section.perspective === "global")
  const current = destinationOf(shown, pathname, navigation.menus)
  const switchTo = (next: Perspective) => {
    chosen.setPerspective(next)
    if (perspectiveOf(sections, pathname, next, navigation.menus) !== next) void navigate({ to: PERSPECTIVES.find(({ id }) => id === next)?.home ?? "/" })
  }
  useHashScroll()
  return (
    <ViewProvider><InspectorProvider><TooltipProvider>
      <SidebarProvider data-shell={shell} data-density={density} className="bg-shell" style={{ "--sidebar-width": "17rem", "--sidebar-width-icon": "3rem" } as CSSProperties}>
        <WorkbenchFrame sections={sections} tools={tools} assistant={assistant} account={account} onSignOut={onSignOut}
          locale={locale} pathname={pathname} perspective={perspective} navigation={navigation} current={current} switchTo={switchTo} />
      </SidebarProvider>
    </TooltipProvider></InspectorProvider></ViewProvider>
  )
}
