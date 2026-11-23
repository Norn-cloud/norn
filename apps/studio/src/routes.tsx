import { createRoute, redirect, type AnyRootRoute, type RouteComponent } from "@tanstack/react-router"
import { QuestionBuilderPage } from "./builder.tsx"
import { HomePage } from "./home.tsx"
import { CanvasPage } from "./canvas.tsx"
import { ConnectionsPage } from "./connections.tsx"
import { QuestionTreePage } from "./question-tree.tsx"
import { DashboardBoardPage, DashboardEditorPage, DashboardsPage } from "./dashboards.tsx"
import { GuaranteesPage } from "./guarantees.tsx"
import { MarkingsPage } from "./markings.tsx"
import { FinanceModel, FinanceSources } from "./finance/workspace.tsx"
import { FinanceRecordRoute, FinanceRecordsListRoute } from "./finance/record-pages.tsx"
import { FinanceExamples } from "./finance/examples.tsx"
import { objectTab } from "./finance/definitions.tsx"
import { FinanceWorkflows } from "./finance/behavior-pages.tsx"
import { FinanceHome, FinanceMarkings, FinanceRules } from "./finance/operations.tsx"
import { FinanceOptimisation, FinancePolicyType } from "./finance/policies.tsx"
import { FinanceMetrics } from "./finance/metrics.tsx"
import { financeDemo } from "./lib/finance-demo.ts"
import { PAGES, type PagePath } from "./lib/pages.ts"
import { RecordsPage } from "./grid/records-page.tsx"
import { DecisionsPage } from "./decisions/inbox.tsx"
import { ImportTriagePage } from "./imports/import-triage-page.tsx"
import { RunPage } from "./runs/run-page.tsx"
import { EpisodePage } from "./learning/episode-page.tsx"
import { SourcesItemPage } from "./objects.tsx"
import { QuestionPage, QuestionsPage } from "./questions.tsx"
import { ObjectTypePage, SourcesPage } from "./sources.tsx"
import { TransformPage, TransformsPage } from "./transforms.tsx"
import { ToolBindings } from "./tool-bindings.tsx"
import { WorkflowsPage } from "./workflows.tsx"

const ConnectionsWithBindings = () => <ConnectionsPage><ToolBindings /></ConnectionsPage>

/** Every page's component: one for each entry of PAGES, and no other (checked by the type). */
export const COMPONENTS: { readonly [P in PagePath]: RouteComponent } = {
  "/": HomePage,
  "/sources": SourcesPage,
  "/sources/$concept": ObjectTypePage,
  "/sources/$concept/$": SourcesItemPage,
  "/objects/$type": RecordsPage,
  "/transforms": TransformsPage,
  "/transforms/$id": TransformPage,
  "/decisions": DecisionsPage,
  "/imports": ImportTriagePage,
  "/runs/$id": RunPage,
  "/episodes/$id": EpisodePage,
  "/metrics": QuestionsPage,
  "/metrics/new": QuestionBuilderPage,
  "/metrics/build": QuestionTreePage,
  "/metrics/$name": QuestionPage,
  "/dashboards": DashboardsPage,
  "/dashboards/new": DashboardEditorPage,
  "/dashboards/$id": DashboardBoardPage,
  "/dashboards/$id/edit": DashboardEditorPage,
  "/workflows": WorkflowsPage,
  "/canvas": CanvasPage,
  "/guarantees": GuaranteesPage,
  "/markings": MarkingsPage,
  "/connections": ConnectionsWithBindings,
}

/** Where an old address now lives: Questions became Metrics, and a saved link still opens its page. */
export const MOVED = { "/questions": "/metrics" } as const

/** `/questions/new#x` → `/metrics/new#x`: the same page under its new prefix, nothing else changed. */
export function movedTo(pathname: string): string | undefined {
  for (const [from, to] of Object.entries(MOVED)) if (pathname === from || pathname.startsWith(`${from}/`)) return `${to}${pathname.slice(from.length)}`
  return undefined
}

/** The app's routes under `root`: Home at "/", then every page, then the old addresses that redirect. */
export function routeTree<R extends AnyRootRoute>(root: R) {
  if (financeDemo) return root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/", component: FinanceHome }),
    createRoute({
      getParentRoute: () => root, path: "/sources", component: FinanceSources,
      validateSearch: (search: Record<string, unknown>) => ({ view: search.view === "graph" ? "graph" as const : undefined, model: search.model === "full" || search.model === "aggregates" ? search.model : undefined }),
    }),
    createRoute({
      getParentRoute: () => root, path: "/model", component: FinanceModel,
      validateSearch: (search: Record<string, unknown>) => ({ view: search.view === "catalog" || search.view === "full" || search.view === "borrower" || search.view === "aggregates" ? search.view : undefined }),
    }),
    createRoute({
      getParentRoute: () => root, path: "/sources/$concept", component: FinanceRecordRoute,
      validateSearch: (search: Record<string, unknown>) => ({ tab: search.tab ? objectTab(search.tab) : undefined }),
    }),
    createRoute({ getParentRoute: () => root, path: "/sources/$concept/$", component: FinanceRecordRoute }),
    createRoute({ getParentRoute: () => root, path: "/objects/$type", component: FinanceRecordsListRoute }),
    createRoute({ getParentRoute: () => root, path: "/metrics", component: FinanceMetrics }),
    createRoute({ getParentRoute: () => root, path: "/workflows", component: FinanceWorkflows }),
    createRoute({ getParentRoute: () => root, path: "/workflows/$process", component: FinanceWorkflows }),
    createRoute({ getParentRoute: () => root, path: "/guarantees", component: FinanceRules }),
    createRoute({ getParentRoute: () => root, path: "/markings", component: FinanceMarkings }),
    createRoute({ getParentRoute: () => root, path: "/policies", component: FinancePolicyType }),
    createRoute({ getParentRoute: () => root, path: "/policies/$scope", component: FinancePolicyType }),
    createRoute({ getParentRoute: () => root, path: "/optimisations/collections", component: FinanceOptimisation }),
    createRoute({ getParentRoute: () => root, path: "/examples", beforeLoad: () => { throw redirect({ href: "/examples/dataset", replace: true }) } }),
    createRoute({ getParentRoute: () => root, path: "/examples/$scene", component: FinanceExamples }),
    createRoute({ getParentRoute: () => root, path: "/$", beforeLoad: () => { throw redirect({ href: "/", replace: true }) } }),
  ])
  return root.addChildren([
    ...(Object.keys(PAGES) as PagePath[]).map((path) => createRoute({ getParentRoute: () => root, path, component: COMPONENTS[path] })),
    // One splat per old prefix: it matches the prefix itself and everything under it.
    ...Object.keys(MOVED).map((from) => createRoute({
      getParentRoute: () => root,
      path: `${from}/$`,
      beforeLoad: ({ location }) => { throw redirect({ href: `${movedTo(location.pathname) ?? "/"}${location.searchStr}${location.hash ? `#${location.hash}` : ""}`, replace: true }) },
    })),
  ])
}
