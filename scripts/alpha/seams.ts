import type { Runtime, Seam } from "./contract.ts";
export { definitions } from "./adapters/definitions.ts";
export { surfaces } from "./adapters/surfaces.ts";

// Pending is a visible skip, never alpha acceptance. No environment bypass.
export const runtime: Seam<Runtime> = {
  kind: "pending", reason: "lane A: disposable host and verified ledger replay adapter",
};
