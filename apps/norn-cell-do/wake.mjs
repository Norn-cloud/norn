// TIMER-ALARM-1: the Durable Object's wake. A cell's deadlines (reservation
// expiries, flow timers, decision deadlines, scheduled syncs) are durable in
// the cell itself; the object's one alarm must never be missing while any of
// them is pending. The rules, each failing closed:
//
// - Write-ahead guard: before a request may write, an alarm no later than
//   RECOVERY_MS from now is durable. A deadline the write commits is then
//   always covered, even when the reply is an error, the isolate dies before
//   re-arming, or re-arming fails.
// - After the write (whatever its status), the alarm moves to the cell's next
//   deadline when that is sooner. A scheduling error propagates.
// - An alarm first sets a recovery wake, then runs the due work, then sets
//   the alarm exactly to the next deadline before any external I/O (a sync's
//   fetches). A failure anywhere leaves the recovery wake, and the error
//   propagates so the runtime retries too.
// - Opening a serving cell arms for whatever is already due (cells from
//   before alarms existed, a lost alarm).
//
// The alarm only ever moves earlier outside an alarm run, so a later
// deadline never postpones a guard or a recovery wake; an extra wake finds
// nothing due and sets the alarm exactly.

/** How long a guard or recovery wake waits: the longest a lost wake can delay a deadline. */
export const RECOVERY_MS = 60_000;

export class Wake {
  /** `storage` is the object's storage: getAlarm, setAlarm, deleteAlarm. */
  constructor(storage) {
    this.storage = storage;
    // Requests whose writes may not be reflected in the cell's next deadline yet.
    this.writing = 0;
  }
  /** Move the alarm to `at` (Unix ms) when none is set or the set one is later. A passed time fires at once. */
  async earliest(at, now) {
    if (at === undefined || at === null) return;
    const when = Math.max(at, now);
    const current = await this.storage.getAlarm();
    if (current !== null && current !== undefined && current <= when) return;
    await this.storage.setAlarm(when);
  }
  /** A serving cell opened: arm for whatever is already due. */
  async opened(cell, now) {
    if (!cell.serving()) return;
    await this.earliest(cell.nextDue(now), now);
  }
  /** Before a request that may write: the guard wake is durable first. */
  async beforeWrite(cell, now) {
    this.writing += 1;
    if (!cell.serving()) return;
    try {
      await this.earliest(now + RECOVERY_MS, now);
    } catch (error) {
      // The request is refused before it writes.
      this.writing = Math.max(0, this.writing - 1);
      throw error;
    }
  }
  /** After that request, whatever its reply: the alarm follows the cell's next deadline. */
  async afterWrite(cell, now) {
    this.writing = Math.max(0, this.writing - 1);
    if (!cell.serving()) return;
    await this.earliest(cell.nextDue(now), now);
  }
  /**
   * One alarm run at `now`: the recovery wake, the due work, a due sync's
   * claim, then the exact next alarm, all before any external I/O. Returns
   * what was committed and failed, and the claimed sync for the caller to run.
   */
  async alarm(cell, now) {
    await this.storage.setAlarm(now + RECOVERY_MS);
    const due = cell.runDue(now);
    const { committed, failed } = due;
    due.free?.();
    // SYNC-ONE-1: an idle cell syncs on schedule too; the claim is the cell's.
    const sync = cell.syncDue(now) ?? null;
    // The next deadline, read after the claim (a claimed sync's lease counts).
    const next = cell.nextDue(now);
    if (next !== undefined && next !== null) {
      await this.storage.setAlarm(Math.max(next, now));
    } else if (this.writing === 0) {
      await this.storage.deleteAlarm();
    }
    // A request still writing keeps the recovery wake: its deadline may not be visible yet.
    return { committed, failed: failed ?? null, sync };
  }
}
