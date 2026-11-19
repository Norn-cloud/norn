// Host conformance entry (D122, #1130, #786): the production cell Worker (`../worker.mjs`)
// unchanged, plus seams that exist only in this entry and only in a `fixtures` build:
//
// - a fixed request clock (`x-norn-conformance-now`), so receipts and ledger heads are
//   byte-comparable across runtimes;
// - a hold at a storage boundary (`x-norn-conformance-hold`): the object spins at the
//   n-th SQL statement, right after its synchronous transaction returns, or one timer
//   turn later, and the conformance runner kills the host there;
// - the cell's verified ledger head (`/__conformance/head`) and per-request storage
//   counts (`/__conformance/stats`);
// - `Probe` objects for the Durable Object API properties the cell relies on.
//
// Nothing here changes what the cell decides: requests reach the same Rust dispatch.
//
// The entry serves nothing unless it is opted in: a `fixtures` build (the Wasm has the
// ledger-head accessor) AND `NORN_STAGE=test` AND `NORN_CONFORMANCE=synthetic`. Opted in, it
// serves only the synthetic fixture scope (org_demo/analytics) and its probe objects; anything
// else is refused. The production deploy (apps/studio/scripts/celld-deploy-cell.ts) writes its
// own config with neither variable and refuses a fixtures build, so this entry never serves there.
import router, { Cell as ProductionCell } from "./worker.mjs";
import { NornCell } from "./pkg/norn_cell_do.js";
import { initSync as initProbe, exercise } from "./probe-pkg/workers_sql.js";
import probeModule from "./probe-pkg/workers_sql_bg.wasm";
initProbe({ module: probeModule });

const NOW_HEADER = "x-norn-conformance-now";
const HOLD_HEADER = "x-norn-conformance-hold";
const MARKER_HEADER = "x-norn-conformance-marker";
const OBJECT_PARAM = "conformanceObject";
const INSTANCE = crypto.randomUUID();
// The only scope this entry serves: the reference's fixture scope.
const SYNTHETIC = ["org_demo", "analytics"];
// A runner's out-of-band marker: printed when a hold is reached, before the object spins there.
const MARKER = /^[0-9a-f]{16,64}$/;

/** Whether this Wasm is a `fixtures` build: the head accessor exists only there (and the build says so, where it can). */
export function fixturesBuild(cell = NornCell) {
  if (typeof cell?.prototype?.ledgerHead !== "function") return false;
  return typeof cell.fixturesEnabled === "function" ? cell.fixturesEnabled() === true : true;
}

/** The explicit opt-in: a fixtures build on the test stage, with the conformance switch set. */
export function conformanceEnabled(env, cell = NornCell) {
  return env?.NORN_STAGE === "test" && env?.NORN_CONFORMANCE === "synthetic" && fixturesBuild(cell);
}

const isSynthetic = (scope) => Array.isArray(scope) && scope.length === 2 && scope[0] === SYNTHETIC[0] && scope[1] === SYNTHETIC[1];

function refuse(code, message) {
  return Response.json({ format: "norn/refusal@2", retryable: false, reason: null, code, category: "permission", message, explanation: "a conformance build serves only an opted-in test stage and its synthetic scope" }, { status: 403 });
}

/** Print the runner's marker for a reached hold (stderr, written before the spin starts). */
function mark(state) {
  state.reached = true;
  if (state.marker !== null) console.error(`norn-conformance-hold-reached ${state.marker}`);
}

/** Spin the runtime thread: forever, or for `ms` of wall time where the clock moves. */
function spin(ms) {
  const started = Date.now();
  // A clock frozen inside a synchronous call (workerd, celld) never ends a timed spin, so a timed
  // spin is also bounded by work: about a million loop iterations per millisecond.
  const bound = ms === undefined ? Infinity : ms * 1e6;
  for (let i = 0; i < bound && (ms === undefined || Date.now() - started < ms); i++);
}

/** The hold a request armed: {at: "stmt"|"turn-end"|"post-turn", n?, ms?}. */
function parseHold(value) {
  if (value === null) return null;
  const [at, arg, ms] = value.split(":");
  if (at === "stmt") return { at, n: Number(arg), ms: ms === undefined ? undefined : Number(ms) };
  if (at === "turn-end" || at === "post-turn") return { at, ms: arg === undefined ? undefined : Number(arg) };
  throw new Error(`unknown hold ${value}`);
}

/** `ctx` with its storage counted, and held where the current request asked. */
function instrument(ctx, state) {
  const bind = (target) => (value) => (typeof value === "function" ? value.bind(target) : value);
  const sql = new Proxy(ctx.storage.sql, {
    get(target, prop) {
      if (prop === "exec") {
        return (...args) => {
          state.statements += 1;
          if (state.depth > 0) state.inTransaction += 1;
          const hold = state.hold;
          if (hold?.at === "stmt" && state.depth > 0 && state.inTransaction === hold.n) {
            mark(state);
            spin(hold.ms);
          }
          return target.exec(...args);
        };
      }
      return bind(target)(Reflect.get(target, prop, target));
    },
  });
  const storage = new Proxy(ctx.storage, {
    get(target, prop) {
      if (prop === "sql") return sql;
      if (prop === "transactionSync") {
        return (callback) => {
          state.depth += 1;
          state.transactions += 1;
          try {
            return target.transactionSync(callback);
          } finally {
            state.depth -= 1;
            if (state.depth === 0 && state.hold?.at === "turn-end") {
              mark(state);
              spin(state.hold.ms);
            }
          }
        };
      }
      return bind(target)(Reflect.get(target, prop, target));
    },
  });
  return new Proxy(ctx, {
    get(target, prop) {
      if (prop === "storage") return storage;
      return bind(target)(Reflect.get(target, prop, target));
    },
  });
}

export class Cell extends ProductionCell {
  constructor(ctx, env) {
    const state = { statements: 0, inTransaction: 0, transactions: 0, depth: 0, hold: null, marker: null, reached: false };
    super(instrument(ctx, state), env);
    this.counts = state;
    this.instance = crypto.randomUUID();
    this.constructedAt = Date.now();
  }
  serve(request, body) {
    const now = Number(request.headers.get(NOW_HEADER));
    if (request.headers.get(NOW_HEADER) === null || !Number.isFinite(now)) return super.serve(request, body);
    const url = new URL(request.url);
    return this.cell.fetch(request.method, url.pathname, url.search.slice(1), request.headers.get("authorization") ?? undefined, now, body);
  }
  identity() {
    return { instance: this.instance, isolate: INSTANCE, pid: globalThis.process?.pid ?? null, constructedAt: this.constructedAt };
  }
  async fetch(request) {
    // Refused before anything opens: not opted in, or not the synthetic scope.
    if (!conformanceEnabled(this.env)) return refuse("conformance.disabled", "this conformance entry is not enabled");
    let scope;
    try {
      scope = ProductionCell.scopeOf(request);
    } catch {
      scope = null;
    }
    if (!isSynthetic(scope)) return refuse("conformance.scope", "this conformance entry serves only its synthetic scope");
    const url = new URL(request.url);
    if (url.pathname === "/__conformance/head") {
      await this.open(scope);
      // The Rust-serialized head, carried as the exact string the cell produced.
      const headJson = this.cell.ledgerHead();
      return Response.json({ headJson, head: JSON.parse(headJson), ...this.identity() });
    }
    if (url.pathname === "/__conformance/stats") {
      const { statements, inTransaction, transactions } = this.last ?? {};
      return Response.json({ statements, inTransaction, transactions, ...this.identity() });
    }
    const marker = request.headers.get(MARKER_HEADER);
    Object.assign(this.counts, { statements: 0, inTransaction: 0, transactions: 0, reached: false, hold: parseHold(request.headers.get(HOLD_HEADER)), marker: marker !== null && MARKER.test(marker) ? marker : null });
    try {
      const response = await super.fetch(request);
      if (this.counts.hold?.at === "post-turn") {
        // The request's write turn has ended (and begun committing); the reply has not left.
        await new Promise((resolve) => setTimeout(resolve, 0));
        mark(this.counts);
        spin(this.counts.hold.ms);
      }
      const headers = new Headers(response.headers);
      headers.set("x-norn-conformance-instance", this.instance);
      headers.set("x-norn-conformance-statements", String(this.counts.inTransaction));
      // A hold the request never reached (a point past its last statement) is the runner's error, not the host's.
      if (this.counts.hold !== null) headers.set("x-norn-conformance-hold-reached", String(this.counts.reached));
      if (response.status === 101) return response;
      return new Response(response.body, { status: response.status, headers });
    } finally {
      this.last = { ...this.counts };
      this.counts.hold = null;
      this.counts.marker = null;
    }
  }
}

/** A probe object: the Durable Object API properties the cell relies on, one route each. */
export class Probe {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
    this.instance = crypto.randomUUID();
    this.sql = ctx.storage.sql;
    this.sql.exec("CREATE TABLE IF NOT EXISTS log (seq INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, at INTEGER, detail TEXT)");
    this.sql.exec("CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v)");
  }
  log(kind, detail) {
    this.sql.exec("INSERT INTO log (kind, at, detail) VALUES (?, ?, ?)", kind, Date.now(), detail === undefined ? null : JSON.stringify(detail));
  }
  rows(sql, ...params) {
    return Array.from(this.sql.exec(sql, ...params).raw(), (row) => row.map((v) => (v instanceof ArrayBuffer ? `blob:${v.byteLength}` : v)));
  }
  async fetch(request) {
    if (!conformanceEnabled(this.env)) return refuse("conformance.disabled", "this conformance entry is not enabled");
    const url = new URL(request.url);
    const route = url.pathname.slice("/__probe/".length);
    const param = (name, fallback) => url.searchParams.get(name) ?? fallback;
    const reply = (value, status = 200) => Response.json({ instance: this.instance, ...value }, { status });
    try {
      switch (route) {
        // The existing adapter probe (crates/norn-cell/examples/workers_sql.rs): commit, typed
        // refusal rolled back, nested transaction and SQL transaction control refused, exact
        // values, blobs, duplicate columns.
        case "adapter":
          return reply({ result: exercise(this.ctx.storage) });
        case "tx-throw": {
          // A thrown non-Error value (the glue's rollback marker) rolls back and is rethrown unchanged.
          const before = this.rows("SELECT count(*) FROM log")[0][0];
          let thrown;
          try {
            this.ctx.storage.transactionSync(() => {
              this.log("tx-throw");
              throw "norn:rollback";
            });
          } catch (error) {
            thrown = error;
          }
          const after = this.rows("SELECT count(*) FROM log")[0][0];
          return reply({ rolledBack: after === before, rethrown: thrown === "norn:rollback", thrownType: typeof thrown });
        }
        case "tx-nested": {
          // What the platform does with a nested transactionSync (Norn refuses nesting itself; this
          // records whether a caught inner failure leaves the outer writes in place).
          const before = this.rows("SELECT count(*) FROM log")[0][0];
          let inner = "none";
          this.ctx.storage.transactionSync(() => {
            this.log("outer");
            try {
              this.ctx.storage.transactionSync(() => {
                this.log("inner");
                throw new Error("inner");
              });
            } catch (error) {
              inner = String(error.message ?? error);
            }
          });
          const added = this.rows("SELECT count(*) FROM log")[0][0] - before;
          return reply({ inner, outerKept: added === 1, added });
        }
        case "turn": {
          // One synchronous turn of `n` inserts with a spin in the middle; a concurrent reader must
          // see all of them or none.
          const n = Number(param("n", "50"));
          const tag = param("tag", "turn");
          this.ctx.storage.transactionSync(() => {
            for (let i = 0; i < n; i++) {
              this.log(tag, i);
              if (i === Math.floor(n / 2)) spin(Number(param("ms", "300")));
            }
          });
          return reply({ wrote: n });
        }
        case "count":
          return reply({ count: this.rows("SELECT count(*) FROM log WHERE kind = ?", param("tag", "turn"))[0][0] });
        case "clock": {
          // Is the clock frozen inside one synchronous call? (workerd: yes; Norn reads the clock
          // once per call and passes it into the cell, so either answer is safe.)
          const first = Date.now();
          const p0 = performance.now();
          spin(Number(param("ms", "50")));
          return reply({ frozen: Date.now() === first, dateAdvancedMs: Date.now() - first, perfAdvancedMs: performance.now() - p0 });
        }
        case "sqlite":
          return reply(this.sqlite());
        case "size": {
          // One value of `bytes` in one commit: accepted or refused, and how.
          const bytes = Number(param("bytes", "1048576"));
          try {
            this.sql.exec("INSERT OR REPLACE INTO kv VALUES ('big', zeroblob(?))", bytes);
            const stored = this.rows("SELECT length(v) FROM kv WHERE k = 'big'")[0][0];
            this.sql.exec("DELETE FROM kv WHERE k = 'big'");
            return reply({ bytes, accepted: true, stored });
          } catch (error) {
            return reply({ bytes, accepted: false, error: `${error?.name}: ${error?.message ?? error}`.slice(0, 300) });
          }
        }
        case "alarm-set": {
          const at = Date.now() + Number(param("in", "2000"));
          await this.ctx.storage.setAlarm(at);
          this.log("alarm-set", { at });
          // The handler fails this many times before it succeeds (at-least-once delivery).
          this.sql.exec("INSERT OR REPLACE INTO kv VALUES ('fail', ?)", Number(param("fail", "0")));
          this.sql.exec("DELETE FROM log WHERE kind = 'alarm-failed'");
          return reply({ at });
        }
        case "alarm-rollback": {
          // setAlarm inside a transaction that then throws: the alarm must not survive.
          try {
            await this.ctx.storage.transaction(async () => {
              this.log("alarm-rolled-back");
              await this.ctx.storage.setAlarm(Date.now() + 3_600_000);
              throw new Error("refuse after alarm");
            });
          } catch (error) {
            if (error.message !== "refuse after alarm") throw error;
          }
          return reply({ alarm: await this.ctx.storage.getAlarm(), logged: this.rows("SELECT count(*) FROM log WHERE kind = 'alarm-rolled-back'")[0][0] });
        }
        case "alarm-state":
          return reply({ alarm: await this.ctx.storage.getAlarm(), fired: this.rows("SELECT at, detail FROM log WHERE kind = 'alarm' ORDER BY seq"), failed: this.rows("SELECT at, detail FROM log WHERE kind = 'alarm-failed' ORDER BY seq") });
        case "wait-until": {
          // Background work past the reply: `n` writes, one per `every` ms, each its own turn.
          const n = Number(param("n", "15"));
          const every = Number(param("every", "1000"));
          const tag = param("tag", "bg");
          const work = (async () => {
            for (let i = 0; i < n; i++) {
              await new Promise((resolve) => setTimeout(resolve, every));
              this.ctx.storage.transactionSync(() => {
                this.log(tag, i);
                this.log(`${tag}-pair`, i);
              });
            }
          })();
          this.ctx.waitUntil(work);
          return reply({ started: n });
        }
        case "pairs":
          return reply({ count: this.rows("SELECT count(*) FROM log WHERE kind = ?", param("tag", "bg"))[0][0], pairs: this.rows("SELECT count(*) FROM log WHERE kind = ?", `${param("tag", "bg")}-pair`)[0][0] });
        case "identity":
          this.log("identity", this.instance);
          return reply({ isolate: INSTANCE, pid: globalThis.process?.pid ?? null, writes: this.rows("SELECT count(*) FROM log WHERE kind = 'identity'")[0][0] });
        default:
          return reply({ error: `no probe ${route}` }, 404);
      }
    } catch (error) {
      return reply({ error: `${error?.name}: ${error?.message ?? error}`.slice(0, 500) }, 500);
    }
  }
  async alarm(info) {
    const fail = Number(this.rows("SELECT v FROM kv WHERE k = 'fail'")[0]?.[0] ?? 0);
    const failures = this.rows("SELECT count(*) FROM log WHERE kind = 'alarm-failed'")[0][0];
    // A failed handler's own writes may be rolled back, so the runtime's retry count decides where it gives one.
    if ((info?.retryCount ?? failures) < fail) {
      this.log("alarm-failed", { retryCount: info?.retryCount ?? null });
      throw new Error("conformance: alarm handler failure");
    }
    this.log("alarm", { retryCount: info?.retryCount ?? null, isRetry: info?.isRetry ?? null });
  }
  sqlite() {
    const one = (sql, ...params) => this.rows(sql, ...params)[0];
    const affinity = {};
    this.sql.exec("CREATE TABLE IF NOT EXISTS aff (i INTEGER, t TEXT, r REAL, n NUMERIC, b BLOB, x)");
    this.sql.exec("DELETE FROM aff");
    this.sql.exec("INSERT INTO aff VALUES ('42', 42, '1.5', '9007199254740993', x'00ff', '7')");
    const [types] = [one("SELECT typeof(i), typeof(t), typeof(r), typeof(n), typeof(b), typeof(x) FROM aff")];
    affinity.types = types;
    affinity.n = one("SELECT CAST(n AS TEXT) FROM aff")[0];
    affinity.maxInt = one("SELECT CAST(9223372036854775807 AS TEXT)")[0];
    affinity.bigBinding = (() => {
      try {
        return this.rows("SELECT ?", 9007199254740993n)[0][0]?.toString();
      } catch (error) {
        return `refused: ${error?.message ?? error}`.slice(0, 120);
      }
    })();
    const variables = (count) => {
      try {
        this.rows(`SELECT ${Array.from({ length: count }, () => "?").join(",")}`, ...Array.from({ length: count }, (_, i) => i));
        return true;
      } catch {
        return false;
      }
    };
    const statementBytes = (bytes) => {
      try {
        this.rows(`SELECT '${"x".repeat(bytes)}'`);
        return true;
      } catch {
        return false;
      }
    };
    const fn = (sql) => {
      try {
        return this.rows(sql)[0]?.[0];
      } catch (error) {
        return `error: ${error?.message ?? error}`.slice(0, 120);
      }
    };
    return {
      // celld and Cloudflare refuse sqlite_version(); the refusal is the answer there.
      version: fn("SELECT sqlite_version()"),
      affinity,
      variables100: variables(100),
      variables1000: variables(1000),
      statement100k: statementBytes(100_000),
      json: fn("SELECT json_extract('{\"a\":[1,2]}', '$.a[1]')"),
      strict: fn("CREATE TABLE IF NOT EXISTS strict_probe (a INTEGER) STRICT") ?? "ok",
      foreignKeys: fn("PRAGMA foreign_keys"),
      databaseSize: this.ctx.storage.sql.databaseSize ?? null,
    };
  }
}

export default {
  async fetch(request, env) {
    if (!conformanceEnabled(env)) return refuse("conformance.disabled", "this conformance entry is not enabled");
    const url = new URL(request.url);
    if (url.pathname.startsWith("/__probe/")) {
      return env.PROBES.get(env.PROBES.idFromName(url.searchParams.get("object") ?? "probe")).fetch(request);
    }
    // A separate analytics cell per name (kill sweeps, stalled runners, the many-cell bench): each
    // object provisions its own cell, so each phase starts from the native reference's opened head.
    const object = url.searchParams.get(OBJECT_PARAM);
    if (object !== null) {
      url.searchParams.delete(OBJECT_PARAM);
      const forwarded = new Request(url, request);
      forwarded.headers.set("x-norn-cell", JSON.stringify(["org_demo", "analytics"]));
      return env.CELLS.get(env.CELLS.idFromName(`conformance/${object}`)).fetch(forwarded);
    }
    // Everything else goes through the production router, for the synthetic scope only.
    const orgs = url.searchParams.getAll("orgId");
    const environments = url.searchParams.getAll("environmentId");
    if (!isSynthetic([...orgs, ...environments]) || orgs.length !== 1) return refuse("conformance.scope", "this conformance entry serves only its synthetic scope");
    return router.fetch(request, env);
  },
};
