//! Closed plan shape checks; ontology admission remains an adapter requirement.
use crate::{Aggregate, Field, Placement, Plan, Property, Unsupported};
use std::collections::BTreeSet;

impl Plan {
    /// Check the closed structural profile, including canonical field order.
    /// # Errors
    /// Unsupported shapes; this does not replace upstream ontology admission.
    pub fn validate(&self) -> Result<(), Unsupported> {
        let bad = || Unsupported("invalid closed metric profile".into());
        if self.window.0 >= self.window.1
            || self.filters.len() > 16
            || !self
                .fields
                .get(self.time)
                .is_some_and(|f| matches!(f.kind.base(), Property::Date | Property::Timestamp))
            || self
                .group
                .is_some_and(|i| !self.fields.get(i).is_some_and(text_field))
        {
            return Err(bad());
        }
        let linked = self.fields.iter().any(|f| f.placement == Placement::Target);
        if linked != self.link.is_some() {
            return Err(bad());
        }
        let mut order = vec![self.time];
        if let Some(i) = self.group {
            order.push(i);
        }
        if let Aggregate::Sum {
            amount,
            scale,
            currency,
        } = &self.aggregate
        {
            if *scale > 28
                || !self.fields.get(*amount).is_some_and(|f| {
                    f.placement == Placement::Measure
                        && matches!(f.kind.base(), Property::Decimal { scale: s } if *s == *scale)
                })
            {
                return Err(bad());
            }
            order.push(*amount);
            if let Some((i, code)) = currency {
                if code.is_empty() || !self.fields.get(*i).is_some_and(text_field) {
                    return Err(bad());
                }
                order.push(*i);
            }
        }
        for filter in &self.filters {
            if !self
                .fields
                .get(filter.field)
                .is_some_and(|f| f.placement == Placement::Measure && text_field(f))
            {
                return Err(bad());
            }
            order.push(filter.field);
        }
        let mut seen = BTreeSet::new();
        order.retain(|i| seen.insert(*i));
        if !order.iter().copied().eq(0..self.fields.len()) {
            return Err(bad());
        }
        let mut paths = BTreeSet::new();
        for field in &self.fields {
            if !paths.insert((field.placement == Placement::Target, &field.property)) {
                return Err(bad());
            }
        }
        Ok(())
    }
}
fn text_field(field: &Field) -> bool {
    matches!(field.kind.base(), Property::Text | Property::Enum(_))
}
