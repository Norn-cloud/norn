//! RFC 7946 shape validity and point-in-shape (GEO-1, Core 07; #191).
//!
//! [`validate_shape`] reads `GeoJSON` geometry text and refuses anything that
//! is not a valid WGS84 geometry; [`point_in_shape`] then tests a point
//! against a validated `Polygon` or `MultiPolygon`.
//!
//! The text is read by [`RuntimeValue::from_json_text`] and decoded here, so
//! every number is an exact decimal until this module converts it (see the
//! parent module) and every defect has its own refusal. Polygon validity is
//! the exact pass in `planar`; containment is `geo`'s.
//!
//! A shape is planar in longitude and latitude, as RFC 7946 section 3.1.1
//! defines: an edge is the straight line between two positions in that plane,
//! not a geodesic. Nothing is wrapped: longitudes `-180` and `180` are
//! different values, so a shape that crosses the antimeridian must be split
//! (RFC 7946 section 3.1.9), and a ring edge spanning more than 180 degrees
//! of longitude is refused as an unsplit crossing. An optional third position
//! element (altitude) must be a number and is otherwise ignored, as are
//! `bbox` and foreign members.

use geo::coordinate_position::{CoordPos, CoordinatePosition};
use geo::{Coord, LineString, Polygon};
use norn_ir::{RuntimeObject, RuntimeValue};

use super::{GeoRefusal, Wgs84Point, nearest_binary64, planar};

/// The longest shape text, in bytes: the kernel's caller-input bound.
pub const MAX_SHAPE_BYTES: usize = 256 * 1024;
/// The most positions one shape may hold, over all its members. Checking
/// that no two edges meet costs time quadratic in the positions in the worst
/// case, so this bound, not the byte bound, is what limits the work.
pub const MAX_SHAPE_POSITIONS: usize = 4_096;
/// The most polygon rings one shape may hold, over all its polygons; so also
/// the most polygons. Each ring is tested against the others.
pub const MAX_SHAPE_RINGS: usize = 256;

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
    /// Every geometry type.
    pub const ALL: [Self; 7] = [
        Self::Point,
        Self::MultiPoint,
        Self::LineString,
        Self::MultiLineString,
        Self::Polygon,
        Self::MultiPolygon,
        Self::GeometryCollection,
    ];

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

    /// The geometry type with this `type` value, matched exactly.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
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
/// `GeometryCollection`) of at most [`MAX_SHAPE_BYTES`] bytes,
/// [`MAX_SHAPE_POSITIONS`] positions and [`MAX_SHAPE_RINGS`] polygon rings,
/// where:
///
/// - every position is two or three numbers, longitude in `[-180, 180]` then
///   latitude in `[-90, 90]`;
/// - no geometry, multi-geometry or collection is empty, and a `LineString`
///   has at least two positions;
/// - every polygon ring has at least four positions, is closed (first and
///   last positions identical), has no edge spanning more than 180 degrees of
///   longitude, has non-zero area and does not cross or touch itself;
/// - an exterior ring is counterclockwise and a hole clockwise. RFC 7946
///   section 3.1.6 asks readers to tolerate the other winding; this one does
///   not;
/// - a hole lies strictly inside its exterior ring, and no two rings of the
///   shape share a point, whether they belong to one polygon or to two
///   polygons of a `MultiPolygon`. A polygon may lie inside another's hole.
///
/// So an accepted polygon is valid in the OGC sense and its interior is
/// connected. Rings that touch at a single point, which OGC allows, are
/// refused. A position repeated consecutively is accepted and adds nothing.
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
    let mut budget = Budget::default();
    let (kind, areas) = geometry(&document, false, &mut budget)?;
    Ok(Shape {
        kind,
        positions: budget.positions,
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

/// What a shape has used of its bounds so far.
#[derive(Default)]
struct Budget {
    positions: usize,
    rings: usize,
}

impl Budget {
    fn spend(spent: &mut usize, bound: usize, refusal: GeoRefusal) -> Result<(), GeoRefusal> {
        *spent = spent
            .checked_add(1)
            .filter(|count| *count <= bound)
            .ok_or(refusal)?;
        Ok(())
    }
}

/// One decoded position. The altitude takes part only in ring closure.
#[derive(Clone, Copy)]
struct Position {
    coord: Coord<f64>,
    altitude: Option<f64>,
}

impl Position {
    fn same(&self, other: &Self) -> bool {
        let same = |left: f64, right: f64| left.total_cmp(&right).is_eq();
        same(self.coord.x, other.coord.x)
            && same(self.coord.y, other.coord.y)
            && match (self.altitude, other.altitude) {
                (None, None) => true,
                (Some(left), Some(right)) => same(left, right),
                (None, Some(_)) | (Some(_), None) => false,
            }
    }
}

/// The members of an array, where the geometry requires one.
fn list(value: &RuntimeValue) -> Result<&[RuntimeValue], GeoRefusal> {
    let RuntimeValue::Array(items) = value else {
        return Err(GeoRefusal::ShapeNotAGeometry);
    };
    Ok(items)
}

/// A non-empty array.
fn members(value: &RuntimeValue) -> Result<&[RuntimeValue], GeoRefusal> {
    let items = list(value)?;
    if items.is_empty() {
        return Err(GeoRefusal::ShapeEmpty);
    }
    Ok(items)
}

/// One position: an array of two or three numbers, in WGS84 range, within
/// the shape's position bound.
fn position(value: &RuntimeValue, budget: &mut Budget) -> Result<Position, GeoRefusal> {
    Budget::spend(
        &mut budget.positions,
        MAX_SHAPE_POSITIONS,
        GeoRefusal::ShapeTooManyPositions,
    )?;
    let number = |item: &RuntimeValue| {
        let RuntimeValue::Number(decimal) = item else {
            return Err(GeoRefusal::ShapeNotAGeometry);
        };
        nearest_binary64(decimal)
    };
    let items = list(value)?;
    let mut coords = [0.0_f64; 3];
    for (index, item) in items.iter().enumerate() {
        let parsed = number(item)?;
        if let Some(slot) = coords.get_mut(index) {
            *slot = parsed;
        }
    }
    let [lon, lat, alt] = coords;
    let altitude = match items.len() {
        2 => None,
        3 => Some(alt),
        _ => return Err(GeoRefusal::PositionArity),
    };
    let point = Wgs84Point::new(lat, lon)?;
    Ok(Position {
        coord: Coord {
            x: point.lon(),
            y: point.lat(),
        },
        altitude,
    })
}

fn line(value: &RuntimeValue, budget: &mut Budget) -> Result<(), GeoRefusal> {
    let items = list(value)?;
    if items.len() < 2 {
        return Err(GeoRefusal::LineTooShort);
    }
    items
        .iter()
        .try_for_each(|item| position(item, budget).map(|_pos| ()))
}

/// One polygon ring, with the checks RFC 7946 puts on a linear ring.
fn ring(value: &RuntimeValue, budget: &mut Budget) -> Result<LineString<f64>, GeoRefusal> {
    Budget::spend(
        &mut budget.rings,
        MAX_SHAPE_RINGS,
        GeoRefusal::ShapeTooManyRings,
    )?;
    let items = list(value)?;
    if items.len() < 4 {
        return Err(GeoRefusal::RingTooShort);
    }
    let mut coords = Vec::with_capacity(items.len().min(MAX_SHAPE_POSITIONS));
    let mut first = None;
    let mut last = None;
    for item in items {
        let pos = position(item, budget)?;
        if first.is_none() {
            first = Some(pos);
        }
        last = Some(pos);
        coords.push(pos.coord);
    }
    let (Some(first), Some(last)) = (first, last) else {
        return Err(GeoRefusal::RingTooShort);
    };
    if !first.same(&last) {
        return Err(GeoRefusal::RingNotClosed);
    }
    if coords
        .windows(2)
        .any(|pair| matches!(pair, [start, end] if (end.x - start.x).abs() > 180.0))
    {
        return Err(GeoRefusal::AntimeridianEdge);
    }
    Ok(LineString::new(coords))
}

/// One polygon's rings, exterior first.
fn polygon(value: &RuntimeValue, budget: &mut Budget) -> Result<Polygon<f64>, GeoRefusal> {
    let mut rings = members(value)?
        .iter()
        .map(|item| ring(item, budget))
        .collect::<Result<Vec<_>, GeoRefusal>>()?
        .into_iter();
    let exterior = rings.next().ok_or(GeoRefusal::ShapeEmpty)?;
    Ok(Polygon::new(exterior, rings.collect()))
}

/// The geometry's type, and its `coordinates` or `geometries` member: a
/// geometry object has the one its type defines and not the other.
fn parts(object: &RuntimeObject) -> Result<(ShapeKind, &RuntimeValue), GeoRefusal> {
    let Some(RuntimeValue::String(name)) = object.get("type") else {
        return Err(GeoRefusal::ShapeNotAGeometry);
    };
    let kind = ShapeKind::from_name(name).ok_or(GeoRefusal::ShapeNotAGeometry)?;
    let (own, other) = match kind {
        ShapeKind::GeometryCollection => ("geometries", "coordinates"),
        ShapeKind::Point
        | ShapeKind::MultiPoint
        | ShapeKind::LineString
        | ShapeKind::MultiLineString
        | ShapeKind::Polygon
        | ShapeKind::MultiPolygon => ("coordinates", "geometries"),
    };
    match (object.get(own), object.get(other)) {
        (Some(value), None) => Ok((kind, value)),
        (None | Some(_), Some(_)) | (None, None) => Err(GeoRefusal::ShapeNotAGeometry),
    }
}

/// Check one geometry and return its type and polygons (none unless it is a
/// `Polygon` or `MultiPolygon`).
fn geometry(
    value: &RuntimeValue,
    in_collection: bool,
    budget: &mut Budget,
) -> Result<(ShapeKind, Vec<Polygon<f64>>), GeoRefusal> {
    let RuntimeValue::Object(object) = value else {
        return Err(GeoRefusal::ShapeNotAGeometry);
    };
    let (kind, value) = parts(object)?;
    let areas = match kind {
        ShapeKind::GeometryCollection => {
            if in_collection {
                return Err(GeoRefusal::NestedCollection);
            }
            for member in members(value)? {
                geometry(member, true, budget)?;
            }
            Vec::new()
        }
        ShapeKind::Point => {
            position(value, budget)?;
            Vec::new()
        }
        ShapeKind::MultiPoint => {
            members(value)?
                .iter()
                .try_for_each(|item| position(item, budget).map(|_pos| ()))?;
            Vec::new()
        }
        ShapeKind::LineString => {
            line(value, budget)?;
            Vec::new()
        }
        ShapeKind::MultiLineString => {
            for item in members(value)? {
                line(item, budget)?;
            }
            Vec::new()
        }
        ShapeKind::Polygon => vec![polygon(value, budget)?],
        ShapeKind::MultiPolygon => members(value)?
            .iter()
            .map(|item| polygon(item, budget))
            .collect::<Result<Vec<_>, GeoRefusal>>()?,
    };
    planar::check_polygons(&areas)?;
    Ok((kind, areas))
}
