//! RFC 7946 shape validity and point-in-shape (GEO-1, Core 07; #191).
//!
//! [`validate_shape`] reads `GeoJSON` geometry text and refuses anything that
//! is not a valid WGS84 geometry; [`point_in_shape`] then tests a point
//! against a validated `Polygon` or `MultiPolygon`.
//!
//! Decoding is `geojson`'s, over numbers this module converts itself (see the
//! parent module). Ring validity, winding and containment are `geo`'s.
//!
//! A shape is planar in longitude and latitude, as RFC 7946 section 3.1.1
//! defines: an edge is the straight line between two positions in that plane,
//! not a geodesic. Nothing is wrapped: longitudes `-180` and `180` are
//! different values, so a shape that crosses the antimeridian must be split
//! (RFC 7946 section 3.1.9). An optional third position element (altitude)
//! must be finite and is otherwise ignored, as are `bbox` and foreign
//! members.

use geo::algorithm::validation::{InvalidMultiPolygon, InvalidPolygon};
use geo::coordinate_position::{CoordPos, CoordinatePosition};
use geo::winding_order::WindingOrder;
use geo::{Coord, MultiPolygon, Polygon, Validation, Winding};
use geojson::{GeometryValue, Position};
use norn_ir::RuntimeValue;

use super::{GeoRefusal, Wgs84Point, nearest_binary64};

/// The longest shape text, in bytes: the kernel's caller-input bound.
pub const MAX_SHAPE_BYTES: usize = 256 * 1024;
/// The most positions one shape may hold, over all its members. Validating a
/// ring costs time quadratic in its length (the self-intersection test), so
/// this bound, not the byte bound, is what limits the work.
pub const MAX_SHAPE_POSITIONS: usize = 4_096;

/// The RFC 7946 geometry type of a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShapeKind {
    /// `Point`.
    Point,
    /// `MultiPoint`.
    MultiPoint,
    /// `LineString`.
    LineString,
    /// `MultiLineString`.
    MultiLineString,
    /// `Polygon`.
    Polygon,
    /// `MultiPolygon`.
    MultiPolygon,
    /// `GeometryCollection`.
    GeometryCollection,
}

impl ShapeKind {
    /// The RFC 7946 `type` value.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Point => "Point",
            Self::MultiPoint => "MultiPoint",
            Self::LineString => "LineString",
            Self::MultiLineString => "MultiLineString",
            Self::Polygon => "Polygon",
            Self::MultiPolygon => "MultiPolygon",
            Self::GeometryCollection => "GeometryCollection",
        }
    }

    const fn of(value: &GeometryValue) -> Self {
        match value {
            GeometryValue::Point { .. } => Self::Point,
            GeometryValue::MultiPoint { .. } => Self::MultiPoint,
            GeometryValue::LineString { .. } => Self::LineString,
            GeometryValue::MultiLineString { .. } => Self::MultiLineString,
            GeometryValue::Polygon { .. } => Self::Polygon,
            GeometryValue::MultiPolygon { .. } => Self::MultiPolygon,
            GeometryValue::GeometryCollection { .. } => Self::GeometryCollection,
        }
    }
}

/// A shape [`validate_shape`] accepted. Only that function builds one.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    kind: ShapeKind,
    positions: usize,
    areas: Vec<Polygon<f64>>,
}

impl Shape {
    /// The shape's geometry type.
    #[must_use]
    pub const fn kind(&self) -> ShapeKind {
        self.kind
    }

    /// How many positions the shape holds, over all its members.
    #[must_use]
    pub const fn positions(&self) -> usize {
        self.positions
    }
}

/// Validate `GeoJSON` geometry text.
///
/// Accepted: one RFC 7946 geometry object (`Point`, `MultiPoint`,
/// `LineString`, `MultiLineString`, `Polygon`, `MultiPolygon` or a flat
/// `GeometryCollection`) of at most [`MAX_SHAPE_BYTES`] bytes and
/// [`MAX_SHAPE_POSITIONS`] positions, where:
///
/// - every position is two or three finite numbers, longitude in
///   `[-180, 180]` then latitude in `[-90, 90]`;
/// - no geometry, multi-geometry or collection is empty, and a `LineString`
///   has at least two positions;
/// - every polygon ring has at least four positions, is closed (first and
///   last positions identical) and does not cross or touch itself;
/// - an exterior ring is counterclockwise and a hole clockwise (the RFC 7946
///   right-hand rule, enforced rather than tolerated);
/// - a hole lies inside its exterior ring, rings of one polygon do not cross
///   or share a line, and polygons of one `MultiPolygon` do not overlap or
///   share a line.
///
/// A `Feature` or `FeatureCollection` is not a geometry and is refused.
///
/// # Errors
/// The [`GeoRefusal`] naming the first defect found.
pub fn validate_shape(text: &str) -> Result<Shape, GeoRefusal> {
    if text.len() > MAX_SHAPE_BYTES {
        return Err(GeoRefusal::ShapeTooLarge);
    }
    let document =
        RuntimeValue::from_json_text(text).map_err(|_json| GeoRefusal::ShapeInvalidJson)?;
    let geometry: geojson::Geometry = serde_json::from_value(binary_json(&document)?)
        .map_err(|_shape| GeoRefusal::ShapeNotAGeometry)?;
    let mut positions = 0;
    let areas = check_geometry(&geometry.value, &mut positions)?;
    Ok(Shape {
        kind: ShapeKind::of(&geometry.value),
        positions,
        areas,
    })
}

/// Whether a point is in a validated `Polygon` or `MultiPolygon`.
///
/// The boundary is part of the shape: a point on an edge or a vertex of an
/// exterior ring or of a hole is in the shape. A point strictly inside a hole
/// is not. For a `MultiPolygon` the answer is whether any member polygon
/// holds the point. The test is exact for the binary64 coordinates: no
/// tolerance is applied.
///
/// # Errors
/// [`GeoRefusal::ShapeNotAreal`] for any other geometry type.
pub fn point_in_shape(point: Wgs84Point, shape: &Shape) -> Result<bool, GeoRefusal> {
    match shape.kind {
        ShapeKind::Polygon | ShapeKind::MultiPolygon => {}
        ShapeKind::Point
        | ShapeKind::MultiPoint
        | ShapeKind::LineString
        | ShapeKind::MultiLineString
        | ShapeKind::GeometryCollection => return Err(GeoRefusal::ShapeNotAreal),
    }
    let coord = Coord {
        x: point.lon(),
        y: point.lat(),
    };
    Ok(shape.areas.iter().any(|polygon| {
        matches!(
            polygon.coordinate_position(&coord),
            CoordPos::Inside | CoordPos::OnBoundary
        )
    }))
}

/// The document with every number as its nearest binary64, built directly so
/// `serde_json`'s text parser never reads a coordinate.
fn binary_json(value: &RuntimeValue) -> Result<serde_json::Value, GeoRefusal> {
    Ok(match value {
        RuntimeValue::Null => serde_json::Value::Null,
        RuntimeValue::Bool(flag) => serde_json::Value::Bool(*flag),
        RuntimeValue::Number(decimal) => serde_json::Number::from_f64(nearest_binary64(decimal)?)
            .map(serde_json::Value::Number)
            .ok_or(GeoRefusal::CoordinateNotFinite)?,
        RuntimeValue::String(text) => serde_json::Value::String(text.clone()),
        RuntimeValue::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(binary_json)
                .collect::<Result<_, GeoRefusal>>()?,
        ),
        RuntimeValue::Object(members) => serde_json::Value::Object(
            members
                .iter()
                .map(|(key, member)| Ok((key.clone(), binary_json(member)?)))
                .collect::<Result<_, GeoRefusal>>()?,
        ),
    })
}

/// Check one geometry and return its polygons (none unless it is areal).
fn check_geometry(
    value: &GeometryValue,
    positions: &mut usize,
) -> Result<Vec<Polygon<f64>>, GeoRefusal> {
    match value {
        GeometryValue::GeometryCollection { geometries } => {
            non_empty(geometries)?;
            for member in geometries {
                if ShapeKind::of(&member.value) == ShapeKind::GeometryCollection {
                    return Err(GeoRefusal::NestedCollection);
                }
                check_geometry(&member.value, positions)?;
            }
            Ok(Vec::new())
        }
        GeometryValue::Point { coordinates } => {
            check_position(coordinates, positions)?;
            Ok(Vec::new())
        }
        GeometryValue::MultiPoint { coordinates } => {
            non_empty(coordinates)?;
            check_positions(coordinates, positions)?;
            Ok(Vec::new())
        }
        GeometryValue::LineString { coordinates } => {
            check_line(coordinates, positions)?;
            Ok(Vec::new())
        }
        GeometryValue::MultiLineString { coordinates } => {
            non_empty(coordinates)?;
            for line in coordinates {
                check_line(line, positions)?;
            }
            Ok(Vec::new())
        }
        GeometryValue::Polygon { coordinates } => {
            check_rings(coordinates, positions)?;
            let polygon = Polygon::<f64>::try_from(value)
                .map_err(|_convert| GeoRefusal::ShapeNotAGeometry)?;
            polygon
                .check_validation()
                .map_err(|invalid| polygon_refusal(&invalid))?;
            check_winding(&polygon)?;
            Ok(vec![polygon])
        }
        GeometryValue::MultiPolygon { coordinates } => {
            non_empty(coordinates)?;
            for rings in coordinates {
                check_rings(rings, positions)?;
            }
            let polygons = MultiPolygon::<f64>::try_from(value)
                .map_err(|_convert| GeoRefusal::ShapeNotAGeometry)?;
            polygons
                .check_validation()
                .map_err(|invalid| multi_polygon_refusal(&invalid))?;
            for polygon in &polygons {
                check_winding(polygon)?;
            }
            Ok(polygons.0)
        }
    }
}

fn non_empty<T>(members: &[T]) -> Result<(), GeoRefusal> {
    if members.is_empty() {
        Err(GeoRefusal::ShapeEmpty)
    } else {
        Ok(())
    }
}

/// One position: two or three finite numbers, in WGS84 range, within the
/// shape's position budget.
fn check_position(position: &Position, positions: &mut usize) -> Result<(), GeoRefusal> {
    *positions = positions
        .checked_add(1)
        .filter(|count| *count <= MAX_SHAPE_POSITIONS)
        .ok_or(GeoRefusal::ShapeTooManyPositions)?;
    let (lon, lat) = match position.as_slice() {
        [lon, lat] => (*lon, *lat),
        [lon, lat, altitude] if altitude.is_finite() => (*lon, *lat),
        [_, _, _] => return Err(GeoRefusal::CoordinateNotFinite),
        _ => return Err(GeoRefusal::PositionArity),
    };
    Wgs84Point::new(lat, lon).map(|_point| ())
}

fn check_positions(line: &[Position], positions: &mut usize) -> Result<(), GeoRefusal> {
    line.iter()
        .try_for_each(|position| check_position(position, positions))
}

fn check_line(line: &[Position], positions: &mut usize) -> Result<(), GeoRefusal> {
    if line.len() < 2 {
        return Err(GeoRefusal::LineTooShort);
    }
    check_positions(line, positions)
}

/// One polygon's rings, before conversion: `geo` would close an open ring.
fn check_rings(rings: &[Vec<Position>], positions: &mut usize) -> Result<(), GeoRefusal> {
    non_empty(rings)?;
    for ring in rings {
        let (Some(first), Some(last)) = (ring.first(), ring.last()) else {
            return Err(GeoRefusal::RingTooShort);
        };
        if ring.len() < 4 {
            return Err(GeoRefusal::RingTooShort);
        }
        check_positions(ring, positions)?;
        if !same_position(first, last) {
            return Err(GeoRefusal::RingNotClosed);
        }
    }
    Ok(())
}

/// Whether two positions hold identical values, element for element.
fn same_position(left: &Position, right: &Position) -> bool {
    left.len() == right.len()
        && left
            .as_slice()
            .iter()
            .zip(right.as_slice())
            .all(|(a, b)| a.total_cmp(b).is_eq())
}

/// The RFC 7946 right-hand rule: exterior counterclockwise, holes clockwise.
fn check_winding(polygon: &Polygon<f64>) -> Result<(), GeoRefusal> {
    match polygon.exterior().winding_order() {
        Some(WindingOrder::CounterClockwise) => {}
        Some(WindingOrder::Clockwise) => return Err(GeoRefusal::ExteriorNotCounterclockwise),
        None => return Err(GeoRefusal::RingDegenerate),
    }
    for hole in polygon.interiors() {
        match hole.winding_order() {
            Some(WindingOrder::Clockwise) => {}
            Some(WindingOrder::CounterClockwise) => return Err(GeoRefusal::HoleNotClockwise),
            None => return Err(GeoRefusal::RingDegenerate),
        }
    }
    Ok(())
}

const fn polygon_refusal(invalid: &InvalidPolygon) -> GeoRefusal {
    match invalid {
        InvalidPolygon::TooFewPointsInRing(_) => GeoRefusal::RingDegenerate,
        InvalidPolygon::SelfIntersection(_) => GeoRefusal::RingSelfIntersects,
        InvalidPolygon::NonFiniteCoord(_, _) => GeoRefusal::CoordinateNotFinite,
        InvalidPolygon::InteriorRingNotContainedInExteriorRing(_) => {
            GeoRefusal::HoleOutsideExterior
        }
        InvalidPolygon::IntersectingRingsOnALine(_, _)
        | InvalidPolygon::IntersectingRingsOnAnArea(_, _) => GeoRefusal::RingsIntersect,
    }
}

const fn multi_polygon_refusal(invalid: &InvalidMultiPolygon) -> GeoRefusal {
    match invalid {
        InvalidMultiPolygon::InvalidPolygon(_, polygon) => polygon_refusal(polygon),
        InvalidMultiPolygon::ElementsOverlaps(_, _)
        | InvalidMultiPolygon::ElementsTouchOnALine(_, _) => GeoRefusal::PolygonsOverlap,
    }
}
