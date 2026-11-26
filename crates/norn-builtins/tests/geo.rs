//! GEO-1 (Core 07, #191): the deterministic geo functions.
//!
//! The golden values are independent of this crate. Distances were computed
//! with Python's `math` haversine on H3's authalic radius. Cell indexes came
//! from the H3 C library (v4, through the `h3` Python package), and the first
//! one is the example in H3's own documentation. Containment and validity
//! cases are small enough to decide by hand.

use norn_builtins::geo::{
    self, GeoFunction, GeoRefusal, MAX_SHAPE_BYTES, MAX_SHAPE_POSITIONS, MAX_SHAPE_RINGS,
    ShapeKind, Wgs84Point,
};
use norn_ir::RuntimeValue;

/// A counterclockwise square with a clockwise square hole.
const DONUT: &str = r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[2,2],[2,8],[8,8],[8,2],[2,2]]]}"#;
/// Two squares that share no point.
const SEPARATE: &str = r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[10,0],[10,10],[0,10],[0,0]]],[[[12,12],[20,12],[20,20],[12,20],[12,12]]]]}"#;
/// A square with a square hole, and a smaller square inside that hole.
const ISLAND: &str = r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[2,2],[2,8],[8,8],[8,2],[2,2]]],[[[4,4],[6,4],[6,6],[4,6],[4,4]]]]}"#;
/// A triangle whose hypotenuse runs along `lat == lon` between inexact decimals.
const TRIANGLE: &str =
    r#"{"type":"Polygon","coordinates":[[[0.1,0.1],[0.3,0.1],[0.3,0.3],[0.1,0.1]]]}"#;

fn point(lat: f64, lon: f64) -> Result<Wgs84Point, String> {
    Wgs84Point::new(lat, lon).map_err(|refusal| format!("({lat}, {lon}): {refusal}"))
}

fn number(text: &str) -> Result<RuntimeValue, String> {
    RuntimeValue::from_json_text(text).map_err(|error| format!("{text}: {error}"))
}

fn numbers(texts: &[&str]) -> Result<Vec<RuntimeValue>, String> {
    texts.iter().map(|text| number(text)).collect()
}

fn text(value: &str) -> RuntimeValue {
    RuntimeValue::String(value.to_owned())
}

fn contains(shape: &str, lat: f64, lon: f64) -> Result<bool, String> {
    let shape = geo::validate_shape(shape).map_err(|refusal| refusal.to_string())?;
    geo::point_in_shape(point(lat, lon)?, &shape).map_err(|refusal| refusal.to_string())
}

/// GEO-POINT-RANGE: both bounds are included; anything else is refused.
#[test]
fn a_point_is_finite_and_within_wgs84_range() -> Result<(), String> {
    for (lat, lon) in [(90.0, 180.0), (-90.0, -180.0), (0.0, 0.0)] {
        let accepted = point(lat, lon)?;
        assert_eq!((accepted.lat(), accepted.lon()), (lat, lon));
    }
    for (lat, lon, refusal) in [
        (90.000_001, 0.0, GeoRefusal::LatitudeOutOfRange),
        (-90.000_001, 0.0, GeoRefusal::LatitudeOutOfRange),
        (0.0, 180.000_001, GeoRefusal::LongitudeOutOfRange),
        (0.0, -180.000_001, GeoRefusal::LongitudeOutOfRange),
        (f64::NAN, 0.0, GeoRefusal::CoordinateNotFinite),
        (0.0, f64::NAN, GeoRefusal::CoordinateNotFinite),
        (f64::INFINITY, 0.0, GeoRefusal::CoordinateNotFinite),
        (0.0, f64::NEG_INFINITY, GeoRefusal::CoordinateNotFinite),
    ] {
        assert_eq!(Wgs84Point::new(lat, lon), Err(refusal), "({lat}, {lon})");
    }
    Ok(())
}

/// GEO-DISTANCE-GOLDEN: whole metres on H3's authalic sphere, symmetric.
#[test]
fn distance_is_the_haversine_distance_in_whole_metres() -> Result<(), String> {
    for ((lat1, lon1), (lat2, lon2), metres) in [
        ((0.0, 0.0), (0.0, 0.0), 0),
        ((0.0, 0.0), (0.0, 1.0), 111_195),
        ((89.999_999, 0.0), (90.0, 0.0), 0),
        ((30.0444, 31.2357), (31.2001, 29.9187), 179_985),
        ((51.5074, -0.1278), (40.7128, -74.006), 5_570_228),
        ((-33.8688, 151.2093), (35.6762, 139.6503), 7_825_827),
        // Antipodal: half the sphere's circumference.
        ((90.0, 0.0), (-90.0, 0.0), 20_015_109),
        ((0.0, 0.0), (0.0, 180.0), 20_015_109),
        // The antimeridian is one meridian, whichever sign names it.
        ((0.0, -180.0), (0.0, 180.0), 0),
    ] {
        let (from, to) = (point(lat1, lon1)?, point(lat2, lon2)?);
        assert_eq!(
            geo::distance_meters(from, to),
            Ok(metres),
            "{from:?} {to:?}"
        );
        assert_eq!(
            geo::distance_meters(to, from),
            Ok(metres),
            "{to:?} {from:?}"
        );
    }
    Ok(())
}

/// GEO-DISTANCE-TOTAL: every pair of points has a distance. For some
/// antipodal pairs rounding pushes the haversine term past one; they still
/// answer half the circumference.
#[test]
fn every_antipodal_pair_has_the_antipodal_distance() -> Result<(), String> {
    for step in 0_u32..2_000 {
        let lat = f64::from(step) * 0.089_9 - 89.9;
        let lon = f64::from(step) * 0.179_9 - 179.9;
        let opposite = if lon > 0.0 { lon - 180.0 } else { lon + 180.0 };
        let (from, to) = (point(lat, lon)?, point(-lat, opposite)?);
        assert_eq!(
            geo::distance_meters(from, to),
            Ok(geo::ANTIPODAL_METERS),
            "{from:?}"
        );
    }
    assert_eq!(geo::ANTIPODAL_METERS, 20_015_109);
    Ok(())
}

/// GEO-H3-GOLDEN: the cell index text of the H3 reference implementation.
#[test]
fn h3_cell_is_the_reference_cell_index() -> Result<(), String> {
    for (lat, lon, resolution, cell) in [
        (37.361_559_3, -122.055_323_8, 7, "87283472bffffff"),
        (37.361_559_3, -122.055_323_8, 0, "8029fffffffffff"),
        (37.361_559_3, -122.055_323_8, 15, "8f283470d921c65"),
        (30.0444, 31.2357, 9, "893e628e67bffff"),
        (-33.8688, 151.2093, 5, "85be0e37fffffff"),
        (90.0, 0.0, 3, "830326fffffffff"),
        (-90.0, 0.0, 3, "83f293fffffffff"),
        (0.0, 180.0, 4, "847eb57ffffffff"),
        (0.0, -180.0, 4, "847eb57ffffffff"),
    ] {
        let found = geo::h3_cell(point(lat, lon)?, resolution);
        assert_eq!(found.as_deref(), Ok(cell), "({lat}, {lon}) at {resolution}");
    }
    assert_eq!(
        geo::h3_cell(point(0.0, 0.0)?, 16),
        Err(GeoRefusal::ResolutionOutOfRange)
    );
    assert_eq!(geo::MAX_H3_RESOLUTION, 15);
    Ok(())
}

/// GEO-SHAPE-VALID: each RFC 7946 geometry type, altitude, `bbox` and
/// foreign members.
#[test]
fn valid_shapes_are_accepted_with_their_type_and_size() -> Result<(), String> {
    for (shape, kind, positions) in [
        (
            r#"{"type":"Point","coordinates":[31.2357,30.0444]}"#,
            ShapeKind::Point,
            1,
        ),
        (
            r#"{"type":"Point","coordinates":[31.2357,30.0444,12.5]}"#,
            ShapeKind::Point,
            1,
        ),
        (
            r#"{"type":"MultiPoint","coordinates":[[0,0],[180,90],[-180,-90]]}"#,
            ShapeKind::MultiPoint,
            3,
        ),
        (
            r#"{"type":"LineString","coordinates":[[0,0],[1,1]]}"#,
            ShapeKind::LineString,
            2,
        ),
        (
            r#"{"type":"MultiLineString","coordinates":[[[0,0],[1,1]],[[2,2],[3,3],[4,4]]]}"#,
            ShapeKind::MultiLineString,
            5,
        ),
        (DONUT, ShapeKind::Polygon, 10),
        (SEPARATE, ShapeKind::MultiPolygon, 10),
        (TRIANGLE, ShapeKind::Polygon, 4),
        // A position repeated consecutively is accepted.
        (
            r#"{"type":"Polygon","coordinates":[[[0,0],[0,0],[10,0],[10,0],[10,10],[0,10],[0,0],[0,0]]]}"#,
            ShapeKind::Polygon,
            8,
        ),
        // A vertex on a straight edge is accepted.
        (
            r#"{"type":"Polygon","coordinates":[[[0,0],[5,0],[10,0],[10,10],[0,10],[0,0]]]}"#,
            ShapeKind::Polygon,
            6,
        ),
        // A polygon may sit in another polygon's hole.
        (ISLAND, ShapeKind::MultiPolygon, 15),
        // The whole plane of valid coordinates: no edge spans more than 180
        // degrees of longitude.
        (
            r#"{"type":"Polygon","coordinates":[[[-180,-90],[0,-90],[180,-90],[180,90],[0,90],[-180,90],[-180,-90]]]}"#,
            ShapeKind::Polygon,
            7,
        ),
        (
            r#"{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]},{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}]}"#,
            ShapeKind::GeometryCollection,
            5,
        ),
        (
            r#"{"type":"Point","coordinates":[1,2],"bbox":[1,2,1,2],"note":{"any":["member",1.5,null,true]}}"#,
            ShapeKind::Point,
            1,
        ),
    ] {
        let accepted =
            geo::validate_shape(shape).map_err(|refusal| format!("{shape}: {refusal}"))?;
        assert_eq!(
            (accepted.kind(), accepted.positions()),
            (kind, positions),
            "{shape}"
        );
        assert_eq!(accepted.kind().name(), kind.name());
    }
    Ok(())
}

/// Every shape defect with the refusal that names it.
const REFUSED: &[(&str, GeoRefusal)] = &[
    ("", GeoRefusal::ShapeInvalidJson),
    ("{", GeoRefusal::ShapeInvalidJson),
    (
        r#"{"type":"Point","coordinates":[NaN,0]}"#,
        GeoRefusal::ShapeInvalidJson,
    ),
    (
        r#"{"type":"Point","coordinates":[1e400,0]}"#,
        GeoRefusal::ShapeInvalidJson,
    ),
    (
        r#"{"type":"Point","coordinates":[1,2],"type":"Point"}"#,
        GeoRefusal::ShapeInvalidJson,
    ),
    ("[1,2]", GeoRefusal::ShapeNotAGeometry),
    (r#"{"coordinates":[1,2]}"#, GeoRefusal::ShapeNotAGeometry),
    (
        r#"{"type":"Circle","coordinates":[1,2]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":null}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"FeatureCollection","features":[]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"Point","coordinates":["1","2"]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"Point","coordinates":[null]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"Point","coordinates":["x",1,2,3]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"Point","coordinates":[0,0,0,null]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"Point","coordinates":[1,2],"geometries":[]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]}],"coordinates":[1,2]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"point","coordinates":[1,2]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    (
        r#"{"type":"LineString","coordinates":[[0,0],[1,null]]}"#,
        GeoRefusal::ShapeNotAGeometry,
    ),
    // A position of any other length has one refusal, wherever it sits.
    (
        r#"{"type":"Point","coordinates":[]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"Point","coordinates":[1]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"MultiPoint","coordinates":[[]]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"MultiPoint","coordinates":[[0]]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"LineString","coordinates":[[0,0],[1]]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"LineString","coordinates":[[0,0],[]]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10,1,1],[0,0]]]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"Point","coordinates":[1,2,3,4]}"#,
        GeoRefusal::PositionArity,
    ),
    (
        r#"{"type":"Point","coordinates":[180.000001,0]}"#,
        GeoRefusal::LongitudeOutOfRange,
    ),
    (
        r#"{"type":"Point","coordinates":[0,-90.000001]}"#,
        GeoRefusal::LatitudeOutOfRange,
    ),
    // Latitude first is the classic mistake: 151 is not a latitude.
    (
        r#"{"type":"Point","coordinates":[-33.8688,151.2093]}"#,
        GeoRefusal::LatitudeOutOfRange,
    ),
    (
        r#"{"type":"MultiPoint","coordinates":[]}"#,
        GeoRefusal::ShapeEmpty,
    ),
    (
        r#"{"type":"MultiLineString","coordinates":[]}"#,
        GeoRefusal::ShapeEmpty,
    ),
    (
        r#"{"type":"Polygon","coordinates":[]}"#,
        GeoRefusal::ShapeEmpty,
    ),
    (
        r#"{"type":"MultiPolygon","coordinates":[]}"#,
        GeoRefusal::ShapeEmpty,
    ),
    (
        r#"{"type":"GeometryCollection","geometries":[]}"#,
        GeoRefusal::ShapeEmpty,
    ),
    (
        r#"{"type":"GeometryCollection","geometries":[{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]}]}]}"#,
        GeoRefusal::NestedCollection,
    ),
    (
        r#"{"type":"LineString","coordinates":[[0,0]]}"#,
        GeoRefusal::LineTooShort,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[0,0]]]}"#,
        GeoRefusal::RingTooShort,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10]]]}"#,
        GeoRefusal::RingNotClosed,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0,1],[10,0,1],[10,10,1],[0,0,2]]]}"#,
        GeoRefusal::RingNotClosed,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[2,0],[0,0]]]}"#,
        GeoRefusal::RingDegenerate,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[5,10],[5,15],[5,10],[0,10],[0,0]]]}"#,
        GeoRefusal::RingSelfIntersects,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[1,1],[0,0],[0,0]]]}"#,
        GeoRefusal::RingDegenerate,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,10],[10,0],[0,10],[0,0]]]}"#,
        GeoRefusal::RingSelfIntersects,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[0,10],[10,10],[10,0],[0,0]]]}"#,
        GeoRefusal::ExteriorNotCounterclockwise,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[2,2],[8,2],[8,8],[2,8],[2,2]]]}"#,
        GeoRefusal::HoleNotClockwise,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[20,20],[20,22],[22,22],[22,20],[20,20]]]}"#,
        GeoRefusal::HoleOutsideExterior,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[5,5],[5,15],[15,15],[15,5],[5,5]]]}"#,
        GeoRefusal::RingsIntersect,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[2,2],[2,6],[6,6],[6,2],[2,2]],[[4,4],[4,8],[8,8],[8,4],[4,4]]]}"#,
        GeoRefusal::RingsIntersect,
    ),
    // One hole inside another.
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[2,2],[2,8],[8,8],[8,2],[2,2]],[[4,4],[4,6],[6,6],[6,4],[4,4]]]}"#,
        GeoRefusal::RingsIntersect,
    ),
    // A hole that touches its exterior ring at one point.
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[0,5],[3,8],[6,5],[3,2],[0,5]]]}"#,
        GeoRefusal::RingsIntersect,
    ),
    // A ring that passes through one position twice.
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[4,0],[2,2],[4,4],[0,4],[2,2],[0,0]]]}"#,
        GeoRefusal::RingSelfIntersects,
    ),
    // A ring across the antimeridian that was not split, in either winding.
    (
        r#"{"type":"Polygon","coordinates":[[[170,0],[-170,0],[-170,10],[170,10],[170,0]]]}"#,
        GeoRefusal::AntimeridianEdge,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[170,0],[170,10],[-170,10],[-170,0],[170,0]]]}"#,
        GeoRefusal::AntimeridianEdge,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[-180,-90],[180,-90],[180,90],[-180,90],[-180,-90]]]}"#,
        GeoRefusal::AntimeridianEdge,
    ),
    // Two polygons that share only the vertex `[10,10]`.
    (
        r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[10,0],[10,10],[0,10],[0,0]]],[[[10,10],[20,10],[20,20],[10,20],[10,10]]]]}"#,
        GeoRefusal::PolygonsOverlap,
    ),
    (
        r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[0,2],[0,8],[4,8],[4,2],[0,2]]]}"#,
        GeoRefusal::RingsIntersect,
    ),
    (
        r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[10,0],[10,10],[0,10],[0,0]]],[[[4,4],[6,4],[6,6],[4,6],[4,4]]]]}"#,
        GeoRefusal::PolygonsOverlap,
    ),
    (
        r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[10,0],[10,10],[0,10],[0,0]]],[[[5,5],[15,5],[15,15],[5,15],[5,5]]]]}"#,
        GeoRefusal::PolygonsOverlap,
    ),
    (
        r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[10,0],[10,10],[0,10],[0,0]]],[[[10,0],[20,0],[20,10],[10,10],[10,0]]]]}"#,
        GeoRefusal::PolygonsOverlap,
    ),
];

/// GEO-SHAPE-REFUSE: every defect has its own refusal.
#[test]
fn invalid_shapes_are_refused_with_the_defect() {
    for &(shape, refusal) in REFUSED {
        assert_eq!(geo::validate_shape(shape).err(), Some(refusal), "{shape}");
        let code = refusal.code();
        assert!(
            code.starts_with("geo.shape.") || code.starts_with("geo.coordinate."),
            "{code}"
        );
    }
}

/// `count` two-degree square rings, four degrees apart on a 16-wide grid.
fn squares(count: usize, clockwise: bool) -> Vec<String> {
    (0_u32..17)
        .flat_map(|row| (0_u32..16).map(move |column| (column, row)))
        .take(count)
        .map(|(column, row)| {
            let (x, y) = (f64::from(column) * 4.0 + 1.0, f64::from(row) * 4.0 + 1.0);
            let (right, top) = (x + 2.0, y + 2.0);
            if clockwise {
                format!("[[{x},{y}],[{x},{top}],[{right},{top}],[{right},{y}],[{x},{y}]]")
            } else {
                format!("[[{x},{y}],[{right},{y}],[{right},{top}],[{x},{top}],[{x},{y}]]")
            }
        })
        .collect()
}

/// GEO-SHAPE-BOUNDS: the byte, position and ring bounds are exact, and the
/// largest shapes of many holes and of many members are accepted.
#[test]
fn a_shape_is_bounded_in_bytes_positions_and_rings() -> Result<(), String> {
    let multi_point = |count: usize| {
        format!(
            r#"{{"type":"MultiPoint","coordinates":[{}]}}"#,
            vec!["[0,0]"; count].join(",")
        )
    };
    let largest = geo::validate_shape(&multi_point(MAX_SHAPE_POSITIONS))
        .map_err(|refusal| refusal.to_string())?;
    assert_eq!(largest.positions(), MAX_SHAPE_POSITIONS);
    assert_eq!(
        geo::validate_shape(&multi_point(MAX_SHAPE_POSITIONS + 1)).err(),
        Some(GeoRefusal::ShapeTooManyPositions)
    );
    let small = r#"{"type":"Point","coordinates":[1,2]}"#;
    let padded = |length: usize| format!("{small}{}", " ".repeat(length - small.len()));
    assert!(geo::validate_shape(&padded(MAX_SHAPE_BYTES)).is_ok());
    assert_eq!(
        geo::validate_shape(&padded(MAX_SHAPE_BYTES + 1)).err(),
        Some(GeoRefusal::ShapeTooLarge)
    );

    let many_holes = |holes: usize| {
        let holes = squares(holes, true);
        format!(
            r#"{{"type":"Polygon","coordinates":[[[0,0],[70,0],[70,70],[0,70],[0,0]],{}]}}"#,
            holes.join(",")
        )
    };
    let holed = geo::validate_shape(&many_holes(MAX_SHAPE_RINGS - 1))
        .map_err(|refusal| refusal.to_string())?;
    assert_eq!(holed.positions(), 5 * MAX_SHAPE_RINGS);
    assert_eq!(
        geo::point_in_shape(point(2.0, 2.0)?, &holed),
        Ok(false),
        "inside the first hole"
    );
    assert_eq!(geo::point_in_shape(point(4.0, 4.0)?, &holed), Ok(true));
    assert_eq!(
        geo::validate_shape(&many_holes(MAX_SHAPE_RINGS)).err(),
        Some(GeoRefusal::ShapeTooManyRings)
    );

    let many_members = |members: usize| {
        let members: Vec<String> = squares(members, false)
            .iter()
            .map(|ring| format!("[{ring}]"))
            .collect();
        format!(
            r#"{{"type":"MultiPolygon","coordinates":[{}]}}"#,
            members.join(",")
        )
    };
    let scattered = geo::validate_shape(&many_members(MAX_SHAPE_RINGS))
        .map_err(|refusal| refusal.to_string())?;
    assert_eq!(scattered.positions(), 5 * MAX_SHAPE_RINGS);
    assert_eq!(geo::point_in_shape(point(2.0, 2.0)?, &scattered), Ok(true));
    assert_eq!(geo::point_in_shape(point(4.0, 4.0)?, &scattered), Ok(false));
    assert_eq!(
        geo::validate_shape(&many_members(MAX_SHAPE_RINGS + 1)).err(),
        Some(GeoRefusal::ShapeTooManyRings)
    );
    Ok(())
}

/// GEO-BOUNDARY: the boundary is in the shape, a hole's interior is not.
#[test]
fn point_in_shape_includes_the_boundary() -> Result<(), String> {
    for (shape, lat, lon, inside) in [
        (DONUT, 5.0, 1.0, true),
        (DONUT, 5.0, 5.0, false),
        (DONUT, 5.0, 2.0, true),
        (DONUT, 8.0, 8.0, true),
        (DONUT, 0.0, 5.0, true),
        (DONUT, 0.0, 0.0, true),
        (DONUT, 10.0, 10.0, true),
        (DONUT, 5.0, 10.000_001, false),
        (DONUT, -0.000_001, 5.0, false),
        (DONUT, 5.0, 1.999_999, true),
        (DONUT, 5.0, 2.000_001, false),
        (SEPARATE, 5.0, 5.0, true),
        (SEPARATE, 15.0, 15.0, true),
        (SEPARATE, 10.0, 10.0, true),
        (SEPARATE, 12.0, 12.0, true),
        (SEPARATE, 11.0, 11.0, false),
        (SEPARATE, 15.0, 5.0, false),
        (ISLAND, 5.0, 5.0, true),
        (ISLAND, 3.0, 3.0, false),
        (ISLAND, 1.0, 1.0, true),
        // Exact, with no tolerance: 0.2 is on the hypotenuse, one part in
        // 10^16 north of it is outside.
        (TRIANGLE, 0.2, 0.2, true),
        (TRIANGLE, 0.2, 0.25, true),
        (TRIANGLE, 0.200_000_000_000_000_1, 0.2, false),
    ] {
        assert_eq!(
            contains(shape, lat, lon),
            Ok(inside),
            "({lat}, {lon}) in {shape}"
        );
    }
    let line = geo::validate_shape(r#"{"type":"LineString","coordinates":[[0,0],[1,1]]}"#)
        .map_err(|refusal| refusal.to_string())?;
    assert_eq!(
        geo::point_in_shape(point(0.5, 0.5)?, &line),
        Err(GeoRefusal::ShapeNotAreal)
    );
    Ok(())
}

/// GEO-DECODE-EXACT: a coordinate is the nearest binary64 of its decimal, so
/// spellings of one binary64 agree and exponents are plain numbers.
#[test]
fn coordinates_decode_to_the_nearest_binary64() -> Result<(), String> {
    let cell = |lat: &str| {
        GeoFunction::H3Cell
            .call(&numbers(&[lat, "-122.0553238", "7"])?)
            .map_err(|refusal| refusal.to_string())
    };
    let reference = cell("37.3615593")?;
    assert_eq!(reference, text("87283472bffffff"));
    assert_eq!(cell("37.36155930000000000000001")?, reference);
    assert_eq!(cell("3.73615593e1")?, reference);
    let long = r#"{"type":"Polygon","coordinates":[[[0.1000000000000000000001,0.1],[0.3,0.1],[0.3,0.3],[0.1,0.1000000000000000000001]]]}"#;
    assert_eq!(contains(long, 0.2, 0.2), Ok(true));
    Ok(())
}

/// GEO-CALL: the argument codec, by call name.
#[test]
fn call_applies_a_function_by_name_to_positional_arguments() -> Result<(), String> {
    assert_eq!(
        GeoFunction::ALL.map(GeoFunction::name),
        [
            "geo.distance_meters",
            "geo.h3_cell",
            "geo.point_in_shape",
            "geo.validate_shape"
        ]
    );
    for function in GeoFunction::ALL {
        assert_eq!(GeoFunction::from_name(function.name()), Some(function));
    }
    assert_eq!(GeoFunction::from_name("geo.area"), None);

    assert_eq!(
        geo::call(
            "geo.distance_meters",
            &numbers(&["30.0444", "31.2357", "31.2001", "29.9187"])?
        ),
        Ok(number("179985")?)
    );
    assert_eq!(
        geo::call("geo.h3_cell", &numbers(&["30.0444", "31.2357", "9"])?),
        Ok(text("893e628e67bffff"))
    );
    let mut inside = numbers(&["5", "1"])?;
    inside.push(text(DONUT));
    assert_eq!(
        geo::call("geo.point_in_shape", &inside),
        Ok(RuntimeValue::Bool(true))
    );
    let mut in_hole = numbers(&["5", "5"])?;
    in_hole.push(text(DONUT));
    assert_eq!(
        geo::call("geo.point_in_shape", &in_hole),
        Ok(RuntimeValue::Bool(false))
    );
    assert_eq!(
        geo::call("geo.validate_shape", &[text(DONUT)]),
        Ok(RuntimeValue::Bool(true))
    );

    let open = text(r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10]]]}"#);
    assert_eq!(
        geo::call("geo.validate_shape", std::slice::from_ref(&open)),
        Err(GeoRefusal::RingNotClosed)
    );
    let mut at_open = numbers(&["5", "5"])?;
    at_open.push(open);
    assert_eq!(
        geo::call("geo.point_in_shape", &at_open),
        Err(GeoRefusal::RingNotClosed)
    );

    assert_eq!(geo::call("geo.area", &[]), Err(GeoRefusal::UnknownFunction));
    assert_eq!(
        geo::call("geo.distance_meters", &numbers(&["0", "0", "0"])?),
        Err(GeoRefusal::Arguments)
    );
    assert_eq!(
        geo::call(
            "geo.distance_meters",
            &[text("0"), text("0"), text("0"), text("0")]
        ),
        Err(GeoRefusal::Arguments)
    );
    assert_eq!(
        geo::call("geo.distance_meters", &numbers(&["91", "0", "0", "0"])?),
        Err(GeoRefusal::LatitudeOutOfRange)
    );
    assert_eq!(
        geo::call("geo.distance_meters", &numbers(&["0", "0", "0", "-181"])?),
        Err(GeoRefusal::LongitudeOutOfRange)
    );
    assert_eq!(
        geo::call("geo.validate_shape", &numbers(&["1"])?),
        Err(GeoRefusal::Arguments)
    );
    for resolution in ["16", "-1", "7.5", "256"] {
        assert_eq!(
            geo::call("geo.h3_cell", &numbers(&["0", "0", resolution])?),
            Err(GeoRefusal::ResolutionOutOfRange),
            "{resolution}"
        );
    }
    assert_eq!(
        geo::call("geo.h3_cell", &[number("0")?, number("0")?, text("7")]),
        Err(GeoRefusal::Arguments)
    );
    Ok(())
}

/// GEO-UNREGISTERED: no geo function is a built-in a flow may perform.
#[test]
fn geo_functions_are_not_registered_built_ins() -> Result<(), String> {
    for function in GeoFunction::ALL {
        for id in [
            function.name().to_owned(),
            format!("norn.{}", function.name()),
        ] {
            let resolved = norn_builtins::resolve(&id, 1).map_err(|error| error.to_string())?;
            assert_eq!(resolved, None, "{id}");
        }
    }
    Ok(())
}
