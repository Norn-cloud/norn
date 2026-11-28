//! INCIDENT-1: an internal failure is never silent. Every refusal becomes a
//! response here, and an internal one is logged with its cause (the error's
//! source chain) under an incident id that the caller gets back in the
//! problem body. The cause never reaches the caller; codes, categories and
//! messages stay as they were.
//!
//! The log carries the error kind and its chain only: refusal causes are
//! host and engine errors, which name tables, codes and limits, never request
//! bodies or credentials.

use std::sync::atomic::{AtomicU64, Ordering};

use norn_api::refusal::Refusal;

use crate::server::Response;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Try to generate a fresh opaque 128-bit random incident id
/// (`inc-<32 lowercase hex>`). It carries no per-isolate counter or clock
/// value, so two callers sharing an isolate cannot correlate each other's
/// error rate (#1486).
///
/// # Errors
/// Returns an error if the platform's cryptographic random source fails.
pub fn try_id() -> Result<String, String> {
    let bytes = random_bytes()?;
    Ok(format!("inc-{:032x}", u128::from_be_bytes(bytes)))
}

/// A fresh opaque 128-bit random incident id, or `None` if entropy failed.
#[must_use]
pub fn id() -> Option<String> {
    try_id().ok()
}

/// Format a Unix timestamp in seconds as RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`),
/// using Howard Hinnant's civil-from-days algorithm.
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "bounded calendar decomposition over u64 Unix seconds in i128"
)]
pub fn format_utc(unix_secs: u64) -> String {
    let secs_per_day: u64 = 86_400;
    let days = unix_secs / secs_per_day;
    let rem = unix_secs % secs_per_day;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    let z = i128::from(days) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Prepare `refused` at the server boundary: log internal causes under a fresh
/// incident id (INCIDENT-1) and stamp `decidedAt` when a host clock (`now` in
/// Unix seconds) is provided.
pub fn prepare(refused: &mut Refusal, now: Option<u64>) {
    prepare_with(refused, now, try_id);
}

fn prepare_with(
    refused: &mut Refusal,
    now: Option<u64>,
    next_id: impl FnOnce() -> Result<String, String>,
) {
    if refused.is_internal() {
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        let cause = refused
            .extra
            .as_ref()
            .and_then(|internal| internal.cause.clone())
            .unwrap_or_else(|| String::from("no cause was recorded for this refusal"));
        match next_id() {
            Ok(incident) => {
                log(&format!(
                    "norn incident {incident} (seq {seq}): {} ({}): {cause}",
                    refused.code, refused.status
                ));
                refused.extra.get_or_insert_with(Box::default).incident_id = Some(incident);
            }
            Err(entropy_error) => {
                log(&format!(
                    "norn incident <unavailable> (seq {seq}; entropy failed: {entropy_error}): {} ({}): {cause}",
                    refused.code, refused.status
                ));
                if let Some(internal) = refused.extra.as_mut() {
                    internal.incident_id = None;
                }
            }
        }
    }
    if let Some(now) = now {
        refused.set_decided_at(format_utc(now));
    }
}

/// The response for `refused`. An internal refusal is logged with its cause
/// and carries the incident id; any other refusal is served unchanged.
#[must_use]
pub fn respond(refused: Refusal) -> Response {
    respond_with(refused, try_id)
}

fn respond_with(
    mut refused: Refusal,
    next_id: impl FnOnce() -> Result<String, String>,
) -> Response {
    prepare_with(&mut refused, None, next_id);
    Response {
        status: refused.status,
        body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
    }
}

/// The response for `refused`, stamped with `decidedAt` from the host clock
/// (`now`, Unix seconds) and logged under an incident id when internal.
#[must_use]
pub fn respond_at(mut refused: Refusal, now: u64) -> Response {
    prepare(&mut refused, Some(now));
    Response {
        status: refused.status,
        body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
    }
}

/// Log a panic's payload and location before the runtime aborts.
pub fn install_panic_log() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log(&format!("norn panic: {info}"));
            previous(info);
        }));
    });
}

mod sink {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// #1710: the most bytes of lines one capturing thread keeps.
    pub(super) const MAX_RECENT_BYTES: usize = 64 * 1024;

    thread_local! {
        /// `None` until the thread opts in with [`capture`]: a production
        /// thread keeps nothing but what it writes to stderr.
        static RECENT: RefCell<Option<Recent>> = const { RefCell::new(None) };
    }

    /// The newest lines a capturing thread logged, at most
    /// [`MAX_RECENT_BYTES`] of them; older lines are dropped first.
    #[derive(Default)]
    struct Recent {
        lines: VecDeque<String>,
        bytes: usize,
    }

    impl Recent {
        fn push(&mut self, line: &str) {
            if line.len() > MAX_RECENT_BYTES {
                return;
            }
            self.bytes = self.bytes.saturating_add(line.len());
            self.lines.push_back(line.to_owned());
            while self.bytes > MAX_RECENT_BYTES {
                let Some(oldest) = self.lines.pop_front() else {
                    break;
                };
                self.bytes = self.bytes.saturating_sub(oldest.len());
            }
        }
    }

    pub(super) fn log(line: &str) {
        use std::io::Write as _;
        // A failed write to stderr has nowhere better to go.
        let _ignored = writeln!(std::io::stderr(), "{line}");
        RECENT.with(|recent| {
            if let Some(recent) = recent.borrow_mut().as_mut() {
                recent.push(line);
            }
        });
    }

    /// Start keeping this thread's lines for [`recent`].
    pub(super) fn capture() {
        RECENT.with(|recent| {
            recent.borrow_mut().get_or_insert_with(Recent::default);
        });
    }

    /// 128 bits of OS entropy for an opaque incident identifier.
    pub(super) fn random_bytes() -> Result<[u8; 16], String> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes)
            .map(|()| bytes)
            .map_err(|err| err.to_string())
    }

    /// The lines kept on this thread since it began to [`capture`].
    pub(super) fn recent() -> Vec<String> {
        RECENT.with(|recent| {
            recent
                .borrow()
                .as_ref()
                .map_or_else(Vec::new, |recent| recent.lines.iter().cloned().collect())
        })
    }
}

fn log(line: &str) {
    sink::log(line);
}

fn random_bytes() -> Result<[u8; 16], String> {
    sink::random_bytes()
}

/// #1710: keep the incident lines this thread logs from now on, for
/// [`recent`] (tests and diagnostics opt in). A thread
/// that never calls this keeps nothing in memory; one that does keeps at
/// most the newest 64 KiB of lines.
pub fn capture() {
    sink::capture();
}

/// The incident lines this thread logged since it began to [`capture`];
/// empty on a thread that never did.
#[must_use]
pub fn recent() -> Vec<String> {
    sink::recent()
}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn formats_unix_seconds_as_rfc3339_utc() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_800_000_000), "2027-01-15T08:00:00Z");
        // Leap day: 2024-02-29T23:59:59Z
        assert_eq!(format_utc(1_709_251_199), "2024-02-29T23:59:59Z");
    }

    #[test]
    fn entropy_failure_preserves_internal_refusal_without_issuing_id() {
        super::capture();
        let response = super::respond_with(super::Refusal::host("storage fault"), || {
            Err(String::from("rng offline"))
        });
        assert_eq!(response.status, 500, "{}", response.body);
        assert!(
            !response.body.contains("incidentId"),
            "must not issue a fallback incidentId on entropy failure: {}",
            response.body
        );
        assert!(
            !response.body.contains("storage fault"),
            "cause must never leak: {}",
            response.body
        );
        let logged = super::recent();
        assert!(
            logged.iter().any(|line| {
                line.contains("entropy failed: rng offline")
                    && line.contains(" (seq ")
                    && line.contains("storage fault")
            }),
            "entropy failure and cause must be logged: {logged:?}"
        );
    }

    /// #1710: a thread that never opted in keeps nothing; one that did keeps
    /// at most `MAX_RECENT_BYTES`, the newest lines.
    #[test]
    fn only_a_capturing_thread_keeps_lines_and_at_most_its_bound() {
        let quiet = std::thread::spawn(|| {
            super::log("norn incident quiet");
            super::recent()
        });
        assert!(quiet.join().is_ok_and(|kept| kept.is_empty()));
        super::capture();
        let line = "x".repeat(1024);
        for _ in 0..70 {
            super::log(&line);
        }
        super::log("newest");
        let kept = super::recent();
        let bytes = kept.iter().map(String::len).sum::<usize>();
        assert!(bytes <= super::sink::MAX_RECENT_BYTES, "{bytes}");
        assert_eq!(kept.last().map(String::as_str), Some("newest"));
        super::log(&"y".repeat(super::sink::MAX_RECENT_BYTES.saturating_add(1)));
        assert_eq!(super::recent().last().map(String::as_str), Some("newest"));
    }
}
