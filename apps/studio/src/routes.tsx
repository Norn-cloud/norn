import { createRoute, redirect, type AnyRootRoute, type RouteComponent } from "@tanstack/react-router"
import { QuestionBuilderPage } from "./builder.tsx"
import { QuestionTreePage } from "./question-tree.tsx"
import { DashboardEditorPage, DashboardsPage } from "./dashboards.tsx"
import { GuaranteesPage } from "./guarantees.tsx"
import { PAGES, type PagePath } from "./lib/pages.ts"
import { SourcesItemPage } from "./objects.tsx"
import { QuestionPage, QuestionsPage } from "./questions.tsx"
import { ObjectTypePage, SourcesPage } from "./sources.tsx"
import { StatusPage } from "./status.tsx"
import { WorkflowsPage } from "./workflows.tsx"

/** Every page's component: one for each entry of PAGES, and no other (checked by the type). */
export const COMPONENTS: { readonly [P in PagePath]: RouteComponent } = {
  "/sources": SourcesPage,
  "/sources/$concept": ObjectTypePage,
  "/sources/$concept/$": SourcesItemPage,
  "/questions": QuestionsPage,
  "/questions/new": QuestionBuilderPage,
  "/questions/build": QuestionTreePage,
  "/questions/$name": QuestionPage,
  "/dashboards": DashboardsPage,
  "/dashboards/new": DashboardEditorPage,
  "/dashboards/$id": DashboardEditorPage,
  "/workflows": WorkflowsPage,
  "/guarantees": GuaranteesPage,
  "/status": StatusPage,
}

/** The app's routes under `root`: "/" opens Questions, then every page. */
export function routeTree<R extends AnyRootRoute>(root: R) {
  return root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/", beforeLoad: () => { throw redirect({ to: "/questions" }) } }),
    ...(Object.keys(PAGES) as PagePath[]).map((path) => createRoute({ getParentRoute: () => root, path, component: COMPONENTS[path] })),
  ])
}
