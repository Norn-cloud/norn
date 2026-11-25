//! #1710: the uploads one host instance remembers, bounded by count and by
//! bytes. Forgetting an upload loses nothing: its id is sealed, so
//! [`super::Connect`] rebuilds a begun upload from the id alone
//! (UPLOAD-ID-1), and a completed one is read back from the record the cell
//! kept at `:complete` (UPLOAD-RECORD-1). So the oldest completed uploads are
//! forgotten first, and only then the oldest begun ones.

use std::collections::BTreeMap;

use norn_api::uploads::{self, Store};

/// The most uploads one host instance remembers.
const MAX_HELD: usize = 1024;
/// The most bytes their names, keys and profiles may take, as weighed by
/// [`Upload::weight`].
const MAX_HELD_BYTES: usize = 4 * 1024 * 1024;
/// What every remembered upload is weighed at besides its strings.
const ENTRY_BYTES: usize = 128;

#[derive(Clone)]
pub(super) struct Upload {
    pub(super) store: Store,
    pub(super) key: String,
    pub(super) file_name: String,
    pub(super) content_type: String,
    pub(super) uri: String,
    pub(super) completed: Option<(String, u64, bool)>,
    pub(super) columns: Vec<uploads::Column>,
}

impl Upload {
    /// What remembering this upload under `id` costs, in bytes, roughly:
    /// every string it holds, and a fixed share per entry and per column.
    fn weight(&self, id: &str) -> usize {
        let columns = self.columns.iter().fold(0_usize, |sum, column| {
            let samples = column
                .samples
                .iter()
                .fold(0_usize, |sum, sample| sum.saturating_add(sample.len()));
            sum.saturating_add(column.name.len())
                .saturating_add(column.id.len())
                .saturating_add(samples)
                .saturating_add(ENTRY_BYTES)
        });
        let completed = self
            .completed
            .as_ref()
            .map_or(0, |(sha256, _, _)| sha256.len());
        [
            id.len(),
            self.key.len(),
            self.file_name.len(),
            self.content_type.len(),
            self.uri.len(),
            completed,
            columns,
            ENTRY_BYTES,
        ]
        .into_iter()
        .fold(0_usize, usize::saturating_add)
    }
}

/// The remembered uploads by id, and the order they are forgotten in:
/// completed before begun, each oldest first.
#[derive(Default)]
pub(super) struct Held {
    uploads: BTreeMap<String, (Turn, usize, Upload)>,
    order: BTreeMap<Turn, String>,
    bytes: usize,
    next: u64,
}

/// When an upload is forgotten: `(still begun, when last remembered)`.
type Turn = (bool, u64);

impl Held {
    pub(super) fn get(&self, id: &str) -> Option<&Upload> {
        self.uploads.get(id).map(|(_, _, upload)| upload)
    }

    /// Remember `upload` under `id`, replacing what was held under it, then
    /// forget the first in order until the bounds hold again. An upload
    /// heavier than the whole byte bound is not remembered at all.
    pub(super) fn insert(&mut self, id: String, upload: Upload) {
        self.forget(&id);
        let weight = upload.weight(&id);
        if weight > MAX_HELD_BYTES {
            return;
        }
        let turn = (upload.completed.is_none(), self.next);
        self.next = self.next.saturating_add(1);
        self.bytes = self.bytes.saturating_add(weight);
        self.order.insert(turn, id.clone());
        self.uploads.insert(id, (turn, weight, upload));
        while self.uploads.len() > MAX_HELD || self.bytes > MAX_HELD_BYTES {
            let Some((_, first)) = self.order.pop_first() else {
                break;
            };
            self.forget(&first);
        }
    }

    fn forget(&mut self, id: &str) {
        if let Some((turn, weight, _)) = self.uploads.remove(id) {
            self.order.remove(&turn);
            self.bytes = self.bytes.saturating_sub(weight);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(name: &str, completed: bool) -> Upload {
        Upload {
            store: Store::Uploads,
            key: format!("uploads/org/env/{name}"),
            file_name: name.to_owned(),
            content_type: "text/csv".to_owned(),
            uri: format!("s3://bucket/uploads/org/env/{name}"),
            completed: completed.then(|| ("0".repeat(64), 3, true)),
            columns: Vec::new(),
        }
    }

    fn held(count: usize, completed: impl Fn(usize) -> bool) -> Held {
        let mut held = Held::default();
        for n in 0..count {
            held.insert(format!("id{n}"), upload(&format!("f{n}"), completed(n)));
        }
        held
    }

    #[test]
    fn the_held_uploads_never_exceed_their_count() {
        let held = held(MAX_HELD.saturating_add(10), |_| false);
        assert_eq!(held.uploads.len(), MAX_HELD);
        assert_eq!(held.order.len(), MAX_HELD);
        // The oldest begun uploads went first.
        assert!(held.get("id0").is_none());
        assert!(held.get("id9").is_none());
        assert!(held.get("id10").is_some());
    }

    #[test]
    fn completed_uploads_are_forgotten_before_begun_ones() {
        // Every other upload completed; the newest begun ones stay longest.
        let held = held(MAX_HELD.saturating_add(4), |n| n & 1 == 1);
        assert!(held.get("id0").is_some(), "the oldest begun upload stays");
        for gone in ["id1", "id3", "id5", "id7"] {
            assert!(held.get(gone).is_none(), "{gone}");
        }
        assert!(held.get("id9").is_some());
    }

    const QUARTER: usize = MAX_HELD_BYTES / 4;

    #[test]
    fn the_held_uploads_never_exceed_their_bytes() {
        let mut held = Held::default();
        let heavy = |n: usize| Upload {
            file_name: "x".repeat(QUARTER),
            ..upload(&format!("h{n}"), false)
        };
        for n in 0..8 {
            held.insert(format!("id{n}"), heavy(n));
            assert!(held.bytes <= MAX_HELD_BYTES);
        }
        assert_eq!(held.uploads.len(), 3);
        assert!(held.get("id7").is_some());
        // Too heavy to hold at all: not remembered, nothing else forgotten.
        held.insert(
            "huge".to_owned(),
            Upload {
                file_name: "x".repeat(MAX_HELD_BYTES),
                ..upload("huge", false)
            },
        );
        assert!(held.get("huge").is_none());
        assert_eq!(held.uploads.len(), 3);
    }

    #[test]
    fn remembering_an_upload_again_replaces_it() {
        let mut held = held(2, |_| false);
        let before = held.bytes;
        held.insert("id0".to_owned(), upload("f0", true));
        assert_eq!(held.uploads.len(), 2);
        assert_eq!(held.order.len(), 2);
        assert_eq!(held.bytes, before.saturating_add(64));
        assert!(held.get("id0").is_some_and(|u| u.completed.is_some()));
    }
}
