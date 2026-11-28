//! Pre-indexed lookup of object-type extensions by target name, preserving
//! exact declaration order across multi-level inheritance chains.

use std::collections::BTreeMap;

use norn_ir::{ObjectTypeDef, ObjectTypeExtension};

pub(crate) struct ExtensionIndex<'d> {
    by_target: BTreeMap<&'d str, Vec<(usize, &'d ObjectTypeExtension)>>,
}

impl<'d> ExtensionIndex<'d> {
    pub(crate) fn new(extensions: &'d [ObjectTypeExtension]) -> Self {
        let mut by_target: BTreeMap<&'d str, Vec<(usize, &'d ObjectTypeExtension)>> =
            BTreeMap::new();
        for (index, ext) in extensions.iter().enumerate() {
            by_target
                .entry(ext.target.as_str())
                .or_default()
                .push((index, ext));
        }
        Self { by_target }
    }

    pub(crate) fn applicable_for(
        &self,
        chain: &[&'d ObjectTypeDef],
    ) -> Vec<&'d ObjectTypeExtension> {
        if self.by_target.is_empty() {
            return Vec::new();
        }
        let mut applicable: Vec<(usize, &'d ObjectTypeExtension)> = Vec::new();
        let mut matched_targets = 0_usize;
        for def in chain {
            if let Some(entries) = self.by_target.get(def.name.as_str()) {
                applicable.extend(entries.iter().copied());
                matched_targets = matched_targets.saturating_add(1);
            }
        }
        if matched_targets > 1 {
            applicable.sort_unstable_by_key(|(index, _)| *index);
        }
        applicable.into_iter().map(|(_, ext)| ext).collect()
    }
}
