//! Pure timer ordering and deadline arithmetic; the cell owns persistence and authority.

/// The durable ordering fields of a pending timer. One incarnation per id is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerDeadline<'a> {
    /// Absolute logical deadline in milliseconds.
    pub due: u64,
    /// Stable timer identity, compared by UTF-8 bytes.
    pub id: &'a str,
}

/// FLOW-TIME-1: a backwards clock sample never precedes the persisted floor.
#[must_use]
pub const fn effective_now(sample: u64, floor: u64) -> u64 {
    if sample < floor { floor } else { sample }
}

/// FLOW-TIME-1: schedule relative to trusted logical time, refusing overflow.
///
/// `None` must refuse the whole scheduling commit, never clamp the deadline.
#[must_use]
pub const fn deadline(sample: u64, floor: u64, after_millis: u64) -> Option<u64> {
    effective_now(sample, floor).checked_add(after_millis)
}

/// FLOW-ORDER-1: select the least eligible `(due, id)` independently of input order.
///
/// Selection neither consumes the timer nor grants authority. The caller must
/// fence its incarnation and consume it atomically with the governed action.
#[must_use]
pub fn next_due<'a, 'id>(
    timers: &'a [TimerDeadline<'id>],
    sample: u64,
    floor: u64,
) -> Option<&'a TimerDeadline<'id>> {
    let now = effective_now(sample, floor);
    timers
        .iter()
        .filter(|timer| timer.due <= now)
        .min_by_key(|timer| (timer.due, timer.id))
}
