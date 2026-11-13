import { ArrowRightLeft, Blend, Box, ChartLine, Database, Scale, ShieldCheck, Workflow, type LucideIcon } from "lucide-react"

/**
 * The kinds of Norn artefact Studio shows. Each keeps one icon and one hue everywhere: the hue marks its chips,
 * tiles and inline icons (a detail on a monochrome surface); in the navigation the icons stay neutral grey.
 */
export type Kind = "source" | "mapping" | "objectType" | "transform" | "question" | "workflow" | "guarantee" | "policy"

export interface KindStyle {
  readonly label: string
  readonly icon: LucideIcon
  /** Text colour for the kind's icon and labels in chips and tiles: its hue. */
  readonly text: string
  /** A quiet fill behind the kind's chips and tiles. */
  readonly fill: string
  /** Tints an element's child icon, e.g. a menu button's. */
  readonly svg: string
}

// Literal class names, so Tailwind finds them.
export const KINDS: Readonly<Record<Kind, KindStyle>> = {
  source: { label: "Source", icon: Database, text: "text-kind-source", fill: "bg-kind-source/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
  mapping: { label: "Mapping", icon: ArrowRightLeft, text: "text-kind-mapping", fill: "bg-kind-mapping/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
  objectType: { label: "Object type", icon: Box, text: "text-kind-object", fill: "bg-kind-object/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
  transform: { label: "Transform", icon: Blend, text: "text-kind-transform", fill: "bg-kind-transform/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
  question: { label: "Question", icon: ChartLine, text: "text-kind-question", fill: "bg-kind-question/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
  workflow: { label: "Workflow", icon: Workflow, text: "text-kind-workflow", fill: "bg-kind-workflow/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
  guarantee: { label: "Guarantee", icon: ShieldCheck, text: "text-kind-guarantee", fill: "bg-kind-guarantee/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
  policy: { label: "Policy", icon: Scale, text: "text-kind-policy", fill: "bg-kind-policy/10", svg: "[&>svg]:text-muted-foreground hover:[&>svg]:text-foreground data-[active=true]:[&>svg]:text-foreground" },
}

/** What a check's result means: held, held with a declared loss, refused or broken, or not known. */
export type Status = "held" | "loss" | "refused" | "unknown"

export const STATUS: Readonly<Record<Status, { readonly label: string; readonly text: string; readonly dot: string }>> = {
  held: { label: "Held", text: "text-success", dot: "bg-success" },
  loss: { label: "Declared loss", text: "text-warning", dot: "bg-warning" },
  refused: { label: "Refused", text: "text-destructive", dot: "bg-destructive" },
  unknown: { label: "Unknown", text: "text-muted-foreground", dot: "bg-muted-foreground" },
}
