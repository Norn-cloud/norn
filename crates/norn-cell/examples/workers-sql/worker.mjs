import { initSync, exercise } from "./pkg/workers_sql.js";
import module from "./pkg/workers_sql_bg.wasm";
initSync({ module });

export class Probe {
  constructor(ctx) { this.ctx = ctx; }
  async fetch(request) {
    if (new URL(request.url).pathname === "/state") {
      return Response.json(Array.from(this.ctx.storage.sql.exec("SELECT * FROM probe ORDER BY id").raw(),
        row => row.map(value => value instanceof ArrayBuffer ? Array.from(new Uint8Array(value)) : value)));
    }
    // The alarm write and synchronous Rust transaction share the outer commit.
    return this.ctx.storage.transaction(async txn => {
      const result = exercise(txn);
      await txn.setAlarm(Date.now() + 60000);
      return new Response(result);
    });
  }
  async alarm() { await this.ctx.storage.deleteAlarm(); }
}
export default {
  fetch(request, env) { return env.CELLS.get(env.CELLS.idFromName("adapter-conformance")).fetch(request); }
};
