//! Exact planar validity of polygons (GEO-SHAPE-1, Core 07; #191).
//!
//! Every decision here is an orientation test or a comparison of coordinates.
//! The orientation test is `geo`'s `RobustKernel`, the exact adaptive
//! predicate of `robust`; containment is `geo`'s winding-number test over the
//! same predicate. Nothing computes an intersection point, a distance or a
//! square root, so no host library is involved and no answer depends on
//! rounding.
//!
//! `geo`'s own `Validation` is not used: it compares every pair of holes and
//! polygons with `relate`, which is quadratic in their number, and its line
//! intersection reaches the host C library's `hypot`.
//!
//! The work is bounded by the shape's position and ring bounds: the edge
//! pass compares pairs of edges whose bounding boxes overlap, and the
//! containment pass tests one vertex of each ring against the other rings.

use std::cmp::Ordering;

use geo::coordinate_position::{CoordPos, CoordinatePosition, coord_pos_relative_to_ring};
use geo::kernels::{Kernel, Orientation, RobustKernel};
use geo::winding_order::WindingOrder;
use geo::{Coord, LineString, Polygon, Winding};

use super::GeoRefusal;

/// Check polygons that form one shape: a `Polygon` (one) or a
/// `MultiPolygon` (all its members).
///
/// Accepted polygons have simple rings of non-zero area, an exterior ring
/// wound counterclockwise and holes wound clockwise, every hole strictly
/// inside its exterior ring, and no two rings of the shape sharing a point,
/// except that a polygon may lie strictly inside another polygon's hole. So
/// an accepted shape is valid in the OGC sense, with a connected interior;
/// an OGC-valid shape whose rings touch at a point is refused.
///
/// # Errors
/// The [`GeoRefusal`] naming the first kind of defect, in the order the
/// checks are listed in Core 07 (the two hole checks run per polygon).
pub(super) fn check_polygons(polygons: &[Polygon<f64>]) -> Result<(), GeoRefusal> {
    let mut edges = Vec::new();
    for (polygon_index, polygon) in polygons.iter().enumerate() {
        for (ring_index, ring) in rings(polygon).enumerate() {
            push_edges(ring, polygon_index, ring_index, &mut edges)?;
        }
    }
    check_edges(&mut edges)?;
    for polygon in polygons {
        check_winding(polygon)?;
    }
    for polygon in polygons {
        check_holes(polygon)?;
    }
    check_members(polygons)
}

/// A polygon's rings, exterior first.
fn rings(polygon: &Polygon<f64>) -> impl Iterator<Item = &LineString<f64>> {
    std::iter::once(polygon.exterior()).chain(polygon.interiors())
}

/// One edge of one ring, with its bounding box.
struct Edge {
    start: Coord<f64>,
    end: Coord<f64>,
    polygon: usize,
    ring: usize,
    index: usize,
    /// The index of the edge that starts where this one ends.
    next: usize,
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

fn orient(p: Coord<f64>, q: Coord<f64>, r: Coord<f64>) -> Orientation {
    <RobustKernel as Kernel<f64>>::orient2d(p, q, r)
}

fn same_coord(left: Coord<f64>, right: Coord<f64>) -> bool {
    left.x.partial_cmp(&right.x) == Some(Ordering::Equal)
        && left.y.partial_cmp(&right.y) == Some(Ordering::Equal)
}

/// Add a ring's edges, skipping repeated consecutive positions.
///
/// # Errors
/// [`GeoRefusal::RingDegenerate`] for fewer than three distinct consecutive
/// positions or for positions that all lie on one line.
fn push_edges(
    ring: &LineString<f64>,
    polygon: usize,
    ring_index: usize,
    edges: &mut Vec<Edge>,
) -> Result<(), GeoRefusal> {
    let mut vertices: Vec<Coord<f64>> = Vec::with_capacity(ring.0.len());
    for coord in &ring.0 {
        if vertices
            .last()
            .is_none_or(|last| !same_coord(*last, *coord))
        {
            vertices.push(*coord);
        }
    }
    // The closing position repeats the first.
    if let (Some(first), Some(last)) = (vertices.first(), vertices.last())
        && vertices.len() > 1
        && same_coord(*first, *last)
    {
        vertices.pop();
    }
    let [first, second, rest @ ..] = vertices.as_slice() else {
        return Err(GeoRefusal::RingDegenerate);
    };
    if rest
        .iter()
        .all(|vertex| orient(*first, *second, *vertex) == Orientation::Collinear)
    {
        return Err(GeoRefusal::RingDegenerate);
    }
    let count = vertices.len();
    let ends = vertices.iter().cycle().skip(1);
    for (index, (start, end)) in vertices.iter().zip(ends).enumerate() {
        let next = index
            .checked_add(1)
            .filter(|next| *next < count)
            .unwrap_or(0);
        edges.push(Edge {
            start: *start,
            end: *end,
            polygon,
            ring: ring_index,
            index,
            next,
            min_x: start.x.min(end.x),
            max_x: start.x.max(end.x),
            min_y: start.y.min(end.y),
            max_y: start.y.max(end.y),
        });
    }
    Ok(())
}

/// No two edges of the shape share a point, other than two consecutive edges
/// of a ring at their common end. Edges are swept in order of their least
/// longitude, so only edges whose longitude ranges overlap are compared.
fn check_edges(edges: &mut [Edge]) -> Result<(), GeoRefusal> {
    edges.sort_by(|left, right| left.min_x.total_cmp(&right.min_x));
    let (mut rings_meet, mut polygons_meet) = (false, false);
    let mut rest: &[Edge] = edges;
    while let [edge, later @ ..] = rest {
        for other in later {
            if other.min_x > edge.max_x {
                break;
            }
            if other.min_y > edge.max_y || other.max_y < edge.min_y || !edges_meet(edge, other) {
                continue;
            }
            if edge.polygon != other.polygon {
                polygons_meet = true;
            } else if edge.ring != other.ring {
                rings_meet = true;
            } else {
                return Err(GeoRefusal::RingSelfIntersects);
            }
        }
        rest = later;
    }
    if rings_meet {
        Err(GeoRefusal::RingsIntersect)
    } else if polygons_meet {
        Err(GeoRefusal::PolygonsOverlap)
    } else {
        Ok(())
    }
}

/// Whether two edges share a point they may not share.
fn edges_meet(left: &Edge, right: &Edge) -> bool {
    if left.polygon == right.polygon && left.ring == right.ring {
        if left.next == right.index {
            return folds_back(left.start, left.end, right.end);
        }
        if right.next == left.index {
            return folds_back(right.start, right.end, left.end);
        }
    }
    segments_meet(left, right)
}

/// Whether the path `before`, `corner`, `after` turns straight back on
/// itself, so its two edges overlap beyond `corner`.
fn folds_back(before: Coord<f64>, corner: Coord<f64>, after: Coord<f64>) -> bool {
    let side = |point: Coord<f64>| {
        (
            point.x.partial_cmp(&corner.x),
            point.y.partial_cmp(&corner.y),
        )
    };
    orient(before, corner, after) == Orientation::Collinear && side(before) == side(after)
}

/// Whether two closed segments share at least one point.
fn segments_meet(left: &Edge, right: &Edge) -> bool {
    let (first, second) = (
        orient(left.start, left.end, right.start),
        orient(left.start, left.end, right.end),
    );
    if first == second {
        // Both ends on one side, or all four points on one line. Collinear
        // segments meet exactly when their bounding boxes do.
        return first == Orientation::Collinear
            && left.min_x <= right.max_x
            && right.min_x <= left.max_x
            && left.min_y <= right.max_y
            && right.min_y <= left.max_y;
    }
    orient(right.start, right.end, left.start) != orient(right.start, right.end, left.end)
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

/// Where a ring lies relative to another ring it shares no point with: one
/// vertex decides for the whole ring.
fn ring_position(ring: &LineString<f64>, around: &LineString<f64>) -> CoordPos {
    ring.0.first().map_or(CoordPos::Outside, |vertex| {
        coord_pos_relative_to_ring(*vertex, around)
    })
}

/// Every hole is inside the exterior ring and no hole is inside another.
fn check_holes(polygon: &Polygon<f64>) -> Result<(), GeoRefusal> {
    for hole in polygon.interiors() {
        if ring_position(hole, polygon.exterior()) != CoordPos::Inside {
            return Err(GeoRefusal::HoleOutsideExterior);
        }
    }
    let mut rest = polygon.interiors();
    while let [hole, later @ ..] = rest {
        for other in later {
            if ring_position(hole, other) != CoordPos::Outside
                || ring_position(other, hole) != CoordPos::Outside
            {
                return Err(GeoRefusal::RingsIntersect);
            }
        }
        rest = later;
    }
    Ok(())
}

/// No member polygon lies in another's area. One lying in another's hole is
/// outside that polygon.
fn check_members(polygons: &[Polygon<f64>]) -> Result<(), GeoRefusal> {
    let outside = |member: &Polygon<f64>, around: &Polygon<f64>| {
        member
            .exterior()
            .0
            .first()
            .is_none_or(|vertex| around.coordinate_position(vertex) == CoordPos::Outside)
    };
    let mut rest = polygons;
    while let [member, later @ ..] = rest {
        for other in later {
            if !outside(member, other) || !outside(other, member) {
                return Err(GeoRefusal::PolygonsOverlap);
            }
        }
        rest = later;
    }
    Ok(())
}
