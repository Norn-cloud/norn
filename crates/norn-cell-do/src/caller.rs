//! The principal a cell's policy decides for: the one the current request
//! verified, or a recorded principal acting in its place.

use std::sync::Mutex;

/// The principal a resolver verified for the current request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Norn subject id; the request's actor.
    pub subject_id: String,
    /// Norn roles from the declared role table.
    pub roles: Vec<String>,
    /// `user` or `service`.
    pub actor_kind: &'static str,
    /// The declared role table the roles came from.
    pub role_table: &'static str,
}

/// One thread's caller, and whether it is a recorded principal acting.
#[derive(Debug)]
struct Held {
    thread: Option<std::thread::ThreadId>,
    caller: Caller,
    acting: bool,
}

/// Where the resolver records the current request's verified principal for
/// the policy. Every request records first (possibly nobody), so a caller
/// never outlives its request. A request runs on one thread from resolution
/// to answer, and the slot keeps each thread's caller apart, so requests a
/// host serves concurrently (the dev host's workers, DEV-CONCURRENT-1) never
/// see each other's caller.
#[derive(Debug, Default)]
pub struct CallerSlot(Mutex<Vec<Held>>);

impl CallerSlot {
    /// Record the request's verified principal, or nobody, for the calling
    /// thread's request.
    pub fn record(&self, caller: Option<Caller>) {
        self.replace(caller, false);
    }

    /// Put `caller` in the calling thread's slot and return what it held:
    /// the caller and whether it was acting. `acting` marks a recorded
    /// principal (a run's starter at a flow step) standing in for the
    /// request's own: it carries the roles recorded when it was verified, not
    /// this request's, so whatever needs the request's own principal fails
    /// closed for it. An approved call never stands in this way: it runs
    /// only on its requester's own live request, as that request's caller.
    pub fn replace(&self, caller: Option<Caller>, acting: bool) -> (Option<Caller>, bool) {
        let thread = this_thread();
        let Ok(mut slot) = self.0.lock() else {
            return (None, false);
        };
        let index = slot.iter().position(|held| held.thread == thread);
        let previous = index.map(|index| slot.swap_remove(index));
        if let Some(caller) = caller {
            slot.push(Held {
                thread,
                caller,
                acting,
            });
        }
        previous.map_or((None, false), |held| (Some(held.caller), held.acting))
    }

    /// The calling thread's request's verified principal, if any.
    #[must_use]
    pub fn current(&self) -> Option<Caller> {
        let me = this_thread();
        self.0.lock().ok().and_then(|slot| {
            slot.iter()
                .find(|held| held.thread == me)
                .map(|held| held.caller.clone())
        })
    }

    /// Whether the calling thread's caller is a recorded principal acting
    /// (a run's starter at a flow step), not the one this request verified.
    /// A slot that cannot be read is treated as acting.
    #[must_use]
    pub fn is_acting(&self) -> bool {
        let me = this_thread();
        self.0.lock().map_or(true, |slot| {
            slot.iter().any(|held| held.thread == me && held.acting)
        })
    }
}

/// The calling thread, on a host with threads; the Durable Object cell has
/// one thread and serves one request at a time.
fn this_thread() -> Option<std::thread::ThreadId> {
    (!cfg!(target_arch = "wasm32")).then(|| std::thread::current().id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(subject: &str) -> Caller {
        Caller {
            subject_id: subject.to_owned(),
            roles: vec!["member".to_owned()],
            actor_kind: "user",
            role_table: "studio-roles/1",
        }
    }

    #[test]
    fn an_acting_caller_is_marked_and_the_requests_own_is_restored_unmarked() {
        let slot = CallerSlot::default();
        assert!(!slot.is_acting());
        slot.record(Some(caller("user:rana")));
        assert!(!slot.is_acting());
        let previous = slot.replace(Some(caller("user:omar")), true);
        assert_eq!(previous, (Some(caller("user:rana")), false));
        assert_eq!(slot.current(), Some(caller("user:omar")));
        assert!(slot.is_acting());
        // Restoring puts back exactly what was held, with its own marking.
        let acted = slot.replace(previous.0, previous.1);
        assert_eq!(acted, (Some(caller("user:omar")), true));
        assert_eq!(slot.current(), Some(caller("user:rana")));
        assert!(!slot.is_acting());
        slot.record(None);
        assert_eq!(slot.current(), None);
    }
}
