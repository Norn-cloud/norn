/**
 * Localized labels (LABEL-LOCALE-1, `crates/norn-ir/src/labels.rs`).
 *
 * A definition carries its labels inline (`label`, `localizedDescription`,
 * `propertyLabels`, `valueLabels`, `needsReview`), and the lowering emits
 * them as a separate `labels` artifact, `<owner artifact id>.labels`. The
 * definition's own artifact never contains them, so localizing never changes
 * its hash, the cell model hash or any evidence identity. The Rust checker is
 * the authority on tags, blank text, bidi controls and unknown targets.
 */
import type { Json } from "../canonical.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

/** BCP 47 language tag → text (`{ en: "Invoice", ar: "فاتورة" }`). */
export type LocaleText = Readonly<Record<string, string>>

/** The canonical, language-neutral definition a label describes. */
export type LabelTarget =
  | { readonly kind: "objectType"; readonly objectType: string }
  | { readonly kind: "property"; readonly objectType: string; readonly property: string }
  | { readonly kind: "enumValue"; readonly objectType: string; readonly property: string; readonly value: string }
  | { readonly kind: "action"; readonly actionId: string }
  | { readonly kind: "rule"; readonly ruleId: string }
  | { readonly kind: "law"; readonly lawId: string }
  | { readonly kind: "page"; readonly widgetId: string; readonly pageId: string }

export interface LabelEntry {
  readonly target: LabelTarget
  readonly label: LocaleText
  readonly description?: LocaleText
  readonly review?: Readonly<Record<string, "needs_review" | "reviewed">>
}

/** Labels any definition may carry. */
export interface Localized {
  /** Short name per locale. */
  readonly label?: LocaleText
  /** Longer description per locale; the plain `description` stays the canonical text. */
  readonly localizedDescription?: LocaleText
  /** Locales whose labels were proposed (for example machine-translated) and still need review. */
  readonly needsReview?: ReadonlyArray<string>
}

/** Labels an object type or interface may carry for its members. */
export interface MemberLabels {
  /** Property id → its label per locale. */
  readonly propertyLabels?: Readonly<Record<string, LocaleText>>
  /** Enum property id → stored value → its label per locale. */
  readonly valueLabels?: Readonly<Record<string, Readonly<Record<string, LocaleText>>>>
}

const entry = (target: LabelTarget, label: LocaleText, description: LocaleText | undefined, needsReview: ReadonlyArray<string>): LabelEntry => {
  const review = Object.fromEntries(
    needsReview
      .filter((tag) => Object.keys(label).some((key) => key.toLowerCase() === tag.toLowerCase()))
      .map((tag) => [tag, "needs_review" as const]),
  )
  return {
    target,
    label,
    ...(description === undefined ? {} : { description }),
    ...(Object.keys(review).length === 0 ? {} : { review }),
  }
}

/** The entries a definition declares for its own target and, for a type, its members. */
export function labelEntries(own: LabelTarget, def: Localized & MemberLabels): ReadonlyArray<LabelEntry> {
  const needsReview = def.needsReview ?? []
  const entries: LabelEntry[] = []
  if (def.label !== undefined) entries.push(entry(own, def.label, def.localizedDescription, needsReview))
  if (own.kind === "objectType") {
    for (const [property, label] of Object.entries(def.propertyLabels ?? {})) {
      entries.push(entry({ kind: "property", objectType: own.objectType, property }, label, undefined, needsReview))
    }
    for (const [property, values] of Object.entries(def.valueLabels ?? {})) {
      for (const [value, label] of Object.entries(values)) {
        entries.push(entry({ kind: "enumValue", objectType: own.objectType, property, value }, label, undefined, needsReview))
      }
    }
  }
  return entries
}

/** The owner's envelope identity the labels artifact is named after. */
export interface LabelOwner {
  readonly artifactId: string
  readonly namespace: string
  readonly name: string
  readonly version: string
}

/** Lower a definition's labels to `<owner>.labels`, or nothing when it declares none. */
export function lowerLabels(owner: LabelOwner, entries: ReadonlyArray<LabelEntry>, options: EnvelopeOptions): Json | undefined {
  if (entries.length === 0) return undefined
  return envelope(
    { kind: "labels", artifactId: `${owner.artifactId}.labels`, namespace: owner.namespace, name: `${owner.name} labels`, version: owner.version },
    { labelsVersion: "0.1", entries: entries as unknown as Json },
    options,
  )
}

/** One type's labels in a standalone catalog: its own, and its properties' and enum values'. */
export interface TypeLabels extends Localized {
  /** Property id → label. */
  readonly properties?: Readonly<Record<string, LocaleText>>
  /** Enum property id → stored value → label. */
  readonly values?: Readonly<Record<string, Readonly<Record<string, LocaleText>>>>
}

/**
 * A standalone labels catalog (`Norn.labels`): a tenant or locale pack labels
 * definitions another layer owns, without touching them. Its labels replace
 * the installed ones for the same target and locale (LABEL-LOCALE-1).
 */
export interface LabelsDef {
  readonly _tag: "Labels"
  /** The artifact id (`locale.ar_eg.labels`). */
  readonly id: string
  readonly title: string
  readonly version: string
  /** Locales this layer serves; definitions with no label in one are listed (`labels.lint.missing_label`). */
  readonly requiredLocales?: ReadonlyArray<string>
  /** Locales whose labels were proposed and still need review, for every entry below. */
  readonly needsReview?: ReadonlyArray<string>
  /** Object type or interface name → labels. */
  readonly types?: Readonly<Record<string, TypeLabels>>
  /** Action id → labels. */
  readonly actions?: Readonly<Record<string, Localized>>
}

export function catalog(meta: Omit<LabelsDef, "_tag">): LabelsDef {
  return { _tag: "Labels", ...meta }
}

/** Lower a standalone catalog to its `labels` artifact. */
export function lowerCatalog(def: LabelsDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  const review = def.needsReview ?? []
  const entries = [
    ...Object.entries(def.types ?? {}).flatMap(([objectType, labels]) => labelEntries(
      { kind: "objectType", objectType },
      { ...labels, propertyLabels: labels.properties, valueLabels: labels.values, needsReview: labels.needsReview ?? review },
    )),
    ...Object.entries(def.actions ?? {}).flatMap(([actionId, labels]) => labelEntries(
      { kind: "action", actionId },
      { ...labels, needsReview: labels.needsReview ?? review },
    )),
  ]
  return {
    ok: true,
    artifact: envelope(
      { kind: "labels", artifactId: def.id, namespace: def.id.split(".")[0]!, name: def.title, version: def.version },
      { labelsVersion: "0.1", entries: entries as unknown as Json, ...(def.requiredLocales?.length ? { requiredLocales: [...def.requiredLocales] } : {}) },
      options,
    ),
  }
}
