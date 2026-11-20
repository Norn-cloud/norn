// TIMER-ALARM-1: the Durable Object's wake rules (wake.mjs), over a fake
// storage and cell. Run: bun test ./wake.test.mjs (from apps/norn-cell-do).
import { describe, expect, test } from "bun:test";
import { RECONCILING, RECOVERY_MS, Wake } from "./wake.mjs";

const NOW = 1_800_000_000_000;

/** The object's alarm storage; `failSet` makes scheduling fail. */
function storage() {
  return {
    alarm: null,
    failSet: false,
    sets: [],
    async getAlarm() {
      return this.alarm;
    },
    async setAlarm(at) {
      if (this.failSet) throw new Error("storage unavailable");
      this.alarm = at;
      this.sets.push(at);
    },
    async deleteAlarm() {
      this.alarm = null;
    },
  };
}

/** A cell whose deadlines are `deadlines`; `serving` false for a vacant scope. */
function cell({ deadlines = [], serving = true, runDue, sync = null } = {}) {
  return {
    deadlines,
    synced: false,
    serving: () => serving,
    nextDue() {
      return this.deadlines.length === 0 ? undefined : Math.min(...this.deadlines);
    },
    runDue(now) {
      if (runDue) return runDue(now);
      const due = this.deadlines.filter((at) => at <= now);
      this.deadlines = this.deadlines.filter((at) => at > now);
      return { committed: due.length > 0, failed: undefined, free() {} };
    },
    syncDue() {
      if (this.synced || sync === null) return undefined;
      this.synced = true;
      return sync;
    },
  };
}

describe("a request that writes", () => {
  // The swallowed re-arm (round 1): a scheduling error must surface, never be logged and dropped.
  test("a scheduling failure after the write propagates", async () => {
    const store = storage();
    const wake = new Wake(store);
    const served = cell();
    await wake.beforeWrite(served, NOW);
    served.deadlines.push(NOW + 5_000);
    store.failSet = true;
    await expect(wake.afterWrite(served, NOW)).rejects.toThrow("storage unavailable");
    // The guard set before the write still wakes the cell.
    expect(store.alarm).toBe(NOW + RECOVERY_MS);
  });

  test("a scheduling failure before the write refuses the request", async () => {
    const store = storage();
    store.failSet = true;
    await expect(new Wake(store).beforeWrite(cell(), NOW)).rejects.toThrow("storage unavailable");
  });

  // Round 1: a write that committed a deadline and then answered an error status was never armed.
  test("a write that commits a deadline and then answers an error is armed", async () => {
    const store = storage();
    const wake = new Wake(store);
    const served = cell();
    await wake.beforeWrite(served, NOW);
    // The cell commits a reservation expiring in 5 s, then the reply is a 409.
    served.deadlines.push(NOW + 5_000);
    const reply = { status: 409 };
    await wake.afterWrite(served, NOW);
    expect(reply.status).toBe(409);
    expect(store.alarm).toBe(NOW + 5_000);
  });

  test("a write whose host dies before re-arming is still woken by its guard", async () => {
    const store = storage();
    const served = cell();
    await new Wake(store).beforeWrite(served, NOW);
    served.deadlines.push(NOW + 86_400_000);
    // No afterWrite: the isolate died. The guard fires, and its alarm run finds the deadline.
    expect(store.alarm).toBe(NOW + RECOVERY_MS);
    const restarted = new Wake(store);
    const ran = await restarted.alarm(served, NOW + RECOVERY_MS);
    expect(ran.committed).toBe(false);
    expect(store.alarm).toBe(NOW + 86_400_000);
  });

  test("a later deadline never postpones an earlier alarm", async () => {
    const store = storage();
    store.alarm = NOW + 1_000;
    const wake = new Wake(store);
    const served = cell({ deadlines: [NOW + 50_000] });
    await wake.beforeWrite(served, NOW);
    await wake.afterWrite(served, NOW);
    expect(store.alarm).toBe(NOW + 1_000);
  });

  test("a vacant cell is never armed", async () => {
    const store = storage();
    const wake = new Wake(store);
    const vacant = cell({ serving: false });
    await wake.beforeWrite(vacant, NOW);
    await wake.afterWrite(vacant, NOW);
    await wake.opened(vacant, NOW);
    expect(store.sets).toEqual([]);
  });
});

describe("an alarm", () => {
  test("re-arms exactly before the sync it claims runs", async () => {
    const store = storage();
    const served = cell({ deadlines: [NOW, NOW + 3_600_000], sync: "erpnext.rest" });
    const ran = await new Wake(store).alarm(served, NOW);
    expect(ran.sync).toBe("erpnext.rest");
    expect(ran.committed).toBe(true);
    // Set before the caller performs any of the sync's fetches.
    expect(store.alarm).toBe(NOW + 3_600_000);
  });

  test("a failing handler keeps a recovery wake and propagates", async () => {
    const store = storage();
    const served = cell({
      runDue() {
        throw new Error("host.error");
      },
    });
    await expect(new Wake(store).alarm(served, NOW)).rejects.toThrow("host.error");
    expect(store.alarm).toBe(NOW + RECOVERY_MS);
  });

  test("with nothing pending it clears the alarm, unless a request is still writing", async () => {
    const store = storage();
    const wake = new Wake(store);
    await wake.alarm(cell(), NOW);
    expect(store.alarm).toBe(null);
    await wake.beforeWrite(cell(), NOW);
    await wake.alarm(cell(), NOW);
    expect(store.alarm).toBe(NOW + RECOVERY_MS);
  });

  // Round 2 S1: an alarm during an in-flight write must not trade the
  // recovery wake for a later deadline. The write commits a sooner deadline,
  // then the isolate dies before its rearm: only the recovery wake remains.
  test("a later deadline never replaces the recovery wake while a request writes", async () => {
    const store = storage();
    const wake = new Wake(store);
    const served = cell({ deadlines: [NOW + 3_600_000] });
    await wake.beforeWrite(served, NOW);
    await wake.alarm(served, NOW);
    expect(store.alarm).toBe(NOW + RECOVERY_MS);
    // The write commits a deadline 5 s out and crashes before afterWrite.
    served.deadlines.push(NOW + 5_000);
    // A fresh object (new isolate) sees only the stored alarm.
    expect(store.alarm).toBeLessThanOrEqual(NOW + RECOVERY_MS);
  });

  test("a sooner deadline still wins while a request writes", async () => {
    const store = storage();
    const wake = new Wake(store);
    const served = cell({ deadlines: [NOW + 5_000] });
    await wake.beforeWrite(served, NOW);
    await wake.alarm(served, NOW);
    expect(store.alarm).toBe(NOW + 5_000);
    // Once the write's rearm runs, the alarm follows the cell exactly again.
    served.deadlines = [NOW + 3_600_000];
    await wake.afterWrite(served, NOW);
    await wake.alarm(served, NOW);
    expect(store.alarm).toBe(NOW + 3_600_000);
  });
});

describe("opening", () => {
  // A cell from before alarms existed holds an overdue deadline and no alarm.
  test("a serving cell with a passed deadline and no alarm is armed at once", async () => {
    const store = storage();
    await new Wake(store).opened(cell({ deadlines: [NOW - 10_000] }), NOW);
    expect(store.alarm).toBe(NOW);
  });
});

// SOURCE-RESOLVE-1 (#1478 r5): a cell whose reconciliation failed again
// refuses its deadline lookup (`defs.stale`). The open and the request go on
// with the recovery wake armed, so dispatch answers with its own reply.
describe("a cell still reconciling", () => {
  /** A serving cell whose deadline lookup refuses as the wasm binding throws it. */
  function reconciling() {
    return {
      ...cell(),
      nextDue() {
        throw RECONCILING;
      },
    };
  }

  test("a cold open keeps the recovery wake and does not throw", async () => {
    const store = storage();
    await new Wake(store).opened(reconciling(), NOW);
    expect(store.alarm).toBe(NOW + RECOVERY_MS);
  });

  test("the rearm after a request keeps the recovery wake and leaves the reply standing", async () => {
    const store = storage();
    const wake = new Wake(store);
    const served = reconciling();
    await wake.beforeWrite(served, NOW);
    const reply = await (async () => {
      try {
        return { status: 200, body: "applied" };
      } finally {
        await wake.afterWrite(served, NOW + 1_000);
      }
    })();
    expect(reply).toEqual({ status: 200, body: "applied" });
    expect(store.alarm).toBe(NOW + RECOVERY_MS);
    expect(wake.writing).toBe(0);
  });

  test("any other lookup failure still propagates", async () => {
    const store = storage();
    const broken = { ...cell(), nextDue() { throw "host.error"; } };
    await expect(new Wake(store).opened(broken, NOW)).rejects.toBe("host.error");
  });
});
