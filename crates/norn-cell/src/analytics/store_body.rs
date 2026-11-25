//! A `store` body, current or from a cell replaying a pre-law store: the
//! body of [`super::AnalyticalEngine::store_dataset`], so it is decoded on every target.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::value::RawValue;

use super::{
    LandRequest, LandResponse, LandedFile, LoadRequest, LoadResponse, RESIDUE_PAGE_ROWS,
    StoreRequest, StoredObject, ValidRange,
};

/// A store body from either the current cell or a cell replaying a
/// pre-law store. Legacy bodies can only replay a legacy receipt.
#[derive(Deserialize)]
#[serde(try_from = "Box<RawValue>")]
pub struct StoreBody {
    request: StoreRequest,
    legacy: bool,
}

impl StoreBody {
    /// The decoded request, including the scope and object digests.
    #[must_use]
    pub const fn request(&self) -> &StoreRequest {
        &self.request
    }

    /// Whether this body decoded from the pre-law legacy store shape.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.legacy
    }
}

impl TryFrom<Box<RawValue>> for StoreBody {
    type Error = serde_json::Error;

    fn try_from(body: Box<RawValue>) -> Result<Self, Self::Error> {
        match serde_json::from_str(body.get()) {
            Ok(request) => Ok(Self {
                request,
                legacy: false,
            }),
            Err(error) => legacy_store_request(&body)
                .map(|request| Self {
                    request,
                    legacy: true,
                })
                .map_err(|_old_shape_error| error),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyLanding {
    load: LoadRequest,
    pins: crate::dataset_manifest::ManifestPins,
    #[serde(default)]
    landing: String,
    #[serde(default)]
    attempt: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyLandingAnswer {
    landing: String,
    attempt: String,
    load: LoadResponse,
    data: LandedFile,
    held: LandedFile,
    valid_time: Vec<ValidRange>,
    stored: Option<String>,
}

impl From<LegacyLandingAnswer> for LandResponse {
    fn from(old: LegacyLandingAnswer) -> Self {
        Self {
            landing: old.landing,
            attempt: old.attempt,
            load: old.load,
            data: Some(old.data),
            held: Some(old.held),
            valid_time: old.valid_time,
            stored: old.stored,
            findings: BTreeMap::new(),
            held_by_rules: 0,
            residue_total: 0,
            residue: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyStore {
    land: LegacyLanding,
    landed: LegacyLandingAnswer,
    data: StoredObject,
    held: StoredObject,
}

#[cfg(any(all(feature = "duckdb", feature = "native"), feature = "datafusion"))]
pub(super) fn legacy_land_response(raw: &RawValue) -> serde_json::Result<LandResponse> {
    serde_json::from_str::<LegacyLandingAnswer>(raw.get()).map(Into::into)
}

pub(super) fn legacy_store_request(raw: &RawValue) -> serde_json::Result<StoreRequest> {
    let old: LegacyStore = serde_json::from_str(raw.get())?;
    Ok(StoreRequest {
        land: LandRequest {
            load: old.land.load,
            pins: old.land.pins,
            landing: old.land.landing,
            attempt: old.land.attempt,
            checks: Vec::new(),
            residue_page: RESIDUE_PAGE_ROWS,
        },
        landed: old.landed.into(),
        data: old.data,
        held: old.held,
    })
}
