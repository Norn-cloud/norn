import { useQueryClient } from "@tanstack/react-query"
import { Dot } from "@/components/system/status-dot"
import { Initials } from "@/components/system/list-row"
import { Link, Outlet, useNavigate, useRouterState } from "@tanstack/react-router"
import { Command, CommandEmpty, CommandGroup, CommandGroupLabel, CommandInput, CommandItem, CommandItemIcon, CommandList } from "@ui-sdk/react/command"
import { Dialog, DialogBackdrop, DialogContent, DialogTitle, DialogTrigger } from "@ui-sdk/react/dialog"
import { MenuGroup, MenuRadioGroup, MenuTrigger } from "@ui-sdk/react/menu"
import { PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover"
import { Toolbar, ToolbarButton, ToolbarLink, ToolbarSeparator } from "@ui-sdk/react/toolbar"
import { AssistantPanel, AssistantProvider, useAssistant, type AssistantContent } from "@/assistant-slot"
import { Bell, ChevronDown, ChevronLeft, ChevronsUpDown, CircleEllipsis, Code2, Keyboard, Loader2, LogOut, Palette, PanelBottom, PanelLeft, Search, SlidersHorizontal } from "lucide-react"
import { Suspense, lazy, useEffect, useRef, useState, type ReactNode } from "react"
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "@/components/ui/breadcrumb"
import { Button, buttonVariants } from "@/components/ui/button"
import { ChromeDock, ChromeDockOption, ChromeDockTray, ChromeDockTrigger } from "@/components/ui/chrome-dock"
import { AppearanceProvider, SCHEMES, SHELLS, SKINS, isScheme, isShellChrome, isSkin, useAppearance, type ShellChrome, type Skin } from "@/lib/appearance"
import { TopBarExtras } from "@/top-bar-extras"
import { DENSITY } from "@/lib/perspective"
import { appletHome, appletMenu, appletOf, appletOfSection, appletsOf, claim, decoded, destinationIn, initials, shown, target, type Applet, type AppletMenu, type MenuEntry, type Menus, type Section } from "@/lib/nav-model"
import { AppletRail, AssistantButton, SheetApplets, SubmenuPanel, appletBadge, type MenuState, type RailApplet } from "@/shell-nav"
import { SIDEBAR_ID, Sidebar, SidebarClose, SidebarInset, SidebarProvider, SidebarRail, SidebarTrigger, useSidebar } from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"
import { Hint, KitMenuContent, KitMenuItem, KitMenuLabel, KitMenuRadioItem, KitMenuSeparator, KitPopoverContent, Menu, Popover } from "@/components/system/kit"
import { Inspector, InspectorProvider } from "@/lib/inspector"
import { KINDS } from "@/lib/kinds"
import { cn } from "@/lib/utils"
import { ViewProvider, useView } from "@/lib/view"
import { LOCALES, LocaleProvider, isLocale, useLocale } from "@/lib/locale"
import { WORKSPACES } from "@/lib/workspaces"
import { isRetryableModeError, retryMode, useMode } from "@/lib/live-transport"
import { CopyId } from "@/components/copy-id"
import { explainError } from "@/lib/plain-words"
import { useAccount, type Account } from "@/lib/session"
import { WorkspaceError, useWorkspaces, type WorkspacesDocument } from "@/lib/workspaces-api"
import nornMark from "@/assets/norn-mark.webp"
import { Keys } from "@/components/keys"
import { Mascot } from "@/components/system/mascot"
import { WorkspaceThemeFeed } from "@/components/workspace-theme"

const LazyCustomThemeDialog = lazy(() => import("@/components/custom-theme-dialog").then((module) => ({ default: module.CustomThemeDialog })))

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

/**
 * The open workspace and its connection state (Live, Recorded demo data, Connecting…, unavailable)
 * as one quiet line under the submenu island's title, so the state is never only in the rail
 * switcher's tooltip (DESIGN.md 9.1 and 12.2).
 */
function WorkspaceState() {
  const { open } = useOpenWorkspace(useWorkspaces().data)
  return <span data-workspace-state className="truncate text-meta text-muted-foreground">{open ? `${open.name} · ${open.detail}` : ""}</span>
}

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
              <MenuTrigger data-scope aria-label={`Workspace: ${open?.name ?? ""}`} className="flex size-[32px] max-sm:size-[44px] items-center justify-center rounded-full outline-none transition-colors hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring data-open:bg-sidebar-accent/50">
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
              <span className="truncate capitalize text-foreground">{skin}</span>
            </div>
            {customTheme && <p data-slot="skin-set-by-theme" className="text-meta text-muted-foreground">This workspace's theme, {customTheme.name}, sets the look; your skin applies when it has none.</p>}
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
            data-slot="workspace-theme-trigger"
            onClick={openThemeDialog}
            className="flex w-full items-center justify-between gap-2 rounded-lg border border-border/80 bg-background/70 px-2.5 py-2 text-meta text-foreground transition-colors outline-none hover:bg-surface-subtle focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring"
          >
            <span className="flex items-center gap-1.5">
              <Code2 aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
              <span>Workspace theme…</span>
            </span>
            <span className="truncate text-meta text-muted-foreground">{customTheme ? customTheme.name : "Default"}</span>
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
        : "flex size-[32px] max-sm:size-[44px] items-center justify-center rounded-full outline-none transition-colors hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring data-open:bg-sidebar-accent/50"
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

/**
 * Every destination's items and count, read once for the islands, the breadcrumbs and search. A
 * destination whose items are not read yet (its hook returned nothing) is `unread`, so the menu
 * claims neither items nor "none yet" for it.
 */
const none = () => undefined

function useNavigation(sections: readonly Section[]) {
  // Every section's hooks run on every render, in one order, whichever applet is open.
  const read = sections.map((section) => [section.path, (section.useMenu ?? none)()] as const)
  const menus: Menus = new Map(read.map(([path, entries]) => [path, entries ?? []] as const))
  const unread = new Set(read.filter(([path, entries]) => entries === undefined && sections.some((section) => section.path === path && section.useMenu !== undefined)).map(([path]) => path))
  const badges = new Map(sections.map((section) => [section.path, shown((section.useBadge ?? none)())] as const))
  return { menus, unread, badges }
}

type Navigation = ReturnType<typeof useNavigation>

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

/** "Search… ⌘K", the universal entry: every destination in every applet, every page they list, and the appearance. */
function CommandMenu({ sections, navigation }: { readonly sections: readonly Section[]; readonly navigation: Navigation }) {
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
              {sections.filter((section) => section.index !== false).map((section) => {
                const { path, label, icon: Icon } = section
                const applet = appletOfSection(section)
                return (
                  <CommandItem key={path} value={`section:${path}`} label={label} keywords={[applet.label]} className="flex cursor-default items-center gap-2 rounded-(--radius-row) px-2.5 py-2 text-body data-highlighted:bg-accent">
                    <CommandItemIcon className="flex text-muted-foreground [&_svg]:size-4"><Icon /></CommandItemIcon>{label}
                    {applet.label !== label && <span className="ms-auto text-meta text-muted-foreground">{applet.label}</span>}
                  </CommandItem>
                )
              })}
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

/** Whether the page in the main island shows the full Workspace unavailable state itself (`WorkspaceUnavailable`). */
function usePageShowsUnavailable(watching: boolean): boolean {
  const [shows, setShows] = useState(false)
  useEffect(() => {
    if (!watching) { setShows(false); return }
    const page = document.querySelector("[data-route]")
    if (page === null) return
    const read = () => setShows(page.querySelector("[data-workspace-unavailable]") !== null)
    read()
    const observer = new MutationObserver(read)
    observer.observe(page, { childList: true, subtree: true })
    return () => observer.disconnect()
  }, [watching])
  return shows
}

/**
 * Honest cell connection status (#1424), one line at the top of the main island that never wraps
 * into a column on a phone:
 * - Slow initial cold start (`data-connection="connecting"`): says why and offers Retry now.
 * - Brief automatic retry on transient outages (`data-connection="retrying"`): never lingers forever.
 * - Persistent failure or non-retryable refusal such as `503 cell.unopenable` (`data-connection="unavailable"`):
 *   never says "Retrying…". The page's own Workspace unavailable state is the full one (what the
 *   server said, the incident, when it was last reachable, Retry), so while the page shows it this
 *   line is gone; on a page that does not, it is the compact status: a dot and "Workspace
 *   unavailable", what the server said and the incident from 640px up, and one Retry. Below 640px
 *   the server's words and the incident sit behind "Details", so a phone reader can still quote them.
 */
function ConnectionBanner() {
  const queryClient = useQueryClient()
  const mode = useMode()
  const slow = useSlowConnecting(mode)
  const { locale } = useLocale()
  const { view } = useView()
  const unavailable = mode.data === undefined && (mode.isError || (mode.failureCount > 0 && !isRetryableModeError(mode.failureReason)))
  const pageShows = usePageShowsUnavailable(unavailable)
  const retry = () => {
    void retryMode(queryClient)
    void queryClient.invalidateQueries({
      predicate: (query) => {
        const head = query.queryKey[0]
        return query.state.status === "error" && head !== "studio-scope" && head !== "studio-mode"
      },
    })
  }
  const line = "flex min-h-[44px] flex-nowrap items-center gap-2 px-4 py-1.5 text-body md:px-8"
  if (mode.data !== undefined) return null
  if (mode.failureCount === 0 && !mode.isError) {
    if (!slow) return null
    return (
      <div role="status" data-connection="connecting" className={line}>
        <Loader2 aria-hidden className="size-4 shrink-0 animate-spin text-muted-foreground" />
        <span className="min-w-0 truncate">Connecting to your workspace cell…</span>
        <span className="hidden min-w-0 truncate text-muted-foreground sm:inline">Starting a cold cell can take a moment.</span>
        <Button type="button" variant="outline" size="sm" className="ms-auto shrink-0" onClick={retry}>Retry now</Button>
      </div>
    )
  }
  if (!unavailable) {
    return (
      <div role="status" data-connection="retrying" className={cn(line, "text-foreground")}>
        <Dot tone="held">
          <span className="min-w-0 truncate">Couldn't reach the cell.</span>
        </Dot>
        <span className="hidden text-muted-foreground sm:inline">Reconnecting…</span>
        <Button type="button" variant="outline" size="sm" className="ms-auto shrink-0" disabled={mode.isFetching} onClick={retry}>Retry now</Button>
      </div>
    )
  }
  if (pageShows) return null
  const info = explainError(mode.error ?? mode.failureReason, locale, view)
  return (
    <div role="alert" data-connection="unavailable" data-retryable={info.retryable === false ? "false" : "true"} className={line}>
      <Dot tone="bad">
        <span className="shrink-0 text-foreground">Workspace unavailable</span>
      </Dot>
      <span className="hidden min-w-0 truncate text-muted-foreground sm:inline" title={info.headline}>{info.headline}</span>
      {info.incidentId && <span className="hidden shrink-0 items-center gap-1 text-meta text-muted-foreground sm:inline-flex">Incident <CopyId value={info.incidentId} /></span>}
      <Popover>
        <PopoverTrigger className={cn(buttonVariants({ variant: "ghost", size: "sm" }), "ms-auto shrink-0 sm:hidden")}>Details</PopoverTrigger>
        <KitPopoverContent align="end" data-connection-details="" className="w-[min(18rem,calc(100vw-32px))]">
          <PopoverTitle className="mb-1 text-body">Workspace unavailable</PopoverTitle>
          <p className="text-body text-muted-foreground">{info.headline}</p>
          {info.detail && <p className="mt-1 text-meta text-foreground [overflow-wrap:anywhere]">{info.detail}</p>}
          {info.incidentId && <p className="mt-2 flex items-center gap-1 text-meta text-muted-foreground">Incident <CopyId value={info.incidentId} /></p>}
        </KitPopoverContent>
      </Popover>
      <Button type="button" variant="outline" size="sm" className="shrink-0 sm:ms-auto" disabled={mode.isFetching} onClick={retry}>Retry</Button>
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
const DOCK_BUTTON = "relative grid size-[40px] shrink-0 place-items-center rounded-full text-(--dock-muted) outline-none transition-colors [&_svg]:text-(--dock-icon) hover:text-(--dock-foreground) hover:[&_svg]:text-(--dock-foreground) data-[active=true]:[&_svg]:text-(--dock-foreground) data-open:[&_svg]:text-(--dock-foreground) aria-pressed:[&_svg]:text-(--dock-foreground) focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-(--dock-ring) data-[active=true]:bg-(--dock-well) data-[active=true]:text-(--dock-foreground) data-open:bg-(--dock-well) data-open:text-(--dock-foreground) max-sm:h-auto max-sm:min-h-[44px] max-sm:w-auto max-sm:min-w-[52px] max-sm:content-center max-sm:gap-0.5 max-sm:rounded-[18px] max-sm:px-1.5 max-sm:py-1 [&_svg]:size-[18px]"

/** The label a dock tile shows under its icon below 640px (its accessible name carries it at every width). */
function DockLabel({ children }: { readonly children: ReactNode }) {
  return <span aria-hidden className="hidden max-w-[5.5rem] truncate text-meta leading-none max-sm:block">{children}</span>
}

/**
 * Centered icon-only floating dark pill dock rendered in `dock` shell mode (`data-shell="dock"`),
 * following norn-mockups v6/shell.tsx craft: rounded-[26px] dark pill, size-10 circular buttons,
 * soft #34343a active well, hover tooltips, the assistant mascot, appearance tray, and overflow destinations.
 */
function FloatingDock({ sections, navigation, current, assistant }: {
  readonly sections: readonly Section[]
  readonly navigation: Navigation
  readonly current: Section | undefined
  readonly assistant?: AssistantContent
}) {
  const { open, toggle } = useAssistant()
  const global = sections.filter((section) => section.perspective === "global")
  const firstHref = (section: Section) => navigation.menus.get(section.path)?.find(({ href }) => !href.startsWith("#"))?.href
  const resolvedOwn = sections
    .filter((section) => section.perspective !== "global")
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
              <Mascot tone="ink" size={20} state={open ? "answered" : "idle"} />
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
 * The three-island workbench (DESIGN.md 9.1): on the backdrop, the applet rail, the open applet's
 * submenu island and the main island, which holds the top bar (breadcrumbs, search, page tools),
 * the page, and the inspector and assistant docked on its end side.
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
      <LocaleProvider><AssistantProvider available={props.assistant !== undefined} scopeKey={scopeKey}><LocalizedShell {...props} /></AssistantProvider></LocaleProvider>
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

/** Each applet's own reader choices, kept while Studio is open: branches opened or closed, lists shown whole, the filter. */
function useMenuStates() {
  const [chosen, setChosen] = useState<Readonly<Record<string, Readonly<Record<string, boolean>>>>>({})
  const [whole, setWhole] = useState<ReadonlySet<string>>(() => new Set())
  const [queries, setQueries] = useState<Readonly<Record<string, string>>>({})
  return (id: string): MenuState => ({
    chosen: chosen[id] ?? {},
    onChosenChange: (update) => setChosen((all) => ({ ...all, [id]: update(all[id] ?? {}) })),
    whole,
    onWhole: (key) => setWhole((previous) => {
      const next = new Set(previous)
      if (!next.delete(key)) next.add(key)
      return next
    }),
    query: queries[id] ?? "",
    onQuery: (query) => setQueries((all) => ({ ...all, [id]: query })),
  })
}

/** The rail, the submenu island (or the phone sheet holding both) and the main island, keeping focus across a collapse. */
function WorkbenchFrame({ sections, tools, assistant, account, onSignOut, pathname, navigation, applet, menu, current }: {
  readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly assistant?: AssistantContent
  readonly account: Account; readonly onSignOut: () => void
  readonly pathname: string; readonly navigation: Navigation
  readonly applet: Applet | undefined; readonly menu: AppletMenu | undefined; readonly current: Section | undefined
}) {
  const { open, isMobile, openMobile, setOpenMobile } = useSidebar()
  const triggerRef = useRef<HTMLButtonElement>(null)
  const menuState = useMenuStates()
  const rail: readonly RailApplet[] = appletsOf(sections).map((each) => ({
    applet: each,
    home: appletHome(each, sections, navigation.menus),
    badge: appletBadge(sections.filter((section) => appletOfSection(section).id === each.id).map((section) => navigation.badges.get(section.path))),
  }))

  // The phone sheet shows the current applet's menu when it opens; choosing another applet there shows its menu instead.
  const [sheetApplet, setSheetApplet] = useState<string | undefined>(applet?.id)
  useEffect(() => { if (openMobile) setSheetApplet(applet?.id) }, [openMobile, applet?.id])
  const sheetMenu = (() => {
    const chosen = rail.find(({ applet: each }) => each.id === sheetApplet)?.applet ?? applet
    return chosen && appletMenu(chosen, sections, navigation.menus, navigation.badges)
  })()

  // Focus is never stranded: collapsing the island (it turns inert) while focus is in it hands focus
  // to the current applet on the rail, else to the toggle. Focus anywhere else is left alone.
  const focusInIsland = useRef(false)
  useEffect(() => {
    const onFocusIn = (event: FocusEvent) => {
      focusInIsland.current = event.target instanceof Node && (document.getElementById(SIDEBAR_ID)?.contains(event.target) ?? false)
    }
    document.addEventListener("focusin", onFocusIn)
    return () => document.removeEventListener("focusin", onFocusIn)
  }, [])
  const wasOpen = useRef(open)
  useEffect(() => {
    const collapsed = wasOpen.current && !open
    wasOpen.current = open
    if (isMobile || !collapsed || !focusInIsland.current) return
    focusInIsland.current = false
    const active = document.activeElement
    if (active !== null && active !== document.body && active.isConnected && !document.getElementById(SIDEBAR_ID)?.contains(active)) return
    ;(document.querySelector<HTMLElement>('[data-slot="sidebar-rail"] [aria-current="page"]') ?? triggerRef.current)?.focus()
  }, [open, isMobile])

  const { shell } = useAppearance()
  const isDock = shell === "dock"
  const collapsed = !open && !isMobile
  const resolvedPathname = useRouterState({ select: (state) => state.resolvedLocation?.pathname ?? state.location.pathname })
  const principal = <>
    {assistant && <AssistantButton title={assistant.title} />}
    <AccountMenu account={account} onSignOut={onSignOut} inToolbar={false} variant="rail" />
  </>

  return (
    <>
      {!isDock && (
        <SidebarRail>
          <AppletRail applets={rail} current={applet?.id} top={<ScopeSwitcher variant="rail" />} bottom={principal} />
        </SidebarRail>
      )}
      {!isDock && (
        isMobile
          ? (
            <Sidebar>
              <SheetApplets applets={rail} current={applet?.id} shown={sheetMenu?.applet.id} onShow={setSheetApplet} top={<ScopeSwitcher variant="rail" />} bottom={principal} />
              <div className="flex min-w-0 flex-1 flex-col">
                {sheetMenu && <SubmenuPanel menu={sheetMenu} pathname={pathname} unread={navigation.unread} state={menuState(sheetMenu.applet.id)} onFollow={() => setOpenMobile(false)} close={<SidebarClose />} context={<WorkspaceState />} />}
              </div>
            </Sidebar>
          )
          : (
            <Sidebar>
              {menu
                ? <SubmenuPanel menu={menu} pathname={pathname} unread={navigation.unread} state={menuState(menu.applet.id)} onFollow={() => {}} context={<WorkspaceState />} />
                : <p className="px-[16px] py-[16px] text-body text-muted-foreground">No applet lists this page.</p>}
            </Sidebar>
          )
      )}
      <SidebarInset className={cn("flex-row overflow-clip", isDock && "ms-0")}>
        <div className="@container flex min-w-0 flex-1 flex-col">
          {isDock
            ? (
              <header data-slot="dock-topbar" className="flex h-[4.5rem] shrink-0 items-center gap-3 px-6 md:px-8">
                <Link to="/" aria-label="Norn Studio home" className="flex size-8 shrink-0 items-center justify-center rounded-lg outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring">
                  <img src={nornMark} alt="" width={28} height={28} className="size-7 object-contain" />
                </Link>
                <ScopeSwitcher variant="pill" />
                <div className="hidden min-w-0 overflow-hidden @[36rem]:block">{current && current.path !== "/" && <Crumbs key={current.path} section={current} entries={navigation.menus.get(current.path) ?? []} pathname={pathname} />}</div>
                <div className="ms-auto flex shrink-0 items-center gap-2">
                  <div className="flex shrink-0 justify-center @3xl:w-52"><CommandMenu sections={sections} navigation={navigation} /></div>
                  {tools}
                  <TopBarExtras tickBar="hidden @5xl:flex" language="hidden @3xl:flex" scheme="hidden @md:flex" />
                  <Notifications />
                  <AccountMenu account={account} onSignOut={onSignOut} inToolbar={false} variant="topbar" />
                </div>
              </header>
            )
            : (
              // The top bar sits inside the main island: 52px, borderless, on the island's own material (DESIGN.md 9.1).
              <header data-slot="topbar" className="flex h-[52px] shrink-0 items-center px-2 @sm:px-3 @2xl:px-4">
                <div className="mx-auto flex w-full max-w-[1260px] min-w-0 items-center justify-between gap-1 @sm:gap-2 @2xl:gap-3">
                  <div className="flex min-w-0 flex-1 items-center gap-1.5">
                    <Hint side="bottom" label={<>{collapsed ? "Show sidebar" : "Hide sidebar"} <Keys className="opacity-70">⌘B</Keys></>}>
                      <SidebarTrigger ref={triggerRef} className="-ms-1" />
                    </Hint>
                    {/* The crumbs collapse by the room they have beside the bar's controls, not by the page's width. */}
                    <div className="hidden min-w-0 flex-1 overflow-hidden @[17rem]:block"><div className="@container min-w-0">{current && <Crumbs key={current.path} section={current} entries={navigation.menus.get(current.path) ?? []} pathname={pathname} />}</div></div>
                  </div>
                  <div className="flex shrink-0 justify-center @2xl:w-48 @6xl:w-64 @7xl:w-80"><CommandMenu sections={sections} navigation={navigation} /></div>
                  <div className="flex shrink-0 items-center justify-end gap-1 @xl:gap-2">
                    <BarButtons tools={tools} />
                  </div>
                </div>
              </header>
            )}
          <ConnectionBanner />
          <WorkspaceThemeFeed />
          <div data-route={resolvedPathname} className={isDock ? "mx-auto w-full max-w-[1220px] px-6 pt-4 pb-32 md:px-8 md:pt-5 md:pb-32" : "mx-auto w-full max-w-[1220px] px-6 pt-4 pb-12 sm:px-8"}>
            <Outlet />
          </div>
        </div>
        <Inspector />
        <AssistantPanel assistant={assistant} />
      </SidebarInset>
      {isDock && <FloatingDock sections={sections} navigation={navigation} current={current} assistant={assistant} />}
    </>
  )
}

/**
 * The shell in the reader's direction (in Arabic the islands sit on the right; they follow the
 * document's `dir`). The open applet is the one that lists the page; a page two applets list stays
 * in the one the reader came from.
 */
function LocalizedShell({ sections, tools, assistant, account, onSignOut }: { readonly sections: readonly Section[]; readonly tools?: ReactNode; readonly assistant?: AssistantContent; readonly account: Account; readonly onSignOut: () => void }) {
  const { shell } = useAppearance()
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const navigation = useNavigation(sections)
  // The applet whose rail button or menu row the reader followed decides a page two applets list
  // (Data's first record type is also Builder's); the history entry keeps it, so Back restores it.
  // Else the applet the reader was in on the page before this one does: the last applet shown on
  // that page, so a menu that loads late on this page never locks it in.
  const opened = useRouterState({ select: (state) => state.location.state.applet })
  const [trail, setTrail] = useState<{ readonly path: string; readonly from?: string; readonly shown?: string }>({ path: pathname })
  const from = opened ?? (trail.path === pathname ? trail.from : trail.shown)
  const applet = appletOf(sections, pathname, navigation.menus, from)
  useEffect(() => {
    setTrail((last) => last.path === pathname
      ? (last.shown === applet?.id ? last : { ...last, shown: applet?.id })
      : { path: pathname, from: last.shown, shown: applet?.id })
  }, [pathname, applet?.id])
  const menu = applet && appletMenu(applet, sections, navigation.menus, navigation.badges)
  const current = menu && destinationIn(menu, pathname, navigation.menus)
  useHashScroll()
  return (
    <ViewProvider><InspectorProvider><TooltipProvider>
      <SidebarProvider shortcut={shell !== "dock"} data-shell={shell} data-density={DENSITY[current?.perspective ?? "global"]}>
        <WorkbenchFrame sections={sections} tools={tools} assistant={assistant} account={account} onSignOut={onSignOut}
          pathname={pathname} navigation={navigation} applet={applet} menu={menu} current={current} />
      </SidebarProvider>
    </TooltipProvider></InspectorProvider></ViewProvider>
  )
}
