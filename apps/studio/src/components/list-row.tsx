import { useRender } from "@base-ui/react/use-render"
import type { LucideIcon } from "lucide-react"
import type { ComponentProps, ReactNode } from "react"
import { cn } from "@/lib/utils"

/** The six avatar pairs (DESIGN.md 4.4), as whole class names so Tailwind keeps them. */
const AVATAR = [
  "bg-avatar-1-bg text-avatar-1-fg", "bg-avatar-2-bg text-avatar-2-fg", "bg-avatar-3-bg text-avatar-3-fg",
  "bg-avatar-4-bg text-avatar-4-fg", "bg-avatar-5-bg text-avatar-5-fg", "bg-avatar-6-bg text-avatar-6-fg",
] as const

/**
 * The avatar pair for an id: a stable FNV-1a hash of the principal's or record's **id**, never its
 * display name, so a rename or a switch between Arabic and English keeps the colour.
 */
export function avatarIndex(id: string): number {
  let hash = 0x811c9dc5
  for (let at = 0; at < id.length; at++) {
    hash ^= id.charCodeAt(at)
    hash = Math.imul(hash, 0x01000193) >>> 0
  }
  return hash % AVATAR.length
}

/** The one or two letters a disc shows: the first letters of the first two words, else the first two letters. */
export function initialsOf(name: string): string {
  const words = name.trim().split(/\s+/u).filter(Boolean)
  const letters = words.length >= 2 ? [...words[0]!].slice(0, 1).concat([...words[1]!].slice(0, 1)) : [...(words[0] ?? "")].slice(0, 2)
  return letters.join("").toUpperCase()
}

const DISC = { 24: "size-[24px] text-meta", 28: "size-[28px] text-meta", 36: "size-[36px] text-body", 48: "size-[48px] text-heading" } as const

/** A round initials disc in the avatar pair hashed from `id` (DESIGN.md 10, Initials). Decorative: the name sits beside it. */
export function Initials({ id, name, size = 28, className }: { readonly id: string; readonly name: string; readonly size?: keyof typeof DISC; readonly className?: string }) {
  return (
    <span aria-hidden data-slot="initials" data-avatar={avatarIndex(id) + 1}
      className={cn("grid shrink-0 place-items-center rounded-full leading-none select-none", DISC[size], AVATAR[avatarIndex(id)], className)}>
      {initialsOf(name)}
    </span>
  )
}

/** A neutral round disc for a kind of thing (DESIGN.md 10, TypeDisc): the card, a `--line` ring and a muted icon or glyph. */
export function TypeDisc({ icon: Icon, children, size = 28, className }: { readonly icon?: LucideIcon; readonly children?: ReactNode; readonly size?: keyof typeof DISC; readonly className?: string }) {
  return (
    <span aria-hidden data-slot="type-disc" className={cn("grid shrink-0 place-items-center rounded-full bg-card text-muted-foreground tabular-nums ring-1 ring-line", DISC[size], className)}>
      {Icon ? <Icon className={size >= 36 ? "size-[18px]" : "size-[15px]"} strokeWidth={1.7} /> : children}
    </span>
  )
}

/** The well's list (DESIGN.md 7): `p-1.5` around rows that stack with no gap. Each child is an `<li>`. */
export function RowList({ className, ...props }: ComponentProps<"ul">) {
  return <ul data-slot="row-list" className={cn("flex flex-col p-1.5", className)} {...props} />
}

/**
 * One row of a list in a well (DESIGN.md 10, ListRow): `h-12` (44px below 640px), the row radius,
 * one lead (a 6px dot, a 28px Initials disc or a 28px TypeDisc), the title at step 2 and 400, then a
 * mono code, a SourceTag, and one end slot (a muted metric or a StatusDot). Hover is the well-hover
 * wash; the selected row is the card with a 1px panel ring. The row's accessible name is its title,
 * so the code, tag and end read after it.
 *
 * A button by default; pass `render={<Link … />}` for a link. `passive` makes it a plain row (no
 * hover, no focus) for a list that only shows facts.
 */
export function ListRow({ lead, title, titleLang, code, tag, description, end, selected = false, passive = false, render, className, ...props }: Omit<ComponentProps<"button">, "title" | "children"> & {
  readonly lead?: ReactNode
  readonly title: ReactNode
  readonly titleLang?: string
  /** A short id or code after the title: mono, step 1, muted, kept left-to-right. */
  readonly code?: ReactNode
  /** A SourceTag (or two at most) after the code. */
  readonly tag?: ReactNode
  /** One quiet line under the title, step 1 and muted. */
  readonly description?: ReactNode
  /** The one end slot: a muted metric or a StatusDot. */
  readonly end?: ReactNode
  readonly selected?: boolean
  readonly passive?: boolean
  readonly render?: useRender.RenderProp
}) {
  const content = (
    <>
      {lead}
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="flex min-w-0 items-center gap-2">
          <span lang={titleLang} className="min-w-0 truncate text-body text-foreground">{title}</span>
          {code !== undefined && code !== null && <bdi className="min-w-0 shrink-[2] truncate font-mono text-meta tracking-[-0.01em] text-muted-foreground">{code}</bdi>}
          {tag}
        </span>
        {description !== undefined && description !== null && <span className="text-meta text-pretty text-muted-foreground">{description}</span>}
      </span>
      {end !== undefined && end !== null && <span data-slot="list-row-end" className="ms-auto flex shrink-0 items-center gap-2 ps-3 text-body text-muted-foreground tabular-nums">{end}</span>}
    </>
  )
  return useRender({
    defaultTagName: passive ? "div" : "button",
    render,
    props: {
      ...(passive || render !== undefined ? {} : { type: "button" as const }),
      ...props,
      "data-slot": "list-row",
      "data-selected": selected || undefined,
      className: cn(
        "flex min-h-12 w-full min-w-0 items-center gap-3 rounded-(--radius-row) px-3 py-1.5 text-start max-sm:min-h-[44px]",
        !passive && "cursor-pointer outline-none transition-colors duration-150 hover:bg-(--well-hover) focus-visible:outline-2 focus-visible:outline-solid focus-visible:-outline-offset-2 focus-visible:outline-ring motion-reduce:transition-none",
        "data-selected:bg-card data-selected:ring-1 data-selected:ring-(--panel-border) data-selected:ring-inset",
        className,
      ),
      children: content,
    },
  })
}
