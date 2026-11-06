import { expect, test } from "bun:test"
import { cellStage, probeServesNewBuild, studioPointer } from "../scripts/celld-deploy-cell.ts"

test("a cell deploy targets one named stage with its own bucket", () => {
  expect(cellStage("next", "s3://norn-next-celld")).toEqual({
    stage: "next",
    origin: "https://next.norn.cloud",
    unit: "norn-next-celld",
    studio: "norn-studio-next",
    bucket: "s3://norn-next-celld",
  })
  expect(cellStage("demo", "s3://norn-demo-celld/demo").unit).toBe("norn-demo-celld")
  expect(() => cellStage(undefined, "s3://norn-next-celld")).toThrow("--stage")
  expect(() => cellStage("Next", "s3://norn-next-celld")).toThrow("--stage")
  expect(() => cellStage("prod", "s3://norn-prod-celld")).toThrow("production")
  // Another stage's credentials never deploy this stage's cell.
  expect(() => cellStage("next", "s3://norn-demo-celld/next")).toThrow("own bucket")
  expect(() => cellStage("next", undefined)).toThrow("own bucket")
})

test("the restored fleet application is the stage's own Studio", () => {
  const pointer = JSON.stringify({ script_name: "norn-studio-next", version: "74f29839da8d3301" })
  expect(studioPointer(pointer, "norn-studio-next")).toBe(pointer)
  expect(() => studioPointer(JSON.stringify({ script_name: "norn-cell" }), "norn-studio-next")).toThrow("not norn-studio-next")
})

test("the probe passes on any answer from the new build's route, and fails on the old build's 404", () => {
  const notFound = JSON.stringify({ code: "route.not_found" })
  expect(probeServesNewBuild(400, JSON.stringify({ code: "request.invalid" }))).toBe(true)
  expect(probeServesNewBuild(403, JSON.stringify({ code: "authz.denied" }))).toBe(true)
  expect(probeServesNewBuild(404, notFound)).toBe(false)
  expect(probeServesNewBuild(404, JSON.stringify({ code: "question.not_found" }))).toBe(true)
  expect(probeServesNewBuild(404, "not json")).toBe(false)
  expect(probeServesNewBuild(503, "{}")).toBe(false)
})
