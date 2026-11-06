/** A project Studio can open: its recorded documents, or a note that it has none yet. */
export interface Workspace {
  readonly id: string
  readonly name: string
  readonly group: "Class demos" | "Industry benchmarks" | "First-party"
  /** What the workspace shows, or why it shows nothing yet. */
  readonly detail: string
  /** Whether Studio has server documents recorded for it. Only recorded workspaces open. */
  readonly recorded: boolean
}

const NOT_YET = "No recorded run in Studio yet"

export const WORKSPACES: readonly Workspace[] = [
  { id: "synthetic", name: "Synthetic world", group: "Class demos", detail: "Recorded · scrap yield and three systems", recorded: true },
  // The benchmark harnesses (scripts/bench_*.py) fetch these datasets at test time; Studio has no recorded documents for them.
  { id: "olist", name: "Olist e-commerce", group: "Industry benchmarks", detail: NOT_YET, recorded: false },
  { id: "uci-online-retail", name: "UCI Online Retail", group: "Industry benchmarks", detail: NOT_YET, recorded: false },
  { id: "bpi-2019", name: "BPI Challenge 2019", group: "Industry benchmarks", detail: NOT_YET, recorded: false },
  { id: "jewelry", name: "Jewelry", group: "First-party", detail: NOT_YET, recorded: false },
]

export const WORKSPACE_GROUPS = [...new Set(WORKSPACES.map(({ group }) => group))]
