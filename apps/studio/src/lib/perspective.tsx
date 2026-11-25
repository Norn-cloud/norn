/**
 * The lens a destination belongs to on the one workspace. Workspace is the operator's: home, inbox,
 * records, metrics, dashboards and workflows. Builder is the modeller's: sources and mappings,
 * transforms, the model, the canvas, rules, markings and connections. It sets only how dense the
 * content's rows are; it is never a permission boundary, and since the three-island shell the
 * applet rail is how a reader moves between them (DESIGN.md 9.1). The server decides every read
 * and action.
 */
export type Perspective = "workspace" | "builder"

/** The content's row density for a page in each lens (DESIGN.md 7); shared destinations read comfortably. */
export const DENSITY: Readonly<Record<Perspective | "global", "comfortable" | "compact">> = {
  workspace: "comfortable",
  builder: "compact",
  global: "comfortable",
}
