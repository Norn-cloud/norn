import type { Seam, Surfaces } from "../contract.ts";

// Lane C replaces this with real API/MCP/remote CLI and subscription calls.
export const surfaces: Seam<Surfaces> = {
  kind: "pending", reason: "lane C: API/MCP/remote CLI and subscription adapter",
};
