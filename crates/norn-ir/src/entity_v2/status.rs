//! Resource status of an EntityIR v2 definition (T1-ONT-1c, #2513; ruling
//! 2026-09-09 §5: "resource status (`experimental`/`active`/`deprecated`)").
//!
//! The status is the maturity of a *definition* (an object type or an
//! interface), not a business state of its objects. A business status
//! property and its declared allowed transitions (ruling 2026-09-25 §2,
//! "declared lifecycles") are [`super::lifecycle`] (T3-LIFE-1, #2672).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::present_non_null;

/// Grammar of a [`CalendarDate`] (published as the schema `pattern`;
/// `norn-checker` also checks that the date exists).
pub const CALENDAR_DATE_PATTERN: &str = r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$";

/// An ISO 8601 calendar date `YYYY-MM-DD` with no time and no zone.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct CalendarDate(#[schemars(regex(pattern = CALENDAR_DATE_PATTERN))] pub String);

impl CalendarDate {
    /// The date as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The resource status of a definition named by `N` (an object type or an
/// interface), tagged by `state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields,
    bound(serialize = "N: Serialize", deserialize = "N: Deserialize<'de>")
)]
pub enum ResourceStatus<N> {
    /// Published for trial; its shape may still change incompatibly.
    Experimental {},
    /// Stable: changes follow the compatibility rules of its package.
    Active {},
    /// Still valid, but new use is discouraged.
    Deprecated {
        /// The date the definition was deprecated.
        since: CalendarDate,
        /// The definition that replaces it, of the same kind.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "N")]
        replaced_by: Option<N>,
    },
}

impl<N> Default for ResourceStatus<N> {
    fn default() -> Self {
        Self::Active {}
    }
}

impl<N> ResourceStatus<N> {
    /// Whether the status is `active` (the canonical form omits it).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active {})
    }
}
