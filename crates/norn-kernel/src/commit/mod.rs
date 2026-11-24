//! Admission stages and their shared proposed-state projection.
mod edges;
mod law_links;
mod law_subjects;
mod overlay_core;
mod post_state;
mod pre;
mod stages;

pub(crate) use law_links::{CommitLinks, MAX_LINK_LAW_SUBJECTS};
pub(crate) use law_subjects::linked_subjects;
pub(crate) use post_state::{PostState, Projection};
pub use pre::{check_commit_identifiers, check_fact_key_identity};
pub(crate) use stages::check_commit_local;
