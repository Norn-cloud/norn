import type { LucideIcon } from "lucide-react"
import type { ReactNode } from "react"
import type { Kind } from "./kinds.ts"
import type { Perspective } from "./perspective.tsx"

/**
 * The navigation model behind the three-island shell (DESIGN.md 9.1): every destination, the applet
 * on the rail it belongs to, and the items it lists. The model is the app's (`main.tsx`); the shell
 * derives the rail, the submenu island, the breadcrumbs and search from it, so no word in the shell
 * names a destination.
 */

/** An applet on the rail: a named family of destinations, opened together in the submenu island. */
export interface Applet {
  /** Unique among applets; also the key of the reader's own choices for it (Show all). */
  readonly id: string
  readonly label: string
  readonly icon: LucideIcon
}

export interface Section {
  readonly path: string
  readonly label: string
  readonly icon: LucideIcon
  /** The lens that lists it, which sets the content's row density; a shared destination (Home, Inbox) is `global`. */
  readonly perspective: Perspective | "global"
  /** The applet it belongs to; absent, the destination is an applet of its own (Home, Inbox). */
  readonly applet?: Applet
  /** The quiet heading it is listed under inside its applet's menu: purely organisational, never a wall. */
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
  /**
   * True when an item carrying `group` is pinned by the reader: into the applet of that name when one
   * exists (under "Pinned <label>"), else under a heading of that name in this destination's applet.
   */
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

/** Every destination's items, by destination path, as the shell read them this render. */
export type Menus = ReadonlyMap<string, readonly MenuEntry[]>

/** Whether a route is the entry's page or a page under it. */
export const within = (href: string, pathname: string) => !href.startsWith("#") && (pathname === href || pathname.startsWith(`${href}/`))

/** The prefix of `pathname` an entry claims: its own route, or the other route it also is; none when it claims neither. */
export const claim = (entry: Pick<MenuEntry, "href" | "also">, pathname: string) =>
  within(entry.href, pathname) ? entry.href : entry.also !== undefined && within(entry.also, pathname) ? entry.also : undefined

/** Where an entry goes: its route, or an anchor on its section's page. */
export const target = (section: Pick<Section, "path">, href: string) =>
  href.startsWith("#") ? { to: section.path, hash: href.slice(1) } : { to: href }

/** The section a route belongs to: its own path, or a record page under it. */
export const sectionOf = (sections: readonly Section[], pathname: string) =>
  sections.find(({ path }) => within(path, pathname)) ?? sections.find(({ paths }) => paths?.some((prefix) => within(prefix, pathname)))

/** The destination a route belongs to: by its own routes, else the destination one of whose items claims it. */
export const destinationOf = (sections: readonly Section[], pathname: string, menus: Menus) =>
  sectionOf(sections, pathname) ?? sections.find(({ path }) => (menus.get(path) ?? []).some((entry) => claim(entry, pathname) !== undefined))

/** A section's applet: its own, or one made of the destination itself. */
export const appletOfSection = (section: Section): Applet => section.applet ?? { id: section.path, label: section.label, icon: section.icon }

/** The applets on the rail, in the order the model first lists them. */
export function appletsOf(sections: readonly Section[]): readonly Applet[] {
  const seen = new Map<string, Applet>()
  for (const section of sections) {
    const applet = appletOfSection(section)
    if (!seen.has(applet.id)) seen.set(applet.id, applet)
  }
  return [...seen.values()]
}

const sameName = (a: string, b: string) => a.trim().toLocaleLowerCase() === b.trim().toLocaleLowerCase()

/**
 * Where one item of a destination is listed: the applet and the heading. A pinned item whose group
 * names an applet goes to that applet under "Pinned <label>"; another pinned group is a heading in
 * the destination's own applet; every other item stays under its destination (`heading` absent).
 */
export function placeOf(section: Section, entry: MenuEntry, applets: readonly Applet[]): { readonly applet: string; readonly heading?: string } {
  const own = appletOfSection(section).id
  if (!section.pinned || !entry.group) return { applet: own }
  const named = applets.find(({ label }) => sameName(label, entry.group!))
  return named ? { applet: named.id, heading: `Pinned ${section.label.toLocaleLowerCase()}` } : { applet: own, heading: entry.group }
}

/** One row of an applet's menu: a destination with the items it lists under itself. */
export interface MenuDestination {
  readonly section: Section
  readonly entries: readonly MenuEntry[]
  readonly badge: string | undefined
}

/** One item pinned into an applet under a heading of its own, with the destination that owns it. */
export interface PinnedEntry {
  readonly section: Section
  readonly entry: MenuEntry
}

/** An applet's menu: its destinations under their headings ("" for none), then pinned items by heading. */
export interface AppletMenu {
  readonly applet: Applet
  readonly headings: readonly { readonly heading: string; readonly destinations: readonly MenuDestination[]; readonly pinned: readonly PinnedEntry[] }[]
}

/**
 * The menu the submenu island lists for one applet, from the model and this render's menus and
 * badges: its destinations under their headings in the model's order, then the headings that hold
 * only items pinned into it.
 */
export function appletMenu(applet: Applet, sections: readonly Section[], menus: Menus, badges: ReadonlyMap<string, string | undefined>): AppletMenu {
  const applets = appletsOf(sections)
  const destinations = new Map<string, MenuDestination[]>()
  const pinned = new Map<string, PinnedEntry[]>()
  const push = <T,>(into: Map<string, T[]>, heading: string, value: T) => { into.set(heading, [...(into.get(heading) ?? []), value]) }
  for (const section of sections) {
    const own = appletOfSection(section).id === applet.id
    const stays: MenuEntry[] = []
    for (const entry of menus.get(section.path) ?? []) {
      const place = placeOf(section, entry, applets)
      if (place.heading === undefined) { if (own) stays.push(entry) }
      else if (place.applet === applet.id) push(pinned, place.heading, { section, entry })
    }
    if (own) push(destinations, section.group ?? "", { section, entries: stays, badge: badges.get(section.path) })
  }
  const order = [...destinations.keys(), ...[...pinned.keys()].filter((heading) => !destinations.has(heading))]
  return { applet, headings: order.map((heading) => ({ heading, destinations: destinations.get(heading) ?? [], pinned: pinned.get(heading) ?? [] })) }
}

/** Every applet that lists the page, with how strongly: the length of the item route that claims it, else 0 for its destination's own routes. */
function listings(sections: readonly Section[], pathname: string, menus: Menus): readonly { readonly applet: string; readonly strength: number; readonly order: number }[] {
  const applets = appletsOf(sections)
  const found: { applet: string; strength: number; order: number }[] = []
  sections.forEach((section, order) => {
    for (const entry of menus.get(section.path) ?? []) {
      const at = claim(entry, pathname)
      // The item that is the page itself beats one that only also covers it (a type's page vs its records).
      if (at !== undefined) found.push({ applet: placeOf(section, entry, applets).applet, strength: at.length + (pathname === entry.href ? 0.5 : 0), order })
    }
    if (within(section.path, pathname) || section.paths?.some((prefix) => within(prefix, pathname))) {
      found.push({ applet: appletOfSection(section).id, strength: 0, order })
    }
  })
  return found
}

/**
 * The applet a page is shown in. A page more than one applet lists (a type's record page, which the
 * records and the model both list) stays in the applet the reader came from; else the applet whose
 * item claims the longest route (an item that is the page itself first), then the model's order. A
 * page no applet lists has none.
 */
export function appletOf(sections: readonly Section[], pathname: string, menus: Menus, previous?: string): Applet | undefined {
  const found = listings(sections, pathname, menus)
  if (found.length === 0) return undefined
  const applets = appletsOf(sections)
  const keep = previous !== undefined && found.some(({ applet }) => applet === previous) ? previous : undefined
  const best = keep ?? [...found].sort((a, b) => b.strength - a.strength || a.order - b.order)[0]!.applet
  return applets.find(({ id }) => id === best)
}

/** The current row in an applet's menu: the item that claims the longest prefix of the page, else its destination. */
export function currentIn(menu: AppletMenu, pathname: string): string | null {
  const items = menu.headings.flatMap(({ destinations, pinned }) => [
    ...destinations.flatMap(({ entries }) => entries),
    ...pinned.map(({ entry }) => entry),
  ])
  const claimed = items.map((entry) => ({ entry, at: claim(entry, pathname) })).filter(({ at }) => at !== undefined)
    .sort((a, b) => (b.at?.length ?? 0) - (a.at?.length ?? 0))[0]
  if (claimed) return claimed.entry.href
  const sections = menu.headings.flatMap(({ destinations }) => destinations.map(({ section }) => section))
  const section = sectionOf(sections, pathname)
  return section ? `dest:${section.path}` : null
}

/** The destination of the current page inside an applet, for the breadcrumbs and the row density. */
export function destinationIn(menu: AppletMenu, pathname: string, menus: Menus): Section | undefined {
  const sections = menu.headings.flatMap(({ destinations }) => destinations.map(({ section }) => section))
  const pinned = menu.headings.flatMap(({ pinned }) => pinned).find(({ entry }) => claim(entry, pathname) !== undefined)
  return destinationOf(sections, pathname, menus) ?? pinned?.section
}

/**
 * Where an applet opens: the first of its destinations with a page of its own, in the model's order;
 * a destination with no page (Records) opens its first item that is a route. None when nothing it
 * lists is a page yet (an applet whose only destination has not read its items).
 */
export function appletHome(applet: Applet, sections: readonly Section[], menus: Menus): string | undefined {
  for (const section of sections) {
    if (appletOfSection(section).id !== applet.id) continue
    if (section.index !== false) return section.path
    const first = (menus.get(section.path) ?? []).find(({ href }) => !href.startsWith("#"))
    if (first) return first.href
  }
  return undefined
}

/** How many items a list shows before "Show all": enough for a screen, so a long list never pushes the rest below the fold. */
export const SHOW_FIRST = 8

/**
 * The items a list shows: all of them when the reader asked for all (or there are few), else the
 * first `SHOW_FIRST`, and the current item too when it falls past them, so it is never hidden.
 */
export function visibleItems<T extends Pick<MenuEntry, "href">>(items: readonly T[], all: boolean, current: string | null): { readonly shown: readonly T[]; readonly hidden: number } {
  if (all || items.length <= SHOW_FIRST + 1) return { shown: items, hidden: 0 }
  const head = items.slice(0, SHOW_FIRST)
  const extra = items.slice(SHOW_FIRST).find(({ href }) => href === current)
  const shown = extra ? [...head, extra] : head
  return { shown, hidden: items.length - shown.length }
}

/** Whether a row's text matches the reader's filter (case- and accent-insensitive); an empty filter matches all. */
export function matches(text: ReactNode, query: string): boolean {
  const needle = fold(query)
  if (needle === "") return true
  return typeof text === "string" && fold(text).includes(needle)
}

const fold = (text: string) => text.normalize("NFKD").replace(/\p{M}/gu, "").trim().toLocaleLowerCase()

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

/** A path segment as text; a malformed escape (e.g. `%E0%A4%A`) is shown as written instead of failing the page. */
export function decoded(segment: string): string {
  try {
    return decodeURIComponent(segment)
  } catch {
    return segment
  }
}

export const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((part) => part[0]?.toUpperCase()).join("") || "?"

/** A count as shown: "99+" past 99, the server's text (e.g. "500+") as sent, nothing for zero. */
export const shown = (count: number | string | undefined) =>
  count === undefined || count === 0 || count === "" || count === "0" ? undefined : typeof count === "string" ? count : count > 99 ? "99+" : String(count)

/** A destination's accessible name: its label, then its state when it has a count ("Inbox, 3 open"). */
export const named = (label: string, count: string | undefined) => count ? `${label}, ${count} open` : label
