//! #1747: `norn-olapd`'s pre-admission bounds. A connection is read,
//! authenticated and its body digested in a bounded pre-admission stage,
//! apart from the engine's workers: at most [`MAX_PENDING`] connections are
//! in it at once, at most [`MAX_PENDING_PER_PEER`] from any one peer
//! address, and the bodies authenticated callers have declared and not yet
//! been answered for hold at most [`MAX_HELD_BODY_BYTES`]. A connection past
//! any bound is answered `503` at once and never reaches an engine worker.
//! Every count is held by a guard and given back when the guard drops, so
//! no path that ends a connection (an answer, a refusal, an error or a
//! broken peer) leaks a slot.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use norn_api::olap_credential::Keyring;

use super::transport::MAX_BODY_BYTES;

/// Longest accepted keyring file, in bytes.
const MAX_KEYRING_BYTES: u64 = 64 * 1024;

/// Read raw keyring file bytes, bounded.
fn read_keyring_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_KEYRING_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)
        })
        .map_err(|e| format!("the keyring {} is unreadable: {e}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_KEYRING_BYTES {
        return Err(format!("the keyring {} is too long", path.display()));
    }
    Ok(bytes)
}

/// Parse UTF-8 keyring bytes.
fn parse_keyring_bytes(path: &Path, bytes: &[u8]) -> Result<Keyring, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| format!("the keyring {} is not UTF-8: {e}", path.display()))?;
    Keyring::parse(text).map_err(|e| format!("the keyring {}: {e}", path.display()))
}

/// Read and parse the keyring file, bounded.
pub(super) fn read_keyring(path: &Path) -> Result<Keyring, String> {
    let bytes = read_keyring_bytes(path)?;
    parse_keyring_bytes(path, &bytes)
}

type CachedKeyring = (Vec<u8>, Arc<Keyring>);

/// Byte-verified keyring cache: reads file bytes on every call so rotation and
/// unreadable files take effect immediately, reusing the parsed keyring when
/// bytes are unchanged.
#[derive(Default)]
pub(super) struct KeyringCache {
    cached: Mutex<Option<CachedKeyring>>,
}

impl KeyringCache {
    pub(super) fn load(&self, path: &Path) -> Result<Arc<Keyring>, String> {
        let bytes = read_keyring_bytes(path)?;
        {
            let guard = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((cached_bytes, cached_ring)) = guard.as_ref()
                && *cached_bytes == bytes
            {
                return Ok(Arc::clone(cached_ring));
            }
        }
        let parsed = Arc::new(parse_keyring_bytes(path, &bytes)?);
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((bytes, Arc::clone(&parsed)));
        Ok(parsed)
    }
}

/// Connections in pre-admission at once, from every peer.
pub const MAX_PENDING: usize = 64;
/// Connections in pre-admission at once from one peer address. A peer
/// opening more cannot take the stage from any other peer.
pub const MAX_PENDING_PER_PEER: usize = 16;
/// The declared body bytes that authenticated, not yet answered calls may
/// hold at once.
pub const MAX_HELD_BODY_BYTES: usize = MAX_BODY_BYTES.saturating_mul(4);

/// The counts the bounds are checked against.
#[derive(Debug, Default)]
struct Counts {
    pending: usize,
    peers: BTreeMap<IpAddr, usize>,
    held: usize,
}

/// The shared pre-admission bounds of one daemon.
#[derive(Debug, Default)]
pub(super) struct Admission {
    counts: Mutex<Counts>,
}

impl Admission {
    fn counts(&self) -> MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A pre-admission slot for a connection from `peer`, when the stage
    /// and that peer's share of it have room.
    pub(super) fn enter(self: &Arc<Self>, peer: IpAddr) -> Option<Pending> {
        let mut counts = self.counts();
        let from_peer = counts.peers.get(&peer).copied().unwrap_or(0);
        if counts.pending >= MAX_PENDING || from_peer >= MAX_PENDING_PER_PEER {
            return None;
        }
        counts.pending = counts.pending.saturating_add(1);
        counts.peers.insert(peer, from_peer.saturating_add(1));
        Some(Pending {
            admission: Arc::clone(self),
            peer,
        })
    }

    /// Room for an authenticated call's declared body of `bytes`, held
    /// until the call is answered.
    pub(super) fn hold(self: &Arc<Self>, bytes: usize) -> Option<Held> {
        let mut counts = self.counts();
        let held = counts
            .held
            .checked_add(bytes)
            .filter(|held| *held <= MAX_HELD_BODY_BYTES)?;
        counts.held = held;
        Some(Held {
            admission: Arc::clone(self),
            bytes,
        })
    }
}

/// One connection's pre-admission slot; given back on drop.
#[derive(Debug)]
pub(super) struct Pending {
    admission: Arc<Admission>,
    peer: IpAddr,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let mut counts = self.admission.counts();
        counts.pending = counts.pending.saturating_sub(1);
        let left = counts
            .peers
            .get(&self.peer)
            .copied()
            .unwrap_or(0)
            .saturating_sub(1);
        if left == 0 {
            counts.peers.remove(&self.peer);
        } else {
            counts.peers.insert(self.peer, left);
        }
    }
}

/// One call's held body bytes; given back on drop.
#[derive(Debug)]
pub(super) struct Held {
    admission: Arc<Admission>,
    bytes: usize,
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut counts = self.admission.counts();
        counts.held = counts.held.saturating_sub(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn peer(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, last))
    }

    /// One peer flooding the stage fills its own share only: every other
    /// peer still enters, and the peer enters again once a slot is given back.
    #[test]
    fn a_flooding_peer_fills_only_its_own_share() {
        let admission = Arc::new(Admission::default());
        let flood: Vec<Pending> = (0..MAX_PENDING_PER_PEER)
            .map_while(|_| admission.enter(peer(1)))
            .collect();
        assert_eq!(flood.len(), MAX_PENDING_PER_PEER);
        assert!(admission.enter(peer(1)).is_none(), "past its share");
        let other = admission.enter(peer(2));
        assert!(other.is_some(), "another peer is not starved");
        drop(flood.into_iter().next());
        assert!(admission.enter(peer(1)).is_some(), "a slot came back");
    }

    /// The whole stage is bounded however many peers there are, and every
    /// slot comes back when its connection ends.
    #[test]
    fn the_stage_is_bounded_across_peers_and_drains_back_to_empty() {
        let admission = Arc::new(Admission::default());
        let mut held = Vec::new();
        for index in 0..=u8::MAX {
            if let Some(slot) = admission.enter(peer(index)) {
                held.push(slot);
            }
        }
        assert_eq!(held.len(), MAX_PENDING);
        assert!(admission.enter(peer(7)).is_none());
        drop(held);
        let counts = admission.counts();
        assert_eq!((counts.pending, counts.peers.len()), (0, 0));
    }

    /// Declared bodies are held up to the bound and given back when answered.
    #[test]
    fn held_body_bytes_are_bounded_and_given_back() {
        let admission = Arc::new(Admission::default());
        let first = admission.hold(MAX_HELD_BODY_BYTES);
        assert!(first.is_some());
        assert!(admission.hold(1).is_none(), "past the bound");
        assert!(admission.hold(0).is_some(), "an empty body holds nothing");
        drop(first);
        assert!(admission.hold(MAX_BODY_BYTES).is_some());
        assert!(admission.hold(usize::MAX).is_none(), "no overflow");
    }
}
