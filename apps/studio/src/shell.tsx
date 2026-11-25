import { useQueryClient } from "@tanstack/react-query"
import { Dot } from "@/components/system/status-dot"
import { Initials } from "@/components/system/list-row"
import { Link, Outlet, createLink, useNavigate, useRouterState } from "@tanstack/react-router"
import { Command, CommandEmpty, CommandGroup, CommandGroupLabel, CommandInput, CommandItem, CommandItemIcon, CommandList } from "@ui-sdk/react/command"
import { Dialog, DialogBackdrop, DialogContent, DialogTitle, DialogTrigger } from "@ui-sdk/react/dialog"
import { MenuGroup, MenuRadioGroup, MenuTrigger } from "@ui-sdk/react/menu"
import { PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover"
import { Toolbar, ToolbarButton, ToolbarLink, ToolbarSeparator } from "@ui-sdk/react/toolbar"
import { Tree, TreeGroup, TreeItem, TreeItemIndicator, TreeItemLabel } from "@ui-sdk/react/tree"
import { AssistantPanel, AssistantProvider, useAssistant, type AssistantContent } from "@/assistant-slot"
import { Bell, ChevronDown, ChevronLeft, ChevronRight, ChevronsUpDown, CircleEllipsis, CloudOff, Code2, Keyboard, LayoutGrid, Loader2, LogOut, MessageCircle, Palette, PanelBottom, PanelLeft, Search, SlidersHorizontal, Sparkles, Wrench, type LucideIcon } from "lucide-react"
import { Suspense, lazy, useEffect, useRef, useState, type ComponentProps, type ReactNode, type RefObject } from "react"
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "@/components/ui/breadcrumb"
import { Button } from "@/components/ui/button"
import { ChromeDock, ChromeDockOption, ChromeDockTray, ChromeDockTrigger } from "@/components/ui/chrome-dock"
import { AppearanceProvider, SCHEMES, SHELLS, SKINS, isScheme, isShellChrome, isSkin, useAppearance, type ShellChrome, type Skin } from "@/lib/appearance"
import { TopBarExtras } from "@/top-bar-extras"
import { PERSPECTIVES, PerspectiveProvider, isPerspective, usePerspective, type Perspective } from "@/lib/perspective"
import { SIDEBAR_ID, Sidebar, SidebarClose, SidebarInset, SidebarProvider, SidebarTrigger, useSidebar } from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"
import { Hint, KitMenuContent, KitMenuItem, KitMenuLabel, KitMenuRadioItem, KitMenuSeparator, KitPopoverContent, Menu, Popover } from "@/components/system/kit"
import { Inspector, InspectorProvider } from "@/lib/inspector"
import { KINDS, type Kind } from "@/lib/kinds"
import { cn } from "@/lib/utils"
import { ViewProvider, useView } from "@/lib/view"
import { LOCALES, LocaleProvider, isLocale, useLocale } from "@/lib/locale"
import { WORKSPACES } from "@/lib/workspaces"
import { isRetryableModeError, retryMode, useMode } from "@/lib/live-transport"
import { lastReachableAt, useStudioScope } from "@/lib/definitions"
import { CopyId } from "@/components/copy-id"
import { formatLastReachable } from "@/components/load-state"
import { explainError } from "@/lib/plain-words"
import { useAccount, type Account } from "@/lib/session"
import { WorkspaceError, useWorkspaces, type WorkspacesDocument } from "@/lib/workspaces-api"
import nornMark from "@/assets/norn-mark.webp"
import { Keys } from "@/components/keys"

const LazyCustomThemeDialog = lazy(() => import("@/components/custom-theme-dialog").then((module) => ({ default: module.CustomThemeDialog })))

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

const roleLabel = (role: string) => role === "" ? "" : `${role[0]!.toUpperCase()}${role.slice(1)}`

/** Delays showing the "connecting slowly" hint so fast local/warm loads stay quiet. */
const CONNECTING_HINT_MS = 3_000
const recordedWorkspace = WORKSPACES.find(({ recorded }) => recorded)

/** True once the initial mode probe has been waiting longer than `CONNECTING_HINT_MS`. */
function useSlowConnecting(mode: ReturnType<typeof useMode>): boolean {
  const [slow, setSlow] = useState(false)
  const waiting = mode.data === undefined && mode.isPending && !mode.isError
  useEffect(() => {
    if (!waiting) { setSlow(false); return }
    const timer = setTimeout(() => setSlow(true), CONNECTING_HINT_MS)
    return () => clearTimeout(timer)
  }, [waiting])
  return waiting && slow
}

/** The open workspace as the server reported it; never renders raw org or environment ids. */
function useOpenWorkspace(doc: WorkspacesDocument | null | undefined) {
  const mode = useMode()
  const account = useAccount()
  const slow = useSlowConnecting(mode)
  const live = mode.data?.kind === "live"
  const recorded = mode.data?.kind === "recorded"
  const retrying = !mode.isError && mode.failureCount > 0 && isRetryableModeError(mode.failureReason)
  const active = doc?.workspaces.find((workspace) => workspace.active)
  const organization = active ?? account.data?.organization
  const open = live
    ? { name: organization?.name ?? "Connected workspace", detail: organization?.role ? `Live · ${roleLabel(organization.role)}` : "Live workspace" }
    : recorded
      ? organization
        ? { name: organization.name, detail: "Recorded demo data" }
        : recordedWorkspace
      : {
          name: organization?.name ?? "Workspace",
          detail: mode.isError ? "Workspace unavailable" : retrying ? "Reconnecting…" : slow ? "Connecting to cell…" : "Connecting…",
        }
  const workspaceOpen = organization != null || live
  // The org disc hashes the workspace's own id, never its name (DESIGN.md 4.4).
  const discId = organization?.id ?? (recorded ? recordedWorkspace?.id : undefined) ?? ""
  return { live, open, active, workspaceOpen, discId, recordedSelection: recorded ? recordedWorkspace?.id : undefined }
}

/** An example dataset entry that opens here: published benchmark results, or the recording while no cell is connected. */
const real = (workspace: (typeof WORKSPACES)[number], live: boolean) => workspace.results !== undefined || (workspace.recorded && !live)

/** The workspace switcher: the user's real workspaces and a separate labelled Examples group. */
function ScopeSwitcher({ variant = "sidebar" }: { readonly variant?: "sidebar" | "rail" | "pill" }) {
  const workspaces = useWorkspaces()
  const doc = workspaces.data
  const { live, open, active, workspaceOpen, discId, recordedSelection } = useOpenWorkspace(doc)
  const realWorkspaces = doc?.workspaces ?? []
  const examples = WORKSPACES.filter((workspace) => real(workspace, live))
  const showWorkspaceSection = realWorkspaces.length > 0 || workspaceOpen
  // The org disc takes the avatar pair hashed by the workspace's id (DESIGN.md 4.4, 9.1).
  const disc = <Initials id={discId} name={open?.name ?? ""} letters={initials(open?.name ?? "")} size={24} />
  return (
    <Menu>
      {variant === "pill"
        ? (
          <MenuTrigger data-scope className="group/scope inline-flex h-9 max-w-60 min-w-0 items-center gap-2 rounded-full bg-card ps-1.5 pe-2.5 text-body text-foreground ring-1 ring-input transition-[color,background-color] duration-150 outline-none hover:bg-(--well) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-open:bg-(--well) max-sm:min-h-[44px]">
            {disc}
            <span className="truncate">{open?.name}</span>
            <ChevronDown aria-hidden className="size-3.5 shrink-0 text-subtle-foreground" />
          </MenuTrigger>
        )
        : variant === "rail"
          ? (
            <Hint side="end" label={`${open?.name ?? ""} · ${open?.detail ?? ""}`}>
              <MenuTrigger data-scope aria-label={`Workspace: ${open?.name ?? ""}`} className="flex size-[32px] items-center justify-center rounded-full outline-none transition-colors hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring data-open:bg-sidebar-accent/50">
                {disc}
              </MenuTrigger>
            </Hint>
          )
          : (
            // The workspace and its connection state (Live, Connecting…, unavailable): the state stays visible, never only in a tooltip.
            <MenuTrigger data-scope className="group/scope flex min-h-[40px] w-full min-w-0 items-center gap-[8px] rounded-(--radius-row) px-[8px] py-[4px] text-start transition-colors duration-150 outline-none hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring data-open:bg-sidebar-accent/50 max-sm:min-h-[44px]">
              {disc}
              <span className="grid min-w-0 flex-1 leading-tight" title={`${open?.name ?? ""} · ${open?.detail ?? ""}`}>
                <span className="truncate text-body text-sidebar-foreground">{open?.name}</span>
                <span className="truncate text-meta text-muted-foreground">{open?.detail}</span>
              </span>
              <ChevronsUpDown aria-hidden className="size-3.5 shrink-0 text-subtle-foreground" />
            </MenuTrigger>
          )}
      <KitMenuContent align="start" className="w-72">
        {showWorkspaceSection && (
          <>
            <MenuRadioGroup value={active ? `ws:${active.id}` : workspaceOpen ? "current" : ""}>
              <KitMenuLabel>Your workspaces</KitMenuLabel>
              {workspaceOpen && active === undefined && open && (
                <KitMenuRadioItem value="current" label={open.name} closeOnSelect className="flex-col items-start gap-0">
                  <span className="truncate">{open.name}</span>
                  <span className="text-meta text-muted-foreground">{open.detail}</span>
                </KitMenuRadioItem>
              )}
              {realWorkspaces.map((workspace) => (
                <KitMenuRadioItem key={workspace.id} value={`ws:${workspace.id}`} label={workspace.name} disabled={!workspace.active} closeOnSelect className="flex-col items-start gap-0">
                  <span className="truncate">{workspace.name}</span>
                  <span className="text-meta text-muted-foreground">{workspace.hasCell ? roleLabel(workspace.role) : `${roleLabel(workspace.role)} · No cell yet`}</span>
                </KitMenuRadioItem>
              ))}
            </MenuRadioGroup>
            {realWorkspaces.some((workspace) => !workspace.active) && (
              <p className="px-2 py-1.5 text-meta text-muted-foreground">Switching workspaces is coming soon.</p>
            )}
          </>
        )}
        {workspaces.isError && (
          <div role="alert" className="flex items-center justify-between gap-2 px-2 py-1.5 text-meta text-destructive">
            <span>{workspaces.error instanceof WorkspaceError ? workspaces.error.message : "Could not load workspaces."}</span>
            <button type="button" className="shrink-0 underline underline-offset-2" onClick={() => void workspaces.refetch()}>Retry</button>
          </div>
        )}
        {examples.length > 0 && (
          <>
            {showWorkspaceSection && <KitMenuSeparator />}
            <MenuRadioGroup value={recordedSelection ?? ""}>
              <KitMenuLabel>Examples</KitMenuLabel>
              {examples.map((workspace) => workspace.results !== undefined
                ? (
                  <KitMenuItem key={workspace.id} value={`example:${workspace.id}`} label={workspace.name} render={<a href={workspace.results} target="_blank" rel="noreferrer" />} className="flex-col items-start gap-0">
                    <span>{workspace.name}</span><span className="text-meta text-muted-foreground">{workspace.detail} · results ↗</span>
                  </KitMenuItem>
                )
                : (
                  <KitMenuRadioItem key={workspace.id} value={workspace.id} label={workspace.name} className="flex-col items-start gap-0">
                    <span>{workspace.name}</span><span className="text-meta text-muted-foreground">{workspace.detail}</span>
                  </KitMenuRadioItem>
                ))}
            </MenuRadioGroup>
          </>
        )}
      </KitMenuContent>
    </Menu>
  )
}

/** The view (Business or Technical), language, skin and chrome layout as menu choices. */
function ViewAndLanguage() {
  const { view, setView } = useView()
  const { locale, setLocale } = useLocale()
  const { skin, setSkin, shell, setShell, scheme, setScheme } = useAppearance()
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
    <MenuRadioGroup value={scheme} onValueChange={(next) => { if (isScheme(next)) setScheme(next) }}>
      <KitMenuLabel>Colour scheme</KitMenuLabel>
      {SCHEMES.map(({ id, label }) => <KitMenuRadioItem key={id} value={id} closeOnSelect>{label}</KitMenuRadioItem>)}
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
  const { skin, setSkin, shell, setShell, customTheme } = useAppearance()
  const [themeDialogOpen, setThemeDialogOpen] = useState(false)
  const [themeDialogMounted, setThemeDialogMounted] = useState(false)
  const openThemeDialog = () => {
    setThemeDialogMounted(true)
    setThemeDialogOpen(true)
  }
  return (
    <>
    <ChromeDock
      value={skin}
      items={SKINS.map(({ id }) => id)}
      onValueChange={(next) => { if (isSkin(next)) setSkin(next) }}
      className="relative inline-flex"
    >
      <ChromeDockTrigger
        aria-label="Appearance"
        className={variant === "dock"
          ? DOCK_BUTTON
          : cn(iconButton, "hidden @xl:flex")}
      >
        <Palette aria-hidden strokeWidth={1.7} />
        {variant === "dock" && <DockLabel>Appearance</DockLabel>}
      </ChromeDockTrigger>
      <ChromeDockTray
        side={side}
        align="end"
        sideOffset={8}
        aria-label="Appearance settings"
        className="w-72 origin-(--transform-origin) rounded-(--radius-popup) bg-popover p-3.5 text-popover-foreground shadow-popup ring-1 ring-panel-border outline-none transition-[opacity,scale] duration-150 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0"
      >
        <div className="space-y-3">
          <div className="space-y-1.5">
            <div className="flex items-center justify-between text-meta text-muted-foreground">
              <span>Skin</span>
              <span className="truncate capitalize text-foreground">{customTheme ? customTheme.name : skin}</span>
            </div>
            <div role="group" aria-label="Skin" className="grid grid-cols-2 gap-1.5">
              {SKINS.map((option) => (
                <ChromeDockOption
                  key={option.id}
                  value={option.id}
                  data-skin-option={option.id}
                  className="flex items-center gap-2 rounded-lg border border-border/80 bg-background/70 px-2.5 py-2 text-start text-meta text-foreground transition-colors outline-none hover:bg-surface-subtle focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-[state=selected]:border-action data-[state=selected]:bg-action/10 data-[state=selected]:text-foreground"
                >
                  <span aria-hidden className="size-3.5 shrink-0 rounded-full border border-border/90 shadow-xs" style={{ backgroundColor: option.swatch }} />
                  <span className="truncate">{option.label}</span>
                </ChromeDockOption>
              ))}
            </div>
          </div>
          <div className="h-px bg-border/80" />
          <div className="space-y-1.5">
            <div className="text-meta text-muted-foreground">Shell chrome</div>
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
                    className="flex items-center gap-1.5 rounded-lg border border-border/80 bg-background/70 px-2.5 py-2 text-meta text-foreground transition-colors outline-none hover:bg-surface-subtle focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring aria-pressed:border-action aria-pressed:bg-action/10"
                  >
                    {option.id === "sidebar" ? <PanelLeft aria-hidden className="size-3.5 shrink-0" /> : <PanelBottom aria-hidden className="size-3.5 shrink-0" />}
                    <span className="truncate">{option.label}</span>
                  </button>
                )
              })}
            </div>
          </div>
          <div className="h-px bg-border/80" />
          <button
            type="button"
            data-slot="custom-theme-trigger"
            onClick={openThemeDialog}
            className="flex w-full items-center justify-between gap-2 rounded-lg border border-border/80 bg-background/70 px-2.5 py-2 text-meta text-foreground transition-colors outline-none hover:bg-surface-subtle focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
          >
            <span className="flex items-center gap-1.5">
              <Code2 aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              <span>Custom theme JSON…</span>
            </span>
            {customTheme && <span className="truncate text-meta text-muted-foreground">Active</span>}
          </button>
        </div>
      </ChromeDockTray>
    </ChromeDock>
    {themeDialogMounted && <Suspense fallback={null}><LazyCustomThemeDialog open={themeDialogOpen} onOpenChange={setThemeDialogOpen} /></Suspense>}
    </>
  )
}

/** The signed-in user, their organization and role, and sign-out. The role is shown, never used to hide or allow anything. */
export function AccountMenu({ account, onSignOut, inToolbar = true, variant = "rail" }: {
  readonly account: Account
  readonly onSignOut: () => void
  readonly inToolbar?: boolean
  readonly variant?: "rail" | "sidebar" | "topbar" | "dock"
}) {
  const { user, organization } = account
  const name = user.name || user.email
  // The principal's disc takes the avatar pair hashed by their id, never their name (DESIGN.md 4.4).
  const disc = <Initials id={user.id} name={name} letters={initials(name)} size={24} />
  const triggerClass = variant === "topbar"
    ? "grid size-[36px] place-items-center rounded-full outline-none transition-[box-shadow,transform] hover:ring-2 hover:ring-line focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring max-sm:size-[44px]"
    : variant === "dock"
      ? "flex size-8 items-center justify-center rounded-full bg-(--dock-active) text-meta text-(--dock-foreground) ring-1 ring-(--dock-border) outline-none hover:ring-(--dock-foreground)/40 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
      : variant === "sidebar"
        ? "flex h-[32px] w-full min-w-0 items-center gap-[8px] rounded-(--radius-row) px-[8px] text-start text-body text-sidebar-foreground outline-none transition-colors duration-150 hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring data-open:bg-sidebar-accent/50 max-sm:min-h-[44px]"
        : "flex size-[32px] items-center justify-center rounded-full outline-none transition-colors hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring data-open:bg-sidebar-accent/50"
  const content = variant === "sidebar" ? <>{disc}<span className="truncate">{user.name || user.email}</span></> : variant === "rail" ? disc
    : variant === "topbar" ? <Initials id={user.id} name={name} letters={initials(name)} size={36} /> : initials(name)
  const trigger = inToolbar
    ? <ToolbarButton render={<MenuTrigger />} aria-label="Account" className={triggerClass}>{content}</ToolbarButton>
    : <MenuTrigger aria-label="Account" className={triggerClass}>{content}</MenuTrigger>
  return (
    <Menu onAction={(value) => { if (value === "sign-out") onSignOut() }}>
      <Hint side={variant === "topbar" ? "bottom" : variant === "dock" ? "top" : "end"} label={`${user.name} · ${organization ? `${organization.name} · ${organization.role}` : "No organization"}`}>
        {trigger}
      </Hint>
      <KitMenuContent side={variant === "topbar" ? "bottom" : variant === "dock" || variant === "sidebar" ? "top" : "end"} align={variant === "sidebar" ? "start" : "end"} className="min-w-60">
        <div className="grid px-2 py-1.5 text-body">
          <span>{user.name}</span>
          <span className="text-meta text-muted-foreground">{user.email}</span>
        </div>
        <KitMenuSeparator />
        <div className="grid px-2 py-1.5 text-body">
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

/** Every destination's items and count, read once for the sidebar, the breadcrumbs and search. */
function useNavigation(sections: readonly Section[]) {
  // Every section's hooks run on every render, in one order, whichever perspective is shown.
  const menus = new Map(sections.map((section) => [section.path, (section.useMenu ?? none)() ?? []] as const))
  const badges = new Map(sections.map((section) => [section.path, shown((section.useBadge ?? none)())] as const))
  return { menus, badges }
}

type Navigation = ReturnType<typeof useNavigation>

/**
 * A sidebar link without the router's own active marking: TanStack marks every fuzzy-matching link
 * `aria-current="page"`, but exactly one row is current (DESIGN.md 9.1), so the sidebar says which.
 */
function NavAnchor({ "aria-current": _active, current, ref, ...props }: ComponentProps<"a"> & { readonly current?: boolean }) {
  return <a ref={ref} aria-current={current ? "page" : undefined} {...props} />
}
const NavLink = createLink(NavAnchor)

/** A destination's accessible name: its label, then its state when it has a count ("Inbox, 3 open"). */
const named = (label: string, count: string | undefined) => count ? `${label}, ${count} open` : label

// One rail button (DESIGN.md 9.1): 32×32, muted at rest, a 50% wash on hover or while pressed (the
// assistant), the neutral pill only when current: one destination holds the pill at a time.
const railButton = "relative flex size-[32px] shrink-0 items-center justify-center rounded-(--radius-row) text-muted-foreground transition-colors duration-150 outline-none hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring aria-[current=page]:bg-sidebar-accent aria-[current=page]:text-sidebar-accent-foreground aria-pressed:bg-sidebar-accent/50 motion-reduce:transition-none [&_svg]:size-4"
/** The attention dot: 6px in `--ring`, which reaches 3:1 on the ground, the wash and the pill; the name carries the state. */
const attentionDot = "absolute end-[5px] top-[5px] size-[6px] rounded-full bg-ring ring-2 ring-sidebar"

/** One destination on the collapsed rail: its icon, its name and state as one label and tooltip, and an attention dot. */
function RailLink({ section, count, current, onExpandSection }: {
  readonly section: Section; readonly count: string | undefined; readonly current: boolean; readonly onExpandSection: (path: string) => void
}) {
  const { setOpen } = useSidebar()
  const Icon = section.icon
  const label = named(section.label, count)
  const dot = count && <span data-count aria-hidden className={attentionDot} />
  // A destination with no page of its own (Records) expands the sidebar to show its items.
  if (section.index === false) {
    return (
      <Hint side="end" label={label}>
        <ToolbarButton aria-label={label} aria-current={current ? "page" : undefined} onClick={() => { onExpandSection(section.path); setOpen(true) }} className={railButton}>
          <Icon aria-hidden strokeWidth={1.6} />{dot}
        </ToolbarButton>
      </Hint>
    )
  }
  return (
    <Hint side="end" label={label}>
      <ToolbarLink render={<NavLink to={section.path} current={current} />} aria-label={label} className={railButton}>
        <Icon aria-hidden strokeWidth={1.6} />{dot}
      </ToolbarLink>
    </Hint>
  )
}

/**
 * The collapsed rail (48px): the shared destinations, then each space's destinations under a
 * decorative hairline. Items are hidden; the current destination keeps the pill.
 */
function RailList({ sections, perspective, navigation, current, onExpandSection }: {
  readonly sections: readonly Section[]; readonly perspective: Perspective; readonly navigation: Navigation
  readonly current: Section | undefined; readonly onExpandSection: (path: string) => void
}) {
  const global = sections.filter((section) => section.perspective === "global")
  const own = sections.filter((section) => section.perspective === perspective)
  const groups = [...new Set(own.map(({ group }) => group))]
  const label = PERSPECTIVES.find(({ id }) => id === perspective)?.label ?? ""
  const link = (section: Section) => (
    <RailLink key={section.path} section={section} count={navigation.badges.get(section.path)} current={current?.path === section.path} onExpandSection={onExpandSection} />
  )
  return (
    <Toolbar orientation="vertical" aria-label={`${label} pages`} className="flex flex-col items-center gap-px">
      {global.map(link)}
      {groups.map((group) => (
        <div key={group ?? ""} role="group" aria-label={group ?? label} className="flex flex-col items-center gap-px">
          <div aria-hidden className="my-[8px] h-px w-[16px] bg-sidebar-border" />
          {own.filter((section) => section.group === group).map(link)}
        </div>
      ))}
    </Toolbar>
  )
}

// Focus stays on the tree item; its row is the label inside, so the 2px ring is drawn on the row, inside the 8px gutter.
const focusRow = "outline-none focus-visible:[&>[data-slot=tree-label]]:relative focus-visible:[&>[data-slot=tree-label]]:z-10 focus-visible:[&>[data-slot=tree-label]]:outline-2 focus-visible:[&>[data-slot=tree-label]]:outline-solid focus-visible:[&>[data-slot=tree-label]]:outline-offset-1 focus-visible:[&>[data-slot=tree-label]]:outline-ring"
// One source-list row (DESIGN.md 9.1): 32px (44px in the phone sheet), step 2 at 400, muted at rest, a 50% wash
// on hover with no text change, and the neutral pill with its own ink when current. No ring, bar or accent colour.
const row = "relative flex h-[32px] w-full min-w-0 items-center gap-[8px] rounded-(--radius-row) px-[8px] text-body font-normal text-muted-foreground transition-colors duration-150 outline-none hover:bg-sidebar-accent/50 data-current:bg-sidebar-accent data-current:text-sidebar-accent-foreground motion-reduce:transition-none max-sm:h-auto max-sm:min-h-[44px] [&_svg]:size-4 [&_svg]:shrink-0 max-sm:[&_svg]:size-[18px]"
// A group header: a quiet step-1 label in sentence case with a 32px hit area; its chevron shows on hover and focus
// (always where the reader cannot hover, and in the phone sheet), turns when open, and mirrors in RTL.
const groupHeading = "group/heading flex h-[32px] w-full min-w-0 items-center gap-[4px] rounded-(--radius-row) px-[8px] text-meta font-normal text-muted-foreground outline-none max-sm:min-h-[44px]"
const chevron = "ms-auto flex text-subtle-foreground opacity-0 transition-[opacity,rotate] duration-150 group-hover/heading:opacity-100 [[role=treeitem]:focus-visible>[data-slot=tree-label]>&]:opacity-100 rtl:rotate-180 data-expanded:rotate-90 motion-reduce:transition-none [@media(hover:none)]:opacity-100 group-data-[mobile=true]/sidebar:opacity-100 [&_svg]:size-3.5"

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
 * The expanded source list: the shared destinations (Home, Inbox), then the perspective's spaces
 * (collapsible headings, never walls), each listing its destinations; the open destination lists its
 * items under it. Built on the UI SDK `Tree`, whose `current` marks the one current row.
 */
function SectionTree({ sections, perspective, navigation, pathname, chosen, onChosenChange }: {
  readonly sections: readonly Section[]; readonly perspective: Perspective; readonly navigation: Navigation; readonly pathname: string
  readonly chosen: Readonly<Record<string, boolean>>
  readonly onChosenChange: (update: (previous: Readonly<Record<string, boolean>>) => Readonly<Record<string, boolean>>) => void
}) {
  const { setOpenMobile } = useSidebar()
  const global = sections.filter((section) => section.perspective === "global")
  const own = sections.filter((section) => section.perspective === perspective)
  const listed = [...global, ...own]
  const open = destinationOf(own, pathname, navigation.menus)
  const current = currentOf(listed, navigation, pathname)
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
    <Tree aria-label={`${label} pages`} mode="none" expandOnPress current={current} expanded={expanded}
      onExpandedChange={(next) => onChosenChange((previous) => diffExpanded(expanded, next, previous))}>
      {global.map((section) => <Destination key={section.path} section={section} entries={[]} badge={navigation.badges.get(section.path)} close={close} />)}
      {spaces.map((space) => {
        const members = own.filter((section) => (section.group ?? "") === space)
        const pinnedItems = own.filter((section) => section.pinned).flatMap((section) =>
          (navigation.menus.get(section.path) ?? [])
            .filter((entry) => entry.group === space)
            .map((entry) => {
              const Icon = entry.icon ?? section.icon
              return (
                <TreeItem key={entry.href} value={entry.href} label={typeof entry.label === "string" ? entry.label : undefined} className={focusRow}>
                  <TreeItemLabel render={<NavLink {...target(section, entry.href)} onClick={close} />} className={row}>
                    <Icon aria-hidden strokeWidth={1.6} /><span className="truncate">{entry.label}</span>
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
          <TreeItem key={space} value={`space:${space}`} label={space} className={cn("mt-[12px]", focusRow)}>
            <TreeItemLabel className={groupHeading}>
              <span className="truncate">{space}</span>
              <TreeItemIndicator className={chevron}><ChevronRight aria-hidden /></TreeItemIndicator>
            </TreeItemLabel>
            <TreeGroup>{items}</TreeGroup>
          </TreeItem>
        )
      })}
    </Tree>
  )
}

/** One destination in the source list: its row (a link, or a plain heading when it has no page), then its items. */
function Destination({ section, entries, badge, close }: { readonly section: Section; readonly entries: readonly MenuEntry[]; readonly badge: string | undefined; readonly close: () => void }) {
  const headings = [...new Set(entries.map(({ group }) => group ?? ""))]
  // Items indent to the label column and carry no icon: under one destination they are all one kind.
  const rows = (members: readonly MenuEntry[]) => members.map((entry) => {
    const count = shown(entry.count)
    return (
      <TreeItem key={entry.href} value={entry.href} label={typeof entry.label === "string" ? entry.label : undefined} className={focusRow}>
        <TreeItemLabel render={<NavLink {...target(section, entry.href)} onClick={close} />} className={cn(row, "ps-[32px]")}>
          <span className="truncate">{entry.label}</span>
          {count && <span className="ms-auto text-meta tabular-nums">{count}</span>}
        </TreeItemLabel>
      </TreeItem>
    )
  })
  const children = headings.map((heading) => {
    const members = entries.filter((entry) => (entry.group ?? "") === heading)
    if (heading === "") return rows(members)
    return (
      <TreeItem key={heading} value={`group:${section.path}:${heading}`} label={heading} className={focusRow}>
        <TreeItemLabel className={cn(groupHeading, "ps-[32px]")}>
          <span className="truncate">{heading}</span>
          <span className="tabular-nums">{members.length}</span>
          <TreeItemIndicator className={chevron}><ChevronRight aria-hidden /></TreeItemIndicator>
        </TreeItemLabel>
        <TreeGroup>{rows(members)}</TreeGroup>
      </TreeItem>
    )
  })
  const Icon = section.icon
  const head = <>
    <Icon aria-hidden strokeWidth={1.6} /><span className="truncate">{section.label}</span>
    {badge && <span data-count className="ms-auto text-meta tabular-nums">{badge}</span>}
  </>
  return (
    <TreeItem value={`dest:${section.path}`} data-section-path={section.path} label={section.label} className={focusRow}>
      {section.index === false
        ? <TreeItemLabel className={cn(row, "cursor-default")}>{head}</TreeItemLabel>
        : <TreeItemLabel render={<NavLink to={section.path} onClick={close} />} className={row}>{head}</TreeItemLabel>}
      {entries.length > 0 && <TreeGroup>{children}</TreeGroup>}
    </TreeItem>
  )
}

/** The sidebar's footer row for the assistant: a source-list row expanded, a rail button collapsed (also ⌘J). */
function AssistantRow({ title, rail }: { readonly title: string; readonly rail: boolean }) {
  const { open, toggle } = useAssistant()
  if (rail) {
    return (
      <Hint side="end" label={<>{title} <Keys className="opacity-70">⌘J</Keys></>}>
        <button type="button" aria-label={title} aria-pressed={open} onClick={toggle} className={railButton}><Sparkles aria-hidden strokeWidth={1.6} /></button>
      </Hint>
    )
  }
  return (
    <button type="button" aria-label={title} aria-pressed={open} onClick={toggle}
      className={cn(row, "focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring aria-pressed:bg-sidebar-accent/50")}>
      <Sparkles aria-hidden strokeWidth={1.6} /><span className="truncate">{title}</span>
      <Keys className="ms-auto text-meta">⌘J</Keys>
    </button>
  )
}

/**
 * The sidebar (DESIGN.md 9.1): the workspace switcher, one `Studio navigation` landmark around every
 * destination (the source list expanded, the rail collapsed), then the assistant and the principal.
 */
function SidebarBody({ sections, perspective, navigation, pathname, current, chosen, onChosenChange, account, onSignOut, assistant, navRef, onExpandSection }: {
  readonly sections: readonly Section[]; readonly perspective: Perspective; readonly navigation: Navigation; readonly pathname: string
  readonly current: Section | undefined
  readonly chosen: Readonly<Record<string, boolean>>
  readonly onChosenChange: (update: (previous: Readonly<Record<string, boolean>>) => Readonly<Record<string, boolean>>) => void
  readonly account: Account; readonly onSignOut: () => void; readonly assistant?: AssistantContent
  readonly navRef: RefObject<HTMLElement | null>; readonly onExpandSection: (path: string) => void
}) {
  const { state, isMobile } = useSidebar()
  const rail = state === "collapsed" && !isMobile
  return (
    <>
      <div className={cn("flex h-[48px] shrink-0 items-center gap-[4px] px-[8px] max-sm:h-[56px]", rail && "justify-center")}>
        <ScopeSwitcher variant={rail ? "rail" : "sidebar"} />
        <SidebarClose />
      </div>
      <nav ref={navRef} aria-label="Studio navigation" className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-[8px] pb-[8px]">
        {rail
          ? <RailList sections={sections} perspective={perspective} navigation={navigation} current={current} onExpandSection={onExpandSection} />
          : <SectionTree key={perspective} sections={sections} perspective={perspective} navigation={navigation} pathname={pathname} chosen={chosen} onChosenChange={onChosenChange} />}
      </nav>
      <div className={cn("flex shrink-0 flex-col gap-px px-[8px] pt-[4px] pb-[8px]", rail && "items-center")}>
        {assistant && <AssistantRow title={assistant.title} rail={rail} />}
        <AccountMenu account={account} onSignOut={onSignOut} inToolbar={false} variant={rail ? "rail" : "sidebar"} />
      </div>
    </>
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
            : <BreadcrumbLink asChild><Link to={section.path} activeOptions={{ exact: true }} className="inline-flex min-w-0 items-center gap-0.5"><ChevronLeft aria-hidden className={cn("size-3.5 shrink-0 rtl:rotate-180", last === "entry" ? "@sm:hidden" : "hidden")} /><span className="truncate">{section.label}</span></Link></BreadcrumbLink>}
        </BreadcrumbItem>
        {entry && <>
          <BreadcrumbSeparator className={sep1Class} />
          <BreadcrumbItem className={cn("min-w-0", entryClass)}>
            {last === "entry"
              ? <BreadcrumbPage className="truncate">{entry.label}</BreadcrumbPage>
              : <BreadcrumbLink asChild><Link to={entry.href} activeOptions={{ exact: true }} className="inline-flex min-w-0 items-center gap-0.5"><ChevronLeft aria-hidden className="size-3.5 shrink-0 rtl:rotate-180 @md:hidden" /><span className="truncate">{entry.label}</span></Link></BreadcrumbLink>}
          </BreadcrumbItem>
        </>}
        {record !== undefined && <>
          <BreadcrumbSeparator className="hidden @md:block" />
          <BreadcrumbItem className="hidden min-w-0 @md:inline-flex"><BreadcrumbPage className="truncate font-mono text-meta">{record}</BreadcrumbPage></BreadcrumbItem>
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
    <div role="group" aria-label="Perspective" data-perspective={perspective} className="flex h-8 shrink-0 items-center rounded-full border border-border/80 bg-surface-subtle p-0.5 dark:bg-shell">
      {PERSPECTIVES.map(({ id, label }) => (
        <button key={id} type="button" aria-pressed={perspective === id} onClick={() => onChange(id)}
          className="flex h-[1.625rem] items-center gap-1.5 rounded-full px-2 text-body text-muted-foreground transition-[color,background-color,box-shadow] duration-150 outline-none hover:text-foreground focus-visible:z-10 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring aria-pressed:bg-background aria-pressed:font-medium aria-pressed:text-foreground aria-pressed:shadow-control aria-pressed:ring-1 aria-pressed:ring-border/70 aria-pressed:[&_svg]:text-action dark:aria-pressed:bg-accent dark:aria-pressed:ring-border @sm:px-2 @lg:px-2.5 [&_svg]:size-3.5">
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
      <DialogTrigger shortcut="mod+k" aria-label="Search" className="flex size-[32px] items-center justify-center rounded-full max-sm:size-[44px] text-muted-foreground transition-[color,background-color,border-color] duration-150 outline-none hover:bg-surface-subtle hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring @2xl:w-full @2xl:justify-start @2xl:gap-2 @2xl:border @2xl:border-border/90 @2xl:bg-surface-subtle/70 @2xl:px-2.5 @2xl:hover:border-input @2xl:hover:bg-surface-subtle">
        <Search aria-hidden className="size-4 shrink-0" /><span className="hidden truncate @2xl:inline">Search…</span><Keys className="ms-auto hidden rounded border border-border/90 bg-background px-1.5 py-0.5 text-meta leading-none text-muted-foreground @2xl:inline">⌘K</Keys>
      </DialogTrigger>
      <DialogBackdrop className="fixed inset-0 bg-black/40 backdrop-blur-[2px] transition-opacity duration-150 data-starting-style:opacity-0 data-ending-style:opacity-0" />
      <DialogContent className="fixed top-[12vh] left-1/2 m-0 w-[min(36rem,calc(100vw-2rem))] -translate-x-1/2 overflow-hidden rounded-(--radius-popup) bg-popover p-0 text-popover-foreground shadow-popup ring-1 ring-panel-border outline-none transition-[opacity,scale] duration-150 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0">
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
            <CommandInput aria-label="Search pages, object types, metrics" placeholder="Search pages, object types, metrics…" className="h-12 flex-1 bg-transparent text-body outline-none placeholder:text-muted-foreground" />
          </div>
          <CommandEmpty className="hidden px-4 py-8 text-center text-body text-muted-foreground data-empty:block">Nothing matches.</CommandEmpty>
          <CommandList className="max-h-[min(24rem,60vh)] overflow-y-auto p-1.5">
            <CommandGroup>
              <CommandGroupLabel className="px-2 pt-2 pb-1 text-meta text-muted-foreground">Go to</CommandGroupLabel>
              {sections.filter((section) => section.index !== false).map(({ path, label, icon: Icon, perspective }) => (
                <CommandItem key={path} value={`section:${path}`} label={label} className="flex cursor-default items-center gap-2 rounded-(--radius-row) px-2.5 py-2 text-body data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Icon /></CommandItemIcon>{label}
                  {perspective !== "global" && <span className="ms-auto text-meta text-muted-foreground">{PERSPECTIVES.find(({ id }) => id === perspective)?.label}</span>}
                </CommandItem>
              ))}
            </CommandGroup>
            <CommandGroup>
              <CommandGroupLabel className="px-2 pt-3 pb-1 text-meta text-muted-foreground">Perspective</CommandGroupLabel>
              {PERSPECTIVES.map(({ id, label }) => (
                <CommandItem key={id} value={`perspective:${id}`} label={`Switch to ${label}`} className="flex cursor-default items-center gap-2 rounded-(--radius-row) px-2.5 py-2 text-body data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4">{id === "workspace" ? <LayoutGrid /> : <Wrench />}</CommandItemIcon>Switch to {label}
                </CommandItem>
              ))}
            </CommandGroup>
            <CommandGroup>
              <CommandGroupLabel className="px-2 pt-3 pb-1 text-meta text-muted-foreground">Appearance</CommandGroupLabel>
              {SKINS.map(({ id, label, detail }) => (
                <CommandItem key={id} value={`skin:${id}`} label={`Skin: ${label}`} keywords={[detail]} className="flex cursor-default items-center gap-2 rounded-(--radius-row) px-2.5 py-2 text-body data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Palette /></CommandItemIcon>Skin: {label}
                  <span className="ms-auto text-meta text-muted-foreground">{detail}</span>
                </CommandItem>
              ))}
              {SHELLS.map(({ id, label, detail }) => (
                <CommandItem key={id} value={`shell:${id}`} label={`Chrome: ${label}`} keywords={[detail]} className="flex cursor-default items-center gap-2 rounded-(--radius-row) px-2.5 py-2 text-body data-highlighted:bg-accent">
                  <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4">{id === "sidebar" ? <PanelLeft /> : <PanelBottom />}</CommandItemIcon>Chrome: {label}
                  <span className="ms-auto text-meta text-muted-foreground">{detail}</span>
                </CommandItem>
              ))}
            </CommandGroup>
            {lists.filter(({ entries }) => entries.length > 0).map(({ section, entries }) => (
              <CommandGroup key={section.path}>
                <CommandGroupLabel className="px-2 pt-3 pb-1 text-meta text-muted-foreground">{section.label}</CommandGroupLabel>
                {entries.map((entry) => (
                  <CommandItem key={entry.href} value={`entry:${section.path}:${entry.href}`} label={typeof entry.label === "string" ? entry.label : entry.href} keywords={entry.group ? [entry.group] : undefined} className="flex cursor-default items-center gap-2 rounded-(--radius-row) px-2.5 py-2 text-body data-highlighted:bg-accent">
                    {(() => { const Icon = entry.icon ?? (entry.kind ? KINDS[entry.kind].icon : section.icon); return <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Icon /></CommandItemIcon> })()}
                    <span className="truncate">{entry.label}</span>{entry.group && <span className="ms-auto truncate text-meta text-muted-foreground">{entry.group}</span>}
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
 * Honest cell connection banner (#1424):
 * - Slow initial cold start (`data-connection="connecting"`): explains why and offers Retry now.
 * - Brief automatic retry on transient outages (`data-connection="retrying"`): never lingers forever.
 * - Persistent failure or non-retryable refusal such as `503 cell.unopenable` (`data-connection="unavailable"`):
 *   never says "Retrying…"; shows "Workspace unavailable", what the server said, incident id when logged,
 *   when it was last reachable, and an explicit Retry button.
 */
function ConnectionBanner() {
  const queryClient = useQueryClient()
  const scope = useStudioScope()
  const mode = useMode()
  const slow = useSlowConnecting(mode)
  const { locale } = useLocale()
  const { view } = useView()
  const retry = () => {
    void retryMode(queryClient)
    void queryClient.invalidateQueries({
      predicate: (query) => {
        const head = query.queryKey[0]
        return query.state.status === "error" && head !== "studio-scope" && head !== "studio-mode"
      },
    })
  }
  if (mode.data !== undefined) return null
  if (mode.failureCount === 0 && !mode.isError) {
    if (!slow) return null
    return (
      <div role="status" data-connection="connecting" className="flex flex-wrap items-center gap-2 border-b bg-muted/50 px-4 py-2 text-body md:px-8">
        <Loader2 aria-hidden className="size-4 shrink-0 animate-spin text-muted-foreground" />
        <span>Connecting to your workspace cell…</span>
        <span className="text-muted-foreground">Starting a cold cell can take a moment.</span>
        <Button type="button" variant="outline" size="xs" className="ms-auto" onClick={retry}>Retry now</Button>
      </div>
    )
  }
  const retrying = !mode.isError && isRetryableModeError(mode.failureReason)
  if (retrying) {
    return (
      <div role="status" data-connection="retrying" className="flex flex-wrap items-center gap-2 border-b border-line bg-canvas px-4 py-2 text-body text-foreground md:px-8">
        <Dot tone="held" />
        <CloudOff aria-hidden className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.6} />
        <span>Couldn't reach the cell.</span>
        <span className="text-muted-foreground">Reconnecting…</span>
        <Button type="button" variant="outline" size="xs" className="ms-auto" disabled={mode.isFetching} onClick={retry}>Retry now</Button>
      </div>
    )
  }
  const error = mode.error ?? mode.failureReason
  const info = explainError(error, locale, view)
  const reachable = lastReachableAt(scope.data)
  const reachableLabel = formatLastReachable(reachable)
  return (
    <div role="alert" data-connection="unavailable" data-retryable={info.retryable === false ? "false" : "true"}
      className="flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b border-line bg-canvas px-4 py-2.5 text-body md:px-8">
      <Dot tone="bad" />
      <CloudOff aria-hidden className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.6} />
      <div className="flex min-w-0 flex-1 flex-wrap items-baseline gap-x-2 gap-y-0.5">
        <span className="text-foreground">Workspace unavailable.</span>
        <span className="text-muted-foreground">{info.headline}</span>
        {info.detail && <span className="text-meta text-muted-foreground">{info.detail}</span>}
        {reachable !== null && reachableLabel !== null && (
          <span className="text-meta text-muted-foreground">
            · Last reachable <time dateTime={new Date(reachable).toISOString()} title={new Date(reachable).toISOString()}>{reachableLabel}</time>
          </span>
        )}
        {info.incidentId && (
          <span className="inline-flex items-center gap-1 text-meta text-muted-foreground">· Incident <CopyId value={info.incidentId} /></span>
        )}
        {view === "technical" && info.code && (
          <span className="font-mono text-meta text-muted-foreground">· {info.code}</span>
        )}
      </div>
      <Button type="button" variant="outline" size="xs" className="ms-auto" disabled={mode.isFetching} onClick={retry}>Retry</Button>
    </div>
  )
}

const iconButton = "flex size-[32px] items-center justify-center rounded-full text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring data-open:bg-accent max-sm:size-[44px] [&_svg]:size-4"

/**
 * Notifications, honestly: Studio has no notification feed from the cell yet, so the popover says so
 * and points to the Inbox, where the server lists what needs the reader. It never says "Nothing yet",
 * which would claim the feed was read and empty.
 */
function Notifications() {
  return (
    <Popover>
      <PopoverTrigger aria-label="Notifications" className={iconButton}><Bell aria-hidden strokeWidth={1.6} /></PopoverTrigger>
      <KitPopoverContent align="end" className="w-72">
        <PopoverTitle className="mb-1 text-body">Notifications</PopoverTitle>
        <p data-notifications="unavailable" className="text-body text-muted-foreground">Studio doesn't receive notifications from this cell yet. What needs you is in the <Link to="/decisions" className="text-link underline-offset-4 hover:underline">Inbox</Link>.</p>
      </KitPopoverContent>
    </Popover>
  )
}

/** The page bar's end-side buttons: the page's own tools, then appearance, shortcuts and notifications. */
function BarButtons({ tools }: { readonly tools?: ReactNode }) {
  const { view } = useView()
  const { isMobile } = useSidebar()
  return (
    <div className="flex items-center gap-0.5">
      {view === "technical" && <span className="hidden rounded-full bg-(--well) px-2 py-0.5 text-meta text-muted-foreground ring-1 ring-line lg:inline">Technical view</span>}
      {/* On a phone the account menu is behind the sheet, so one small menu holds the view and language. */}
      {/* Rendered only on a phone, so its choices never shadow the account menu's. */}
      {isMobile && <Menu>
        <MenuTrigger aria-label="Language and view" className={iconButton}><SlidersHorizontal aria-hidden strokeWidth={1.6} /></MenuTrigger>
        <KitMenuContent align="end" className="w-52"><ViewAndLanguage /></KitMenuContent>
      </Menu>}
      {tools}
      <TopBarExtras tickBar="hidden @6xl:flex" language="hidden @5xl:flex" scheme="hidden @xl:flex" />
      <AppearanceTray />
      <Popover>
        <PopoverTrigger aria-label="Keyboard shortcuts" className={cn(iconButton, "hidden @xl:flex")}><Keyboard aria-hidden strokeWidth={1.6} /></PopoverTrigger>
        <KitPopoverContent align="end" className="w-64">
          <PopoverTitle className="mb-2 text-body">Keyboard shortcuts</PopoverTitle>
          <dl className="grid grid-cols-[1fr_auto] gap-y-1.5 text-body">
            <dt className="text-muted-foreground">Search</dt><dd><Keys>⌘K</Keys></dd>
            <dt className="text-muted-foreground">Show or hide the sidebar</dt><dd><Keys>⌘B</Keys></dd>
            <dt className="text-muted-foreground">Move between pages in the sidebar</dt><dd><Keys>↑ ↓</Keys></dd>
            <dt className="text-muted-foreground">Open or close a space</dt><dd><Keys>→ ←</Keys></dd>
            <dt className="text-muted-foreground">Jump to a page</dt><dd className="text-muted-foreground">type its name</dd>
          </dl>
        </KitPopoverContent>
      </Popover>
      <Notifications />
    </div>
  )
}

/**
 * A dock destination (DESIGN.md 9.2): a 40px round icon button from 640px up; below 640px a tile of
 * at least 44 × 44px that shows its label under the icon, so a phone reader never guesses.
 */
const DOCK_BUTTON = "relative grid size-[40px] shrink-0 place-items-center rounded-full text-(--dock-muted) outline-none transition-colors hover:text-(--dock-foreground) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-(--dock-ring) data-[active=true]:bg-(--dock-well) data-[active=true]:text-(--dock-foreground) data-open:bg-(--dock-well) data-open:text-(--dock-foreground) max-sm:h-auto max-sm:min-h-[44px] max-sm:w-auto max-sm:min-w-[52px] max-sm:content-center max-sm:gap-0.5 max-sm:rounded-[18px] max-sm:px-1.5 max-sm:py-1 [&_svg]:size-[18px]"

/** The label a dock tile shows under its icon below 640px (its accessible name carries it at every width). */
function DockLabel({ children }: { readonly children: ReactNode }) {
  return <span aria-hidden className="hidden max-w-[5.5rem] truncate text-meta leading-none max-sm:block">{children}</span>
}

/** Speech-bubble "AI" glyph from v6/shell.tsx, in the ink of the control it sits in (the dock is dark in every skin). */
function AiGlyph({ className }: { readonly className?: string }) {
  return (
    <span aria-hidden className={cn("relative grid size-[18px] place-items-center", className)}>
      <MessageCircle className="absolute inset-0 size-full" strokeWidth={1.8} />
      {/* The letters are part of the icon (they scale with it), so they are drawn, not set in the type scale. */}
      <svg viewBox="0 0 24 24" className="absolute inset-0 size-full"><text x="12" y="14.6" textAnchor="middle" fontSize="7.5" fill="currentColor" fontFamily="var(--font-sans)">AI</text></svg>
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
  const dockBtn = DOCK_BUTTON

  return (
    <nav
      data-slot="floating-dock"
      aria-label="Floating dock"
      className="pointer-events-none fixed inset-x-0 bottom-6 z-40 flex justify-center px-4"
    >
      <Toolbar
        aria-label="Dock navigation"
        className="pointer-events-auto flex max-w-full items-center gap-1 overflow-x-auto rounded-[26px] bg-(--dock-surface) p-1.5 text-(--dock-foreground) shadow-(--dock-shadow) ring-1 ring-(--dock-border) backdrop-blur-xl [scrollbar-width:none] max-sm:rounded-[24px]"
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
                <DockLabel>{section.label}</DockLabel>
                {count && <span aria-hidden className="absolute top-[7px] end-[7px] size-[6px] rounded-full bg-(--dock-indicator) ring-2 ring-(--dock-surface)" />}
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
                <DockLabel>{section.label}</DockLabel>
                {count && <span aria-hidden className="absolute top-[7px] end-[7px] size-[6px] rounded-full bg-(--dock-indicator) ring-2 ring-(--dock-surface)" />}
              </ToolbarLink>
            </Hint>
          )
        })}
        <ToolbarSeparator className="mx-1 h-5 w-px shrink-0 bg-(--dock-border)" />
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
              <DockLabel>{assistant.title}</DockLabel>
            </ToolbarButton>
          </Hint>
        )}
        <AppearanceTray side="top" variant="dock" />
        {overflow.length > 0 && (
          <Menu>
            <Hint side="top" label="More destinations">
              <MenuTrigger aria-label="More destinations" className={dockBtn}>
                <CircleEllipsis aria-hidden className="size-[18px]" strokeWidth={1.7} />
                <DockLabel>More</DockLabel>
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
                      {count && <span aria-hidden className="ms-auto text-meta text-muted-foreground tabular-nums">{count}</span>}
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
  const scopeKey = `${props.account.user.id}:${props.account.organization?.id ?? ""}:${props.account.environmentId ?? ""}`
  return (
    <AppearanceProvider initialSkin={props.initialSkin} initialShell={props.initialShell}>
      <LocaleProvider><PerspectiveProvider><AssistantProvider available={props.assistant !== undefined} scopeKey={scopeKey}><LocalizedShell {...props} /></AssistantProvider></PerspectiveProvider></LocaleProvider>
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
function WorkbenchFrame({ sections, tools, assistant, account, onSignOut, pathname, perspective, navigation, current, switchTo }: {
  readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly assistant?: AssistantContent
  readonly account: Account; readonly onSignOut: () => void
  readonly pathname: string; readonly perspective: Perspective; readonly navigation: Navigation
  readonly current: Section | undefined; readonly switchTo: (next: Perspective) => void
}) {
  const { open, isMobile } = useSidebar()
  const navRef = useRef<HTMLElement>(null)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const wasOpenRef = useRef(open)
  // Where focus last was inside the sidebar (anywhere: header, list or footer), so a collapse that
  // unmounts that control can hand focus to its rail counterpart.
  const lastInSidebarRef = useRef<{ readonly scope: boolean; readonly label: string | null } | null>(null)
  const pendingFocusSectionRef = useRef<string | null>(null)
  const [chosenByPerspective, setChosenByPerspective] = useState<Readonly<Record<Perspective, Readonly<Record<string, boolean>>>>>({ workspace: {}, builder: {} })
  const updateChosen = (update: (previous: Readonly<Record<string, boolean>>) => Readonly<Record<string, boolean>>) =>
    setChosenByPerspective((all) => ({ ...all, [perspective]: update(all[perspective]) }))
  const expandSection = (path: string) => {
    pendingFocusSectionRef.current = path
    const section = sections.find((candidate) => candidate.path === path && candidate.perspective === perspective)
    if (section) updateChosen((previous) => ({ ...previous, [`space:${section.group ?? ""}`]: true, [`dest:${section.path}`]: true }))
  }

  useEffect(() => {
    const onFocusIn = (event: FocusEvent) => {
      const target = event.target
      lastInSidebarRef.current = target instanceof HTMLElement && document.getElementById(SIDEBAR_ID)?.contains(target)
        ? { scope: target.closest("[data-scope]") !== null, label: target.getAttribute("aria-label") }
        : null
    }
    document.addEventListener("focusin", onFocusIn)
    return () => document.removeEventListener("focusin", onFocusIn)
  }, [])

  // Focus is never stranded. Expanding from a rail destination with no page lands on that destination's
  // row. Otherwise, when collapsing or expanding unmounts the focused control, focus moves to its
  // counterpart in the other form (the workspace switcher, the assistant, the account), else to the
  // current page's row or icon, else to the toggle. A focused control that survives, or focus outside
  // the sidebar, is left alone.
  useEffect(() => {
    const wasOpen = wasOpenRef.current
    wasOpenRef.current = open
    if (isMobile || wasOpen === open) return
    const nav = navRef.current
    if (open && pendingFocusSectionRef.current !== null) {
      const path = pendingFocusSectionRef.current
      pendingFocusSectionRef.current = null
      const sectionItem = Array.from(nav?.querySelectorAll<HTMLElement>("[data-section-path]") ?? [])
        .find((element) => element.dataset.sectionPath === path)
      sectionItem?.focus()
      if (nav && !nav.contains(document.activeElement)) {
        nav.querySelector<HTMLElement>('[role="treeitem"], a[href], button')?.focus()
      }
      return
    }
    const last = lastInSidebarRef.current
    const active = document.activeElement
    const lost = active === null || active === document.body || !active.isConnected || (!open && (nav?.contains(active) ?? false))
    if (last === null || !lost) return
    const sidebar = document.getElementById(SIDEBAR_ID)
    const counterpart = last.scope
      ? sidebar?.querySelector<HTMLElement>("[data-scope]")
      : last.label !== null ? sidebar?.querySelector<HTMLElement>(`[aria-label="${CSS.escape(last.label)}"]`) : null
    ;(counterpart ?? nav?.querySelector<HTMLElement>('[aria-current="page"]') ?? triggerRef.current)?.focus()
  }, [open, isMobile])

  const { shell } = useAppearance()
  const isDock = shell === "dock"
  const collapsed = !open && !isMobile
  const resolvedPathname = useRouterState({ select: (state) => state.resolvedLocation?.pathname ?? state.location.pathname })

  return (
    <>
      {!isDock && (
        <Sidebar>
          <SidebarBody sections={sections} perspective={perspective} navigation={navigation} pathname={pathname} current={current}
            chosen={chosenByPerspective[perspective]} onChosenChange={updateChosen} account={account} onSignOut={onSignOut}
            assistant={assistant} navRef={navRef} onExpandSection={expandSection} />
        </Sidebar>
      )}
      <SidebarInset className="flex-row overflow-clip">
        <div className="@container flex min-w-0 flex-1 flex-col">
          {isDock
            ? (
              <header data-slot="dock-topbar" className="flex h-[4.5rem] shrink-0 items-center gap-3 bg-canvas px-6 md:px-8">
                <Link to="/" aria-label="Norn Studio home" className="flex size-8 shrink-0 items-center justify-center rounded-lg outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring">
                  <img src={nornMark} alt="" width={28} height={28} className="size-7 object-contain" />
                </Link>
                <ScopeSwitcher variant="pill" />
                <div className="hidden min-w-0 overflow-hidden @[36rem]:block">{current && current.path !== "/" && <Crumbs key={current.path} section={current} entries={navigation.menus.get(current.path) ?? []} pathname={pathname} />}</div>
                <div className="ms-auto flex shrink-0 items-center gap-2">
                  <div className="flex shrink-0 justify-center @3xl:w-52"><CommandMenu sections={sections} navigation={navigation} onPerspective={switchTo} /></div>
                  <PerspectiveSwitch perspective={perspective} onChange={switchTo} />
                  {tools}
                  <TopBarExtras tickBar="hidden @5xl:flex" language="hidden @3xl:flex" scheme="hidden @md:flex" />
                  <Notifications />
                  <AccountMenu account={account} onSignOut={onSignOut} inToolbar={false} variant="topbar" />
                </div>
              </header>
            )
            : (
              // The top bar spans the content column only: 52px, borderless, on the canvas (DESIGN.md 9.1).
              <header data-slot="topbar" className="flex h-[52px] shrink-0 items-center bg-canvas px-2 @sm:px-3 @2xl:px-4">
                <div className="mx-auto flex w-full max-w-[1260px] min-w-0 items-center justify-between gap-1 @sm:gap-2 @2xl:gap-3">
                  <div className="flex min-w-0 flex-1 items-center gap-1.5">
                    <Hint side="bottom" label={<>{collapsed ? "Show sidebar" : "Hide sidebar"} <Keys className="opacity-70">⌘B</Keys></>}>
                      <SidebarTrigger ref={triggerRef} className="-ms-1" />
                    </Hint>
                    {/* The crumbs collapse by the room they have beside the bar's controls, not by the page's width. */}
                    <div className="hidden min-w-0 flex-1 overflow-hidden @[17rem]:block"><div className="@container min-w-0">{current && <Crumbs key={current.path} section={current} entries={navigation.menus.get(current.path) ?? []} pathname={pathname} />}</div></div>
                  </div>
                  <div className="flex shrink-0 justify-center @2xl:w-48 @6xl:w-64 @7xl:w-80"><CommandMenu sections={sections} navigation={navigation} onPerspective={switchTo} /></div>
                  <div className="flex shrink-0 items-center justify-end gap-1 @xl:gap-2">
                    <PerspectiveSwitch perspective={perspective} onChange={switchTo} />
                    <BarButtons tools={tools} />
                  </div>
                </div>
              </header>
            )}
          <ConnectionBanner />
          <div data-route={resolvedPathname} className={isDock ? "mx-auto w-full max-w-[1220px] px-6 pt-4 pb-32 md:px-8 md:pt-5 md:pb-32" : "mx-auto w-full max-w-[1220px] px-6 pt-4 pb-12 sm:px-8"}>
            <Outlet />
          </div>
        </div>
        <Inspector />
        <AssistantPanel assistant={assistant} />
      </SidebarInset>
      {isDock && <FloatingDock sections={sections} perspective={perspective} navigation={navigation} current={current} assistant={assistant} />}
    </>
  )
}

/** The shell in the reader's direction: in Arabic the sidebar sits on the right (it follows the document's `dir`). */
function LocalizedShell({ sections, tools, assistant, account, onSignOut }: { readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly assistant?: AssistantContent; readonly account: Account; readonly onSignOut: () => void }) {
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
      <SidebarProvider shortcut={shell !== "dock"} data-shell={shell} data-density={density} className="bg-canvas">
        <WorkbenchFrame sections={sections} tools={tools} assistant={assistant} account={account} onSignOut={onSignOut}
          pathname={pathname} perspective={perspective} navigation={navigation} current={current} switchTo={switchTo} />
      </SidebarProvider>
    </TooltipProvider></InspectorProvider></ViewProvider>
  )
}
