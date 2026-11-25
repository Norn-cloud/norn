"use client"

import * as React from "react"
import { Dialog } from "@base-ui/react/dialog"
import { PanelLeftIcon, XIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { useIsMobile, useMediaQuery } from "@/hooks/use-mobile"

// The thin source-list sidebar of DESIGN.md section 9.1: 232px expanded, a 48px icon rail collapsed,
// and below 640px a modal sheet from the start edge. Widths are px so they do not scale with the 90% root.
const SIDEBAR_WIDTH = "232px"
const SIDEBAR_WIDTH_ICON = "48px"
const SIDEBAR_KEYBOARD_SHORTCUT = "b"
const SIDEBAR_KEY = "norn.studio.sidebar"
/** The column's id, which the toggle names in `aria-controls` from 640px up. */
export const SIDEBAR_ID = "studio-sidebar"
/** The phone sheet's id, which the toggle names only while the sheet is open (a closed dialog is not in the DOM). */
export const SIDEBAR_SHEET_ID = "studio-sidebar-sheet"

type SidebarContextProps = {
  state: "expanded" | "collapsed"
  open: boolean
  setOpen: (open: boolean | ((open: boolean) => boolean)) => void
  openMobile: boolean
  setOpenMobile: (open: boolean) => void
  isMobile: boolean
  toggleSidebar: () => void
  /** The toggle, where focus returns when the phone sheet closes. */
  triggerRef: React.RefObject<HTMLButtonElement | null>
}

const SidebarContext = React.createContext<SidebarContextProps | null>(null)

function useSidebar() {
  const context = React.useContext(SidebarContext)
  if (!context) {
    throw new Error("useSidebar must be used within a SidebarProvider.")
  }

  return context
}

/** The reader's own choice from an earlier visit, or nothing when they never chose (or storage is blocked). */
function storedSidebar(): boolean | undefined {
  try {
    const kept = globalThis.localStorage?.getItem(SIDEBAR_KEY)
    return kept === "expanded" ? true : kept === "collapsed" ? false : undefined
  } catch {
    return undefined
  }
}

/**
 * Whether the sidebar is expanded, and the phone sheet open. From 1024px it starts expanded, from 640
 * to 1023px it starts as the rail, and the reader's own choice, kept per browser, wins at every width
 * from 640px up. ⌘B / Ctrl+B toggles it.
 */
function SidebarProvider({
  className,
  style,
  children,
  ...props
}: React.ComponentProps<"div">) {
  const isMobile = useIsMobile()
  const wide = useMediaQuery("(min-width: 1024px)", true)
  const [openMobile, setOpenMobile] = React.useState(false)
  const [chosen, setChosen] = React.useState<boolean | undefined>(storedSidebar)
  const open = chosen ?? wide
  const triggerRef = React.useRef<HTMLButtonElement>(null)

  const setOpen = React.useCallback(
    (value: boolean | ((value: boolean) => boolean)) => {
      const next = typeof value === "function" ? value(open) : value
      setChosen(next)
      try {
        globalThis.localStorage?.setItem(SIDEBAR_KEY, next ? "expanded" : "collapsed")
      } catch {
        /* not kept: storage is blocked */
      }
    },
    [open]
  )

  // The phone sheet never outlives the phone layout.
  React.useEffect(() => {
    if (!isMobile) setOpenMobile(false)
  }, [isMobile])

  const toggleSidebar = React.useCallback(() => {
    return isMobile ? setOpenMobile((value) => !value) : setOpen((value) => !value)
  }, [isMobile, setOpen])

  React.useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === SIDEBAR_KEYBOARD_SHORTCUT && (event.metaKey || event.ctrlKey)) {
        event.preventDefault()
        toggleSidebar()
      }
    }

    window.addEventListener("keydown", handleKeyDown)
    return () => window.removeEventListener("keydown", handleKeyDown)
  }, [toggleSidebar])

  const state = open ? "expanded" : "collapsed"

  const contextValue = React.useMemo<SidebarContextProps>(
    () => ({ state, open, setOpen, isMobile, openMobile, setOpenMobile, toggleSidebar, triggerRef }),
    [state, open, setOpen, isMobile, openMobile, toggleSidebar]
  )

  return (
    <SidebarContext.Provider value={contextValue}>
      <div
        data-slot="sidebar-wrapper"
        style={
          {
            "--sidebar-width": SIDEBAR_WIDTH,
            "--sidebar-width-icon": SIDEBAR_WIDTH_ICON,
            ...style,
          } as React.CSSProperties
        }
        className={cn("group/sidebar-wrapper flex min-h-svh w-full", className)}
        {...props}
      >
        {children}
      </div>
    </SidebarContext.Provider>
  )
}

/**
 * The sidebar: a flat column on the start side with a hairline on its inline-end edge, or below 640px
 * a modal sheet (Base UI `Dialog`: focus is trapped, the page is inert, Escape closes it and focus
 * returns to the toggle), named "Navigation".
 */
function Sidebar({ className, children, ...props }: React.ComponentProps<"div">) {
  const { isMobile, state, openMobile, setOpenMobile, triggerRef } = useSidebar()

  if (isMobile) {
    return (
      <Dialog.Root open={openMobile} onOpenChange={setOpenMobile}>
        <Dialog.Portal>
          <Dialog.Backdrop
            data-slot="sidebar-backdrop"
            className="fixed inset-0 z-50 bg-black/[0.08] backdrop-blur-[1px] transition-opacity duration-150 data-ending-style:opacity-0 data-starting-style:opacity-0 motion-reduce:transition-none dark:bg-black/50"
          />
          <Dialog.Popup
            id={SIDEBAR_SHEET_ID}
            data-slot="sidebar"
            data-mobile="true"
            data-state="expanded"
            finalFocus={triggerRef}
            className="group/sidebar fixed start-[8px] top-[8px] bottom-[8px] z-50 flex w-[min(320px,calc(100vw-56px))] flex-col overflow-hidden rounded-(--radius-sheet) bg-shell text-sidebar-foreground shadow-sheet ring-1 ring-panel-border outline-none transition-[opacity,translate] duration-150 data-ending-style:-translate-x-[16px] data-ending-style:opacity-0 data-starting-style:-translate-x-[16px] data-starting-style:opacity-0 motion-reduce:transition-none rtl:data-ending-style:translate-x-[16px] rtl:data-starting-style:translate-x-[16px]"
          >
            <Dialog.Title className="sr-only">Navigation</Dialog.Title>
            {children}
          </Dialog.Popup>
        </Dialog.Portal>
      </Dialog.Root>
    )
  }

  return (
    <div
      id={SIDEBAR_ID}
      data-slot="sidebar"
      data-state={state}
      data-collapsible={state === "collapsed" ? "icon" : ""}
      className={cn(
        "group/sidebar sticky top-0 z-10 flex h-svh w-(--sidebar-width) shrink-0 flex-col overflow-hidden border-e border-sidebar-border bg-sidebar text-sidebar-foreground transition-[width] duration-200 ease-out data-[state=collapsed]:w-(--sidebar-width-icon) motion-reduce:transition-none",
        className
      )}
      {...props}
    >
      {children}
    </div>
  )
}

/** The phone sheet's close button (44px, "Close navigation"); nothing outside the sheet. */
function SidebarClose({ className }: { readonly className?: string }) {
  const { isMobile } = useSidebar()
  if (!isMobile) return null
  return (
    <Dialog.Close
      data-slot="sidebar-close"
      aria-label="Close navigation"
      className={cn(
        "flex size-[44px] shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none transition-colors hover:bg-sidebar-accent/50 focus-visible:outline-2 focus-visible:outline-solid focus-visible:-outline-offset-2 focus-visible:outline-ring [&_svg]:size-[18px]",
        className
      )}
    >
      <XIcon aria-hidden />
    </Dialog.Close>
  )
}

/**
 * The sidebar's toggle: named "Sidebar" in both states, with `aria-expanded`. From 640px it controls
 * the column; below, it opens the sheet (`aria-haspopup="dialog"`) and names it only while it is open.
 */
function SidebarTrigger({ className, onClick, ref, ...props }: React.ComponentProps<"button">) {
  const { toggleSidebar, open, openMobile, isMobile, triggerRef } = useSidebar()
  const attach = React.useCallback((node: HTMLButtonElement | null) => {
    triggerRef.current = node
    if (typeof ref === "function") ref(node)
    else if (ref) ref.current = node
  }, [ref, triggerRef])

  return (
    <button
      type="button"
      data-sidebar="trigger"
      data-slot="sidebar-trigger"
      aria-label="Sidebar"
      aria-expanded={isMobile ? openMobile : open}
      aria-controls={isMobile ? (openMobile ? SIDEBAR_SHEET_ID : undefined) : SIDEBAR_ID}
      aria-haspopup={isMobile ? "dialog" : undefined}
      ref={attach}
      className={cn(
        "flex size-[32px] shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-2 focus-visible:outline-ring max-sm:size-[44px] [&_svg]:size-4 rtl:[&_svg]:-scale-x-100",
        className
      )}
      onClick={(event) => {
        onClick?.(event)
        toggleSidebar()
      }}
      {...props}
    >
      <PanelLeftIcon aria-hidden strokeWidth={1.6} />
    </button>
  )
}

/** The content column beside the sidebar: the unframed canvas (a framed panel around cards would be a card in a card). */
function SidebarInset({ className, ...props }: React.ComponentProps<"main">) {
  return (
    <main
      data-slot="sidebar-inset"
      className={cn("relative flex w-full min-w-0 flex-1 flex-col bg-canvas", className)}
      {...props}
    />
  )
}

export {
  Sidebar,
  SidebarClose,
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
}
