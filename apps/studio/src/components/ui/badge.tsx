import * as React from "react"
import { useRender } from "@base-ui/react/use-render"
import { cva, type VariantProps } from "class-variance-authority"
import { cn } from "@/lib/utils"

// DESIGN.md 10 and 13: a neutral badge is an outline tag (SourceTag: step 1, --radius-chip, a --line ring,
// no fill, muted text); a state is a 6px dot and its word in muted text. Nothing is a filled or tinted pill.
const dot = "px-0 before:size-[6px] before:shrink-0 before:rounded-full before:content-['']"
const badgeVariants = cva(
  "group/badge inline-flex h-[1.2rem] w-fit shrink-0 items-center justify-center gap-1.5 overflow-hidden rounded-(--radius-chip) px-1.5 text-meta whitespace-nowrap text-muted-foreground transition-colors focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring aria-invalid:ring-destructive [&>svg]:pointer-events-none [&>svg]:size-3!",
  {
    variants: {
      variant: {
        default: "ring-1 ring-line ring-inset [a]:hover:text-foreground",
        secondary: "ring-1 ring-line ring-inset [a]:hover:text-foreground",
        outline: "ring-1 ring-line ring-inset [a]:hover:text-foreground",
        destructive: cn(dot, "before:bg-dot-bad"),
        success: cn(dot, "before:bg-dot-live"),
        warning: cn(dot, "before:bg-dot-held"),
        info: cn(dot, "before:bg-dot-accent"),
        ghost: "hover:bg-accent hover:text-foreground",
        link: "text-link underline-offset-4 hover:underline",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  }
)

function Badge({
  className,
  variant = "default",
  asChild = false,
  render,
  children,
  ref,
  ...props
}: React.ComponentProps<"span"> &
  VariantProps<typeof badgeVariants> & {
    asChild?: boolean
    render?: useRender.RenderProp
  }) {
  const slotted = asChild && React.isValidElement(children)
  return useRender({
    defaultTagName: "span",
    render: render ?? (slotted ? children : undefined),
    ref,
    props: {
      "data-slot": "badge",
      "data-variant": variant,
      className: cn(badgeVariants({ variant }), className),
      ...props,
      ...(slotted ? {} : { children }),
    },
  })
}

export { Badge, badgeVariants }
