//! TXN-CELL-1: atomic admission covers proposal keys and every supported read.
//!
//! A local profile is not a splitting certificate. Unsupported inherited laws
//! retain their existing refusal; applicability never supplies ownership evidence.

use std::cell::RefCell;
use std::collections::BTreeSet;

use crate::cell_host::{LinkReadTarget, ObjectTypeName};
use crate::{CellCommit, CommitError, CommitView, Consumed, FactKey, FactVersion, LinkKey};

pub use crate::profile_ownership::{AtomicOwnership, supports_local};

pub(crate) struct ProfileView<'a, V: ?Sized> {
    inner: &'a V,
    reads: RefCell<BTreeSet<FactKey>>,
}

impl<'a, V: CommitView + ?Sized> ProfileView<'a, V> {
    pub(crate) fn new(inner: &'a V) -> Self {
        Self {
            inner,
            reads: RefCell::new(BTreeSet::new()),
        }
    }

    fn fact(&self, key: &FactKey) {
        self.reads.borrow_mut().insert(key.clone());
    }

    fn link(&self, key: &LinkKey) {
        self.fact(&key.source);
        self.fact(&key.target);
    }

    pub(crate) fn validate_consumed(
        &self,
        consumed: Option<&Consumed>,
    ) -> Result<(), CommitError<V::Error>> {
        if let Some(consumed) = consumed {
            let ownership = self
                .inner
                .consumed_ownership(consumed)
                .map_err(CommitError::Host)?;
            if !supports_local(ownership) {
                return Err(CommitError::AtomicDeliveryBoundary {
                    consumed: consumed.clone(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn validate<E, C, T>(
        &self,
        commit: &CellCommit<E, C, T>,
        model: crate::CommitModel<'_, '_>,
    ) -> Result<(), CommitError<V::Error>> {
        for read in &commit.reads {
            self.fact(&read.key);
        }
        for write in &commit.writes {
            self.fact(&write.key);
        }
        for write in &commit.links {
            self.link(&write.key);
        }
        for read in &commit.link_reads {
            self.fact(&read.source);
            for target in &read.targets {
                self.link(&target.key);
            }
        }
        for key in self.reads.borrow().iter() {
            let ownership = self
                .inner
                .atomic_ownership(&model.canonical(key))
                .map_err(CommitError::Host)?;
            if !supports_local(ownership) {
                return Err(CommitError::AtomicBoundary { key: key.clone() });
            }
        }
        Ok(())
    }
}

impl<V: CommitView + ?Sized> CommitView for ProfileView<'_, V> {
    type Error = V::Error;

    fn pending(&self, consumed: &Consumed) -> Result<bool, Self::Error> {
        self.inner.pending(consumed)
    }

    fn version(&self, key: &FactKey) -> Result<FactVersion, Self::Error> {
        self.fact(key);
        self.inner.version(key)
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Self::Error> {
        self.fact(key);
        self.inner.concrete(key)
    }

    fn can_read_observed_source_owners(&self) -> bool {
        self.inner.can_read_observed_source_owners()
    }

    fn observed_source_owner(
        &self,
        key: &FactKey,
    ) -> Result<Option<crate::snapshot_observe::ObservedSourceOwner>, Self::Error> {
        self.fact(key);
        self.inner.observed_source_owner(key)
    }

    fn link_version(&self, key: &LinkKey) -> Result<FactVersion, Self::Error> {
        self.link(key);
        self.inner.link_version(key)
    }

    fn link_targets(
        &self,
        source: &FactKey,
        link: &str,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        self.fact(source);
        let result = self.inner.link_targets(source, link)?;
        if let Some(targets) = &result {
            for target in targets {
                self.link(&target.key);
            }
        }
        Ok(result)
    }

    fn has_link_target(
        &self,
        source: &FactKey,
        link: &str,
        excluded: &BTreeSet<FactKey>,
    ) -> Result<Option<bool>, Self::Error> {
        self.fact(source);
        for key in excluded {
            self.fact(key);
        }
        // Preserve bounded existence reads on hosts whose adjacency is large.
        // Their ownership contract covers the source's entire link partition.
        self.inner.has_link_target(source, link, excluded)
    }

    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Self::Error> {
        self.fact(key);
        let links = self.inner.links_of(key)?;
        for link in &links {
            self.link(link);
        }
        Ok(links)
    }
}
