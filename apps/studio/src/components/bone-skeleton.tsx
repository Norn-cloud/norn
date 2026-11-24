import { configureBoneyard, Skeleton as BoneyardSkeleton, type AnimationStyle } from "boneyard-js/react"
import { useEffect, useState, type ReactNode } from "react"
import { STUDIO_BONES } from "@/bones/registry"

/** Named first-party Studio loading shapes captured into `src/bones/*.bones.json`. */
export type StudioBoneName =
  | "home-inbox"
  | "home-key-numbers"
  | "sources-overview"
  | "records-table"
  | "semantic-map"
  | "decisions-list"

// Configure boneyard-js once with Studio's monochrome surface tokens so every captured
// UI-shape skeleton adapts cleanly across Polar, Open, Luma, Glass, dark mode, and custom JSON themes.
configureBoneyard({
  color: "var(--muted)",
  darkColor: "var(--well)",
  shimmerColor: "var(--accent)",
  darkShimmerColor: "var(--accent)",
  animate: "shimmer",
  select: "viewport",
})

function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches
  })
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return
    const query = window.matchMedia("(prefers-reduced-motion: reduce)")
    const sync = () => setReduced(query.matches)
    sync()
    query.addEventListener("change", sync)
    return () => query.removeEventListener("change", sync)
  }, [])
  return reduced
}

/**
 * First-party UI-shape skeleton boundary powered by `boneyard-js/react` (MIT).
 * Renders pre-captured responsive `.bones.json` geometry matching the exact shape of each
 * Studio view before content loads, respecting monochrome theme tokens and reduced motion.
 */
export function BoneSkeleton({
  name,
  loading,
  label = "Loading",
  fallback,
  fixture,
  animate,
  className,
  children,
}: {
  readonly name: StudioBoneName
  readonly loading: boolean
  readonly label?: string
  readonly fallback?: ReactNode
  readonly fixture?: ReactNode
  readonly animate?: AnimationStyle
  readonly className?: string
  readonly children?: ReactNode
}) {
  const reducedMotion = usePrefersReducedMotion()
  const resolvedAnimate: AnimationStyle = reducedMotion ? "solid" : (animate ?? "shimmer")

  return (
    <div
      data-slot="bone-skeleton"
      data-bone={name}
      role={loading ? "status" : undefined}
      aria-busy={loading ? "true" : undefined}
      aria-label={loading ? label : undefined}
      className={className}
    >
      <BoneyardSkeleton
        name={name}
        loading={loading}
        initialBones={STUDIO_BONES[name]}
        color="var(--muted)"
        darkColor="var(--well)"
        animate={resolvedAnimate}
        // boneyard-js scales captured bones to the container's first measured height. Rendering the
        // fallback before it picks a breakpoint would lock that height to the fallback's (#1705), so
        // a captured skeleton gets no fallback and opens at its own captured height.
        fallback={STUDIO_BONES[name] === undefined ? fallback : undefined}
        fixture={fixture}
        select="viewport"
      >
        {children ?? null}
      </BoneyardSkeleton>
    </div>
  )
}
