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
//   fetches); while any request is still writing, to the earlier of that
//   deadline and the recovery wake, until that request's own rearm runs. A failure anywhere leaves the recovery wake, and the error
//   propagates so the runtime retries too.
// - Opening a serving cell arms for whatever is already due (cells from
//   before alarms existed, a lost alarm).
//
// The alarm only ever moves earlier outside an alarm run, so a later
// deadline never postpones a guard or a recovery wake; an extra wake finds
// nothing due and sets the alarm exactly.

/** How long a guard or recovery wake waits: the longest a lost wake can delay a deadline. */
export const RECOVERY_MS = 60_000;

/** S12 (#1551 round 5): whether an error from the cell indicates a terminal history-scrub fence. */
function isFenced(error) {
  const message = error instanceof Error ? error.message : String(error ?? "");
  return message.startsWith("cell.fenced");
}

export class Wake {
  /** `storage` is the object's storage: getAlarm, setAlarm, deleteAlarm. */
  constructor(storage) {
    this.storage = storage;
    // Requests whose writes may not be reflected in the cell's next deadline yet.
    this.writing = 0;
    // The Unix ms timestamp the object's alarm was last armed for, or null when cleared.
    this.armed = null;
  }
  async setAlarm(when) {
    await this.storage.setAlarm(when);
    this.armed = when;
  }
  async deleteAlarm() {
    await this.storage.deleteAlarm();
    this.armed = null;
  }
  /** Move the alarm to `at` (Unix ms) when none is set or the set one is later. A passed time fires at once. */
  async earliest(at, now) {
    if (at === undefined || at === null) return;
    const when = Math.max(at, now);
    const current = await this.storage.getAlarm();
    if (current !== null && current !== undefined && current <= when) {
      this.armed = current;
      return;
    }
    await this.setAlarm(when);
  }
  /** A serving cell opened: arm for whatever is already due. */
  async opened(cell, now) {
    if (!cell.serving()) return;
    let next;
    try {
      next = cell.nextDue(now);
    } catch (error) {
      if (isFenced(error)) return;
      throw error;
    }
    await this.earliest(next, now);
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
    let next;
    try {
      next = cell.nextDue(now);
    } catch (error) {
      if (isFenced(error)) {
        await this.deleteAlarm();
        return;
      }
      throw error;
    }
    await this.earliest(next, now);
  }
  /**
   * One alarm run at `now`: the recovery wake, the due work, a due sync's
   * claim, then the exact next alarm, all before any external I/O. Returns
   * what was committed and failed, and the claimed sync for the caller to run.
   */
  async alarm(cell, now) {
    this.armed = null;
    await this.setAlarm(now + RECOVERY_MS);
    const due = cell.runDue(now);
    const { committed, failed } = due;
    due.free?.();
    // SYNC-ONE-1: an idle cell syncs on schedule too; the claim is the cell's.
    const sync = cell.syncDue(now) ?? null;
    // The next deadline, read after the claim (a claimed sync's lease counts).
    let next;
    try {
      next = cell.nextDue(now);
    } catch (error) {
      // S12 (#1551 round 5): a fenced cell is terminal; clear the recovery
      // alarm and return the failure report instead of looping forever.
      if (isFenced(error)) {
        await this.deleteAlarm();
        return { committed, failed: failed ?? String(error instanceof Error ? error.message : error), sync: null };
      }
      throw error;
    }
    const recovery = now + RECOVERY_MS;
    if (this.writing > 0) {
      // A request still writing keeps the recovery wake until its own rearm
      // runs: a deadline it commits may not be visible yet, and a crash after
      // that commit leaves only this alarm. A sooner deadline still wins.
      if (next !== undefined && next !== null && Math.max(next, now) < recovery) {
        await this.setAlarm(Math.max(next, now));
      }
    } else if (next !== undefined && next !== null) {
      await this.setAlarm(Math.max(next, now));
    } else {
      await this.deleteAlarm();
    }
    return { committed, failed: failed ?? null, sync };
  }
}
