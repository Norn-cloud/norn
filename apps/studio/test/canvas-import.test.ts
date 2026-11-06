import { expect, test } from "bun:test"

// The canvas pages import under bun, without Vite: the checker package is found by Vite's import.meta.glob only
// when the checker loads (lib/canvas/checker.ts), never at import, so tests that walk every page need no stub.
test("the canvas pages and their checker loader import without Vite", async () => {
  const pages = await Promise.all([import("../src/canvas.tsx"), import("../src/automation-canvas.tsx"), import("../src/canvas-frame.tsx"), import("../src/agent-canvas.tsx"), import("../src/decision-tree.tsx")])
  expect(pages.map((page) => Object.keys(page).length > 0)).toEqual([true, true, true, true, true])
  const { loadChecker } = await import("../src/lib/canvas/checker.ts")
  await expect(loadChecker({})).rejects.toThrow("The checker is not part of this build")
  const { loadAgentChecker } = await import("../src/lib/canvas/checker.ts")
  await expect(loadAgentChecker({})).rejects.toThrow("The checker is not part of this build")
})
