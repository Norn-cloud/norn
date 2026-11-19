import * as React from "react"
import { cn } from "@/lib/utils"
import { Dialog as Root, DialogClose as Close, DialogContent as Content, DialogDescription as Description, DialogTitle as Title, DialogTrigger as Trigger } from "@ui-sdk/react/dialog"

import { Button } from "@/components/ui/button"
import { XIcon } from "lucide-react"

// A centred dialog on the UI SDK's dialog behaviour: focus moves in and is trapped, Escape and an
// outside press close it, focus returns to what opened it, and the page behind is inert. The look is ours.
// It closes at once and dims the page with its own ::backdrop, not a Backdrop part: while a modal with a
// Backdrop part animates out, the SDK swallows a press over it, so the next click on the page behind
// would be lost (Radix let it through). The SDK also waits on transitions in the content, its own
// included, so those stop as it closes.
// A modal sits in the browser's top layer, above anything portalled to the body, so the Radix menus,
// popovers and tooltips opened inside one portal into it instead (`useLayer`).

/** The open state, as Radix's root took it, so callers are unchanged. */
type OpenProps = {
  open?: boolean
  defaultOpen?: boolean
  onOpenChange?: (open: boolean) => void
  modal?: boolean
  children?: React.ReactNode
}

/** A part's props with a plain class name (the SDK also takes one per state). */
type Styled<C extends React.ElementType> = Omit<React.ComponentProps<C>, "className"> & { className?: string }

/** A part that can lend its behaviour to its one child, as Radix's `asChild` did (the SDK's `render`). */
type SlotProps<C extends React.ElementType> = Omit<Styled<C>, "children" | "render"> & {
  asChild?: boolean
  children?: React.ReactNode
}

function slot(asChild: boolean | undefined, children: React.ReactNode) {
  return asChild && React.isValidElement(children) ? { render: children as React.ReactElement<Record<string, unknown>> } : { children }
}

/** The open panel that Radix overlays portal into, so they share its top layer; none outside a panel. */
const LayerContext = React.createContext<HTMLElement | null>(null)

/** The container for a Radix `Portal`: the panel it was opened from, or the body (Radix's default). */
function useLayer(): HTMLElement | undefined {
  return React.useContext(LayerContext) ?? undefined
}

/** A ref for a panel that also provides it as the layer its overlays portal into. */
function useLayerRef() {
  const ref = React.useRef<HTMLDivElement | null>(null)
  const [layer, setLayer] = React.useState<HTMLElement | null>(null)
  const attach = React.useCallback((el: HTMLDivElement | null) => { ref.current = el; setLayer(el) }, [])
  return { ref, attach, layer }
}

function LayerProvider({ layer, children }: { layer: HTMLElement | null; children: React.ReactNode }) {
  return <LayerContext.Provider value={layer}>{children}</LayerContext.Provider>
}

const TABBABLE = "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])"

/**
 * Where focus goes as a dialog opens, for callers that pass Radix's `onOpenAutoFocus`: the handler
 * runs first, and a `preventDefault()` keeps the focus it moved; otherwise the first control.
 */
function firstFocus(content: HTMLElement | null, handler: (event: Event) => void): HTMLElement | undefined {
  if (content === null) return undefined
  const event = new Event("openautofocus", { cancelable: true })
  content.addEventListener("openautofocus", handler, { once: true })
  content.dispatchEvent(event)
  const active = document.activeElement
  if (event.defaultPrevented && active instanceof HTMLElement && content.contains(active)) return active
  return content.querySelector<HTMLElement>(TABBABLE) ?? content
}

function Dialog({ open, defaultOpen, onOpenChange, modal = true, children }: OpenProps) {
  return <Root open={open} defaultOpen={defaultOpen} modal={modal} onOpenChange={onOpenChange && ((next) => onOpenChange(next))}>{children}</Root>
}

function DialogTrigger({ asChild, children, ...props }: SlotProps<typeof Trigger>) {
  return <Trigger data-slot="dialog-trigger" {...props} {...slot(asChild, children)} />
}

function DialogClose({ asChild, children, ...props }: SlotProps<typeof Close>) {
  return <Close data-slot="dialog-close" {...props} {...slot(asChild, children)} />
}

function DialogContent({
  className,
  children,
  showCloseButton = true,
  ...props
}: Omit<Styled<typeof Content>, "children"> & {
  children?: React.ReactNode
  showCloseButton?: boolean
}) {
  const { attach, layer } = useLayerRef()
  return (
    <Content
      ref={attach}
      data-slot="dialog-content"
      className={cn(
        "fixed top-1/2 left-1/2 z-50 m-0 grid w-full max-w-[calc(100%-2rem)] -translate-x-1/2 -translate-y-1/2 gap-6 rounded-4xl bg-popover p-6 text-sm text-popover-foreground shadow-xl ring-1 ring-foreground/5 outline-none transition-[opacity,scale] duration-100 sm:max-w-md dark:ring-foreground/10 data-starting-style:scale-95 data-starting-style:opacity-0 data-closed:transition-none data-closed:**:transition-none backdrop:bg-black/30 supports-backdrop-filter:backdrop:backdrop-blur-sm",
        className
      )}
      {...props}
    >
      <LayerProvider layer={layer}>{children}</LayerProvider>
      {showCloseButton && (
        <Close data-slot="dialog-close" aria-label="Close" render={<Button variant="ghost" className="absolute top-4 right-4 bg-secondary" size="icon-sm" />}>
          <XIcon />
        </Close>
      )}
    </Content>
  )
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-header"
      className={cn("flex flex-col gap-1.5", className)}
      {...props}
    />
  )
}

function DialogFooter({
  className,
  showCloseButton = false,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  showCloseButton?: boolean
}) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn(
        "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end",
        className
      )}
      {...props}
    >
      {children}
      {showCloseButton && <Close render={<Button variant="outline" />}>Close</Close>}
    </div>
  )
}

function DialogTitle({ className, asChild, children, ...props }: SlotProps<typeof Title>) {
  return (
    <Title
      data-slot="dialog-title"
      className={cn("font-heading text-base leading-none font-medium", className)}
      {...props}
      {...slot(asChild, children)}
    />
  )
}

function DialogDescription({ className, asChild, children, ...props }: SlotProps<typeof Description>) {
  return (
    <Description
      data-slot="dialog-description"
      className={cn(
        "text-sm text-muted-foreground *:[a]:underline *:[a]:underline-offset-3 *:[a]:hover:text-foreground",
        className
      )}
      {...props}
      {...slot(asChild, children)}
    />
  )
}

export {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
  firstFocus,
  LayerProvider,
  slot,
  useLayer,
  useLayerRef,
  type OpenProps,
  type SlotProps,
  type Styled,
}
