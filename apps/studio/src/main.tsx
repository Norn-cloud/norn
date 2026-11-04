import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRoute, createRouter, redirect } from "@tanstack/react-router"
import { ChartLine, Database, ShieldCheck, Workflow } from "lucide-react"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { EmptyState, Panel, Well } from "@/components/panel"
import { QuestionsPage, useQuestionsMenu } from "./questions.tsx"
import { Shell, type Section } from "./shell.tsx"
import { SourcesPage, useSourcesMenu } from "./sources.tsx"
import { WorkflowsPage, useWorkflowsMenu } from "./workflows.tsx"
import "./index.css"

const SECTIONS: readonly Section[] = [
  { path: "/sources", label: "Sources", icon: Database, summary: "Records from every connected system, mapped into one business model, with the rows held back and why.", useMenu: useSourcesMenu },
  { path: "/questions", label: "Questions", icon: ChartLine, summary: "Business questions answered exactly, down to the records behind every number.", useMenu: useQuestionsMenu },
  { path: "/workflows", label: "Workflows", icon: Workflow, summary: "How each business process moves, and which rules allow or refuse every step.", useMenu: useWorkflowsMenu },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, summary: "Every rule the system keeps, and how strongly each one is checked." },
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
      component: section.path === "/questions" ? QuestionsPage : section.path === "/sources" ? SourcesPage : section.path === "/workflows" ? WorkflowsPage : () => <NotYet {...section} />,
    })),
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
