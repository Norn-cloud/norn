import { test } from "bun:test";
import { runAlpha } from "../../../scripts/alpha/run.ts";
import { definitions, runtime, surfaces } from "../../../scripts/alpha/seams.ts";

const pending = [definitions, surfaces, runtime]
  .flatMap((seam) => seam.kind === "pending" ? [seam.reason] : []);

// Existing admitted norn-ts test target discovers this case. Missing adapters
// are visibly skipped; once all seams are ready no opt-in flag can suppress it.
test.skipIf(pending.length > 0)(`engine alpha #3762${pending.length ? `: ${pending.join("; ")}` : ""}`, async () => {
  if (definitions.kind !== "ready" || surfaces.kind !== "ready" || runtime.kind !== "ready") {
    throw new Error("alpha adapters became unavailable");
  }
  await runAlpha(definitions.adapter, surfaces.adapter, runtime.adapter);
}, 120_000);
