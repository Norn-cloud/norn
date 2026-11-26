import { decoded, type Menus } from "./nav-model.ts"

/**
 * Browser-local shell view state (DESIGN.md 9.1 & 18):
 * - Submenu branch open/closed choices and "Show all" toggles (`norn.studio.menu`).
 * - Recently opened records, runs and views for the Home & Inbox submenu (`norn.studio.recent:<scope>`).
 *
 * Every read and write tolerates blocked or corrupted `localStorage` without throwing.
 */

export const MENU_STORAGE_KEY = "norn.studio.menu"
export const RECENT_STORAGE_PREFIX = "norn.studio.recent"
export const MAX_RECENT = 8
const MAX_STORED_BRANCHES = 200
const MAX_STORED_WHOLE = 100

export type ChosenBranches = Readonly<Record<string, Readonly<Record<string, boolean>>>>

export interface StoredMenuState {
  readonly chosen: ChosenBranches
  readonly whole: ReadonlySet<string>
}

export type RecentKind = "record" | "run" | "view"

export interface RecentItem {
  readonly href: string
  readonly label: string
  readonly kind: RecentKind
  readonly detail?: string
  readonly visitedAt: number
}

type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">

function defaultStorage(): StorageLike | undefined {
  try {
    return globalThis.localStorage
  } catch {
    return undefined
  }
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)

/** Read persisted submenu branch open/closed choices and "Show all" keys. */
export function readMenuState(storage: StorageLike | undefined = defaultStorage()): StoredMenuState {
  const empty: StoredMenuState = { chosen: {}, whole: new Set() }
  try {
    const raw = storage?.getItem(MENU_STORAGE_KEY)
    if (!raw) return empty
    const parsed: unknown = JSON.parse(raw)
    if (!isRecord(parsed)) return empty

    const chosen: Record<string, Record<string, boolean>> = {}
    let branchCount = 0
    if (isRecord(parsed.chosen)) {
      for (const [appletId, branches] of Object.entries(parsed.chosen)) {
        if (!isRecord(branches)) continue
        const clean: Record<string, boolean> = {}
        for (const [branchId, open] of Object.entries(branches)) {
          if (typeof open !== "boolean") continue
          if (branchCount >= MAX_STORED_BRANCHES) break
          clean[branchId] = open
          branchCount += 1
        }
        if (Object.keys(clean).length > 0) chosen[appletId] = clean
      }
    }

    const whole = new Set<string>()
    if (Array.isArray(parsed.whole)) {
      for (const key of parsed.whole) {
        if (typeof key === "string" && key !== "" && whole.size < MAX_STORED_WHOLE) whole.add(key)
      }
    }

    return { chosen, whole }
  } catch {
    return empty
  }
}

/** Persist submenu branch open/closed choices and "Show all" keys. */
export function writeMenuState(
  chosen: ChosenBranches,
  whole: ReadonlySet<string>,
  storage: StorageLike | undefined = defaultStorage(),
): void {
  try {
    storage?.setItem(MENU_STORAGE_KEY, JSON.stringify({ chosen, whole: [...whole].slice(0, MAX_STORED_WHOLE) }))
  } catch {
    /* storage blocked or full */
  }
}

const recentKey = (scopeKey: string) => `${RECENT_STORAGE_PREFIX}:${scopeKey}`

const isRecentKind = (value: unknown): value is RecentKind =>
  value === "record" || value === "run" || value === "view"

/** Read the per-scope list of recently opened records, runs and views. */
export function readRecent(scopeKey: string, storage: StorageLike | undefined = defaultStorage()): readonly RecentItem[] {
  try {
    const raw = storage?.getItem(recentKey(scopeKey))
    if (!raw) return []
    const parsed: unknown = JSON.parse(raw)
    if (!Array.isArray(parsed)) return []
    const items: RecentItem[] = []
    const seen = new Set<string>()
    for (const entry of parsed) {
      if (!isRecord(entry)) continue
      if (typeof entry.href !== "string" || !entry.href.startsWith("/")) continue
      if (typeof entry.label !== "string" || entry.label === "") continue
      if (!isRecentKind(entry.kind)) continue
      if (seen.has(entry.href)) continue
      seen.add(entry.href)
      const detail = typeof entry.detail === "string" && entry.detail !== "" ? entry.detail : undefined
      const visitedAt = typeof entry.visitedAt === "number" && Number.isFinite(entry.visitedAt) ? entry.visitedAt : 0
      items.push({ href: entry.href, label: entry.label, kind: entry.kind, ...(detail ? { detail } : {}), visitedAt })
      if (items.length >= MAX_RECENT) break
    }
    return items
  } catch {
    return []
  }
}

/** Persist the per-scope recent list. */
export function writeRecent(
  scopeKey: string,
  items: readonly RecentItem[],
  storage: StorageLike | undefined = defaultStorage(),
): void {
  try {
    if (items.length === 0) storage?.removeItem(recentKey(scopeKey))
    else storage?.setItem(recentKey(scopeKey), JSON.stringify(items.slice(0, MAX_RECENT)))
  } catch {
    /* storage blocked or full */
  }
}

/** Clear the per-scope recent list from storage. */
export function clearRecent(scopeKey: string, storage: StorageLike | undefined = defaultStorage()): void {
  try {
    storage?.removeItem(recentKey(scopeKey))
  } catch {
    /* storage blocked */
  }
}

/** Move `next` to the front of `items`, deduplicating by `href` and capping at `MAX_RECENT`. */
export function recordRecent(items: readonly RecentItem[], next: RecentItem): readonly RecentItem[] {
  const rest = items.filter((item) => item.href !== next.href)
  return [next, ...rest].slice(0, MAX_RECENT)
}

/**
 * Derive a recent item when `pathname` opens an individual record (`/sources/$concept/$`),
 * a workflow run (`/runs/$id`), or a saved view (`/views/$id`). Returns `null` for all other routes.
 */
export function recentForPath(pathname: string, menus: Menus, now = Date.now()): RecentItem | null {
  if (pathname.startsWith("/sources/")) {
    const rest = pathname.slice("/sources/".length)
    const slash = rest.indexOf("/")
    if (slash > 0 && slash < rest.length - 1) {
      const concept = rest.slice(0, slash)
      const id = decoded(rest.slice(slash + 1))
      if (id === "") return null
      const typeHref = `/sources/${concept}`
      const sourceEntry = (menus.get("/sources") ?? []).find((entry) => entry.href === typeHref)
        ?? (menus.get("/objects") ?? []).find((entry) => entry.also === typeHref || entry.href === `/objects/${concept}`)
      const detail = typeof sourceEntry?.label === "string" && sourceEntry.label !== "" ? sourceEntry.label : decoded(concept)
      return { href: pathname, label: id, kind: "record", detail, visitedAt: now }
    }
    return null
  }

  if (pathname.startsWith("/runs/")) {
    const id = decoded(pathname.slice("/runs/".length))
    if (id === "") return null
    return { href: pathname, label: id, kind: "run", visitedAt: now }
  }

  if (pathname.startsWith("/views/")) {
    const rest = pathname.slice("/views/".length)
    if (rest === "" || rest === "new" || rest.startsWith("new/")) return null
    const idSegment = rest.endsWith("/edit") ? rest.slice(0, -"/edit".length) : rest
    if (idSegment === "" || idSegment.includes("/")) return null
    const canonicalHref = `/views/${idSegment}`
    const viewEntry = (menus.get("/views") ?? []).find((entry) => entry.href === canonicalHref)
    const label = typeof viewEntry?.label === "string" && viewEntry.label !== "" ? viewEntry.label : decoded(idSegment)
    return { href: canonicalHref, label, kind: "view", visitedAt: now }
  }

  return null
}
