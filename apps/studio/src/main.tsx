import { AgentButton } from "./agent/panel.tsx"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database, Inbox, LayoutDashboard, Network, Plug, ShieldCheck, Workflow } from "lucide-react"
import { StrictMode } from "react"
import { Button } from "@/components/ui/button"
import { authApi, useAccount, useRefreshAccount } from "@/lib/session"
import { LoginPage } from "./login.tsx"
import { oauthPage } from "./oauth.tsx"
import { createRoot } from "react-dom/client"
import { useDecisionBadge } from "./decisions/inbox.tsx"
import { useGuaranteesMenu } from "./guarantees.tsx"
import { useQuestionsMenu } from "./questions.tsx"
import { routeTree } from "./routes.tsx"
import { Shell, type Section } from "./shell.tsx"
import { useSourcesMenu } from "./sources.tsx"
import { useWorkflowsMenu } from "./workflows.tsx"
import "./index.css"

const SECTIONS: readonly Section[] = [
  { path: "/decisions", label: "Decisions", icon: Inbox, summary: "Choices your flows wait on you to make, soonest deadline first.", useBadge: useDecisionBadge, group: "Work" },
  { path: "/sources", label: "Sources", icon: Database, summary: "Records from every connected system, mapped into one business model, with the rows held back and why.", kind: "source", useMenu: useSourcesMenu, group: "Model" },
  { path: "/questions", label: "Questions", icon: ChartLine, summary: "Business questions answered exactly, down to the records behind every number.", kind: "question", useMenu: useQuestionsMenu, group: "Model" },
  { path: "/dashboards", label: "Dashboards", icon: LayoutDashboard, summary: "Saved sets of questions, each a definition the cell checks before it is kept.", group: "Model" },
  { path: "/workflows", label: "Workflows", icon: Workflow, summary: "How each business process moves, and which rules allow or refuse every step.", kind: "workflow", useMenu: useWorkflowsMenu, group: "Rules" },
  { path: "/canvas", label: "Canvas", icon: Network, summary: "Draw and change a process, with every edit checked as you make it.", group: "Rules" },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, summary: "The recorded law, and how strongly it is checked.", kind: "guarantee", useMenu: useGuaranteesMenu, group: "Rules" },
  { path: "/connections", label: "Connections", icon: Plug, summary: "Apps and tools from your tool gateway, run through Norn's policy.", group: "System" },
]

/**
 * Signed out (an explicit 401), a visitor sees only the sign-in page; signed in, the Studio.
 * A failed check keeps the last known account; with none, it offers a retry, never the sign-in page.
 */
function Root() {
  const account = useAccount()
  const refresh = useRefreshAccount()
  if (account.data != null) return <Shell sections={SECTIONS} tools={<AgentButton />} account={account.data} onSignOut={() => void signOut(refresh)} />
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

/** Sign out, and forget the account only once the server has ended the session. */
async function signOut(refresh: () => Promise<void>) {
  try {
    await authApi.signOut()
  } catch {
    window.alert("Sign-out did not complete; you are still signed in. Try again.")
    return
  }
  await refresh()
}

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
      <QueryClientProvider client={new QueryClient()}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    ) : <OAuthPage />}
  </StrictMode>,
)
