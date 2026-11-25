import { createLink } from "@tanstack/react-router"
import { Toolbar, ToolbarButton, ToolbarLink } from "@ui-sdk/react/toolbar"
import { Tree, TreeGroup, TreeItem, TreeItemIndicator, TreeItemLabel } from "@ui-sdk/react/tree"
import { ChevronDown, ChevronRight, Search, Sparkles, X } from "lucide-react"
import { useId, useRef, type ComponentProps, type ReactNode } from "react"
import { useAssistant } from "@/assistant-slot"
import { Hint } from "@/components/system/kit"
import { Keys } from "@/components/keys"
import { cn } from "@/lib/utils"
import {
  currentIn, diffExpanded, matches, named, shown, target, visibleItems,
  type Applet, type AppletMenu, type MenuDestination, type MenuEntry, type PinnedEntry,
} from "@/lib/nav-model"

/**
 * The three-island shell's navigation (DESIGN.md 9.1): the applet rail, the submenu island's panel,
 * and the phone sheet that holds both. Two landmarks: "Applets" (the rail) and "<applet> menu" (the
 * open applet's items). Nothing here names a destination; every row comes from the app's model.
 */

/**
 * A link without the router's own active marking: TanStack marks every fuzzy-matching link
 * `aria-current="page"`, but exactly one rail button and one menu row are current, so the shell says which.
 */
function NavAnchor({ "aria-current": _active, current, ref, ...props }: ComponentProps<"a"> & { readonly current?: boolean }) {
  return <a ref={ref} aria-current={current ? "page" : undefined} {...props} />
}
export const NavLink = createLink(NavAnchor)

/** The rail's and the sheet's applet button: 40px (44px in the phone sheet), muted, the neutral pill only when current. */
export const railButton = "relative flex size-[40px] shrink-0 items-center justify-center rounded-(--radius-row) text-muted-foreground transition-colors duration-150 outline-none hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-1 focus-visible:outline-ring aria-[current=page]:bg-sidebar-accent aria-[current=page]:text-sidebar-accent-foreground aria-pressed:bg-sidebar-accent/50 motion-reduce:transition-none max-sm:size-[44px] [&_svg]:size-[18px]"
/** The attention dot: 6px in `--ring`, which reaches 3:1 on the island, the wash and the pill; the name carries the state. */
const attentionDot = "absolute end-[6px] top-[6px] size-[6px] rounded-full bg-ring"

/** An applet's count: its one counted destination's, else the sum of its numeric counts. */
export function appletBadge(counts: readonly (string | undefined)[]): string | undefined {
  const present = counts.filter((count): count is string => count !== undefined)
  if (present.length <= 1) return present[0]
  const numbers = present.map((count) => Number(count))
  return numbers.every(Number.isFinite) ? shown(numbers.reduce((a, b) => a + b, 0)) : present[0]
}

export interface RailApplet {
  readonly applet: Applet
  /** Where it opens; none while nothing it lists is a page yet. */
  readonly home: string | undefined
  readonly badge: string | undefined
}

/** One applet on the rail: its icon, its name and state as one label and an end-side tooltip, and an attention dot. */
function AppletLink({ item, current }: { readonly item: RailApplet; readonly current: boolean }) {
  const Icon = item.applet.icon
  const label = named(item.applet.label, item.badge)
  const dot = item.badge && <span data-count aria-hidden className={attentionDot} />
  return (
    <Hint side="end" label={label}>
      {item.home === undefined
        ? <ToolbarButton aria-label={`${label}, nothing to open yet`} aria-disabled aria-current={current ? "page" : undefined} className={cn(railButton, "opacity-60")}><Icon aria-hidden strokeWidth={1.6} />{dot}</ToolbarButton>
        : <ToolbarLink render={<NavLink to={item.home} current={current} />} aria-label={label} data-applet={item.applet.id} className={railButton}><Icon aria-hidden strokeWidth={1.6} />{dot}</ToolbarLink>}
    </Hint>
  )
}

/**
 * The applet rail (the first island): the workspace switcher, the "Applets" landmark with one
 * button per applet (a UI SDK vertical `Toolbar`: one tab stop, arrows move between applets), then
 * the assistant and the principal. Exactly one applet carries `aria-current="page"`.
 */
export function AppletRail({ applets, current, top, bottom }: {
  readonly applets: readonly RailApplet[]; readonly current: string | undefined
  readonly top: ReactNode; readonly bottom: ReactNode
}) {
  return (
    <>
      <div className="flex h-[52px] w-full shrink-0 items-center justify-center">{top}</div>
      <nav aria-label="Applets" className="flex min-h-0 w-full flex-1 flex-col items-center overflow-y-auto overflow-x-hidden py-[4px]">
        <Toolbar orientation="vertical" aria-label="Applets" className="flex flex-col items-center gap-[4px]">
          {applets.map((item) => <AppletLink key={item.applet.id} item={item} current={item.applet.id === current} />)}
        </Toolbar>
      </nav>
      <div className="flex w-full shrink-0 flex-col items-center gap-[4px] pt-[4px] pb-[10px]">{bottom}</div>
    </>
  )
}

/**
 * The phone sheet's applet column: the same applets as buttons that show their menu beside them
 * (`aria-pressed`); the one that holds the current page carries `aria-current="page"`. A followed
 * row closes the sheet, so choosing an applet never navigates by itself.
 */
export function SheetApplets({ applets, current, shown: open, onShow, top, bottom }: {
  readonly applets: readonly RailApplet[]; readonly current: string | undefined; readonly shown: string | undefined
  readonly onShow: (id: string) => void; readonly top: ReactNode; readonly bottom: ReactNode
}) {
  return (
    <div className="flex w-[64px] shrink-0 flex-col items-center border-e border-sidebar-border">
      <div className="flex h-[56px] w-full shrink-0 items-center justify-center">{top}</div>
      <nav aria-label="Applets" className="flex min-h-0 w-full flex-1 flex-col items-center overflow-y-auto overflow-x-hidden py-[4px]">
        <Toolbar orientation="vertical" aria-label="Applets" className="flex flex-col items-center gap-[4px]">
          {applets.map(({ applet, badge }) => {
            const Icon = applet.icon
            return (
              <ToolbarButton key={applet.id} aria-label={named(applet.label, badge)} aria-pressed={open === applet.id}
                aria-current={current === applet.id ? "page" : undefined} data-applet={applet.id} onClick={() => onShow(applet.id)} className={railButton}>
                <Icon aria-hidden strokeWidth={1.6} />{badge && <span data-count aria-hidden className={attentionDot} />}
              </ToolbarButton>
            )
          })}
        </Toolbar>
      </nav>
      <div className="flex w-full shrink-0 flex-col items-center gap-[4px] pt-[4px] pb-[10px]">{bottom}</div>
    </div>
  )
}

/** The assistant's rail button (also ⌘J): pressed while open, which takes the 50% wash, never the pill. */
export function AssistantButton({ title }: { readonly title: string }) {
  const { open, toggle } = useAssistant()
  return (
    <Hint side="end" label={<>{title} <Keys className="opacity-70">⌘J</Keys></>}>
      <button type="button" aria-label={title} aria-pressed={open} onClick={toggle} className={railButton}><Sparkles aria-hidden strokeWidth={1.6} /></button>
    </Hint>
  )
}

// Focus stays on the tree item; its row is the label inside, so the 2px ring is drawn on the row, inside the 8px gutter.
const focusRow = "outline-none focus-visible:[&>[data-slot=tree-label]]:relative focus-visible:[&>[data-slot=tree-label]]:z-10 focus-visible:[&>[data-slot=tree-label]]:outline-2 focus-visible:[&>[data-slot=tree-label]]:outline-solid focus-visible:[&>[data-slot=tree-label]]:outline-offset-1 focus-visible:[&>[data-slot=tree-label]]:outline-ring"
// One menu row (DESIGN.md 9.1): 32px (44px in the phone sheet), step 2 at 400, muted at rest, a 50% wash
// on hover with no text change, and the neutral pill with its own ink when current. No ring, bar or accent colour.
const row = "relative flex h-[32px] w-full min-w-0 items-center gap-[8px] rounded-(--radius-row) px-[8px] text-body font-normal text-muted-foreground transition-colors duration-150 outline-none hover:bg-sidebar-accent/50 data-current:bg-sidebar-accent data-current:text-sidebar-accent-foreground motion-reduce:transition-none max-sm:h-auto max-sm:min-h-[44px] [&_svg]:size-4 [&_svg]:shrink-0 max-sm:[&_svg]:size-[18px]"
// A group header: a quiet step-1 label in sentence case with a 32px hit area; its chevron shows on hover and focus
// (always where the reader cannot hover, and in the phone sheet), turns when open, and mirrors in RTL.
const groupHeading = "group/heading flex h-[32px] w-full min-w-0 items-center gap-[4px] rounded-(--radius-row) px-[8px] text-meta font-normal text-muted-foreground outline-none max-sm:min-h-[44px]"
const chevron = "ms-auto flex text-subtle-foreground opacity-0 transition-[opacity,rotate] duration-150 group-hover/heading:opacity-100 [[role=treeitem]:focus-visible>[data-slot=tree-label]>&]:opacity-100 rtl:rotate-180 data-expanded:rotate-90 motion-reduce:transition-none [@media(hover:none)]:opacity-100 group-data-[mobile=true]/sidebar:opacity-100 [&_svg]:size-3.5"
// "Show all": a quiet step-1 row in the list's own column, its chevron turning when the list is whole.
const moreRow = "flex h-[32px] w-full min-w-0 items-center gap-[4px] rounded-(--radius-row) px-[8px] text-meta text-muted-foreground transition-colors duration-150 hover:bg-sidebar-accent/50 motion-reduce:transition-none max-sm:min-h-[44px] [&_svg]:size-3.5"

export interface MenuState {
  /** The branches the reader opened or closed in this applet, by tree value. */
  readonly chosen: Readonly<Record<string, boolean>>
  readonly onChosenChange: (update: (previous: Readonly<Record<string, boolean>>) => Readonly<Record<string, boolean>>) => void
  /** The lists the reader asked to see whole, by list key. */
  readonly whole: ReadonlySet<string>
  readonly onWhole: (key: string) => void
  /** The reader's filter over this applet's rows. */
  readonly query: string
  readonly onQuery: (query: string) => void
}

/** Whether any destination in the menu lists items of its own, which is when a filter field helps. */
export const filterable = (menu: AppletMenu) => menu.headings.some(({ destinations, pinned }) => pinned.length > 0 || destinations.some(({ section, entries }) => section.useMenu !== undefined && entries.length > 0))

/** The menu narrowed to the rows that match the filter: a matching destination keeps all its items. */
function narrowed(menu: AppletMenu, query: string): AppletMenu {
  if (query.trim() === "") return menu
  return {
    applet: menu.applet,
    headings: menu.headings.map(({ heading, destinations, pinned }) => ({
      heading,
      destinations: destinations
        .map((destination) => matches(destination.section.label, query) ? destination : { ...destination, entries: destination.entries.filter(({ label }) => matches(label, query)) })
        .filter((destination) => matches(destination.section.label, query) || destination.entries.length > 0),
      pinned: matches(heading, query) ? pinned : pinned.filter(({ entry }) => matches(entry.label, query)),
    })).filter(({ destinations, pinned }) => destinations.length + pinned.length > 0),
  }
}

/**
 * The submenu island's panel: the applet's name, a filter field when it lists items, and the
 * "<applet> menu" landmark around a UI SDK `Tree` (the APG navigation treeview: arrows, Right/Left,
 * Home/End, type-ahead; `current` marks the one current row). Long lists show their first rows and
 * a "Show all" row; a filter that matches nothing says so.
 */
export function SubmenuPanel({ menu, pathname, unread, state, onFollow, close }: {
  readonly menu: AppletMenu; readonly pathname: string
  /** Destinations whose items are not read yet (pending or failed): they claim neither items nor "none yet". */
  readonly unread: ReadonlySet<string>
  readonly state: MenuState; readonly onFollow: () => void; readonly close?: ReactNode
}) {
  const { applet } = menu
  const navRef = useRef<HTMLElement>(null)
  const headingId = useId()
  const filtering = state.query.trim() !== ""
  const shownMenu = narrowed(menu, state.query)
  return (
    <>
      <div className="flex h-[52px] shrink-0 items-center gap-[4px] ps-[16px] pe-[8px] max-sm:h-[56px]">
        <span id={headingId} className="min-w-0 flex-1 truncate text-body text-foreground">{applet.label}</span>
        {close}
      </div>
      {filterable(menu) && (
        <div className="shrink-0 px-[8px] pb-[8px]">
          <label className="flex h-[32px] items-center gap-[8px] rounded-(--radius-row) bg-sidebar-accent/50 px-[8px] text-muted-foreground ring-1 ring-sidebar-border focus-within:outline-2 focus-within:outline-solid focus-within:outline-offset-1 focus-within:outline-ring max-sm:min-h-[44px] [&_svg]:size-3.5">
            <Search aria-hidden strokeWidth={1.6} className="shrink-0" />
            <input type="search" value={state.query} onChange={(event) => state.onQuery(event.target.value)} aria-label={`Filter ${applet.label}`} placeholder="Filter…"
              onKeyDown={(event) => {
                // Down moves into the list; Escape empties a filter before it reaches anything else.
                if (event.key === "ArrowDown") { event.preventDefault(); navRef.current?.querySelector<HTMLElement>('[role="treeitem"]')?.focus() }
                if (event.key === "Escape" && state.query !== "") { event.preventDefault(); event.stopPropagation(); state.onQuery("") }
              }}
              className="min-w-0 flex-1 bg-transparent text-body text-foreground outline-none placeholder:text-muted-foreground [&::-webkit-search-cancel-button]:hidden" />
            {filtering && <button type="button" aria-label="Clear filter" onClick={() => state.onQuery("")} className="-me-[4px] flex size-[24px] shrink-0 items-center justify-center rounded-full hover:bg-sidebar-accent max-sm:size-[36px]"><X aria-hidden /></button>}
          </label>
        </div>
      )}
      <nav ref={navRef} aria-label={`${applet.label} menu`} className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-[8px] pb-[8px]">
        {filtering && shownMenu.headings.length === 0
          ? <p role="status" className="px-[8px] py-[6px] text-body text-muted-foreground">Nothing matches “{state.query.trim()}”.</p>
          : <MenuTree key={applet.id} menu={shownMenu} full={menu} pathname={pathname} unread={unread} state={state} filtering={filtering} onFollow={onFollow} labelledBy={headingId} />}
      </nav>
    </>
  )
}

/** The menu's tree: headings (collapsible), their destinations and pinned items, each list capped by "Show all". */
function MenuTree({ menu, full, pathname, unread, state, filtering, onFollow, labelledBy }: {
  readonly menu: AppletMenu; readonly full: AppletMenu; readonly pathname: string; readonly unread: ReadonlySet<string>
  readonly state: MenuState; readonly filtering: boolean; readonly onFollow: () => void; readonly labelledBy: string
}) {
  const current = currentIn(full, pathname)
  // Headings and every list start open; the reader's own choices win, except that the current row is never hidden.
  const defaults = new Map<string, boolean>()
  const holding = new Set<string>()
  for (const { heading, destinations, pinned } of menu.headings) {
    const space = `space:${heading}`
    if (heading !== "") defaults.set(space, true)
    for (const { section, entries } of destinations) {
      if (entries.length > 0) defaults.set(`dest:${section.path}`, true)
      for (const sub of new Set(entries.map(({ group }) => group ?? ""))) if (sub !== "") defaults.set(`group:${section.path}:${sub}`, true)
      const at = entries.find(({ href }) => href === current)
      if (at || current === `dest:${section.path}`) {
        holding.add(space)
        if (at) holding.add(`dest:${section.path}`)
        if (at?.group) holding.add(`group:${section.path}:${at.group}`)
      }
    }
    if (pinned.some(({ entry }) => entry.href === current)) holding.add(space)
  }
  const values = [...defaults.keys()]
  const expanded = values.filter((value) => filtering || holding.has(value) || (state.chosen[value] ?? defaults.get(value)))
  const list = (key: string, items: readonly MenuEntry[]) => filtering ? { shown: items, hidden: 0 } : visibleItems(items, state.whole.has(key), current)
  const more = (key: string, hidden: number, total: number, indent: boolean) => {
    // A filtered list shows every match, so it offers neither.
    if (filtering || (hidden === 0 && !state.whole.has(key))) return null
    const whole = state.whole.has(key)
    const label = whole ? "Show fewer" : `Show all ${total}`
    return (
      <TreeItem key={`more:${key}`} value={`more:${key}`} label={label} data-more={key} className={focusRow}>
        <TreeItemLabel className={cn(moreRow, indent && "ps-[32px]")}>
          <span className="truncate">{label}</span>
          <ChevronDown aria-hidden className={cn("shrink-0 transition-transform duration-150 motion-reduce:transition-none", whole && "rotate-180")} />
        </TreeItemLabel>
      </TreeItem>
    )
  }
  const pinnedRows = (heading: string, pinned: readonly PinnedEntry[]) => {
    const key = `${menu.applet.id}:pinned:${heading}`
    const { shown: rows, hidden } = list(key, pinned.map(({ entry }) => entry))
    return [
      ...rows.map((entry) => {
        const owner = pinned.find((item) => item.entry === entry)!.section
        const Icon = entry.icon ?? owner.icon
        return (
          <TreeItem key={entry.href} value={entry.href} label={typeof entry.label === "string" ? entry.label : undefined} className={focusRow}>
            <TreeItemLabel render={<NavLink {...target(owner, entry.href)} onClick={onFollow} />} className={row}>
              <Icon aria-hidden strokeWidth={1.6} /><span className="truncate">{entry.label}</span>
            </TreeItemLabel>
          </TreeItem>
        )
      }),
      more(key, hidden, pinned.length, false),
    ]
  }
  return (
    <Tree aria-labelledby={labelledBy} mode="none" expandOnPress current={current} expanded={expanded}
      onExpandedChange={(next) => state.onChosenChange((previous) => diffExpanded(expanded, next, previous))}
      onAction={(value) => { if (value.startsWith("more:")) state.onWhole(value.slice("more:".length)) }}>
      {menu.headings.map(({ heading, destinations, pinned }) => {
        const items = [
          ...destinations.map((destination) => <Destination key={destination.section.path} destination={destination} applet={menu.applet} unread={unread} list={list} more={more} onFollow={onFollow} />),
          ...pinnedRows(heading, pinned),
        ]
        if (heading === "") return items
        return (
          <TreeItem key={heading} value={`space:${heading}`} label={heading} className={cn("mt-[12px] first:mt-0", focusRow)}>
            <TreeItemLabel className={groupHeading}>
              <span className="truncate">{heading}</span>
              <TreeItemIndicator className={chevron}><ChevronRight aria-hidden /></TreeItemIndicator>
            </TreeItemLabel>
            <TreeGroup>{items}</TreeGroup>
          </TreeItem>
        )
      })}
    </Tree>
  )
}

/** One destination in the menu: its row (a link, or a plain heading when it has no page), then its items. */
function Destination({ destination, applet, unread, list, more, onFollow }: {
  readonly destination: MenuDestination; readonly applet: Applet; readonly unread: ReadonlySet<string>
  readonly list: (key: string, items: readonly MenuEntry[]) => { readonly shown: readonly MenuEntry[]; readonly hidden: number }
  readonly more: (key: string, hidden: number, total: number, indent: boolean) => ReactNode
  readonly onFollow: () => void
}) {
  const { section, entries, badge } = destination
  const headings = [...new Set(entries.map(({ group }) => group ?? ""))]
  // Items indent to the label column and carry no icon: under one destination they are all one kind.
  const rows = (heading: string, members: readonly MenuEntry[]) => {
    const key = `${applet.id}:${section.path}:${heading}`
    const { shown: visible, hidden } = list(key, members)
    return [
      ...visible.map((entry) => {
        const count = shown(entry.count)
        return (
          <TreeItem key={entry.href} value={entry.href} label={typeof entry.label === "string" ? entry.label : undefined} className={focusRow}>
            <TreeItemLabel render={<NavLink {...target(section, entry.href)} onClick={onFollow} />} className={cn(row, "ps-[32px]")}>
              <span className="truncate">{entry.label}</span>
              {count && <span className="ms-auto text-meta tabular-nums">{count}</span>}
            </TreeItemLabel>
          </TreeItem>
        )
      }),
      more(key, hidden, members.length, true),
    ]
  }
  const children = headings.map((heading) => {
    const members = entries.filter((entry) => (entry.group ?? "") === heading)
    if (heading === "") return rows(heading, members)
    return (
      <TreeItem key={heading} value={`group:${section.path}:${heading}`} label={heading} className={focusRow}>
        <TreeItemLabel className={cn(groupHeading, "ps-[32px]")}>
          <span className="truncate">{heading}</span>
          <span className="tabular-nums">{members.length}</span>
          <TreeItemIndicator className={chevron}><ChevronRight aria-hidden /></TreeItemIndicator>
        </TreeItemLabel>
        <TreeGroup>{rows(heading, members)}</TreeGroup>
      </TreeItem>
    )
  })
  const Icon = section.icon
  // A destination with no page lists only its items; once they are read and there are none, it says so.
  const none = section.index === false && entries.length === 0 && !unread.has(section.path)
  const head = <>
    <Icon aria-hidden strokeWidth={1.6} /><span className="truncate">{section.label}</span>
    {badge && <span data-count className="ms-auto text-meta tabular-nums">{badge}</span>}
    {none && <span data-empty className="ms-auto text-meta">None yet</span>}
  </>
  return (
    <TreeItem value={`dest:${section.path}`} data-section-path={section.path} label={section.label} className={focusRow}>
      {section.index === false
        ? <TreeItemLabel className={cn(row, "cursor-default")}>{head}</TreeItemLabel>
        : <TreeItemLabel render={<NavLink to={section.path} onClick={onFollow} />} className={row}>{head}</TreeItemLabel>}
      {entries.length > 0 && <TreeGroup>{children}</TreeGroup>}
    </TreeItem>
  )
}
