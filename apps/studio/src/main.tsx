import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { Link, Outlet, RouterProvider, createRootRoute, createRoute, createRouter, redirect, useRouterState } from "@tanstack/react-router"
import { ChartLine, Database, ShieldCheck, Workflow, type LucideIcon } from "lucide-react"
import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import {
  Sidebar, SidebarContent, SidebarGroup, SidebarGroupContent, SidebarHeader, SidebarInset,
  SidebarMenu, SidebarMenuButton, SidebarMenuItem, SidebarProvider, SidebarTrigger,
} from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"
import { QuestionsPage } from "./questions.tsx"
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
        <Sidebar>
          <SidebarHeader className="px-4 py-5">
            <span className="text-lg font-semibold tracking-tight">Norn Studio</span>
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
        </Sidebar>
        <SidebarInset>
          <header className="flex h-12 items-center border-b px-4"><SidebarTrigger /></header>
          <main className="mx-auto w-full max-w-6xl p-6 md:p-10"><Outlet /></main>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  )
}

function NotYet({ label, summary }: Section) {
  return (
    <section className="space-y-2">
      <h1 className="text-3xl font-semibold tracking-tight">{label}</h1>
      <p className="max-w-2xl text-muted-foreground">{summary}</p>
      <p className="pt-6 text-sm text-muted-foreground">This section is not available yet.</p>
    </section>
  )
}

const root = createRootRoute({ component: Shell })
const router = createRouter({
  routeTree: root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/", beforeLoad: () => { throw redirect({ to: "/questions" }) } }),
    ...SECTIONS.map((section) => createRoute({
      getParentRoute: () => root,
      path: section.path,
      component: section.path === "/questions" ? QuestionsPage : () => <NotYet {...section} />,
    })),
  ]),
})

createRoot(document.getElementById("root") as HTMLElement).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
)
