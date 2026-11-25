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
        "fixed z-50 m-0 flex max-h-none flex-col bg-card text-body text-card-foreground outline-none transition-[translate,opacity] duration-150 ease-out data-starting-style:opacity-0 data-closed:transition-none data-closed:**:transition-none motion-reduce:transition-none backdrop:bg-black/[0.08] supports-backdrop-filter:backdrop:backdrop-blur-[1px] dark:backdrop:bg-black/50",
        // The floating inset DetailSheet (DESIGN.md 11): exactly 12px from three edges, 33rem wide, the sheet
        // radius, a hairline ring and the sheet shadow; below 640px it fills the screen less 8px.
        "data-[side=left]:inset-y-[12px] data-[side=left]:start-[12px] data-[side=right]:inset-y-[12px] data-[side=right]:end-[12px] data-[side=left]:h-auto data-[side=right]:h-auto data-[side=left]:w-[33rem] data-[side=right]:w-[33rem] data-[side=left]:max-w-[calc(100vw-24px)] data-[side=right]:max-w-[calc(100vw-24px)] data-[side=left]:rounded-(--radius-sheet) data-[side=right]:rounded-(--radius-sheet) data-[side=left]:shadow-sheet data-[side=right]:shadow-sheet data-[side=left]:ring-1 data-[side=right]:ring-1 data-[side=left]:ring-panel-border data-[side=right]:ring-panel-border max-sm:data-[side=left]:inset-[8px] max-sm:data-[side=right]:inset-[8px] max-sm:data-[side=left]:w-auto max-sm:data-[side=right]:w-auto max-sm:data-[side=left]:max-w-none max-sm:data-[side=right]:max-w-none",
        "data-[side=bottom]:inset-x-0 data-[side=bottom]:bottom-0 data-[side=bottom]:h-auto data-[side=bottom]:border-t data-[side=bottom]:border-line data-[side=top]:inset-x-0 data-[side=top]:top-0 data-[side=top]:h-auto data-[side=top]:border-b data-[side=top]:border-line",
        // A 16px slide from its own edge, mirrored in RTL.
        "data-[side=bottom]:data-starting-style:translate-y-[16px] data-[side=top]:data-starting-style:-translate-y-[16px] data-[side=right]:data-starting-style:translate-x-[16px] rtl:data-[side=right]:data-starting-style:-translate-x-[16px] data-[side=left]:data-starting-style:-translate-x-[16px] rtl:data-[side=left]:data-starting-style:translate-x-[16px]",
        className
      )}
      {...props}
    >
      <LayerProvider layer={layer}>{children}</LayerProvider>
      {showCloseButton && (
        <Close data-slot="sheet-close" aria-label="Close" render={<Button variant="ghost" className="absolute top-[16px] end-[16px] size-[36px] bg-secondary max-sm:size-[44px]" size="icon" />}>
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
      className={cn("flex flex-col gap-1 px-6 pt-5 pe-16 pb-4", className)}
      {...props}
    />
  )
}

/** The sheet's scrolling body: its sections stacked 20px apart (DESIGN.md 11). */
function SheetBody({ className, ...props }: React.ComponentProps<"div">) {
  return <div data-slot="sheet-body" className={cn("grid min-w-0 gap-5 px-5 pb-5", className)} {...props} />
}

/** One section of a sheet: a quiet step-2 label, then its content in one well. */
function SheetSection({ label, children, className, ...props }: Omit<React.ComponentProps<"section">, "children"> & { readonly label: React.ReactNode; readonly children: React.ReactNode }) {
  return (
    <section data-slot="sheet-section" className={cn("grid min-w-0 gap-2", className)} {...props}>
      <h3 className="px-1 text-body text-muted-foreground">{label}</h3>
      <div data-slot="well" className="min-w-0 rounded-(--radius-well) border border-(--well-border) bg-(--well) p-1.5">{children}</div>
    </section>
  )
}

/**
 * The sheet's footer under a hairline: secondary actions as pills on the start side and at most one
 * primary action on the end side. When the cell refuses the action, its refusal stands in its place.
 */
function SheetFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="sheet-footer"
      className={cn("sticky bottom-0 mt-auto flex flex-wrap items-center gap-2 border-t border-line bg-card px-5 py-4", className)}
      {...props}
    />
  )
}

function SheetTitle({ className, asChild, children, ...props }: SlotProps<typeof Title>) {
  return (
    <Title
      data-slot="sheet-title"
      className={cn("font-heading text-heading font-medium text-foreground", className)}
      {...props}
      {...slot(asChild, children)}
    />
  )
}

function SheetDescription({ className, asChild, children, ...props }: SlotProps<typeof Description>) {
  return (
    <Description
      data-slot="sheet-description"
      className={cn("text-meta text-muted-foreground", className)}
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
  SheetBody,
  SheetSection,
  SheetFooter,
  SheetTitle,
  SheetDescription,
}
