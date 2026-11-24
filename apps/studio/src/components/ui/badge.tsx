import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { cn } from "@/lib/utils"
import { Slot } from "radix-ui"

const badgeVariants = cva(
  "group/badge inline-flex h-[1.375rem] w-fit shrink-0 items-center justify-center gap-1 overflow-hidden rounded-md border border-transparent px-2 py-0.5 text-xs font-medium whitespace-nowrap transition-colors focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 has-data-[icon=inline-end]:pr-1.5 has-data-[icon=inline-start]:pl-1.5 aria-invalid:border-destructive aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 [&>svg]:pointer-events-none [&>svg]:size-3!",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground [a]:hover:bg-action-hover",
        secondary:
          "bg-surface-subtle text-secondary-foreground ring-1 ring-inset ring-border/80 [a]:hover:bg-accent",
        destructive:
          "bg-tone-red/10 text-tone-red ring-1 ring-inset ring-tone-red/20 focus-visible:ring-destructive/20 dark:bg-tone-red/15 dark:ring-tone-red/25 dark:focus-visible:ring-destructive/40 [a]:hover:bg-tone-red/18",
        outline:
          "border-border bg-background/70 text-muted-foreground [a]:hover:bg-surface-subtle [a]:hover:text-foreground",
        // States, as tinted detail pills on a neutral surface; callers pair each with its own icon so hue is never the only cue.
        success: "bg-tone-green/10 text-tone-green ring-1 ring-inset ring-tone-green/20 dark:bg-tone-green/15 dark:ring-tone-green/25",
        warning: "bg-tone-amber/12 text-tone-amber ring-1 ring-inset ring-tone-amber/22 dark:bg-tone-amber/15 dark:ring-tone-amber/25",
        info: "bg-tone-violet/10 text-tone-violet ring-1 ring-inset ring-tone-violet/20 dark:bg-tone-violet/15 dark:ring-tone-violet/25",
        ghost:
          "hover:bg-accent hover:text-foreground dark:hover:bg-accent/60",
        link: "text-action underline-offset-4 hover:underline",
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
  ...props
}: React.ComponentProps<"span"> &
  VariantProps<typeof badgeVariants> & { asChild?: boolean }) {
  const Comp = asChild ? Slot.Root : "span"

  return (
    <Comp
      data-slot="badge"
      data-variant={variant}
      className={cn(badgeVariants({ variant }), className)}
      {...props}
    />
  )
}

export { Badge, badgeVariants }
