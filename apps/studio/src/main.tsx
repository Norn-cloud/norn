import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRouter } from "@tanstack/react-router"
import { Blend, ChartLine, Database, Fingerprint, House, Inbox, LayoutDashboard, Network, Plug, ShieldCheck, Workflow } from "lucide-react"
import { StrictMode } from "react"
import { Button } from "@/components/ui/button"
import { endSession, useAccount } from "@/lib/session"
import { LoginPage } from "./login.tsx"
import { oauthPage } from "./oauth.tsx"
import { createRoot } from "react-dom/client"
import { useDecisionBadge } from "./decisions/inbox.tsx"
import { useGuaranteesMenu } from "./guarantees.tsx"
import { useQuestionsMenu } from "./questions.tsx"
import { routeTree } from "./routes.tsx"
import { Sidekick, useSidekickServed } from "./sidekick/panel.tsx"
import { Shell, type Section } from "./shell.tsx"
import { financeDemo } from "./lib/finance-demo.ts"
import { useSourcesMenu } from "./sources.tsx"
import { useWorkflowsMenu } from "./workflows.tsx"
import "./index.css"

const SECTIONS: readonly Section[] = [
  { path: "/", label: "Home", icon: House, summary: "What needs you, and the numbers that matter.", group: "Work" },
  { path: "/decisions", label: "Decisions", icon: Inbox, summary: "Choices your flows wait on you to make, soonest deadline first.", useBadge: useDecisionBadge, group: "Work", paths: ["/episodes"], liveOnly: true },
  { path: "/sources", label: "Sources", icon: Database, summary: "Records from every connected system, mapped into one business model, with the rows held back and why.", kind: "source", useMenu: useSourcesMenu, group: "Model", paths: ["/objects"], action: { label: "Connect a source", to: "/connections", liveOnly: true } },
  { path: "/transforms", label: "Transforms", icon: Blend, summary: "Derived datasets the cell checks: typed columns, declared joins, every row traced to its records.", kind: "transform", group: "Model", liveOnly: true },
  { path: "/metrics", label: "Metrics", icon: ChartLine, summary: "The numbers your records add up to, answered exactly, down to the records behind every number.", kind: "question", useMenu: useQuestionsMenu, group: "Model", action: { label: "Explore a metric", to: "/metrics/new", liveOnly: true } },
  { path: "/dashboards", label: "Dashboards", icon: LayoutDashboard, summary: "Saved sets of questions, each a definition the cell checks before it is kept.", group: "Model", action: { label: "New dashboard", to: "/dashboards/new" }, liveOnly: true },
  { path: "/workflows", label: "Workflows", icon: Workflow, summary: "How each business process moves, and which rules allow or refuse every step.", kind: "workflow", useMenu: useWorkflowsMenu, group: "Rules", paths: ["/runs"], action: { label: "Open the canvas", to: "/canvas" } },
  { path: "/canvas", label: "Canvas", icon: Network, summary: "Draft and check changes to a process in your browser.", group: "Rules" },
  { path: "/markings", label: "Markings", icon: Fingerprint, summary: "Personal data: how each marking is shown, the facts that carry it, and who may reveal it.", group: "Rules", liveOnly: true },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, summary: "The recorded law, and how strongly it is checked.", kind: "guarantee", useMenu: useGuaranteesMenu, group: "Rules" },
  { path: "/connections", label: "Connections", icon: Plug, summary: "Apps and tools from your tool gateway, run through Norn's policy.", group: "System", liveOnly: true },
]

const FINANCE_SECTIONS: readonly Section[] = [
  { path: "/sources", label: "Sources", icon: Database, summary: "Mapped lending, brokerage, and payment systems.", kind: "source", group: "Model" },
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
    return <Shell sections={financeDemo ? FINANCE_SECTIONS : SECTIONS} assistant={sidekick ? { title: "Sidekick", content: <Sidekick /> } : undefined} account={account.data} onSignOut={() => {
        if (usesAccess) window.location.assign("/cdn-cgi/access/logout")
        else void signOut()
      }}
      tools={brand && <span className="me-1.5 hidden min-w-0 items-center gap-2 rounded-full border bg-card py-1 ps-2.5 pe-2 text-[10px] whitespace-nowrap text-muted-foreground md:inline-flex"><span>Prepared for</span><img src={brand.logoUrl} alt={brand.name} referrerPolicy="no-referrer" className="h-5 max-w-20 object-contain" /></span>} />
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
