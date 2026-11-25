import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRouter } from "@tanstack/react-router"
import { Activity, BarChart3, Blend, Blocks, Bookmark, BookMarked, ChartLine, ChartPie, Columns2, Database, FileDiff, Fingerprint, GitCompareArrows, GraduationCap, House, Inbox, LayoutDashboard, Map as MapIcon, Network, Plug, Rows3, Settings, ShieldCheck, Table2, Workflow } from "lucide-react"
import { StrictMode, Suspense, lazy, useEffect, useMemo, useState } from "react"
import { useAssistant } from "@/assistant-slot"
import { LoadState } from "@/components/load-state"
import { Button } from "@/components/ui/button"
import { CellUnavailable, scopeFromAccount, type StudioScope } from "@/lib/definitions"
import { CellRefusal, NotServed } from "@/lib/live-transport"
import { endSession, useAccount, type Account } from "@/lib/session"
import { WorkspaceProvider } from "@/lib/workspace"
import { applyAppearance, applyScheme, storedScheme, storedShellChrome, storedSkin } from "@/lib/appearance"
import "./bones/registry.ts"
import { isSectionVisible, useLearningCapabilities } from "@/lib/learning/capabilities"
import {
  useDecisionBadge,
  useImportTriageBadge,
  useQuestionsMenu,
  useRecordsMenu,
  useRulesMenu,
  useSourcesMenu,
  useViewsMenu,
  useWorkflowsMenu,
} from "@/lib/nav-menus"
import { LoginPage } from "./login.tsx"
import { oauthPage } from "./oauth.tsx"
import { createRoot } from "react-dom/client"
import { routeTree } from "./routes.tsx"
import { useSidekickServed } from "./sidekick/served.ts"
import { Shell } from "./shell.tsx"
import type { Applet, Section } from "./lib/nav-model.ts"
import "./index.css"

// Loaded only once the user opens Sidekick, keeping Streamdown and the ACP SDK out of the initial bundle.
const LazySidekick = lazy(() => import("./sidekick/panel.tsx").then((module) => ({ default: module.Sidekick })))

function DeferredSidekick({ scope, scopeKey }: { readonly scope?: StudioScope | null; readonly scopeKey?: string }) {
  const { open } = useAssistant()
  const [activated, setActivated] = useState(open)
  useEffect(() => { if (open) setActivated(true) }, [open])
  if (!activated) return null
  return (
    <Suspense fallback={<p role="status" className="p-4 text-body text-muted-foreground">Loading Sidekick…</p>}>
      <LazySidekick scope={scope} scopeKey={scopeKey} />
    </Suspense>
  )
}

/**
 * The applets on the rail (DESIGN.md 9.1). Home and Inbox are applets of their own: one shared home
 * and one inbox across everything installed. Each other destination names the applet it opens in.
 */
const DATA: Applet = { id: "data", label: "Data", icon: Table2 }
const ANALYSIS: Applet = { id: "analysis", label: "Analysis", icon: ChartPie }
const OPERATIONS: Applet = { id: "operations", label: "Operations", icon: Activity }
const LEARNING: Applet = { id: "learning", label: "Learning", icon: GraduationCap }
const BUILDER: Applet = { id: "builder", label: "Builder", icon: Blocks }
const SETTINGS: Applet = { id: "settings", label: "Settings", icon: Settings }

/**
 * Every destination: the lens that lists it (its row density), its applet, and the heading it sits
 * under in that applet's menu.
 */
const SECTIONS: readonly Section[] = [
  { path: "/", label: "Home", icon: House, perspective: "global" },
  { path: "/decisions", label: "Inbox", icon: Inbox, perspective: "global", useBadge: useDecisionBadge },
  // Workspace: what an operator works with.
  { path: "/objects", label: "Records", icon: Rows3, perspective: "workspace", applet: DATA, kind: "source", useMenu: useRecordsMenu, index: false },
  { path: "/views", label: "Views", icon: Bookmark, perspective: "workspace", applet: DATA, useMenu: useViewsMenu, pinned: true },
  { path: "/metrics", label: "Metrics", icon: ChartLine, perspective: "workspace", applet: ANALYSIS, kind: "question", useMenu: useQuestionsMenu },
  { path: "/dashboards", label: "Dashboards", icon: LayoutDashboard, perspective: "workspace", applet: ANALYSIS },
  { path: "/workflows", label: "Workflows", icon: Workflow, perspective: "workspace", applet: OPERATIONS, kind: "workflow", useMenu: useWorkflowsMenu, paths: ["/runs"] },
  { path: "/imports", label: "Import triage", icon: Columns2, perspective: "workspace", applet: OPERATIONS, useBadge: useImportTriageBadge },
  { path: "/learning/episodes", label: "Episodes", icon: GitCompareArrows, perspective: "workspace", applet: LEARNING, paths: ["/episodes"] },
  { path: "/learning/cases", label: "Cases & eval sets", icon: BookMarked, perspective: "workspace", applet: LEARNING },
  { path: "/learning/misfit", label: "Misfit board", icon: BarChart3, perspective: "workspace", applet: LEARNING },
  { path: "/learning/proposals", label: "Proposals", icon: FileDiff, perspective: "workspace", applet: LEARNING },
  // Builder: how the model is made.
  { path: "/sources", label: "Sources", icon: Database, perspective: "builder", applet: BUILDER, group: "Model", kind: "source", useMenu: useSourcesMenu, paths: ["/objects"] },
  { path: "/map", label: "Semantic map", icon: MapIcon, perspective: "builder", applet: BUILDER, group: "Model", kind: "objectType" },
  { path: "/transforms", label: "Transforms", icon: Blend, perspective: "builder", applet: BUILDER, group: "Model", kind: "transform" },
  { path: "/canvas", label: "Canvas", icon: Network, perspective: "builder", applet: BUILDER, group: "Logic" },
  { path: "/rules", label: "Rules", icon: ShieldCheck, perspective: "builder", applet: BUILDER, group: "Logic", kind: "guarantee", useMenu: useRulesMenu },
  { path: "/markings", label: "Markings", icon: Fingerprint, perspective: "builder", applet: BUILDER, group: "Logic" },
  { path: "/connections", label: "Connections", icon: Plug, perspective: "builder", applet: SETTINGS },
]

/**
 * Signed-in shell: reads Learning capabilities once from the current scoped cell's catalog
 * (`GET /v1/definitions`, shared with every route gate via `useMode()`). Learning entries carry no
 * menu/badge hooks, so omitting unavailable ones keeps Shell's hook order invariant.
 */
function SignedInShell({ account, sidekick }: { readonly account: Account; readonly sidekick: boolean }) {
  const learning = useLearningCapabilities()
  const sections = useMemo(() => SECTIONS.filter((section) => isSectionVisible(section.path, learning)), [learning])
  const sidekickScopeKey = `${account.user.id}:${account.organization?.id ?? ""}:${account.environmentId ?? ""}`
  const sidekickScope = scopeFromAccount(account)
  // The workspace is decided once, here, from the server's mode answer; every page reads in it (#1762).
  return (
    <WorkspaceProvider>
      <Shell
        sections={sections}
        assistant={sidekick ? { title: "Sidekick", content: <DeferredSidekick key={sidekickScopeKey} scope={sidekickScope} scopeKey={sidekickScopeKey} /> } : undefined}
        account={account}
        onSignOut={() => void signOut()}
      />
    </WorkspaceProvider>
  )
}

/**
 * Signed out (an explicit 401), a visitor sees only the sign-in page; signed in, the Studio.
 * A failed check keeps the last known account; with none, it offers a retry, never the sign-in page.
 */
function Root() {
  const account = useAccount()
  // Sidekick is offered only where a bridge serves it (a private deployment); elsewhere the rail shows no entry for it.
  const sidekick = useSidekickServed()
  if (account.data != null) return <SignedInShell account={account.data} sidekick={sidekick} />
  if (account.isPending) {
    // The shared loading state: a skeleton, then "still waiting" and a retry if the check runs long.
    return (
      <main className="mx-auto flex min-h-svh max-w-xl flex-col justify-center p-8">
        <LoadState status="pending" data={undefined} error={null} isFetching onRetry={() => void account.refetch({ cancelRefetch: true })} loadingLabel="Opening Studio">{() => null}</LoadState>
      </main>
    )
  }
  if (account.isError) {
    return (
      <main className="flex min-h-svh flex-col items-center justify-center gap-3 p-8 text-muted-foreground">
        <p>Studio could not reach its server.</p>
        <Button variant="outline" onClick={() => void account.refetch()}>Try again</Button>
      </main>
    )
  }
  return <LoginPage />
}

/** Sign out, and forget the account and its reads only once the server has ended the session. */
async function signOut() {
  try {
    await endSession(queryClient)
  } catch {
    window.alert("Sign-out did not complete; you are still signed in. Try again.")
  }
}

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: (failureCount, error) => {
        if (error instanceof NotServed) return false
        if (error instanceof CellUnavailable && error.retryable === false) return false
        if (error instanceof CellRefusal && error.retryable === false) return false
        return failureCount < 3
      },
    },
  },
})
function RoutePending() {
  return <div role="status" aria-label="Loading page" className="min-h-40 py-8 text-body text-muted-foreground">Loading…</div>
}

const root = createRootRoute({ component: Root })
const router = createRouter({
  routeTree: routeTree(root),
  defaultPendingComponent: RoutePending,
  defaultPendingMs: 0,
  defaultPendingMinMs: 0,
})

// Dark and light are peers: the reader's pinned scheme, else the system's. The workspace's served
// theme (which may pin an appearance) paints once the shell has read it.
applyScheme(storedScheme())
applyAppearance(storedSkin(), storedShellChrome())
window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => applyScheme(storedScheme()))

// An assistant's OAuth flow (MCP) lands on its own pages, outside the Studio shell.
const OAuthPage = oauthPage(window.location.pathname)

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    {OAuthPage === null ? (
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    ) : <OAuthPage />}
  </StrictMode>,
)
