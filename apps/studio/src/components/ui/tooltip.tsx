"use client"

import * as React from "react"
import { Tooltip as TooltipPrimitive } from "@base-ui/react/tooltip"
import { cn } from "@/lib/utils"
import { useLayer } from "@/components/ui/dialog"

const TooltipControlContext = React.createContext<{
  readonly setOpen: (open: boolean) => void
} | null>(null)

function TooltipProvider({
  delayDuration = 0,
  delay,
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Provider> & {
  delayDuration?: number
}) {
  return (
    <TooltipPrimitive.Provider
      delay={delay ?? delayDuration}
      {...props}
    />
  )
}

function Tooltip({
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Root>) {
  const [uncontrolledOpen, setUncontrolledOpen] = React.useState(defaultOpen)
  const open = openProp ?? uncontrolledOpen
  const handleOpenChange = React.useCallback(
    (next: boolean, details?: Parameters<NonNullable<typeof onOpenChange>>[1]) => {
      if (openProp === undefined) setUncontrolledOpen(next)
      onOpenChange?.(next, details as never)
    },
    [openProp, onOpenChange],
  )
  const control = React.useMemo(
    () => ({ setOpen: (next: boolean) => handleOpenChange(next) }),
    [handleOpenChange],
  )
  return (
    <TooltipControlContext.Provider value={control}>
      <TooltipPrimitive.Root open={open} onOpenChange={handleOpenChange} {...props} />
    </TooltipControlContext.Provider>
  )
}

function TooltipTrigger({
  asChild = false,
  render,
  children,
  onFocus,
  onBlur,
  ...props
}: Omit<React.ComponentProps<typeof TooltipPrimitive.Trigger>, "render"> & {
  asChild?: boolean
  render?: React.ComponentProps<typeof TooltipPrimitive.Trigger>["render"]
}) {
  const control = React.useContext(TooltipControlContext)
  const slotted = asChild && React.isValidElement(children)
  return (
    <TooltipPrimitive.Trigger
      data-slot="tooltip-trigger"
      render={render ?? (slotted ? (children as React.ReactElement<Record<string, unknown>>) : undefined)}
      onFocus={(event) => {
        control?.setOpen(true)
        onFocus?.(event)
      }}
      onBlur={(event) => {
        control?.setOpen(false)
        onBlur?.(event)
      }}
      {...props}
    >
      {slotted ? undefined : children}
    </TooltipPrimitive.Trigger>
  )
}

function TooltipContent({
  className,
  side = "top",
  align = "center",
  sideOffset = 4,
  children,
  ...props
}: React.ComponentProps<typeof TooltipPrimitive.Popup> & {
  side?: React.ComponentProps<typeof TooltipPrimitive.Positioner>["side"]
  align?: React.ComponentProps<typeof TooltipPrimitive.Positioner>["align"]
  sideOffset?: number
}) {
  return (
    <TooltipPrimitive.Portal container={useLayer()}>
      <TooltipPrimitive.Positioner
        data-slot="tooltip-positioner"
        side={side}
        align={align}
        sideOffset={sideOffset}
        className="z-50 outline-none"
      >
        <TooltipPrimitive.Popup
          role="tooltip"
          data-slot="tooltip-content"
          className={cn(
            "z-50 inline-flex w-fit max-w-xs origin-(--transform-origin) items-center gap-1.5 rounded-xl bg-foreground px-3 py-1.5 text-meta text-background has-data-[slot=kbd]:pe-1.5 data-[side=bottom]:slide-in-from-top-2 data-[side=left]:slide-in-from-right-2 data-[side=right]:slide-in-from-left-2 data-[side=top]:slide-in-from-bottom-2 **:data-[slot=kbd]:relative **:data-[slot=kbd]:isolate **:data-[slot=kbd]:z-50 **:data-[slot=kbd]:rounded-lg data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
            className
          )}
          {...props}
        >
          {children}
          <TooltipPrimitive.Arrow className="z-50 size-2.5 translate-y-[calc(-50%_-_2px)] rotate-45 rounded-[2px] bg-foreground fill-foreground data-[side=left]:translate-x-[-1.5px] data-[side=right]:translate-x-[1.5px]" />
        </TooltipPrimitive.Popup>
      </TooltipPrimitive.Positioner>
    </TooltipPrimitive.Portal>
  )
}

export { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger }
