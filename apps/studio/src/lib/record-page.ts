import type { ObjectVersion } from "../evidence/review.ts"
import type { Concept, Field } from "./catalog.ts"
import { conceptTitle } from "./catalog.ts"
import type { LinkPage, Schemas } from "./transport.ts"

/** Facts a record's title already shows; never repeated as key facts. */
const TITLE_FIELDS = new Set(["id", "label", "display_name"])
const KEY_FACTS = 6

/**
 * The record's key facts: the first facts its definition declares, in its presentation order,
 * that read as one value (no nested records or JSON), at most six. The order is the author's.
 */
export function keyFacts(concept: Pick<Concept, "fields">): Field[] {
  return concept.fields.filter((field) => !TITLE_FIELDS.has(field.id) && field.scalar.kind !== "record" && field.scalar.kind !== "json-ext").slice(0, KEY_FACTS)
}

/** The server's counts of a record's visible links by type and direction (HOST-READ-4b), as `/links` sends them on a first page. */
export type LinkCounts = Schemas["NornLinkCounts"]

export type CountedLinkPage = LinkPage & { readonly counts?: LinkCounts }

export interface LinkGroup {
  readonly key: string
  readonly direction: "out" | "in"
  readonly link: string
  /** The type at the other end. */
  readonly otherType: string
  readonly label: string
  /** The server's count; `null` when this cell does not count links. */
  readonly count: number | null
  /** The count is a lower bound: the server stopped counting. */
  readonly atLeast: boolean
  /** This group's links on the first page the server returned: the other end's ids. */
  readonly ids: readonly string[]
}

/**
 * A record's links grouped by link type and direction. With the server's counts, one group per
 * counted type (the declared relationships with no link are left out); without them, one group per
 * declared relationship, uncounted. Ids come from the first page only.
 */
export function linkGroups(concept: Pick<Concept, "name" | "relationships">, incoming: readonly { readonly from: Pick<Concept, "name" | "title">; readonly relationship: { readonly id: string } }[], id: string, page: CountedLinkPage): LinkGroup[] {
  // An id under another type is another record: both ends' types must be the group's.
  const ids = (direction: "out" | "in", link: string, sourceType: string, targetType: string) => page.items
    .filter((edge) => edge.link === link && edge.sourceType === sourceType && edge.targetType === targetType && (direction === "out" ? edge.source === id : edge.target === id))
    .map((edge) => direction === "out" ? edge.target : edge.source)
  const words = (link: string) => link.replaceAll("_", " ")
  if (page.counts) {
    const { complete } = page.counts
    return page.counts.groups.map(({ sourceType, link, targetType, direction, count }) => ({
      key: `${direction}/${sourceType}/${link}/${targetType}`,
      direction,
      link,
      otherType: direction === "out" ? targetType : sourceType,
      label: direction === "out" ? `${words(link)} → ${conceptTitle(targetType)}` : `${conceptTitle(sourceType)} records (${words(link)})`,
      count,
      atLeast: !complete,
      ids: ids(direction, link, sourceType, targetType),
    }))
  }
  return [
    ...concept.relationships.map(({ id: link, target }): LinkGroup => ({ key: `out/${concept.name}/${link}/${target}`, direction: "out", link, otherType: target, label: `${words(link)} → ${conceptTitle(target)}`, count: null, atLeast: false, ids: ids("out", link, concept.name, target) })),
    ...incoming.map(({ from, relationship }): LinkGroup => ({ key: `in/${from.name}/${relationship.id}/${concept.name}`, direction: "in", link: relationship.id, otherType: from.name, label: `${from.title} records (${words(relationship.id)})`, count: null, atLeast: false, ids: ids("in", relationship.id, from.name, concept.name) })),
  ]
}

/** A count as the reader sees it: exact, or the server's lower bound ("500+"). */
export const countText = ({ count, atLeast }: Pick<LinkGroup, "count" | "atLeast">): string => count === null ? "" : `${count.toLocaleString("en")}${atLeast ? "+" : ""}`

export type WriterKind = ObjectVersion["writer"]["kind"]

export interface TimelineEntry {
  readonly version: number
  readonly commit: number
  readonly kind: WriterKind
  /** When Norn knew it: the commit's logical time. */
  readonly known: string
  /** When it was true: the version's value of the type's time property, when the cell names one and the version has it. */
  readonly valid: string | null
}

/** The history's versions as timeline entries, in the server's order (newest first). */
export function timeline(versions: readonly ObjectVersion[], timeProperty: string | null): TimelineEntry[] {
  return versions.map(({ version, commit, logicalTime, writer, values }) => {
    const at = timeProperty === null ? undefined : values?.[timeProperty]
    return { version, commit, kind: writer.kind, known: new Date(logicalTime).toISOString(), valid: typeof at === "string" ? at : null }
  })
}

/**
 * Entries ordered by valid time, newest first; entries with no valid time keep their server order
 * after the rest. Knowledge order is the server's own and is never re-sorted.
 */
export function byValidTime(entries: readonly TimelineEntry[]): TimelineEntry[] {
  const timed = entries.filter((entry) => entry.valid !== null)
  const untimed = entries.filter((entry) => entry.valid === null)
  return [...timed.sort((a, b) => Date.parse(b.valid ?? "") - Date.parse(a.valid ?? "") || b.version - a.version), ...untimed]
}

/** How many entries each kind of writer has, over the versions the server listed. */
export function writerCounts(entries: readonly TimelineEntry[]): Readonly<Record<WriterKind, number>> {
  const counts: Record<WriterKind, number> = { mapping: 0, action: 0, other: 0 }
  for (const { kind } of entries) counts[kind] += 1
  return counts
}

/**
 * The one property the cell places this type's records in time by: the time property its metrics
 * declare (`GET /v1/questions:metrics`). `null` when it declares none, or more than one.
 */
export function timePropertyOf(metrics: readonly { readonly objectType: string; readonly timeProperty: string | null }[], objectType: string): string | null {
  const named = new Set(metrics.filter((metric) => metric.objectType === objectType && metric.timeProperty !== null).map(({ timeProperty }) => timeProperty))
  return named.size === 1 ? [...named][0] ?? null : null
}
