import { decoded, type Menus } from "./nav-model.ts"
import type { Mode } from "./live-transport.ts"
import type { Account } from "./session.ts"

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
const FORBIDDEN_KEYS = new Set(["__proto__", "constructor", "prototype"])

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

export interface RecentScopeMode {
  readonly status: "pending" | "error" | "success"
  readonly data: Mode | undefined
}

export interface RecentTracker {
  readonly pathname: string
  readonly enteredAccountKey: string
  readonly enteredFingerprint: string | null
  readonly hadSettled: boolean
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

/** Reject keys that can collide with or pollute `Object.prototype`. */
export const isSafeRecordKey = (key: string): boolean => key !== "" && !FORBIDDEN_KEYS.has(key)

function nullRecord<T>(): Record<string, T> {
  return Object.create(null) as Record<string, T>
}

/** Own-property lookup that ignores inherited `Object.prototype` keys. */
export function getOwn<T>(record: Readonly<Record<string, T>> | undefined, key: string): T | undefined {
  if (record === undefined || !isSafeRecordKey(key)) return undefined
  return Object.hasOwn(record, key) ? record[key] : undefined
}

/** Immutably update one applet's chosen-branch map using null-prototype dictionaries and safe keys only. */
export function updateChosenBranches(
  previous: ChosenBranches,
  appletId: string,
  update: (branches: Readonly<Record<string, boolean>>) => Readonly<Record<string, boolean>>,
): ChosenBranches {
  if (!isSafeRecordKey(appletId)) return previous
  const nextChosen = nullRecord<Readonly<Record<string, boolean>>>()
  for (const [key, value] of Object.entries(previous)) {
    if (isSafeRecordKey(key)) nextChosen[key] = value
  }
  const current = getOwn(previous, appletId) ?? nullRecord<boolean>()
  const updated = update(current)
  const clean = nullRecord<boolean>()
  for (const [branchId, open] of Object.entries(updated)) {
    if (isSafeRecordKey(branchId) && typeof open === "boolean") clean[branchId] = open
  }
  nextChosen[appletId] = clean
  return nextChosen
}

/** Read persisted submenu branch open/closed choices and "Show all" keys. */
export function readMenuState(storage: StorageLike | undefined = defaultStorage()): StoredMenuState {
  const empty: StoredMenuState = { chosen: nullRecord(), whole: new Set() }
  try {
    const raw = storage?.getItem(MENU_STORAGE_KEY)
    if (!raw) return empty
    const parsed: unknown = JSON.parse(raw)
    if (!isRecord(parsed)) return empty

    const chosen = nullRecord<Record<string, boolean>>()
    let branchCount = 0
    if (isRecord(parsed.chosen)) {
      for (const [appletId, branches] of Object.entries(parsed.chosen)) {
        if (!isSafeRecordKey(appletId) || !isRecord(branches)) continue
        const clean = nullRecord<boolean>()
        for (const [branchId, open] of Object.entries(branches)) {
          if (!isSafeRecordKey(branchId) || typeof open !== "boolean") continue
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

/** The `localStorage` key for one scope fingerprint's recent list. */
export const recentStorageKey = (scopeKey: string): string => `${RECENT_STORAGE_PREFIX}:${scopeKey}`

/**
 * Stable identity tuple for the currently signed-in account, organization, environment, and role.
 * Used to detect workspace or role changes even before the mode probe settles.
 */
export const accountScopeIdentity = (account: Account): string =>
  JSON.stringify([
    account.user.id,
    account.organization?.id ?? null,
    account.environmentId ?? null,
    account.organization?.role ?? null,
  ])

/**
 * Collision-free fingerprint binding browser-local Recent items to the confirmed live or recorded
 * workspace scope and role. Returns `null` while mode is pending/failed or while `account` and
 * `mode.data.scope` disagree (mid-switch or explicit `environmentId: null`), failing closed so
 * nothing is read or written across scopes.
 */
export function recentScopeFingerprint(account: Account, mode: RecentScopeMode): string | null {
  if (mode.status !== "success" || mode.data === undefined) return null
  if (mode.data.kind === "live") {
    if (account.organization === null || account.organization.id !== mode.data.scope.orgId) return null
    if (account.environmentId === null) return null
    if (
      account.environmentId !== undefined
      && account.environmentId !== mode.data.scope.environmentId
    ) return null
    return JSON.stringify([
      "live",
      account.user.id,
      mode.data.scope.orgId,
      mode.data.scope.environmentId,
      account.organization.role,
    ])
  }
  return JSON.stringify([
    "recorded",
    account.user.id,
    account.organization?.id ?? "",
    account.environmentId ?? "",
    account.organization?.role ?? "",
  ])
}

/** Initial navigation-bound tracker state for {@link stepRecentTracker}. */
export const initialRecentTracker = (
  pathname: string,
  fingerprint: string | null,
  accountKey = "",
): RecentTracker => ({
  pathname,
  enteredAccountKey: accountKey,
  enteredFingerprint: fingerprint,
  hadSettled: fingerprint !== null,
})

/**
 * Decide whether the current `(pathname, fingerprint, accountKey)` may record a Recent item.
 * A pathname opened before a workspace switch or role change (`enteredAccountKey !== accountKey`
 * or `enteredFingerprint !== fingerprint`, even before the first mode settles or across a
 * transient `null` while the new scope loads) is never recorded under the new scope.
 */
export function stepRecentTracker(
  tracker: RecentTracker,
  pathname: string,
  fingerprint: string | null,
  accountKey = tracker.enteredAccountKey,
): { readonly next: RecentTracker; readonly shouldRecord: boolean } {
  if (pathname !== tracker.pathname) {
    const next: RecentTracker = {
      pathname,
      enteredAccountKey: accountKey,
      enteredFingerprint: fingerprint,
      hadSettled: tracker.hadSettled || fingerprint !== null,
    }
    return { next, shouldRecord: fingerprint !== null }
  }
  if (accountKey !== tracker.enteredAccountKey) {
    return {
      next: { ...tracker, hadSettled: true },
      shouldRecord: false,
    }
  }
  if (fingerprint === null) {
    return { next: tracker, shouldRecord: false }
  }
  if (tracker.enteredFingerprint === null && !tracker.hadSettled) {
    return {
      next: { pathname, enteredAccountKey: accountKey, enteredFingerprint: fingerprint, hadSettled: true },
      shouldRecord: true,
    }
  }
  if (tracker.enteredFingerprint !== fingerprint) {
    return { next: tracker, shouldRecord: false }
  }
  return { next: tracker, shouldRecord: true }
}

const isRecentKind = (value: unknown): value is RecentKind =>
  value === "record" || value === "run" || value === "view"

/** Read the per-scope list of recently opened records, runs and views. */
export function readRecent(scopeKey: string, storage: StorageLike | undefined = defaultStorage()): readonly RecentItem[] {
  try {
    const raw = storage?.getItem(recentStorageKey(scopeKey))
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
    if (items.length === 0) storage?.removeItem(recentStorageKey(scopeKey))
    else storage?.setItem(recentStorageKey(scopeKey), JSON.stringify(items.slice(0, MAX_RECENT)))
  } catch {
    /* storage blocked or full */
  }
}

/** Clear the per-scope recent list from storage. */
export function clearRecent(scopeKey: string, storage: StorageLike | undefined = defaultStorage()): void {
  try {
    storage?.removeItem(recentStorageKey(scopeKey))
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
 * Re-read the latest stored list right before merging and persisting `next`, so a tab with stale
 * in-memory state never resurrects items another tab cleared.
 */
export function appendRecent(
  scopeKey: string,
  next: RecentItem,
  storage: StorageLike | undefined = defaultStorage(),
): readonly RecentItem[] {
  const merged = recordRecent(readRecent(scopeKey, storage), next)
  writeRecent(scopeKey, merged, storage)
  return merged
}

/**
 * Refresh the label/detail of an already-recorded visit in place, ONLY if that `href` is still
 * present in fresh storage. If another tab (or this tab) cleared Recent meanwhile, this is a
 * no-op so late-arriving menu labels never resurrect a cleared list without navigation.
 */
export function updateRecentLabelIfPresent(
  scopeKey: string,
  next: Pick<RecentItem, "href" | "label" | "detail">,
  storage: StorageLike | undefined = defaultStorage(),
): readonly RecentItem[] {
  const current = readRecent(scopeKey, storage)
  const index = current.findIndex((item) => item.href === next.href)
  if (index === -1) return current
  const existing = current[index]!
  if (existing.label === next.label && existing.detail === next.detail) return current
  const updated: RecentItem = {
    ...existing,
    label: next.label,
    ...(next.detail !== undefined ? { detail: next.detail } : {}),
  }
  if (next.detail === undefined) delete (updated as { detail?: string }).detail
  const merged = current.map((item, i) => (i === index ? updated : item))
  writeRecent(scopeKey, merged, storage)
  return merged
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
