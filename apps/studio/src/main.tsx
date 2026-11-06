import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRoute, createRouter, redirect } from "@tanstack/react-router"
import { ChartLine, Database, Gauge, Network, ShieldCheck, Workflow } from "lucide-react"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { EmptyState, Panel, Well } from "@/components/panel"
import { QuestionPage, QuestionsPage, useQuestionsMenu } from "./questions.tsx"
import { Shell, type Section } from "./shell.tsx"
import { ObjectTypePage, SourcesPage, useSourcesMenu } from "./sources.tsx"
import { StatusPage } from "./status.tsx"
import { GuaranteesPage, useGuaranteesMenu } from "./guarantees.tsx"
import { WorkflowsPage, useWorkflowsMenu } from "./workflows.tsx"
import { CanvasPage } from "./canvas.tsx"
import { ObjectPage } from "./objects.tsx"
import "./index.css"

const SECTIONS: readonly Section[] = [
  { path: "/sources", label: "Sources", icon: Database, summary: "Records from every connected system, mapped into one business model, with the rows held back and why.", useMenu: useSourcesMenu },
  { path: "/questions", label: "Questions", icon: ChartLine, summary: "Business questions answered exactly, down to the records behind every number.", useMenu: useQuestionsMenu },
  { path: "/workflows", label: "Workflows", icon: Workflow, summary: "How each business process moves, and which rules allow or refuse every step.", useMenu: useWorkflowsMenu },
  { path: "/canvas", label: "Canvas", icon: Network, summary: "Draw and change a process, with every edit checked as you make it." },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, summary: "The recorded law, and how strongly it is checked.", useMenu: useGuaranteesMenu },
  { path: "/status", label: "Status", icon: Gauge, summary: "Where the alpha stands, area by area." },
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

const root = createRootRoute({ component: () => <Shell sections={SECTIONS} /> })
const router = createRouter({
  routeTree: root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/", beforeLoad: () => { throw redirect({ to: "/questions" }) } }),
    ...SECTIONS.map((section) => createRoute({
      getParentRoute: () => root,
      path: section.path,
      component: section.path === "/questions" ? QuestionsPage : section.path === "/sources" ? SourcesPage : section.path === "/workflows" ? WorkflowsPage : section.path === "/canvas" ? CanvasPage : section.path === "/guarantees" ? GuaranteesPage : section.path === "/status" ? StatusPage : () => <NotYet {...section} />,
    })),
    createRoute({ getParentRoute: () => root, path: "/sources/$concept", component: ObjectTypePage }),
    createRoute({ getParentRoute: () => root, path: "/sources/$concept/$id", component: ObjectPage }),
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
