import { useId } from "react"
import { cn } from "@/lib/utils"

/**
 * Norn's assistant mascot (DESIGN.md 19.4): an original character, a soft pebble with two eyes and a
 * looped thread on its head (the knot of the Norn mark). It stands in for the assistant wherever one
 * acts: Sidekick's avatar and empty state, the Ask Norn bar, the rail and dock button. It is never a
 * sparkle: the sparkle "AI" glyph is banned (`test/type-lock.test.ts`).
 *
 * Every state is drawn from the same parts, so the character reads at 16px (two eyes and a
 * silhouette) and at 96px. Motion is CSS (`index.css`, `[data-slot=mascot]`) and stops under
 * `prefers-reduced-motion`, where each state is still its own still pose.
 *
 * Two tones: `brand` fills the body with `--mascot-body` (its own violet, the one colour the
 * monochrome chrome allows on the AI surface) and the eyes with
 * `--mascot-eye`; `ink` draws the body in `currentColor` with the eyes cut out, for chrome that is
 * monochrome by rule (the rail, the dock, the Ask bar's lead). Both are tokens, both schemes.
 *
 * Alternative silhouettes (`variant`) are kept so the owner can swap the character: `pebble` is the
 * pick; `loom` and `drop` are the other two candidates (DESIGN.md 19.4).
 */
export type MascotState = "idle" | "thinking" | "answered" | "unavailable" | "asking"
export type MascotVariant = "pebble" | "loom" | "drop"

export const MASCOT_STATES: readonly MascotState[] = ["idle", "thinking", "answered", "unavailable", "asking"]
export const MASCOT_VARIANTS: readonly MascotVariant[] = ["pebble", "loom", "drop"]

/** Each candidate's body, and the thread that makes it a Norn: one path each, in a 32 × 32 box. */
const BODY: Readonly<Record<MascotVariant, { readonly body: string; readonly thread: string; readonly mark?: string; readonly eyeY: number }>> = {
  // A soft pebble, a touch wider at the base, with a looped thread standing on its head.
  pebble: {
    body: "M16 7C23.6 7 27.5 10.6 27.5 18.2C27.5 25.2 23.4 28.4 16 28.4C8.6 28.4 4.5 25.2 4.5 18.2C4.5 10.6 8.4 7 16 7Z",
    thread: "M16 7.2V5.1M16 5.1C13.9 4.9 13.6 1.7 16 1.7C18.4 1.7 18.1 4.9 16 5.1",
    eyeY: 15.2,
  },
  // A round ball of yarn: a full circle with one thread wound across its brow and trailing off the side.
  loom: {
    body: "M16 4.6C23.4 4.6 28.6 9.8 28.6 17.2C28.6 24.6 23.4 29.4 16 29.4C8.6 29.4 3.4 24.6 3.4 17.2C3.4 9.8 8.6 4.6 16 4.6Z",
    thread: "M27.6 11.6C29.2 12.2 30.4 11.4 30.8 9.6",
    // The wound thread is drawn like the eyes: in the eye colour, or cut out of the body.
    mark: "M5.6 11.6C10.8 9.4 21.2 9.4 27.6 11.6",
    eyeY: 15.6,
  },
  // A drop that stands point-up, its tip curling into a thread.
  drop: {
    body: "M16 4.2C19.4 8.4 27.2 13.8 27.2 20.4C27.2 26 22.6 29.4 16 29.4C9.4 29.4 4.8 26 4.8 20.4C4.8 13.8 12.6 8.4 16 4.2Z",
    thread: "M16 4.4C16.6 2.6 18.4 1.6 20.2 2.2",
    eyeY: 17.6,
  },
}

/** The face: the variant's mark (if any) and the eyes of each state, around the variant's eye line. */
function Face({ state, y, mark }: { readonly state: MascotState; readonly y: number; readonly mark: string | undefined }) {
  return <>{mark && <path data-part="mark" d={mark} fill="none" strokeWidth="1.7" strokeLinecap="round" />}<Eyes state={state} y={y} /></>
}

/** The eyes of each state, as filled shapes or (when answered) strokes. */
function Eyes({ state, y }: { readonly state: MascotState; readonly y: number }) {
  switch (state) {
    case "idle":
      return <g data-part="eyes"><rect x="10.4" y={y} width="3.4" height="6" rx="1.7" /><rect x="18.2" y={y} width="3.4" height="6" rx="1.7" /></g>
    case "thinking":
      // Eyes up and toward the reading end, as if looking for the answer.
      return <g data-part="eyes"><rect x="11.4" y={y - 1.4} width="3.4" height="5" rx="1.7" /><rect x="19.2" y={y - 1.4} width="3.4" height="5" rx="1.7" /></g>
    case "answered":
      // Two happy arcs and a small smile.
      return (
        <g data-part="eyes" data-stroke fill="none" strokeLinecap="round" strokeWidth="2">
          <path d={`M10.2 ${y + 4.2}Q12.1 ${y + 1.2} 14 ${y + 4.2}`} />
          <path d={`M18 ${y + 4.2}Q19.9 ${y + 1.2} 21.8 ${y + 4.2}`} />
          <path d={`M14.2 ${y + 7.6}Q16 ${y + 9} 17.8 ${y + 7.6}`} strokeWidth="1.5" />
        </g>
      )
    case "unavailable":
      // Eyes closed to flat lines: it can't answer right now (refused, or no bridge).
      return <g data-part="eyes"><rect x="10.2" y={y + 3.2} width="3.8" height="1.8" rx="0.9" /><rect x="18" y={y + 3.2} width="3.8" height="1.8" rx="0.9" /></g>
    case "asking":
      // Round, attentive eyes: it is waiting on the person's answer.
      return <g data-part="eyes"><circle cx="12.1" cy={y + 3} r="2.2" /><circle cx="19.9" cy={y + 3} r="2.2" /></g>
  }
}

export interface MascotProps {
  readonly state?: MascotState
  readonly variant?: MascotVariant
  readonly tone?: "brand" | "ink"
  /** Rendered size in CSS px; the drawing keeps its 32 × 32 proportions. */
  readonly size?: number
  /** An accessible name makes it an image; without one it is decorative (`aria-hidden`). */
  readonly label?: string
  readonly className?: string
}

export function Mascot({ state = "idle", variant = "pebble", tone = "brand", size = 32, label, className }: MascotProps) {
  const id = useId()
  const mask = `mascot-eyes-${id.replace(/[^a-zA-Z0-9_-]/g, "")}`
  const { body, thread, mark, eyeY } = BODY[variant]
  const ink = tone === "ink"
  return (
    <svg
      data-slot="mascot"
      data-state={state}
      data-variant={variant}
      data-tone={tone}
      viewBox="0 0 32 32"
      width={size}
      height={size}
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      focusable="false"
      className={cn("shrink-0 overflow-visible rtl:-scale-x-100", ink ? "text-current" : "text-(--mascot-body)", className)}
    >
      {label && <title>{label}</title>}
      {ink && (
        <mask id={mask} maskUnits="userSpaceOnUse" x="-4" y="-4" width="40" height="40">
          <rect x="-4" y="-4" width="40" height="40" fill="white" />
          <g fill="black" stroke="black" strokeWidth={0}><Face state={state} y={eyeY} mark={mark} /></g>
        </mask>
      )}
      <g data-part="figure">
        <path data-part="thread" d={thread} fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" />
        {state === "asking" && <path data-part="hand" d="M26.4 16.4L29.6 10.6" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round" />}
        <path data-part="body" d={body} fill="currentColor" mask={ink ? `url(#${mask})` : undefined} />
        {!ink && <g className="fill-(--mascot-eye) stroke-(--mascot-eye)" strokeWidth={0}><Face state={state} y={eyeY} mark={mark} /></g>}
      </g>
    </svg>
  )
}
