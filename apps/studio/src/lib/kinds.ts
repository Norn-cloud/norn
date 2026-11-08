import { ArrowRightLeft, Box, ChartLine, Database, Scale, ShieldCheck, Workflow, type LucideIcon } from "lucide-react"

/** The kinds of Norn artefact Studio shows. Each keeps one colour and one icon everywhere. */
export type Kind = "source" | "mapping" | "objectType" | "question" | "workflow" | "guarantee" | "policy"

export interface KindStyle {
  readonly label: string
  readonly icon: LucideIcon
  /** Text colour for the kind's icon and labels. */
  readonly text: string
  /** A quiet fill behind the kind's chips and tiles. */
  readonly fill: string
  /** Tints an element's child icon, e.g. a menu button's. */
  readonly svg: string
}

// Literal class names, so Tailwind finds them.
export const KINDS: Readonly<Record<Kind, KindStyle>> = {
  source: { label: "Source", icon: Database, text: "text-slate-600 dark:text-slate-400", fill: "bg-slate-500/10", svg: "[&>svg]:text-slate-600 dark:[&>svg]:text-slate-400" },
  mapping: { label: "Mapping", icon: ArrowRightLeft, text: "text-violet-600 dark:text-violet-400", fill: "bg-violet-500/10", svg: "[&>svg]:text-violet-600 dark:[&>svg]:text-violet-400" },
  objectType: { label: "Object type", icon: Box, text: "text-blue-600 dark:text-blue-400", fill: "bg-blue-500/10", svg: "[&>svg]:text-blue-600 dark:[&>svg]:text-blue-400" },
  question: { label: "Question", icon: ChartLine, text: "text-teal-600 dark:text-teal-400", fill: "bg-teal-500/10", svg: "[&>svg]:text-teal-600 dark:[&>svg]:text-teal-400" },
  workflow: { label: "Workflow", icon: Workflow, text: "text-orange-600 dark:text-orange-400", fill: "bg-orange-500/10", svg: "[&>svg]:text-orange-600 dark:[&>svg]:text-orange-400" },
  guarantee: { label: "Guarantee", icon: ShieldCheck, text: "text-green-600 dark:text-green-400", fill: "bg-green-500/10", svg: "[&>svg]:text-green-600 dark:[&>svg]:text-green-400" },
  policy: { label: "Policy", icon: Scale, text: "text-rose-600 dark:text-rose-400", fill: "bg-rose-500/10", svg: "[&>svg]:text-rose-600 dark:[&>svg]:text-rose-400" },
}

/** What a check's result means: held, held with a declared loss, refused or broken, or not known. */
export type Status = "held" | "loss" | "refused" | "unknown"

export const STATUS: Readonly<Record<Status, { readonly label: string; readonly text: string; readonly dot: string }>> = {
  held: { label: "Held", text: "text-green-700 dark:text-green-400", dot: "bg-green-500" },
  loss: { label: "Declared loss", text: "text-amber-700 dark:text-amber-400", dot: "bg-amber-500" },
  refused: { label: "Refused", text: "text-red-700 dark:text-red-400", dot: "bg-red-500" },
  unknown: { label: "Unknown", text: "text-muted-foreground", dot: "bg-zinc-400" },
}
