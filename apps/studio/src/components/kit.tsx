import { Menu, MenuContent, MenuGroupLabel, MenuItem, MenuItemIndicator, MenuRadioItem, MenuSeparator } from "@ui-sdk/react/menu"
import { Popover, PopoverContent } from "@ui-sdk/react/popover"
import { Tooltip, TooltipContent, TooltipTrigger } from "@ui-sdk/react/tooltip"
import { Check } from "lucide-react"
import type { ComponentProps, ReactElement, ReactNode } from "react"
import { cn } from "@/lib/utils"

// Studio's look for UI SDK parts: the behaviour (keys, focus, dismissal, ARIA) is UI SDK's, the
// tokens are shadcn's. Popups render in the top layer, so they carry their own surface.

const surface = "rounded-xl bg-popover p-1 text-popover-foreground shadow-lg ring-1 ring-border outline-none"
const motion = "origin-(--transform-origin) transition-[opacity,scale] duration-150 data-starting-style:scale-95 data-starting-style:opacity-0 data-ending-style:scale-95 data-ending-style:opacity-0"
const row = "flex cursor-default items-center gap-2 rounded-lg px-2 py-1.5 text-sm outline-none select-none data-highlighted:bg-accent data-highlighted:text-accent-foreground data-disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0"

export { Menu, Popover }

export function KitMenuContent({ className, ...props }: ComponentProps<typeof MenuContent>) {
  return <MenuContent sideOffset={6} className={cn(surface, motion, "min-w-48", className)} {...props} />
}

export function KitMenuItem({ className, ...props }: ComponentProps<typeof MenuItem>) {
  return <MenuItem className={cn(row, className)} {...props} />
}

export function KitMenuRadioItem({ className, children, ...props }: Omit<ComponentProps<typeof MenuRadioItem>, "children"> & { readonly children?: ReactNode }) {
  return (
    <MenuRadioItem className={cn(row, "relative pe-8", className)} {...props}>
      {children}
      <MenuItemIndicator className="absolute end-2 flex"><Check aria-hidden /></MenuItemIndicator>
    </MenuRadioItem>
  )
}

export function KitMenuLabel({ className, ...props }: ComponentProps<typeof MenuGroupLabel>) {
  return <MenuGroupLabel className={cn("px-2 pt-1.5 pb-1 text-xs text-muted-foreground", className)} {...props} />
}

export function KitMenuSeparator({ className, ...props }: ComponentProps<typeof MenuSeparator>) {
  return <MenuSeparator className={cn("-mx-1 my-1 h-px bg-border", className)} {...props} />
}

export function KitPopoverContent({ className, ...props }: ComponentProps<typeof PopoverContent>) {
  return <PopoverContent sideOffset={6} className={cn(surface, motion, "w-72 p-3", className)} {...props} />
}

/** A hint on hover and focus; it names an icon-only trigger. */
export function Hint({ label, side = "right", children }: { readonly label: ReactNode; readonly side?: "top" | "bottom" | "left" | "right" | "start" | "end"; readonly children: ReactElement<Record<string, unknown>> }) {
  return (
    <Tooltip>
      <TooltipTrigger render={children} />
      <TooltipContent side={side} sideOffset={8} className={cn("rounded-md bg-foreground px-2 py-1 text-xs text-background shadow-md", motion)}>{label}</TooltipContent>
    </Tooltip>
  )
}
