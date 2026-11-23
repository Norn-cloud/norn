import { expect, test } from "@playwright/test"

test.skip(process.env.VITE_STUDIO_FINANCE_DEMO !== "1", "Requires the finance preview build")
test.use({ viewport: { width: 1440, height: 960 } })

for (const colorScheme of ["light", "dark"] as const) {
  test.describe(colorScheme, () => {
    test.use({ colorScheme })

    test.beforeEach(async ({ page }) => {
      await page.route("**/api/me", (route) => route.fulfill({ json: {
        user: { id: "u_e2e", name: "E2E", email: "viewer@example.com" },
        organization: { id: "org_demo", name: "Demo", role: "viewer" },
        environmentId: null,
        previewBrand: { name: "Partner", logoUrl: "/favicon-32.png" },
      } }))
    })

    test("the lifecycle opens with readable labels and working transition selection", async ({ page }) => {
      await page.goto("/workflows")
      for (const [phase, command, detail] of [
        ["Origination", "disburse", "Dual-approval threshold"],
        ["Servicing", "openHardship", "Loan.restructure"],
      ] as const) {
        await page.getByRole("button", { name: phase, exact: true }).click()
        const graph = page.getByLabel(`${phase} state graph`)
        const transition = graph.getByRole("button", { name: command, exact: true })
        await expect(transition).toBeVisible()
        await expect.poll(() => graph.evaluate((canvas) => {
          const viewport = canvas.querySelector(".react-flow__viewport")
          if (!viewport) return false
          const zoom = new DOMMatrixReadOnly(getComputedStyle(viewport).transform).a
          const titles = [...canvas.querySelectorAll(".react-flow__node .canvas-node span:last-child")]
          const labels = [...canvas.querySelectorAll(".react-flow__edgelabel-renderer button")]
          const renderedSize = (element: Element) => parseFloat(getComputedStyle(element).fontSize) * zoom
          const overlaps = (first: Element, second: Element) => {
            const a = first.getBoundingClientRect()
            const b = second.getBoundingClientRect()
            return a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
          }
          return {
            readable: titles.length > 0 && labels.length > 0
              && titles.every((title) => renderedSize(title) >= 12)
              && labels.every((label) => renderedSize(label) >= 11),
            overlaps: labels.flatMap((label, index) =>
              [...labels.slice(index + 1), ...titles].filter((other) => overlaps(label, other))
                .map((other) => [label.textContent, other.textContent]),
            ),
          }
        })).toEqual({ readable: true, overlaps: [] })
        await transition.click()
        await expect(page.getByLabel("Transition inspector").getByText(detail, { exact: true })).toBeVisible()
      }
    })

    test("Home shows the complete workspace name and the partner mark", async ({ page }) => {
      await page.goto("/")
      const name = page.locator("[data-scope]").getByText("Beltone · Financial services", { exact: true })
      await expect(name).toBeVisible()
      expect(await name.evaluate((element) => element.scrollWidth)).toBeLessThanOrEqual(await name.evaluate((element) => element.clientWidth))

      const logo = page.getByRole("img", { name: "Partner" })
      await expect(logo).toBeVisible()
      await expect.poll(() => logo.evaluate((image) => image instanceof HTMLImageElement && image.naturalWidth > 0)).toBe(true)
      if (colorScheme === "dark") {
        await expect(logo.locator("..")).toHaveCSS("background-color", "rgb(255, 255, 255)")
      }
    })
  })
}
