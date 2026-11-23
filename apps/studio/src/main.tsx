import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRouter } from "@tanstack/react-router"
import { Blend, ChartLine, Columns2, Database, FileScan, Fingerprint, House, Inbox, LayoutDashboard, ListChecks, Network, Plug, Rows3, ShieldCheck, SlidersHorizontal, Workflow } from "lucide-react"
import { StrictMode } from "react"
import { Button } from "@/components/ui/button"
import { endSession, useAccount } from "@/lib/session"
import { LoginPage } from "./login.tsx"
import { oauthPage } from "./oauth.tsx"
import { createRoot } from "react-dom/client"
import { useDecisionBadge } from "./decisions/inbox.tsx"
import { useGuaranteesMenu } from "./guarantees.tsx"
import { useImportTriageBadge } from "./imports/import-triage-page.tsx"
import { useQuestionsMenu } from "./questions.tsx"
import { routeTree } from "./routes.tsx"
import { Sidekick, useSidekickServed } from "./sidekick/panel.tsx"
import { Shell, type Section } from "./shell.tsx"
import { useRecordsMenu, useSourcesMenu } from "./sources.tsx"
import { useWorkflowsMenu } from "./workflows.tsx"
import { financeDemo } from "./lib/finance-demo.ts"
import { financeRecordsMenu, financeSourceMenu } from "./finance/record-pages.tsx"
import { financeWorkflowMenu } from "./finance/behavior-pages.tsx"
import { financeRuleMenu } from "./finance/operations.tsx"
import { financeMetricMenu } from "./finance/metrics.tsx"
import { financePolicyMenu } from "./finance/policies.tsx"
import { financeExampleMenu } from "./finance/examples.tsx"
import "./index.css"

/**
 * Every destination, in the perspective that lists it and the space (sidebar section) it sits under.
 * Home and Inbox are global: one shared home and one inbox across everything installed.
 */
const SECTIONS: readonly Section[] = [
  { path: "/", label: "Home", icon: House, perspective: "global" },
  { path: "/decisions", label: "Inbox", icon: Inbox, perspective: "global", useBadge: useDecisionBadge, paths: ["/episodes"] },
  // Workspace: what an operator works with.
  { path: "/objects", label: "Records", icon: Rows3, perspective: "workspace", group: "Data", kind: "source", useMenu: useRecordsMenu, index: false },
  { path: "/metrics", label: "Metrics", icon: ChartLine, perspective: "workspace", group: "Analysis", kind: "question", useMenu: useQuestionsMenu },
  { path: "/dashboards", label: "Dashboards", icon: LayoutDashboard, perspective: "workspace", group: "Analysis" },
  { path: "/workflows", label: "Workflows", icon: Workflow, perspective: "workspace", group: "Operations", kind: "workflow", useMenu: useWorkflowsMenu, paths: ["/runs"] },
  { path: "/imports", label: "Import triage", icon: Columns2, perspective: "workspace", group: "Operations", useBadge: useImportTriageBadge },
  // Builder: how the model is made.
  { path: "/sources", label: "Sources", icon: Database, perspective: "builder", group: "Model", kind: "source", useMenu: useSourcesMenu, paths: ["/objects"] },
  { path: "/transforms", label: "Transforms", icon: Blend, perspective: "builder", group: "Model", kind: "transform" },
  { path: "/canvas", label: "Canvas", icon: Network, perspective: "builder", group: "Logic" },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, perspective: "builder", group: "Logic", kind: "guarantee", useMenu: useGuaranteesMenu },
  { path: "/markings", label: "Markings", icon: Fingerprint, perspective: "builder", group: "Logic" },
  { path: "/connections", label: "Connections", icon: Plug, perspective: "builder", group: "Settings" },
]

const FINANCE_SPACE = "Beltone · Financial services"

const FINANCE_SECTIONS: readonly Section[] = [
  { path: "/", label: "Home", icon: House, perspective: "global" },
  // Workspace: operator views under the Beltone · Financial services space.
  { path: "/objects", label: "Records", icon: Rows3, perspective: "workspace", group: FINANCE_SPACE, kind: "source", useMenu: financeRecordsMenu, index: false },
  { path: "/workflows", label: "Workflows", icon: Workflow, perspective: "workspace", group: FINANCE_SPACE, kind: "workflow", useMenu: financeWorkflowMenu },
  { path: "/metrics", label: "Metrics", icon: ChartLine, perspective: "workspace", group: FINANCE_SPACE, kind: "question", useMenu: financeMetricMenu },
  { path: "/optimisations/collections", label: "Optimisation", icon: ListChecks, perspective: "workspace", group: FINANCE_SPACE },
  // Builder: modelling, mappings, rules, policies, markings and examples.
  { path: "/sources", label: "Sources", icon: Database, perspective: "builder", group: FINANCE_SPACE, kind: "source", useMenu: financeSourceMenu },
  { path: "/model", label: "Model", icon: Network, perspective: "builder", group: FINANCE_SPACE, kind: "objectType" },
  { path: "/guarantees", label: "Rules", icon: ShieldCheck, perspective: "builder", group: FINANCE_SPACE, kind: "guarantee", useMenu: financeRuleMenu },
  { path: "/policies", label: "Policies", icon: SlidersHorizontal, perspective: "builder", group: FINANCE_SPACE, kind: "policy", useMenu: financePolicyMenu },
  { path: "/markings", label: "Markings", icon: Fingerprint, perspective: "builder", group: FINANCE_SPACE },
  { path: "/examples", label: "Examples", icon: FileScan, perspective: "builder", group: FINANCE_SPACE, useMenu: financeExampleMenu, index: false },
]

/**
 * Signed out (an explicit 401), a visitor sees only the sign-in page; signed in, the Studio.
 * A failed check keeps the last known account; with none, it offers a retry, never the sign-in page.
 */
function Root() {
  const account = useAccount()
  // Sidekick is offered only where a bridge serves it (a private deployment); elsewhere the rail shows no entry for it.
  const sidekick = useSidekickServed()
  if (account.data != null) {
    const brand = account.data.previewBrand
    const usesAccess = account.data.authentication === "cloudflare-access"
    const handleSignOut = () => {
      if (usesAccess) window.location.assign("/cdn-cgi/access/logout")
      else void signOut()
    }
    return (
      <Shell
        sections={financeDemo ? FINANCE_SECTIONS : SECTIONS}
        assistant={sidekick ? { title: "Sidekick", content: <Sidekick /> } : undefined}
        account={account.data}
        onSignOut={handleSignOut}
        tools={brand && (
          <span className="me-1 hidden min-w-0 items-center gap-1.5 rounded-md border bg-card px-2 py-1 text-[11px] whitespace-nowrap text-muted-foreground md:inline-flex">
            <span>Prepared for</span>
            <img src={brand.logoUrl} alt={brand.name} referrerPolicy="no-referrer" className="h-4 max-w-16 object-contain" />
          </span>
        )}
      />
    )
  }
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

const queryClient = new QueryClient()
const root = createRootRoute({ component: Root })
const router = createRouter({ routeTree: routeTree(root) })

// Dark and light are peers: follow the reader's system setting.
const dark = window.matchMedia("(prefers-color-scheme: dark)")
const applyScheme = () => document.documentElement.classList.toggle("dark", dark.matches)
applyScheme()
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
