import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { Link, Outlet, RouterProvider, createRootRoute, createRoute, createRouter, redirect, useRouterState } from "@tanstack/react-router"
import { ChartLine, Database, ShieldCheck, Workflow, type LucideIcon } from "lucide-react"
import { StrictMode, type JSX } from "react"
import { createRoot } from "react-dom/client"
import {
  Sidebar, SidebarContent, SidebarGroup, SidebarGroupContent, SidebarHeader, SidebarInset,
  SidebarMenu, SidebarMenuButton, SidebarMenuItem, SidebarProvider, SidebarRail, SidebarTrigger,
} from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"
import { QuestionsPage } from "./questions.tsx"
import { WorkflowsPage } from "./workflows.tsx"
import "./index.css"

interface Section {
  readonly path: string
  readonly label: string
  readonly icon: LucideIcon
  readonly summary: string
}

const SECTIONS: readonly Section[] = [
  { path: "/sources", label: "Sources", icon: Database, summary: "Records from every connected system, mapped into one business model, with the rows held back and why." },
  { path: "/questions", label: "Questions", icon: ChartLine, summary: "Business questions answered exactly, down to the records behind every number." },
  { path: "/workflows", label: "Workflows", icon: Workflow, summary: "How each business process moves, and which rules allow or refuse every step." },
  { path: "/guarantees", label: "Guarantees", icon: ShieldCheck, summary: "Every rule the system keeps, and how strongly each one is checked." },
]

function Shell() {
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  return (
    <TooltipProvider>
      <SidebarProvider>
        <Sidebar variant="inset" collapsible="icon">
          <SidebarHeader>
            <SidebarMenu>
              <SidebarMenuItem>
                <SidebarMenuButton size="lg" asChild>
                  <Link to="/questions">
                    <span className="flex aspect-square size-8 items-center justify-center rounded-md bg-primary text-sm font-semibold text-primary-foreground">N</span>
                    <span className="text-panel font-medium">Norn Studio</span>
                  </Link>
                </SidebarMenuButton>
              </SidebarMenuItem>
            </SidebarMenu>
          </SidebarHeader>
          <SidebarContent>
            <SidebarGroup>
              <SidebarGroupContent>
                <SidebarMenu>
                  {SECTIONS.map(({ path, label, icon: Icon }) => (
                    <SidebarMenuItem key={path}>
                      <SidebarMenuButton asChild isActive={pathname === path} tooltip={label}>
                        <Link to={path}><Icon /><span>{label}</span></Link>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          </SidebarContent>
          <SidebarRail />
        </Sidebar>
        <SidebarInset>
          <header className="flex h-12 items-center gap-2 border-b px-4">
            <SidebarTrigger />
            <span className="text-meta text-muted-foreground">{SECTIONS.find(({ path }) => path === pathname)?.label}</span>
          </header>
          <main className="mx-auto w-full max-w-6xl p-6 md:p-10"><Outlet /></main>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  )
}

function NotYet({ label, summary }: Section) {
  return (
    <section className="space-y-2">
      <h1 className="text-title font-medium">{label}</h1>
      <p className="max-w-2xl text-muted-foreground">{summary}</p>
      <p className="pt-6 text-sm text-muted-foreground">This section is not available yet.</p>
    </section>
  )
}

const PAGES: Readonly<Record<string, () => JSX.Element>> = { "/questions": QuestionsPage, "/workflows": WorkflowsPage }

const root = createRootRoute({ component: Shell })
const router = createRouter({
  routeTree: root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/", beforeLoad: () => { throw redirect({ to: "/questions" }) } }),
    ...SECTIONS.map((section) => createRoute({
      getParentRoute: () => root,
      path: section.path,
      component: PAGES[section.path] ?? (() => <NotYet {...section} />),
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
