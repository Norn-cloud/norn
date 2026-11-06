import { AgentButton } from "./agent/panel.tsx"
import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { RouterProvider, createRootRoute, createRouter } from "@tanstack/react-router"
import { ChartLine, Database, Gauge, LayoutDashboard, ShieldCheck, Workflow } from "lucide-react"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { useGuaranteesMenu } from "./guarantees.tsx"
import { useQuestionsMenu } from "./questions.tsx"
import { routeTree } from "./routes.tsx"
import { Shell, type Section } from "./shell.tsx"
import { useSourcesMenu } from "./sources.tsx"
import { useWorkflowsMenu } from "./workflows.tsx"
import "./index.css"

const SECTIONS: readonly Section[] = [
  { path: "/sources", label: "Sources", icon: Database, summary: "Records from every connected system, mapped into one business model, with the rows held back and why.", kind: "source", useMenu: useSourcesMenu, group: "Model" },
  { path: "/questions", label: "Questions", icon: ChartLine, summary: "Business questions answered exactly, down to the records behind every number.", kind: "question", useMenu: useQuestionsMenu, group: "Model" },
  { path: "/dashboards", label: "Dashboards", icon: LayoutDashboard, summary: "Saved sets of questions, each a definition the cell checks before it is kept.", group: "Model" },
  { path: "/workflows", label: "Workflows", icon: Workflow, summary: "How each business process moves, and which rules allow or refuse every step.", kind: "workflow", useMenu: useWorkflowsMenu, group: "Rules" },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, summary: "The recorded law, and how strongly it is checked.", kind: "guarantee", useMenu: useGuaranteesMenu, group: "Rules" },
  { path: "/status", label: "Status", icon: Gauge, summary: "Where the alpha stands, area by area.", foot: true },
]

const root = createRootRoute({ component: () => <Shell sections={SECTIONS} tools={<AgentButton />} /> })
const router = createRouter({ routeTree: routeTree(root) })

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
