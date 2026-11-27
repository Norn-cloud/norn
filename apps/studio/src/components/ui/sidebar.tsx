"use client"

import * as React from "react"
import { Dialog } from "@base-ui/react/dialog"
import { PanelLeftIcon, XIcon } from "lucide-react"
import { cn } from "@/lib/utils"
import { useIsMobile, useMediaQuery } from "@/hooks/use-mobile"

// The three-island shell of DESIGN.md section 9.1: a 64px applet rail, a 256px submenu island that
// collapses to nothing, and the main island, each floating 8px apart on the backdrop. Below 640px
// the rail and the submenu leave the layout for one modal "Navigation" sheet from the start edge.
// Widths are px so they do not scale with the 90% root.
const RAIL_WIDTH = "64px"
const SIDEBAR_WIDTH = "256px"
const SIDEBAR_KEYBOARD_SHORTCUT = "b"
const SIDEBAR_KEY = "norn.studio.sidebar"
/** The submenu island's id, which the toggle names in `aria-controls` from 640px up. */
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
 * Whether the submenu island is shown, and the phone sheet open. From 1024px the island starts
 * shown, from 640 to 1023px it starts collapsed (rail and main island only), and the reader's own
 * choice, kept per browser, wins at every width from 640px up. ⌘B / Ctrl+B toggles it while
 * `shortcut` is on; a shell that shows no islands (the dock) turns it off, so the key neither stores
 * a choice nor opens a sheet nobody sees.
 */
function SidebarProvider({
  shortcut = true,
  className,
  style,
  children,
  ...props
}: React.ComponentProps<"div"> & { readonly shortcut?: boolean }) {
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
    if (!shortcut) return
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === SIDEBAR_KEYBOARD_SHORTCUT && (event.metaKey || event.ctrlKey)) {
        event.preventDefault()
        toggleSidebar()
      }
    }

    window.addEventListener("keydown", handleKeyDown)
    return () => window.removeEventListener("keydown", handleKeyDown)
  }, [shortcut, toggleSidebar])

  const state = open ? "expanded" : "collapsed"

  const contextValue = React.useMemo<SidebarContextProps>(
    () => ({ state, open, setOpen, isMobile, openMobile, setOpenMobile, toggleSidebar, triggerRef }),
    [state, open, setOpen, isMobile, openMobile, toggleSidebar]
  )

  return (
    <SidebarContext.Provider value={contextValue}>
      {/* The backdrop (DESIGN.md 4.7): the ground and its ambient glows, painted by index.css from the tokens. */}
      <div
        data-slot="sidebar-wrapper"
        style={
          {
            "--rail-width": RAIL_WIDTH,
            "--sidebar-width": SIDEBAR_WIDTH,
            ...style,
          } as React.CSSProperties
        }
        className={cn("group/sidebar-wrapper flex min-h-svh w-full p-[8px] max-sm:p-0", className)}
        {...props}
      >
        {children}
      </div>
    </SidebarContext.Provider>
  )
}

/**
 * Whether an Escape belongs to a layer open inside the phone sheet rather than to the sheet. The
 * workspace switcher and account menus (UI SDK) open in place in the top layer (`popover`), so Base
 * UI's dismiss reads their Escape as the sheet's; a layer that already took it marks it handled.
 */
function escapeIsInnerLayers(event: Event): boolean {
  if (event.defaultPrevented) return true
  return event.target instanceof Element && event.target.closest("[popover]") !== null
}

// An island that stays beside the scrolling page: the full window height less the 8px backdrop margin.
const sideIsland = "sticky top-[8px] flex h-[calc(100svh-16px)] shrink-0 flex-col overflow-hidden text-sidebar-foreground"

/** The applet rail (64px), the first island; it leaves the layout below 640px, where the sheet holds it. */
function SidebarRail({ className, ...props }: React.ComponentProps<"div">) {
  const { isMobile } = useSidebar()
  if (isMobile) return null
  return <div data-slot="sidebar-rail" data-shell-island="nav" className={cn(sideIsland, "w-(--rail-width) items-center", className)} {...props} />
}

/**
 * The submenu island (256px), floating beside the rail; collapsed it takes no room and leaves the
 * tab order (`inert`). Below 640px it is a modal sheet instead (Base UI `Dialog`: focus is trapped,
 * the page is inert, Escape closes it and focus returns to the toggle), named "Navigation", holding
 * the rail and the submenu side by side. Escape closes the innermost layer first: a menu open inside
 * the sheet closes alone, and the next Escape closes the sheet.
 */
function Sidebar({ className, children, ...props }: React.ComponentProps<"div">) {
  const { isMobile, state, openMobile, setOpenMobile, triggerRef } = useSidebar()

  if (isMobile) {
    return (
      <Dialog.Root
        open={openMobile}
        onOpenChange={(next, details) => {
          if (!next && details.reason === "escape-key" && escapeIsInnerLayers(details.event)) {
            // The sheet stays open, and the Escape still reaches the UI SDK's document-level listener,
            // which closes the menu open in the top layer (Base UI would otherwise stop its propagation).
            details.cancel()
            details.allowPropagation()
            return
          }
          setOpenMobile(next)
        }}
      >
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
            className="group/sidebar fixed start-[8px] top-[8px] bottom-[8px] z-50 flex w-[min(360px,calc(100vw-56px))] overflow-hidden rounded-(--radius-sheet) bg-shell text-sidebar-foreground shadow-sheet ring-1 ring-panel-border outline-none transition-[opacity,translate] duration-150 data-ending-style:-translate-x-[16px] data-ending-style:opacity-0 data-starting-style:-translate-x-[16px] data-starting-style:opacity-0 motion-reduce:transition-none rtl:data-ending-style:translate-x-[16px] rtl:data-starting-style:translate-x-[16px]"
          >
            <Dialog.Title className="sr-only">Navigation</Dialog.Title>
            {children}
          </Dialog.Popup>
        </Dialog.Portal>
      </Dialog.Root>
    )
  }

  const collapsed = state === "collapsed"
  return (
    <div
      id={SIDEBAR_ID}
      data-slot="sidebar"
      data-shell-island="nav"
      data-state={state}
      inert={collapsed}
      aria-hidden={collapsed || undefined}
      className={cn(
        sideIsland,
        "group/sidebar ms-[8px] w-(--sidebar-width) transition-[width,margin,opacity,border-width] duration-200 ease-out data-[state=collapsed]:ms-0 data-[state=collapsed]:w-0 data-[state=collapsed]:border-0 data-[state=collapsed]:opacity-0 data-[state=collapsed]:shadow-none motion-reduce:transition-none",
        className
      )}
      {...props}
    >
      {/* The content keeps its width while the island narrows, so nothing reflows mid-collapse. */}
      <div className="flex h-full w-(--sidebar-width) min-w-(--sidebar-width) flex-col">{children}</div>
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
 * The submenu island's toggle: named "Sidebar" in both states, with `aria-expanded`. From 640px it
 * controls the island; below, it opens the sheet (`aria-haspopup="dialog"`) and names it only while
 * it is open.
 */
function SidebarTrigger({ className, onClick, ref, ...props }: React.ComponentProps<"button">) {
  const { toggleSidebar, open, openMobile, isMobile, triggerRef } = useSidebar()
  const attach = React.useCallback((node: HTMLButtonElement | null) => {
    triggerRef.current = node
    if (typeof ref === "function") ref(node)
    else if (ref) ref.current = node
  }, [ref, triggerRef])

  return (
    // A wrapping tooltip passes its own props (data-slot, handlers) first; the toggle's name, state and slot win.
    <button
      {...props}
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
    >
      <PanelLeftIcon aria-hidden strokeWidth={1.6} />
    </button>
  )
}

/**
 * The main island: the content and its top bar on one floating glass panel (DESIGN.md 9.1). Below
 * 640px it fills the window, with no margin, edge or radius.
 */
function SidebarInset({ className, ...props }: React.ComponentProps<"main">) {
  return (
    <main
      data-slot="sidebar-inset"
      data-shell-island="main"
      className={cn("relative ms-[8px] flex min-h-[calc(100svh-16px)] w-full min-w-0 flex-1 flex-col max-sm:ms-0 max-sm:min-h-svh max-sm:rounded-none max-sm:border-0 max-sm:shadow-none", className)}
      {...props}
    />
  )
}

export {
  Sidebar,
  SidebarClose,
  SidebarInset,
  SidebarProvider,
  SidebarRail,
  SidebarTrigger,
  useSidebar,
}
