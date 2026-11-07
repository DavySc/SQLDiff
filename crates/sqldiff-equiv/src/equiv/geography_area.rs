//! Which side of a polygon's rings is its inside, on both engines.
//!
//! SQL Server geography follows the left-hand rule: walking along a ring in
//! the order written, the area on the left is the polygon's inside, so a
//! clockwise exterior ring is the whole globe except the ring (compatibility
//! level 110 and later accept polygons larger than a hemisphere; Microsoft
//! Learn, "Polygon"). PostGIS geography ignores the direction and takes "the
//! smaller thing", which it does imperfectly (Paul Ramsey, postgis-users,
//! 2019-08). The same bytes are then two different regions unless the
//! polygon is one both engines read alike.
//!
//! That is the case for a polygon that is small (all vertices within about
//! 72 degrees of one centre, which makes the inside the small side), simple
//! (no ring touches itself or another ring), with a counter-clockwise
//! exterior ring and clockwise holes seen from outside the sphere. This
//! module checks exactly that, on the gnomonic projection around the centre,
//! where a great-circle arc is a straight line, so the planar tests are the
//! spherical ones. Everything it cannot decide with room to spare (edges
//! nearer than 1e-9 radians, rings that nearly reverse on themselves, more
//! than 6,000 edges) it refuses.

/// A ring: (longitude, latitude) in degrees, first point repeated last.
pub type Ring = Vec<(f64, f64)>;
/// A polygon: the exterior ring, then the holes.
pub type Polygon = Vec<Ring>;

/// Vertices must lie within `acos(MIN_DOT)` of the centre (about 72.5 degrees).
const MIN_DOT: f64 = 0.3;
/// Edges and rings closer than this (radians on the sphere; the shortest edge in the WWI borders is 1.7e-8) are refused.
const MIN_GAP: f64 = 1e-9;
const MAX_EDGES: usize = 6000;

type P = (f64, f64);

fn unit(lon: f64, lat: f64) -> [f64; 3] {
    let (lo, la) = (lon.to_radians(), lat.to_radians());
    [la.cos() * lo.cos(), la.cos() * lo.sin(), la.sin()]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn scale(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn sub(a: P, b: P) -> P {
    (a.0 - b.0, a.1 - b.1)
}

fn cross(a: P, b: P) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

fn len(a: P) -> f64 {
    (a.0 * a.0 + a.1 * a.1).sqrt()
}

/// Distance from `p` to segment `a`-`b`.
fn point_segment(p: P, a: P, b: P) -> f64 {
    let ab = sub(b, a);
    let l2 = ab.0 * ab.0 + ab.1 * ab.1;
    let t = if l2 == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * ab.0 + (p.1 - a.1) * ab.1) / l2).clamp(0.0, 1.0)
    };
    len(sub(p, (a.0 + t * ab.0, a.1 + t * ab.1)))
}

/// Distance between segments `a`-`b` and `c`-`d` (0 when they cross).
fn segment_gap(a: P, b: P, c: P, d: P) -> f64 {
    let s = |p: P, q: P, r: P| cross(sub(q, p), sub(r, p)).signum();
    if s(a, b, c) * s(a, b, d) < 0.0 && s(c, d, a) * s(c, d, b) < 0.0 {
        return 0.0;
    }
    point_segment(a, c, d)
        .min(point_segment(b, c, d))
        .min(point_segment(c, a, b))
        .min(point_segment(d, a, b))
}

fn bbox_gap(a: (P, P), b: (P, P)) -> f64 {
    let dx = (a.0 .0.max(b.0 .0) - a.1 .0.min(b.1 .0)).max(0.0);
    let dy = (a.0 .1.max(b.0 .1) - a.1 .1.min(b.1 .1)).max(0.0);
    len((dx, dy))
}

/// Whether `p` is inside the closed ring (crossing number; the callers keep
/// `p` away from the ring's edges).
fn inside(p: P, ring: &[P]) -> bool {
    let mut c = false;
    for w in ring.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            c = !c;
        }
    }
    c
}

fn area(ring: &[P]) -> f64 {
    ring.windows(2).map(|w| cross(w[0], w[1])).sum::<f64>() / 2.0
}

/// Whether every polygon of `polys` is one SQL Server and PostGIS place on
/// the same side of its rings, and the polygons are apart from each other.
pub fn same_region(polys: &[Polygon]) -> bool {
    why_not(polys).is_ok()
}

/// The reason `same_region` refuses (the source line of the check).
pub fn why_not(polys: &[Polygon]) -> Result<(), &'static str> {
    let polys: Vec<&Polygon> = polys.iter().filter(|p| !p.is_empty()).collect();
    if polys.is_empty() {
        return Ok(());
    }
    let edges: usize = polys
        .iter()
        .flat_map(|p| p.iter())
        .map(|r| r.len() - 1)
        .sum();
    if edges > MAX_EDGES {
        return Err(concat!("line", line!()));
    }
    // Longitudes of one polygon within a window narrower than 180 degrees
    // without wrapping, latitudes strictly inside the poles.
    for p in &polys {
        let lons = p.iter().flatten().map(|v| v.0);
        let (lo, hi) = lons.fold((f64::MAX, f64::MIN), |(l, h), x| (l.min(x), h.max(x)));
        if hi - lo >= 180.0 || p.iter().flatten().any(|v| v.1.abs() >= 90.0) {
            return Err(concat!("line", line!()));
        }
    }
    // The centre and the tangent plane around it: east, north, up.
    let vs: Vec<[f64; 3]> = polys
        .iter()
        .flat_map(|p| p.iter().flatten())
        .map(|v| unit(v.0, v.1))
        .collect();
    let sum = vs
        .iter()
        .fold([0.0; 3], |s, v| [s[0] + v[0], s[1] + v[1], s[2] + v[2]]);
    if norm(sum) < 1e-9 {
        return Err(concat!("line", line!()));
    }
    let c = scale(sum, 1.0 / norm(sum));
    if vs.iter().any(|v| dot(*v, c) < MIN_DOT) {
        return Err(concat!("line", line!()));
    }
    let helper = if c[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let east = cross3(helper, c);
    let east = scale(east, 1.0 / norm(east));
    let north = cross3(c, east);
    // Gnomonic projection: great-circle arcs become straight lines, and
    // east x north = up, so counter-clockwise here is counter-clockwise seen
    // from outside the sphere.
    let flat = |r: &Ring| -> Vec<P> {
        r.iter()
            .map(|v| {
                let u = unit(v.0, v.1);
                let w = dot(u, c);
                (dot(u, east) / w, dot(u, north) / w)
            })
            .collect()
    };
    let flat_polys: Vec<Vec<Vec<P>>> = polys
        .iter()
        .map(|p| p.iter().map(&flat).collect())
        .collect();
    // Orientation: exterior counter-clockwise, holes clockwise.
    for p in &flat_polys {
        for (i, r) in p.iter().enumerate() {
            let a = area(r);
            if a.abs() < 1e-10 || (i == 0) != (a > 0.0) {
                return Err(concat!("line", line!()));
            }
        }
    }
    // Every edge, tagged with its ring.
    struct Edge {
        a: P,
        b: P,
        ring: usize,
        index: usize,
        count: usize,
        bbox: (P, P),
    }
    let mut es: Vec<Edge> = Vec::new();
    let mut ring_id = 0;
    for p in &flat_polys {
        for r in p {
            let count = r.len() - 1;
            for i in 0..count {
                let (a, b) = (r[i], r[i + 1]);
                if len(sub(b, a)) < MIN_GAP {
                    return Err(concat!("line", line!()));
                }
                es.push(Edge {
                    a,
                    b,
                    ring: ring_id,
                    index: i,
                    count,
                    bbox: ((a.0.min(b.0), a.1.min(b.1)), (a.0.max(b.0), a.1.max(b.1))),
                });
            }
            ring_id += 1;
        }
    }
    for i in 0..es.len() {
        for j in i + 1..es.len() {
            let (x, y) = (&es[i], &es[j]);
            if bbox_gap(x.bbox, y.bbox) >= MIN_GAP {
                continue;
            }
            let adjacent = x.ring == y.ring
                && (y.index == x.index + 1 || (x.index == 0 && y.index + 1 == x.count));
            if adjacent {
                // Edges that meet at a vertex must not fold back on each other.
                let (first, second) = if y.index == x.index + 1 {
                    (x, y)
                } else {
                    (y, x)
                };
                let (d1, d2) = (sub(first.b, first.a), sub(second.b, second.a));
                let sin = cross(d1, d2) / (len(d1) * len(d2));
                let cos = (d1.0 * d2.0 + d1.1 * d2.1) / (len(d1) * len(d2));
                if cos < 0.0 && sin.abs() < 1e-6 {
                    return Err(concat!("line", line!()));
                }
            } else if segment_gap(x.a, x.b, y.a, y.b) < MIN_GAP {
                return Err(concat!("line", line!()));
            }
        }
    }
    // Holes inside their exterior and apart from each other; polygons apart.
    for p in &flat_polys {
        for (i, h) in p.iter().enumerate().skip(1) {
            if !inside(h[0], &p[0]) {
                return Err(concat!("line", line!()));
            }
            for (k, other) in p.iter().enumerate().skip(1) {
                if k != i && inside(h[0], other) {
                    return Err(concat!("line", line!()));
                }
            }
        }
    }
    for (i, p) in flat_polys.iter().enumerate() {
        for q in flat_polys.iter().skip(i + 1) {
            if inside(q[0][0], &p[0]) || inside(p[0][0], &q[0]) {
                return Err(concat!("line", line!()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x: f64, y: f64, s: f64, ccw: bool) -> Ring {
        let mut r = vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s), (x, y)];
        if !ccw {
            r.reverse();
        }
        r
    }

    #[test]
    fn orientation_decides() {
        assert!(same_region(&[vec![sq(0.0, 0.0, 1.0, true)]]));
        assert!(!same_region(&[vec![sq(0.0, 0.0, 1.0, false)]]));
        // A clockwise hole in a counter-clockwise shell; the other way round.
        assert!(same_region(&[vec![
            sq(0.0, 0.0, 10.0, true),
            sq(2.0, 2.0, 2.0, false)
        ]]));
        assert!(!same_region(&[vec![
            sq(0.0, 0.0, 10.0, true),
            sq(2.0, 2.0, 2.0, true)
        ]]));
        assert!(!same_region(&[vec![
            sq(0.0, 0.0, 10.0, false),
            sq(2.0, 2.0, 2.0, false)
        ]]));
    }

    #[test]
    fn simple_small_and_apart() {
        // Bow tie, hole outside, overlapping polygons, a polygon in a polygon.
        let bow = vec![(0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)];
        assert!(!same_region(&[vec![bow]]));
        assert!(!same_region(&[vec![
            sq(0.0, 0.0, 10.0, true),
            sq(20.0, 2.0, 2.0, false)
        ]]));
        assert!(!same_region(&[
            vec![sq(0.0, 0.0, 1.0, true)],
            vec![sq(0.5, 0.5, 1.0, true)]
        ]));
        assert!(!same_region(&[
            vec![sq(0.0, 0.0, 10.0, true)],
            vec![sq(2.0, 2.0, 1.0, true)]
        ]));
        assert!(same_region(&[
            vec![sq(0.0, 0.0, 1.0, true)],
            vec![sq(5.0, 5.0, 1.0, true)]
        ]));
        // Too large: a window of 180 degrees; a pole.
        assert!(!same_region(&[vec![sq(-90.0, 0.0, 180.0, true)]]));
        assert!(!same_region(&[vec![sq(0.0, 80.0, 20.0, true)]]));
    }
}
