import { createRoute, lazyRouteComponent, redirect, type AnyRootRoute, type RouteComponent } from "@tanstack/react-router"
import { HomePage } from "./home.tsx"
import { PAGES, type PagePath } from "./lib/pages.ts"

const ConnectionsWithBindings = lazyRouteComponent(async () => {
  const [{ ConnectionsPage }, { ToolBindings }] = await Promise.all([
    import("./connections.tsx"),
    import("./tool-bindings.tsx"),
  ])
  return { default: () => <ConnectionsPage><ToolBindings /></ConnectionsPage> }
})

/** Every page's component: one for each entry of PAGES, and no other (checked by the type). */
export const COMPONENTS: { readonly [P in PagePath]: RouteComponent } = {
  "/": HomePage,
  "/sources": lazyRouteComponent(() => import("./sources.tsx"), "SourcesPage"),
  "/sources/$concept": lazyRouteComponent(() => import("./sources.tsx"), "ObjectTypePage"),
  "/sources/$concept/$": lazyRouteComponent(() => import("./record/record-page.tsx"), "SourcesItemPage"),
  "/objects/$type": lazyRouteComponent(() => import("./grid/records-page.tsx"), "RecordsPage"),
  "/transforms": lazyRouteComponent(() => import("./transforms.tsx"), "TransformsPage"),
  "/transforms/$id": lazyRouteComponent(() => import("./transforms.tsx"), "TransformPage"),
  "/decisions": lazyRouteComponent(() => import("./decisions/inbox.tsx"), "DecisionsPage"),
  "/imports": lazyRouteComponent(() => import("./imports/import-triage-page.tsx"), "ImportTriagePage"),
  "/runs/$id": lazyRouteComponent(() => import("./runs/run-page.tsx"), "RunPage"),
  "/learning/episodes": lazyRouteComponent(() => import("./learning/episodes-page.tsx"), "EpisodesPage"),
  "/episodes/$id": lazyRouteComponent(() => import("./learning/episode-page.tsx"), "EpisodePage"),
  "/learning/cases": lazyRouteComponent(() => import("./learning/cases-page.tsx"), "CasesPage"),
  "/learning/misfit": lazyRouteComponent(() => import("./learning/misfit-page.tsx"), "MisfitPage"),
  "/learning/proposals": lazyRouteComponent(() => import("./learning/proposals-page.tsx"), "ProposalsPage"),
  "/learning/proposals/$id": lazyRouteComponent(() => import("./learning/proposals-page.tsx"), "ProposalDetailPage"),
  "/metrics": lazyRouteComponent(() => import("./questions.tsx"), "QuestionsPage"),
  "/metrics/new": lazyRouteComponent(() => import("./builder.tsx"), "QuestionBuilderPage"),
  "/metrics/build": lazyRouteComponent(() => import("./question-tree.tsx"), "QuestionTreePage"),
  "/metrics/$name": lazyRouteComponent(() => import("./questions.tsx"), "QuestionPage"),
  "/dashboards": lazyRouteComponent(() => import("./dashboards.tsx"), "DashboardsPage"),
  "/dashboards/new": lazyRouteComponent(() => import("./dashboards.tsx"), "DashboardEditorPage"),
  "/dashboards/$id": lazyRouteComponent(() => import("./dashboards.tsx"), "DashboardBoardPage"),
  "/dashboards/$id/edit": lazyRouteComponent(() => import("./dashboards.tsx"), "DashboardEditorPage"),
  "/views": lazyRouteComponent(() => import("./views.tsx"), "ViewsPage"),
  "/views/new": lazyRouteComponent(() => import("./views.tsx"), "ViewEditorPage"),
  "/views/$id": lazyRouteComponent(() => import("./views.tsx"), "ViewDetailPage"),
  "/views/$id/edit": lazyRouteComponent(() => import("./views.tsx"), "ViewEditorPage"),
  "/workflows": lazyRouteComponent(() => import("./workflows.tsx"), "WorkflowsPage"),
  "/map": lazyRouteComponent(() => import("./semantic-map.tsx"), "SemanticMapPage"),
  "/canvas": lazyRouteComponent(() => import("./canvas.tsx"), "CanvasPage"),
  "/rules": lazyRouteComponent(() => import("./guarantees.tsx"), "RulesPage"),
  "/markings": lazyRouteComponent(() => import("./markings.tsx"), "MarkingsPage"),
  "/connections": ConnectionsWithBindings,
}

/** Where an old address now lives: Questions became Metrics, Guarantees became Rules; a saved link still opens its page. */
export const MOVED = { "/questions": "/metrics", "/guarantees": "/rules" } as const

/** `/questions/new#x` → `/metrics/new#x`: the same page under its new prefix, nothing else changed. */
export function movedTo(pathname: string): string | undefined {
  for (const [from, to] of Object.entries(MOVED)) if (pathname === from || pathname.startsWith(`${from}/`)) return `${to}${pathname.slice(from.length)}`
  return undefined
}

/** The app's routes under `root`: Home at "/", then every page, then the old addresses that redirect. */
export function routeTree<R extends AnyRootRoute>(root: R) {
  return root.addChildren([
    ...(Object.keys(PAGES) as PagePath[]).map((path) => createRoute({ getParentRoute: () => root, path, component: COMPONENTS[path] })),
    // One splat per old prefix: it matches the prefix itself and everything under it.
    ...Object.keys(MOVED).map((from) => createRoute({
      getParentRoute: () => root,
      path: `${from}/$`,
      beforeLoad: ({ location }) => {
        const hash = location.hash ? (location.hash.startsWith("#") ? location.hash : `#${location.hash}`) : ""
        throw redirect({ href: `${movedTo(location.pathname) ?? "/"}${location.searchStr}${hash}`, replace: true })
      },
    })),
  ])
}
