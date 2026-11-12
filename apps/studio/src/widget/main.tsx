import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createMemoryHistory, createRootRoute, createRouter } from "@tanstack/react-router"
import { StrictMode, useEffect, useLayoutEffect } from "react"
import { createRoot } from "react-dom/client"
import { Bridge, type HostContext } from "./bridge"
import { DeliveryView, Frame, useDelivery, type View } from "./views"
import "../index.css"

/**
 * The chat widget's entry. The Worker serves this page as `ui://norn/<view>.html` and names the
 * view and Studio's origin in two meta tags; the host then delivers the tool's input and result.
 */
const meta = (name: string) => document.querySelector(`meta[name="${name}"]`)?.getAttribute("content") ?? null
const VIEWS: readonly View[] = ["answer", "lineage", "dashboard", "receipt"]
const named = meta("norn-view")
const view: View = VIEWS.find((candidate) => candidate === named) ?? "answer"
const studio = meta("norn-studio")
const bridge = new Bridge(window.parent)

const applyTheme = (context: HostContext) => {
  const dark = context.theme === undefined ? window.matchMedia("(prefers-color-scheme: dark)").matches : context.theme === "dark"
  document.documentElement.classList.toggle("dark", dark)
}

function Widget() {
  const delivery = useDelivery(bridge)
  useEffect(() => {
    void bridge.connect().then(applyTheme).catch(() => applyTheme({}))
    return bridge.on("ui/notifications/host-context-changed", (params) => applyTheme(params as HostContext))
  }, [])
  // The host sizes the frame to the content: the rendered root, never the viewport (which only grows).
  useLayoutEffect(() => {
    const root = document.getElementById("root")
    if (root === null) return
    const report = () => { const box = root.getBoundingClientRect(); bridge.reportSize(box.width, box.height) }
    const observer = new ResizeObserver(report)
    observer.observe(root)
    report()
    return () => observer.disconnect()
  }, [])
  // Studio links open in a new tab through the host, never inside the sandbox.
  useEffect(() => {
    const follow = (event: MouseEvent) => {
      const anchor = (event.target as Element | null)?.closest?.("a[href]")
      if (!(anchor instanceof HTMLAnchorElement)) return
      event.preventDefault()
      const href = anchor.getAttribute("href") ?? ""
      if (studio !== null && href.startsWith("/")) bridge.openLink(new URL(href, studio).href)
      else if (/^https:\/\//.test(href)) bridge.openLink(href)
    }
    document.addEventListener("click", follow, true)
    return () => document.removeEventListener("click", follow, true)
  }, [])
  return <Frame><DeliveryView view={view} delivery={delivery} bridge={bridge} studio={studio} /></Frame>
}

// Studio's components link to Studio pages, so they render inside a router; it never navigates here.
const router = createRouter({ routeTree: createRootRoute({ component: Widget }), history: createMemoryHistory() })

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
)
