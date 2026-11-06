/** A project Studio can open: its recorded documents, or a note that it has none here. */
export interface Workspace {
  readonly id: string
  readonly name: string
  readonly group: "Class demos" | "Industry benchmarks" | "First-party"
  /** What the workspace shows, or why it shows nothing here. */
  readonly detail: string
  /** Whether Studio has server documents recorded for it. Only recorded workspaces open. */
  readonly recorded: boolean
  /** Where its results live instead, when they cannot be recorded in this public repository. */
  readonly results?: string
}

const NOT_YET = "No recorded run in Studio yet"

export const WORKSPACES: readonly Workspace[] = [
  { id: "class-demos", name: "Notebook demos", group: "Class demos", detail: "Coming soon", recorded: false },
  // Recorded runs of the two CC BY benchmarks are a later unit; their harnesses (scripts/bench_*.py) fetch the data at test time.
  { id: "uci-online-retail", name: "UCI Online Retail", group: "Industry benchmarks", detail: NOT_YET, recorded: false },
  { id: "bpi-2019", name: "BPI Challenge 2019", group: "Industry benchmarks", detail: NOT_YET, recorded: false },
  // CC BY-NC-SA: nothing derived from it is committed here.
  { id: "olist", name: "Olist e-commerce", group: "Industry benchmarks", detail: "Benchmark run offline (licence NC)", recorded: false, results: "https://github.com/Norn-cloud/norn/issues/531" },
  // The synthetic recorded world is the jewelry business: gold scrap, its yield and the three systems that feed it.
  { id: "jewelry", name: "Jewelry", group: "First-party", detail: "Recorded · synthetic gold-scrap data", recorded: true },
]

export const WORKSPACE_GROUPS = [...new Set(WORKSPACES.map(({ group }) => group))]
