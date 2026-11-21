//! Plane geometry of measured shapes: outline points, convex hull, the maximum Feret
//! diameter, the minimum-area enclosing rectangle, area and boundary length.

use crate::error::{Result, VisionError};
use crate::geometry::{Mask, Shape};

use super::MeasuredGeometry;

/// A shape's measurable form: the points whose convex hull is the shape's, its area and the
/// length of its boundary, all in source pixels.
pub(super) struct Outline {
    pub(super) geometry: MeasuredGeometry,
    pub(super) hull: Vec<[f64; 2]>,
    pub(super) area: f64,
    pub(super) boundary: f64,
}

impl Outline {
    pub(super) fn of(shape: &Shape) -> Result<Self> {
        let (geometry, points, area, boundary) = match shape {
            Shape::Mask(mask) => (
                MeasuredGeometry::Mask,
                mask_corners(mask),
                mask.area() as f64,
                mask_boundary(mask),
            ),
            Shape::Polygon(polygon) => {
                let points: Vec<[f64; 2]> =
                    polygon.points.iter().map(|[x, y]| [f64::from(*x), f64::from(*y)]).collect();
                let area = shoelace(&points).abs();
                let boundary = closed_length(&points);
                (MeasuredGeometry::Polygon, points, area, boundary)
            }
            Shape::Box(bbox) => {
                let (x0, y0, x1, y1) = (
                    f64::from(bbox.x0),
                    f64::from(bbox.y0),
                    f64::from(bbox.x1),
                    f64::from(bbox.y1),
                );
                let points = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
                let (w, h) = ((x1 - x0).max(0.0), (y1 - y0).max(0.0));
                (MeasuredGeometry::Box, points, w * h, 2.0 * (w + h))
            }
            Shape::Point(_) | Shape::Keypoints(_) => {
                return Err(VisionError::InvalidInput(
                    "a point or keypoints have no extent to measure; measure a mask, a polygon or a box"
                        .into(),
                ));
            }
        };
        if points.iter().flatten().any(|c| !c.is_finite()) {
            return Err(VisionError::InvalidInput(
                "the region's coordinates must be finite to measure it".into(),
            ));
        }
        let hull = convex_hull(points);
        if hull.is_empty() || area <= 0.0 {
            return Err(VisionError::InvalidInput("the region has no area to measure".into()));
        }
        Ok(Self { geometry, hull, area, boundary })
    }
}

/// The corners of every set run of a mask, column by column: the extreme points of the set
/// pixels as unit squares, so their hull is the mask's.
fn mask_corners(mask: &Mask) -> Vec<[f64; 2]> {
    let height = u64::from(mask.height);
    let mut corners = Vec::new();
    if height == 0 {
        return corners;
    }
    let mut index = 0u64;
    for (run, &count) in mask.counts.iter().enumerate() {
        let (start, len) = (index, u64::from(count));
        index += len;
        if run % 2 == 0 || len == 0 {
            continue;
        }
        let end = start + len - 1;
        for column in start / height..=end / height {
            let top = if column == start / height { start % height } else { 0 };
            let bottom = if column == end / height { end % height } else { height - 1 };
            let (x0, x1) = (column as f64, column as f64 + 1.0);
            let (y0, y1) = (top as f64, bottom as f64 + 1.0);
            corners.extend([[x0, y0], [x1, y0], [x0, y1], [x1, y1]]);
        }
    }
    corners
}

/// The number of pixel edges between a set pixel and an unset one or the raster's border.
fn mask_boundary(mask: &Mask) -> f64 {
    let (w, h) = (mask.width as usize, mask.height as usize);
    let bits = mask.decode();
    let set = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && bits[y as usize * w + x as usize]
    };
    let mut edges = 0u64;
    for y in 0..h as isize {
        for x in 0..w as isize {
            if set(x, y) {
                edges += [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .filter(|(dx, dy)| !set(x + dx, y + dy))
                    .count() as u64;
            }
        }
    }
    edges as f64
}

fn shoelace(points: &[[f64; 2]]) -> f64 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let ([x0, y0], [x1, y1]) = (points[i], points[(i + 1) % n]);
            x0 * y1 - x1 * y0
        })
        .sum::<f64>()
        / 2.0
}

fn closed_length(points: &[[f64; 2]]) -> f64 {
    let n = points.len();
    (0..n).map(|i| distance(points[i], points[(i + 1) % n])).sum()
}

fn distance([x0, y0]: [f64; 2], [x1, y1]: [f64; 2]) -> f64 {
    (x1 - x0).hypot(y1 - y0)
}

fn cross(o: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// The convex hull, counter-clockwise without collinear points (Andrew's monotone chain).
pub(super) fn convex_hull(mut points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let mut hull: Vec<[f64; 2]> = Vec::with_capacity(points.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let ordered: Box<dyn Iterator<Item = &[f64; 2]>> =
            if pass == 0 { Box::new(points.iter()) } else { Box::new(points.iter().rev()) };
        for &p in ordered {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// The maximum Feret diameter of a hull: its length and the two points it joins.
pub(super) fn max_feret(hull: &[[f64; 2]]) -> (f64, [[f64; 2]; 2]) {
    let mut best = (0.0, [hull[0], hull[0]]);
    for (i, &a) in hull.iter().enumerate() {
        for &b in &hull[i + 1..] {
            let length = distance(a, b);
            if length > best.0 {
                best = (length, [a, b]);
            }
        }
    }
    best
}

/// The sides of the minimum-area rectangle enclosing a hull, the longer first. One side of
/// that rectangle lies along a hull edge, so each edge's direction is tried.
pub(super) fn min_area_rect(hull: &[[f64; 2]]) -> [f64; 2] {
    if hull.len() < 3 {
        let (length, _) = max_feret(hull);
        return [length, 0.0];
    }
    let mut best: Option<(f64, [f64; 2])> = None;
    for i in 0..hull.len() {
        let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
        let length = distance(a, b);
        if length == 0.0 {
            continue;
        }
        let u = [(b[0] - a[0]) / length, (b[1] - a[1]) / length];
        let v = [-u[1], u[0]];
        let (mut u0, mut u1, mut v0, mut v1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in hull {
            let pu = p[0] * u[0] + p[1] * u[1];
            let pv = p[0] * v[0] + p[1] * v[1];
            (u0, u1, v0, v1) = (u0.min(pu), u1.max(pu), v0.min(pv), v1.max(pv));
        }
        let (w, h) = (u1 - u0, v1 - v0);
        if best.is_none_or(|(area, _)| w * h < area) {
            best = Some((w * h, [w.max(h), w.min(h)]));
        }
    }
    best.map_or([0.0, 0.0], |(_, sides)| sides)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{BBox, Polygon};

    #[test]
    fn a_square_mask_has_its_pixel_corners_as_hull() {
        let mask = Mask::from_box(10, 10, &BBox::new(2.0, 3.0, 6.0, 5.0));
        let outline = Outline::of(&Shape::Mask(mask)).unwrap();
        assert_eq!(outline.hull, vec![[2.0, 3.0], [6.0, 3.0], [6.0, 5.0], [2.0, 5.0]]);
        assert_eq!(outline.area, 8.0);
        assert_eq!(outline.boundary, 12.0);
        assert_eq!(min_area_rect(&outline.hull), [4.0, 2.0]);
        let (length, _) = max_feret(&outline.hull);
        assert!((length - 20f64.sqrt()).abs() < 1e-9);
    }

    #[test]
    fn a_rotated_square_polygon_keeps_its_sides() {
        let (c, s) = (30f64.to_radians().cos(), 30f64.to_radians().sin());
        let points: Vec<[f32; 2]> = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]
            .iter()
            .map(|[x, y]: &[f64; 2]| {
                [(200.0 + x * c - y * s) as f32, (200.0 + x * s + y * c) as f32]
            })
            .collect();
        let outline = Outline::of(&Shape::Polygon(Polygon { points })).unwrap();
        let [a, b] = min_area_rect(&outline.hull);
        assert!((a - 100.0).abs() < 1e-3 && (b - 100.0).abs() < 1e-3, "{a} x {b}");
        assert!((outline.area - 10_000.0).abs() < 0.5);
    }

    #[test]
    fn a_run_spanning_columns_covers_them() {
        // Column 0 rows 1..3 and column 1 rows 0..1 are one run in column-major order.
        let mut bits = vec![false; 6];
        bits[2] = true;
        bits[4] = true;
        bits[1] = true;
        let mask = Mask::encode(2, 3, &bits);
        let hull = convex_hull(mask_corners(&mask));
        assert_eq!(
            hull,
            vec![[0.0, 1.0], [1.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 3.0], [0.0, 3.0]]
        );
    }
}
