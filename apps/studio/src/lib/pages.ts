/**
 * Every page Studio routes to, each with one concrete URL to open it at. The
 * route table must give each a component (a missing or extra page does not
 * compile); test/routes.test.tsx renders every one through the router, and
 * test/e2e/pages.ts opens every one in a browser on a deployment.
 */
export const PAGES = {
  "/sources": "/sources",
  "/sources/$concept": "/sources/canon.Item",
  "/sources/$concept/$": "/sources/canon.Item/demo/item/gold",
  "/questions": "/questions",
  "/questions/new": "/questions/new",
  "/questions/$name": "/questions/demo.yield_week",
  "/dashboards": "/dashboards",
  "/dashboards/new": "/dashboards/new",
  "/dashboards/$id": "/dashboards/studio.dashboard.missing",
  "/workflows": "/workflows",
  "/guarantees": "/guarantees",
  "/status": "/status",
} as const

export type PagePath = keyof typeof PAGES
