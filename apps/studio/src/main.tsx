import { AgentButton } from "./agent/panel.tsx"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRoute, createRouter, redirect } from "@tanstack/react-router"
import { ChartLine, Database, Gauge, Network, ShieldCheck, Workflow } from "lucide-react"
import { StrictMode } from "react"
import { Button } from "@/components/ui/button"
import { authApi, useAccount, useRefreshAccount } from "@/lib/session"
import { LoginPage } from "./login.tsx"
import { createRoot } from "react-dom/client"
import { EmptyState, Panel, Well } from "@/components/panel"
import { QuestionPage, QuestionsPage, useQuestionsMenu } from "./questions.tsx"
import { Shell, type Section } from "./shell.tsx"
import { ObjectTypePage, SourcesPage, useSourcesMenu } from "./sources.tsx"
import { StatusPage } from "./status.tsx"
import { GuaranteesPage, useGuaranteesMenu } from "./guarantees.tsx"
import { WorkflowsPage, useWorkflowsMenu } from "./workflows.tsx"
import { CanvasPage } from "./canvas.tsx"
import { SourcesItemPage } from "./objects.tsx"
import "./index.css"

const SECTIONS: readonly Section[] = [
  { path: "/sources", label: "Sources", icon: Database, summary: "Records from every connected system, mapped into one business model, with the rows held back and why.", kind: "source", useMenu: useSourcesMenu, group: "Model" },
  { path: "/questions", label: "Questions", icon: ChartLine, summary: "Business questions answered exactly, down to the records behind every number.", kind: "question", useMenu: useQuestionsMenu, group: "Model" },
  { path: "/workflows", label: "Workflows", icon: Workflow, summary: "How each business process moves, and which rules allow or refuse every step.", kind: "workflow", useMenu: useWorkflowsMenu, group: "Rules" },
  { path: "/canvas", label: "Canvas", icon: Network, summary: "Draw and change a process, with every edit checked as you make it.", group: "Rules" },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, summary: "The recorded law, and how strongly it is checked.", kind: "guarantee", useMenu: useGuaranteesMenu, group: "Rules" },
  { path: "/status", label: "Status", icon: Gauge, summary: "Where the alpha stands, area by area.", foot: true },
]

function NotYet({ label, summary, icon }: Section) {
  return (
    <section className="space-y-6">
      <div className="space-y-2">
        <h1 className="text-title font-medium">{label}</h1>
        <p className="max-w-2xl text-muted-foreground">{summary}</p>
      </div>
      <Panel icon={icon} label={label}>
        <Well><EmptyState icon={icon}>The server does not publish this section's documents yet.</EmptyState></Well>
      </Panel>
    </section>
  )
}

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
const router = createRouter({
  routeTree: root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/", beforeLoad: () => { throw redirect({ to: "/questions" }) } }),
    ...SECTIONS.map((section) => createRoute({
      getParentRoute: () => root,
      path: section.path,
      component: section.path === "/questions" ? QuestionsPage : section.path === "/sources" ? SourcesPage : section.path === "/workflows" ? WorkflowsPage : section.path === "/canvas" ? CanvasPage : section.path === "/guarantees" ? GuaranteesPage : section.path === "/status" ? StatusPage : () => <NotYet {...section} />,
    })),
    createRoute({ getParentRoute: () => root, path: "/sources/$concept", component: ObjectTypePage }),
    createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: SourcesItemPage }),
    createRoute({ getParentRoute: () => root, path: "/questions/$name", component: QuestionPage }),
  ]),
})

// Dark and light are peers: follow the reader's system setting.
const dark = window.matchMedia("(prefers-color-scheme: dark)")
const applyScheme = () => document.documentElement.classList.toggle("dark", dark.matches)
applyScheme()
dark.addEventListener("change", applyScheme)

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
)
