import type { Page } from "@playwright/test"

/**
 * Helper to stub `GET /api/host/v1/definitions` with specific advertised learning object types
 * and provisional `publishedQuestions` capability metadata (#1220, PR #1631 S1).
 */
export interface AdvertisedLearning {
  readonly objectTypes?: readonly string[]
  readonly publishedQuestions?: readonly string[]
  readonly extraArtifacts?: readonly object[]
}

export const ALL_LEARNING_RESOURCES: AdvertisedLearning = {
  objectTypes: [
    "learning.Episode",
    "learning.CuratedCase",
    "learning.EvalSet",
    "learning.GapProposal",
  ],
  publishedQuestions: [
    "learning.misfit_by_law",
    "learning.triage_over_time",
  ],
}

export function learningCatalogPayload(advertised: AdvertisedLearning = {}) {
  const objectTypes = advertised.objectTypes ?? []
  const artifacts = [
    ...objectTypes.map((name) => ({
      artifactId: `${name}.entity`,
      kind: "entity",
      lifecycle: "active",
      name,
      version: "0.1.0",
      payload: {
        entityVersion: "2.0",
        objectType: { name, properties: [] },
      },
    })),
    ...(advertised.extraArtifacts ?? []),
  ]
  return {
    admitted: true,
    version: 1,
    artifacts,
    ...(advertised.publishedQuestions !== undefined ? { publishedQuestions: advertised.publishedQuestions } : {}),
  }
}

export async function advertiseLearning(page: Page, advertised: AdvertisedLearning = ALL_LEARNING_RESOURCES) {
  await page.route("**/api/host/v1/definitions**", (route) =>
    route.fulfill({ json: learningCatalogPayload(advertised) }),
  )
}
