import { createRoute, redirect, type AnyRootRoute, type RouteComponent } from "@tanstack/react-router"
import { QuestionBuilderPage } from "./builder.tsx"
import { CanvasPage } from "./canvas.tsx"
import { ConnectionsPage } from "./connections.tsx"
import { QuestionTreePage } from "./question-tree.tsx"
import { DashboardBoardPage, DashboardEditorPage, DashboardsPage } from "./dashboards.tsx"
import { GuaranteesPage } from "./guarantees.tsx"
import { PAGES, type PagePath } from "./lib/pages.ts"
import { RecordsPage } from "./grid/records-page.tsx"
import { DecisionsPage } from "./decisions/inbox.tsx"
import { RunPage } from "./runs/run-page.tsx"
import { EpisodePage } from "./learning/episode-page.tsx"
import { SourcesItemPage } from "./objects.tsx"
import { QuestionPage, QuestionsPage } from "./questions.tsx"
import { ObjectTypePage, SourcesPage } from "./sources.tsx"
import { StatusPage } from "./status.tsx"
import { TransformPage, TransformsPage } from "./transforms.tsx"
import { ToolBindings } from "./tool-bindings.tsx"
import { WorkflowsPage } from "./workflows.tsx"

const ConnectionsWithBindings = () => <ConnectionsPage><ToolBindings /></ConnectionsPage>

/** Every page's component: one for each entry of PAGES, and no other (checked by the type). */
export const COMPONENTS: { readonly [P in PagePath]: RouteComponent } = {
  "/sources": SourcesPage,
  "/sources/$concept": ObjectTypePage,
  "/sources/$concept/$": SourcesItemPage,
  "/objects/$type": RecordsPage,
  "/transforms": TransformsPage,
  "/transforms/$id": TransformPage,
  "/decisions": DecisionsPage,
  "/runs/$id": RunPage,
  "/episodes/$id": EpisodePage,
  "/questions": QuestionsPage,
  "/questions/new": QuestionBuilderPage,
  "/questions/build": QuestionTreePage,
  "/questions/$name": QuestionPage,
  "/dashboards": DashboardsPage,
  "/dashboards/new": DashboardEditorPage,
  "/dashboards/$id": DashboardBoardPage,
  "/dashboards/$id/edit": DashboardEditorPage,
  "/workflows": WorkflowsPage,
  "/canvas": CanvasPage,
  "/guarantees": GuaranteesPage,
  "/connections": ConnectionsWithBindings,
  "/status": StatusPage,
}

/** The app's routes under `root`: "/" opens Questions, then every page. */
export function routeTree<R extends AnyRootRoute>(root: R) {
  return root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/", beforeLoad: () => { throw redirect({ to: "/questions" }) } }),
    ...(Object.keys(PAGES) as PagePath[]).map((path) => createRoute({ getParentRoute: () => root, path, component: COMPONENTS[path] })),
  ])
}
