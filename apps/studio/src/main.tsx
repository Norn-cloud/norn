import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRouter } from "@tanstack/react-router"
import { BarChart3, Blend, Bookmark, BookMarked, ChartLine, Columns2, Database, FileDiff, Fingerprint, GitCompareArrows, House, Inbox, LayoutDashboard, Map as MapIcon, Network, Plug, Rows3, ShieldCheck, Workflow } from "lucide-react"
import { StrictMode, useMemo } from "react"
import { Button } from "@/components/ui/button"
import { CellUnavailable } from "@/lib/definitions"
import { CellRefusal, NotServed } from "@/lib/live-transport"
import { endSession, useAccount, type Account } from "@/lib/session"
import { applyAppearance, storedShellChrome, storedSkin } from "@/lib/appearance"
import { storedCustomTheme } from "@/lib/studio-theme"
import "./bones/registry.ts"
import { isSectionVisible, useLearningCapabilities } from "@/lib/learning/capabilities"
import { LoginPage } from "./login.tsx"
import { oauthPage } from "./oauth.tsx"
import { createRoot } from "react-dom/client"
import { useDecisionBadge } from "./decisions/inbox.tsx"
import { useRulesMenu } from "./guarantees.tsx"
import { useImportTriageBadge } from "./imports/import-triage-page.tsx"
import { useQuestionsMenu } from "./questions.tsx"
import { routeTree } from "./routes.tsx"
import { Sidekick, useSidekickServed } from "./sidekick/panel.tsx"
import { Shell, type Section } from "./shell.tsx"
import { useRecordsMenu, useSourcesMenu } from "./sources.tsx"
import { useViewsMenu } from "./views.tsx"
import { useWorkflowsMenu } from "./workflows.tsx"
import "./index.css"

/**
 * Every destination, in the perspective that lists it and the space (sidebar section) it sits under.
 * Home and Inbox are global: one shared home and one inbox across everything installed.
 */
const SECTIONS: readonly Section[] = [
  { path: "/", label: "Home", icon: House, perspective: "global" },
  { path: "/decisions", label: "Inbox", icon: Inbox, perspective: "global", useBadge: useDecisionBadge },
  // Workspace: what an operator works with.
  { path: "/objects", label: "Records", icon: Rows3, perspective: "workspace", group: "Data", kind: "source", useMenu: useRecordsMenu, index: false },
  { path: "/views", label: "Views", icon: Bookmark, perspective: "workspace", group: "Data", useMenu: useViewsMenu, pinned: true },
  { path: "/metrics", label: "Metrics", icon: ChartLine, perspective: "workspace", group: "Analysis", kind: "question", useMenu: useQuestionsMenu },
  { path: "/dashboards", label: "Dashboards", icon: LayoutDashboard, perspective: "workspace", group: "Analysis" },
  { path: "/workflows", label: "Workflows", icon: Workflow, perspective: "workspace", group: "Operations", kind: "workflow", useMenu: useWorkflowsMenu, paths: ["/runs"] },
  { path: "/imports", label: "Import triage", icon: Columns2, perspective: "workspace", group: "Operations", useBadge: useImportTriageBadge },
  { path: "/learning/episodes", label: "Episodes", icon: GitCompareArrows, perspective: "workspace", group: "Learning", paths: ["/episodes"] },
  { path: "/learning/cases", label: "Cases & eval sets", icon: BookMarked, perspective: "workspace", group: "Learning" },
  { path: "/learning/misfit", label: "Misfit board", icon: BarChart3, perspective: "workspace", group: "Learning" },
  { path: "/learning/proposals", label: "Proposals", icon: FileDiff, perspective: "workspace", group: "Learning" },
  // Builder: how the model is made.
  { path: "/sources", label: "Sources", icon: Database, perspective: "builder", group: "Model", kind: "source", useMenu: useSourcesMenu, paths: ["/objects"] },
  { path: "/map", label: "Semantic map", icon: MapIcon, perspective: "builder", group: "Model", kind: "objectType" },
  { path: "/transforms", label: "Transforms", icon: Blend, perspective: "builder", group: "Model", kind: "transform" },
  { path: "/canvas", label: "Canvas", icon: Network, perspective: "builder", group: "Logic" },
  { path: "/rules", label: "Rules", icon: ShieldCheck, perspective: "builder", group: "Logic", kind: "guarantee", useMenu: useRulesMenu },
  { path: "/markings", label: "Markings", icon: Fingerprint, perspective: "builder", group: "Logic" },
  { path: "/connections", label: "Connections", icon: Plug, perspective: "builder", group: "Settings" },
]

/**
 * Signed-in shell: reads Learning capabilities once from the current scoped cell's catalog
 * (`GET /v1/definitions`, shared with every route gate via `useMode()`). Learning entries carry no
 * menu/badge hooks, so omitting unavailable ones keeps Shell's hook order invariant.
 */
function SignedInShell({ account, sidekick }: { readonly account: Account; readonly sidekick: boolean }) {
  const learning = useLearningCapabilities()
  const sections = useMemo(() => SECTIONS.filter((section) => isSectionVisible(section.path, learning)), [learning])
  return <Shell sections={sections} assistant={sidekick ? { title: "Sidekick", content: <Sidekick /> } : undefined} account={account} onSignOut={() => void signOut()} />
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
  if (account.isPending) return null
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
const root = createRootRoute({ component: Root })
const router = createRouter({ routeTree: routeTree(root) })

// Dark and light are peers: follow the reader's system setting unless a custom theme pins an explicit appearance.
const dark = window.matchMedia("(prefers-color-scheme: dark)")
const applyScheme = () => {
  const forced = document.documentElement.dataset.forcedAppearance
  if (forced === "dark" || forced === "light") return
  document.documentElement.classList.toggle("dark", dark.matches)
}
applyScheme()
applyAppearance(storedSkin(), storedShellChrome(), undefined, storedCustomTheme())
dark.addEventListener("change", applyScheme)

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
