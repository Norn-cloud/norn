import * as React from "react"

/** Below this width the sidebar leaves the layout and opens as a sheet (DESIGN.md 9.1), and touch targets grow to 44px. */
export const MOBILE_BREAKPOINT = 640

/** Whether a media query matches, kept in sync with the window; `serverValue` while there is no window. */
export function useMediaQuery(query: string, serverValue = false): boolean {
  const subscribe = React.useCallback((onChange: () => void) => {
    if (typeof globalThis.window?.matchMedia !== "function") return () => {}
    const list = globalThis.window.matchMedia(query)
    list.addEventListener("change", onChange)
    return () => list.removeEventListener("change", onChange)
  }, [query])
  const snapshot = () => typeof globalThis.window?.matchMedia === "function" ? globalThis.window.matchMedia(query).matches : serverValue
  return React.useSyncExternalStore(subscribe, snapshot, () => serverValue)
}

/** True below `breakpoint` CSS px (640 by default, the phone layout). */
export function useIsMobile(breakpoint: number = MOBILE_BREAKPOINT): boolean {
  return useMediaQuery(`(max-width: ${breakpoint - 0.02}px)`)
}
