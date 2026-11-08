import { expect, test } from "bun:test"
import { createHash } from "node:crypto"
import { resolve } from "node:path"
import { TOOL_ROOT } from "../src/profile.ts"
import pins from "./existing-cli-pins.json"

// Pins captured against main 2a6c17a7ae; the same outputs were verified on the tree with init/fmt.
test("existing_cli_output_unchanged", async () => {
  for (const pin of pins) {
    const child = Bun.spawn([process.execPath, "tools/norn-ts/src/cli.ts", pin.command, pin.input, "--org", "org_demo", "--env", "dev"], {
      cwd: resolve(TOOL_ROOT, "../.."), stdout: "pipe", stderr: "pipe",
    })
    const [stdout, stderr, status] = await Promise.all([new Response(child.stdout).arrayBuffer(), new Response(child.stderr).text(), child.exited])
    expect(status, `${pin.command} ${pin.input}: ${stderr}`).toBe(pin.status)
    expect(createHash("sha256").update(new Uint8Array(stdout)).digest("hex")).toBe(pin.sha256)
  }
}, 120000)
