import { initSync, exercise } from "./pkg/workers_sql.js";
import module from "./pkg/workers_sql_bg.wasm";
initSync({ module });

export class Probe {
  constructor(ctx) { this.ctx = ctx; }
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/alarm-refusal") {
      try {
        await this.ctx.storage.transaction(async txn => {
          txn.sql.exec("INSERT INTO probe VALUES (4,'alarm-refused',NULL)").toArray();
          await txn.setAlarm(Date.now() + 7200000);
          throw new Error("refuse after alarm");
        });
      } catch (error) {
        if (error.message !== "refuse after alarm") throw error;
        return new Response("refused", { status: 422 });
      }
      throw new Error("refusal committed");
    }
    if (path === "/state") {
      return Response.json({ alarm: await this.ctx.storage.getAlarm(), rows: Array.from(this.ctx.storage.sql.exec("SELECT * FROM probe ORDER BY id").raw(),
        row => row.map(value => value instanceof ArrayBuffer || ArrayBuffer.isView(value) ? Array.from(new Uint8Array(value)) : value)) });
    }
    // The alarm write and synchronous Rust transaction share the outer commit.
    return this.ctx.storage.transaction(async txn => {
      const result = exercise(txn);
      await txn.setAlarm(Date.now() + 3600000);
      return new Response(result);
    });
  }
  async alarm() { await this.ctx.storage.deleteAlarm(); }
}
export default {
  fetch(request, env) { return env.CELLS.get(env.CELLS.idFromName("adapter-conformance")).fetch(request); }
};
