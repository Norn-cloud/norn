import type { Definitions, Runtime, Seam, Surfaces } from "./contract.ts";

// Replace each pending seam with a real adapter when its lane lands.
// Pending is a visible skip, never alpha acceptance. No environment bypass.
export const definitions: Seam<Definitions> = {
  kind: "pending", reason: "lane B: authored fixture and versioned install adapter",
};
export const surfaces: Seam<Surfaces> = {
  kind: "pending", reason: "lane C: API/MCP/remote CLI and subscription adapter",
};
export const runtime: Seam<Runtime> = {
  kind: "pending", reason: "lane A: disposable host and verified ledger replay adapter",
};
