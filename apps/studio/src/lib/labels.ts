import type { Schemas } from "../../../../packages/norn-sdk/src/index.ts"
import type { Artifact } from "./transport.ts"

type Target = Schemas["NornIrLabelTarget"]

/** A label the reader sees, with the tag it is written in (for `lang`) and whether it awaits review. */
export interface Label {
  readonly text: string
  readonly description: string | undefined
  readonly tag: string
  readonly needsReview: boolean
}

/** The stable key of a label target, the same as `norn_ir::LabelTarget::key`. */
export function targetKey(target: Target): string {
  switch (target.kind) {
    case "objectType": return `objectType:${target.objectType}`
    case "property": return `property:${target.objectType}.${target.property}`
    case "enumValue": return `enumValue:${target.objectType}.${target.property}=${target.value}`
    case "action": return `action:${target.actionId}`
    case "rule": return `rule:${target.ruleId}`
    case "law": return `law:${target.lawId}`
    case "page": return `page:${target.widgetId}/${target.pageId}`
  }
}

/** Target key → BCP 47 tag → label. */
export type LabelIndex = ReadonlyMap<string, ReadonlyMap<string, Label>>

/**
 * Layer every `labels` artifact in artifact order (the installed base comes
 * first, as the server lists it): for one target and tag, compared ignoring
 * case, the later catalog wins; no tag is dropped (`norn_ir::labels::resolve`).
 */
export function labelIndex(artifacts: readonly Artifact[]): LabelIndex {
  const index = new Map<string, Map<string, Label>>()
  for (const artifact of artifacts) {
    if (artifact.kind !== "labels") continue
    for (const entry of artifact.payload.entries) {
      const key = targetKey(entry.target)
      const tags = index.get(key) ?? new Map<string, Label>()
      for (const [tag, text] of Object.entries(entry.label)) {
        for (const existing of [...tags.keys()]) if (existing.toLowerCase() === tag.toLowerCase()) tags.delete(existing)
        const description = Object.entries(entry.description ?? {}).find(([key]) => key.toLowerCase() === tag.toLowerCase())?.[1]
        const review = Object.entries(entry.review ?? {}).find(([key]) => key.toLowerCase() === tag.toLowerCase())?.[1]
        tags.set(tag, { text, description, tag, needsReview: review === "needs_review" })
      }
      index.set(key, tags)
    }
  }
  return index
}

/**
 * The label for `locale`: its exact tag first, then a regional variant of the
 * same language (`ar-EG` for `ar`). Nothing else: a missing label returns
 * undefined, and the caller shows the canonical name, marked as such.
 */
export function pick(index: LabelIndex, key: string, locale: string): Label | undefined {
  const tags = index.get(key)
  if (!tags) return undefined
  const wanted = locale.toLowerCase()
  const all = [...tags.values()]
  return all.find(({ tag }) => tag.toLowerCase() === wanted)
    ?? all.filter(({ tag }) => tag.toLowerCase().split("-")[0] === wanted).sort((a, b) => a.tag.localeCompare(b.tag))[0]
}
