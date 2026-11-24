import { createRoute, redirect, type AnyRootRoute, type RouteComponent } from "@tanstack/react-router"
import { QuestionBuilderPage } from "./builder.tsx"
import { HomePage } from "./home.tsx"
import { CanvasPage } from "./canvas.tsx"
import { ConnectionsPage } from "./connections.tsx"
import { QuestionTreePage } from "./question-tree.tsx"
import { DashboardBoardPage, DashboardEditorPage, DashboardsPage } from "./dashboards.tsx"
import { RulesPage } from "./guarantees.tsx"
import { MarkingsPage } from "./markings.tsx"
import { PAGES, type PagePath } from "./lib/pages.ts"
import { RecordsPage } from "./grid/records-page.tsx"
import { DecisionsPage } from "./decisions/inbox.tsx"
import { ImportTriagePage } from "./imports/import-triage-page.tsx"
import { RunPage } from "./runs/run-page.tsx"
import { CasesPage } from "./learning/cases-page.tsx"
import { EpisodePage } from "./learning/episode-page.tsx"
import { EpisodesPage } from "./learning/episodes-page.tsx"
import { MisfitPage } from "./learning/misfit-page.tsx"
import { ProposalDetailPage, ProposalsPage } from "./learning/proposals-page.tsx"
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
  "/learning/episodes": EpisodesPage,
  "/episodes/$id": EpisodePage,
  "/learning/cases": CasesPage,
  "/learning/misfit": MisfitPage,
  "/learning/proposals": ProposalsPage,
  "/learning/proposals/$id": ProposalDetailPage,
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
  "/rules": RulesPage,
  "/markings": MarkingsPage,
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
