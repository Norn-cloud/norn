//! Deterministic geo functions (GEO-1, Core 07; #191).
//!
//! Four pure functions over WGS84 coordinates and RFC 7946 `GeoJSON` text:
//! great-circle distance, H3 cell index, shape validity and point-in-shape.
//! Each returns a discrete value (integer metres, a hexadecimal cell index, a
//! boolean) or a typed [`GeoRefusal`]; no binary64 value ever leaves this
//! module, so no receipt or key holds a float.
//!
//! The same input gives the same output on every host, by construction:
//!
//! - **Decode.** A coordinate is a decimal ([`Decimal`], or a number token of
//!   `GeoJSON` text read by [`RuntimeValue::from_json_text`]). It is used as
//!   the nearest binary64 to that decimal, ties to even, through the standard
//!   library's correctly rounded parser. `serde_json`'s own float parser is
//!   never used: its result depends on a Cargo feature that differs between
//!   build graphs.
//! - **Trigonometry.** Distance and cell indexing come from `h3o`, built
//!   without its `std` feature, so every `sin`, `cos`, `tan`, `asin`, `acos`
//!   and `atan2` is the pure-Rust `libm` crate, not the host's C library.
//!   `deny.toml` bans `h3o`'s `std` feature.
//! - **Predicates.** Ring validity, winding and containment come from `geo`,
//!   whose orientation test is the exact adaptive predicate of `robust`.
//!   Only `+`, `-`, `*`, `/` and `sqrt` are involved, which IEEE 754 defines
//!   exactly.
//!
//! `h3o`, `geo` and `geojson` are pinned to one exact version each in this
//! crate's `Cargo.toml`, which the HOST-TREE-1 source-tree pin covers; what
//! they depend on is pinned by `Cargo.lock`.
//!
//! These functions are not registered: [`crate::resolve`] answers nothing for
//! them, so no flow can `perform` one yet. [`call`] is the argument codec a
//! mapping-time or host binding uses.

pub mod shape;

use norn_ir::{Decimal, RuntimeValue};

pub use shape::{
    MAX_SHAPE_BYTES, MAX_SHAPE_POSITIONS, Shape, ShapeKind, point_in_shape, validate_shape,
};

/// The highest H3 resolution.
pub const MAX_H3_RESOLUTION: u8 = 15;
/// The distance between antipodal points, in whole metres: half the
/// circumference of the sphere [`distance_meters`] measures on.
pub const ANTIPODAL_METERS: u64 = 20_015_109;

/// Why a geo function refuses. Every refusal is final for its input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GeoRefusal {
    /// A coordinate is `NaN` or infinite.
    CoordinateNotFinite,
    /// A latitude is outside `[-90, 90]` degrees.
    LatitudeOutOfRange,
    /// A longitude is outside `[-180, 180]` degrees.
    LongitudeOutOfRange,
    /// An H3 resolution is not an integer in `0..=15`.
    ResolutionOutOfRange,
    /// A distance has no whole-metre value. Defensive: no pair of valid
    /// points reaches it.
    DistanceUnrepresentable,
    /// The shape text is longer than [`MAX_SHAPE_BYTES`].
    ShapeTooLarge,
    /// The shape holds more than [`MAX_SHAPE_POSITIONS`] positions.
    ShapeTooManyPositions,
    /// The shape text is not one exact JSON document.
    ShapeInvalidJson,
    /// The document is not an RFC 7946 geometry object.
    ShapeNotAGeometry,
    /// A geometry, a multi-geometry or a collection has no member.
    ShapeEmpty,
    /// A position has more than three numbers. (Fewer than two is
    /// [`Self::ShapeNotAGeometry`].)
    PositionArity,
    /// A `GeometryCollection` holds another `GeometryCollection`.
    NestedCollection,
    /// A `LineString` has fewer than two positions.
    LineTooShort,
    /// A linear ring has fewer than four positions.
    RingTooShort,
    /// A linear ring's first and last positions differ.
    RingNotClosed,
    /// A linear ring has fewer than three distinct positions.
    RingDegenerate,
    /// A linear ring crosses or touches itself.
    RingSelfIntersects,
    /// An exterior ring is not counterclockwise.
    ExteriorNotCounterclockwise,
    /// An interior ring (a hole) is not clockwise.
    HoleNotClockwise,
    /// A hole is not inside its polygon's exterior ring.
    HoleOutsideExterior,
    /// Two rings of one polygon cross or share a line.
    RingsIntersect,
    /// Two polygons of one `MultiPolygon` overlap or share a line.
    PolygonsOverlap,
    /// Point-in-shape was asked of a shape that is not a `Polygon` or
    /// `MultiPolygon`.
    ShapeNotAreal,
    /// [`call`] was given a name that is not a geo function.
    UnknownFunction,
    /// [`call`] was given the wrong number or kind of arguments.
    Arguments,
}

impl GeoRefusal {
    /// The stable refusal code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::CoordinateNotFinite => "geo.coordinate.not_finite",
            Self::LatitudeOutOfRange => "geo.coordinate.latitude_out_of_range",
            Self::LongitudeOutOfRange => "geo.coordinate.longitude_out_of_range",
            Self::ResolutionOutOfRange => "geo.h3.resolution_out_of_range",
            Self::DistanceUnrepresentable => "geo.distance.unrepresentable",
            Self::ShapeTooLarge => "geo.shape.too_large",
            Self::ShapeTooManyPositions => "geo.shape.too_many_positions",
            Self::ShapeInvalidJson => "geo.shape.invalid_json",
            Self::ShapeNotAGeometry => "geo.shape.not_a_geometry",
            Self::ShapeEmpty => "geo.shape.empty",
            Self::PositionArity => "geo.shape.position_arity",
            Self::NestedCollection => "geo.shape.nested_collection",
            Self::LineTooShort => "geo.shape.line_too_short",
            Self::RingTooShort => "geo.shape.ring_too_short",
            Self::RingNotClosed => "geo.shape.ring_not_closed",
            Self::RingDegenerate => "geo.shape.ring_degenerate",
            Self::RingSelfIntersects => "geo.shape.ring_self_intersects",
            Self::ExteriorNotCounterclockwise => "geo.shape.exterior_not_counterclockwise",
            Self::HoleNotClockwise => "geo.shape.hole_not_clockwise",
            Self::HoleOutsideExterior => "geo.shape.hole_outside_exterior",
            Self::RingsIntersect => "geo.shape.rings_intersect",
            Self::PolygonsOverlap => "geo.shape.polygons_overlap",
            Self::ShapeNotAreal => "geo.shape.not_areal",
            Self::UnknownFunction => "geo.call.unknown_function",
            Self::Arguments => "geo.call.arguments",
        }
    }
}

impl std::fmt::Display for GeoRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for GeoRefusal {}

/// The nearest binary64 to a decimal, ties to even. The standard library's
/// parser is correctly rounded on every target.
pub(crate) fn nearest_binary64(decimal: &Decimal) -> Result<f64, GeoRefusal> {
    decimal
        .to_string()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or(GeoRefusal::CoordinateNotFinite)
}

/// A WGS84 point in degrees: finite, latitude in `[-90, 90]` and longitude in
/// `[-180, 180]`, both bounds included. Longitudes `-180` and `180` stay
/// distinct values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wgs84Point {
    lat: f64,
    lon: f64,
}

impl Wgs84Point {
    /// A point from binary64 degrees.
    ///
    /// # Errors
    /// [`GeoRefusal::CoordinateNotFinite`], [`GeoRefusal::LatitudeOutOfRange`]
    /// or [`GeoRefusal::LongitudeOutOfRange`].
    pub fn new(lat: f64, lon: f64) -> Result<Self, GeoRefusal> {
        if !lat.is_finite() || !lon.is_finite() {
            return Err(GeoRefusal::CoordinateNotFinite);
        }
        if !(-90.0..=90.0).contains(&lat) {
            return Err(GeoRefusal::LatitudeOutOfRange);
        }
        if !(-180.0..=180.0).contains(&lon) {
            return Err(GeoRefusal::LongitudeOutOfRange);
        }
        Ok(Self { lat, lon })
    }

    /// A point from decimal degrees, each read as its nearest binary64. The
    /// range is checked on that binary64 value.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn from_decimal(lat: &Decimal, lon: &Decimal) -> Result<Self, GeoRefusal> {
        Self::new(nearest_binary64(lat)?, nearest_binary64(lon)?)
    }

    /// Latitude in degrees.
    #[must_use]
    pub const fn lat(self) -> f64 {
        self.lat
    }

    /// Longitude in degrees.
    #[must_use]
    pub const fn lon(self) -> f64 {
        self.lon
    }

    fn lat_lng(self) -> Result<h3o::LatLng, GeoRefusal> {
        h3o::LatLng::new(self.lat, self.lon).map_err(|_invalid| GeoRefusal::CoordinateNotFinite)
    }
}

/// The great-circle distance between two points, in whole metres.
///
/// The haversine distance on a sphere of radius 6,371,007.180918475 m (H3's
/// authalic Earth radius), as `h3o` computes it, rounded to the nearest
/// integer with ties to even. It is a spherical distance, not a WGS84
/// ellipsoidal (geodesic) one, and can differ from that by up to about 0.5%.
///
/// Every pair of points has a distance. For some antipodal pairs rounding
/// pushes the haversine term above one and `h3o` returns `NaN`; the distance
/// is then [`ANTIPODAL_METERS`], which is exact to the metre there: the true
/// distance is within 0.3 m below half the circumference.
///
/// # Errors
/// [`GeoRefusal::DistanceUnrepresentable`] is defensive: no pair of valid
/// points reaches it.
pub fn distance_meters(from: Wgs84Point, to: Wgs84Point) -> Result<u64, GeoRefusal> {
    let metres = from.lat_lng()?.distance_m(to.lat_lng()?);
    if metres.is_nan() {
        return Ok(ANTIPODAL_METERS);
    }
    if !metres.is_finite() || metres.is_sign_negative() {
        return Err(GeoRefusal::DistanceUnrepresentable);
    }
    // An integer-valued binary64 prints as its exact digits.
    format!("{:.0}", metres.round_ties_even())
        .parse::<u64>()
        .map_err(|_digits| GeoRefusal::DistanceUnrepresentable)
}

/// The H3 cell containing a point at a resolution, as the canonical index
/// text: 15 lower-case hexadecimal digits, no prefix.
///
/// # Errors
/// [`GeoRefusal::ResolutionOutOfRange`] above [`MAX_H3_RESOLUTION`].
pub fn h3_cell(point: Wgs84Point, resolution: u8) -> Result<String, GeoRefusal> {
    let resolution = h3o::Resolution::try_from(resolution)
        .map_err(|_invalid| GeoRefusal::ResolutionOutOfRange)?;
    Ok(point.lat_lng()?.to_cell(resolution).to_string())
}

/// One geo function, by its call name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GeoFunction {
    /// `geo.distance_meters(lat1, lon1, lat2, lon2)`: a whole number.
    DistanceMeters,
    /// `geo.h3_cell(lat, lon, resolution)`: the cell index text.
    H3Cell,
    /// `geo.point_in_shape(lat, lon, shape)`: a boolean.
    PointInShape,
    /// `geo.validate_shape(shape)`: `true`, or a refusal naming the defect.
    ValidateShape,
}

impl GeoFunction {
    /// Every geo function.
    pub const ALL: [Self; 4] = [
        Self::DistanceMeters,
        Self::H3Cell,
        Self::PointInShape,
        Self::ValidateShape,
    ];

    /// The call name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DistanceMeters => "geo.distance_meters",
            Self::H3Cell => "geo.h3_cell",
            Self::PointInShape => "geo.point_in_shape",
            Self::ValidateShape => "geo.validate_shape",
        }
    }

    /// The function with this call name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|function| function.name() == name)
    }

    /// Apply the function to positional arguments: coordinates and the
    /// resolution are numbers, a shape is `GeoJSON` text.
    ///
    /// # Errors
    /// [`GeoRefusal::Arguments`] for the wrong number or kind of arguments,
    /// otherwise the function's own refusal.
    pub fn call(self, arguments: &[RuntimeValue]) -> Result<RuntimeValue, GeoRefusal> {
        match (self, arguments) {
            (Self::DistanceMeters, [lat1, lon1, lat2, lon2]) => {
                distance_meters(point(lat1, lon1)?, point(lat2, lon2)?)
                    .map(|metres| RuntimeValue::Number(Decimal::from(metres)))
            }
            (Self::H3Cell, [lat, lon, RuntimeValue::Number(resolution)]) => {
                let point = point(lat, lon)?;
                let resolution = resolution
                    .to_string()
                    .parse::<u8>()
                    .map_err(|_integer| GeoRefusal::ResolutionOutOfRange)?;
                h3_cell(point, resolution).map(RuntimeValue::String)
            }
            (Self::PointInShape, [lat, lon, RuntimeValue::String(text)]) => {
                let point = point(lat, lon)?;
                point_in_shape(point, &validate_shape(text)?).map(RuntimeValue::Bool)
            }
            (Self::ValidateShape, [RuntimeValue::String(text)]) => {
                validate_shape(text).map(|_shape| RuntimeValue::Bool(true))
            }
            (Self::DistanceMeters | Self::H3Cell | Self::PointInShape | Self::ValidateShape, _) => {
                Err(GeoRefusal::Arguments)
            }
        }
    }
}

fn point(lat: &RuntimeValue, lon: &RuntimeValue) -> Result<Wgs84Point, GeoRefusal> {
    let (RuntimeValue::Number(lat), RuntimeValue::Number(lon)) = (lat, lon) else {
        return Err(GeoRefusal::Arguments);
    };
    Wgs84Point::from_decimal(lat, lon)
}

/// Apply the geo function named `name` to positional arguments.
///
/// # Errors
/// [`GeoRefusal::UnknownFunction`], or as [`GeoFunction::call`].
pub fn call(name: &str, arguments: &[RuntimeValue]) -> Result<RuntimeValue, GeoRefusal> {
    GeoFunction::from_name(name)
        .ok_or(GeoRefusal::UnknownFunction)?
        .call(arguments)
}
