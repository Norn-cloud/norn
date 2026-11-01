import type { Definitions, Seam } from "../contract.ts";

// Lane B replaces this with the real compile/check/versioned-install adapter.
export const definitions: Seam<Definitions> = {
  kind: "pending", reason: "lane B: authored fixture and versioned install adapter",
};
