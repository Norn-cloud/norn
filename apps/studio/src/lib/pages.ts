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
  "/objects/$type": "/objects/canon.Item",
  "/transforms": "/transforms",
  "/transforms/$id": "/transforms/shop.revenue_by_region",
  "/decisions": "/decisions",
  "/runs/$id": "/runs/run_restock_7",
  "/episodes/$id": "/episodes/ep_demo_overridden",
  "/questions": "/questions",
  "/questions/new": "/questions/new",
  "/questions/build": "/questions/build",
  "/questions/$name": "/questions/demo.yield_week",
  "/dashboards": "/dashboards",
  "/dashboards/new": "/dashboards/new",
  "/dashboards/$id": "/dashboards/studio.dashboard.missing",
  "/dashboards/$id/edit": "/dashboards/studio.dashboard.missing/edit",
  "/workflows": "/workflows",
  "/canvas": "/canvas",
  "/guarantees": "/guarantees",
  "/connections": "/connections",
} as const

export type PagePath = keyof typeof PAGES

/**
 * Pages another route serves on purpose: an object type's URL is matched by
 * the record route's splat with nothing after it, and SourcesItemPage renders
 * the object type page for it.
 */
export const SERVED_BY: Partial<Record<PagePath, PagePath>> = { "/sources/$concept": "/sources/$concept/$" }
