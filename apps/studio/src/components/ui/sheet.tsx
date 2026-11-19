import * as React from "react"
import { cn } from "@/lib/utils"
import { DialogClose as Close, DialogContent as Content, DialogDescription as Description, DialogTitle as Title, DialogTrigger as Trigger } from "@ui-sdk/react/dialog"

import { Button } from "@/components/ui/button"
import { XIcon } from "lucide-react"
import { Dialog, firstFocus, LayerProvider, slot, useLayerRef, type OpenProps, type SlotProps, type Styled } from "@/components/ui/dialog"

// A side panel on the UI SDK's dialog behaviour: focus moves in and is trapped, Escape and an outside
// press close it, focus returns to what opened it, and the page behind is inert. The look is ours.
// It closes at once and dims the page with its own ::backdrop, not a Backdrop part: while a modal with a
// Backdrop part animates out, the SDK swallows a press over it, so the next click on the page behind
// would be lost (Radix let it through). The SDK also waits on transitions in the content, its own
// included, so those stop as it closes. Radix overlays opened inside it portal into it (see dialog.tsx).

function Sheet(props: OpenProps) {
  return <Dialog {...props} />
}

function SheetTrigger({ asChild, children, ...props }: SlotProps<typeof Trigger>) {
  return <Trigger data-slot="sheet-trigger" {...props} {...slot(asChild, children)} />
}

function SheetClose({ asChild, children, ...props }: SlotProps<typeof Close>) {
  return <Close data-slot="sheet-close" {...props} {...slot(asChild, children)} />
}

type Side = "top" | "right" | "bottom" | "left"

function SheetContent({
  className,
  children,
  side = "right",
  showCloseButton = true,
  onOpenAutoFocus,
  ...props
}: Omit<Styled<typeof Content>, "children" | "initialFocus"> & {
  children?: React.ReactNode
  side?: Side
  showCloseButton?: boolean
  /** Runs as the sheet opens; `preventDefault()` keeps the focus it moved instead of the first control. */
  onOpenAutoFocus?: (event: Event) => void
}) {
  const { ref, attach, layer } = useLayerRef()
  return (
    <Content
      ref={attach}
      data-slot="sheet-content"
      data-side={side}
      initialFocus={onOpenAutoFocus && (() => firstFocus(ref.current, onOpenAutoFocus))}
      className={cn(
        "fixed z-50 m-0 flex flex-col bg-popover bg-clip-padding text-sm text-popover-foreground shadow-xl outline-none transition-[translate,opacity] duration-200 ease-in-out data-starting-style:opacity-0 data-closed:transition-none data-closed:**:transition-none backdrop:bg-black/30 supports-backdrop-filter:backdrop:backdrop-blur-sm",
        "data-[side=bottom]:inset-x-0 data-[side=bottom]:bottom-0 data-[side=bottom]:h-auto data-[side=bottom]:border-t data-[side=left]:inset-y-0 data-[side=left]:left-0 data-[side=left]:h-full data-[side=left]:w-3/4 data-[side=left]:border-r data-[side=right]:inset-y-0 data-[side=right]:right-0 data-[side=right]:h-full data-[side=right]:w-3/4 data-[side=right]:border-l data-[side=top]:inset-x-0 data-[side=top]:top-0 data-[side=top]:h-auto data-[side=top]:border-b data-[side=left]:sm:max-w-sm data-[side=right]:sm:max-w-sm",
        "data-[side=bottom]:data-starting-style:translate-y-10 data-[side=left]:data-starting-style:-translate-x-10 data-[side=right]:data-starting-style:translate-x-10 data-[side=top]:data-starting-style:-translate-y-10",
        className
      )}
      {...props}
    >
      <LayerProvider layer={layer}>{children}</LayerProvider>
      {showCloseButton && (
        <Close data-slot="sheet-close" aria-label="Close" render={<Button variant="ghost" className="absolute top-4 end-4 bg-secondary" size="icon-sm" />}>
          <XIcon />
        </Close>
      )}
    </Content>
  )
}

function SheetHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="sheet-header"
      className={cn("flex flex-col gap-1.5 p-6", className)}
      {...props}
    />
  )
}

function SheetFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="sheet-footer"
      className={cn("mt-auto flex flex-col gap-2 p-6", className)}
      {...props}
    />
  )
}

function SheetTitle({ className, asChild, children, ...props }: SlotProps<typeof Title>) {
  return (
    <Title
      data-slot="sheet-title"
      className={cn("font-heading text-base font-medium text-foreground", className)}
      {...props}
      {...slot(asChild, children)}
    />
  )
}

function SheetDescription({ className, asChild, children, ...props }: SlotProps<typeof Description>) {
  return (
    <Description
      data-slot="sheet-description"
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
      {...slot(asChild, children)}
    />
  )
}

export {
  Sheet,
  SheetTrigger,
  SheetClose,
  SheetContent,
  SheetHeader,
  SheetFooter,
  SheetTitle,
  SheetDescription,
}
