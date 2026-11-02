//! Geography points as constants: SQL Server's binary form of a geography
//! and PostGIS's extended WKB, read to the same point.
//!
//! Checked on SQL Server 2022 and PostGIS 3.6 (PostgreSQL 18), 2026-10-30:
//! - SQL Server writes a point as SRID (4 bytes, little endian), version 1,
//!   properties 0x0C (valid, single point) plus 0x01 with Z and 0x02 with M,
//!   then latitude, longitude, Z, M as little-endian doubles:
//!   `geography::Point(36.5822667, -87.0655564, 4326)` is
//!   0xE6100000010C610619B7874A4240C982781332C455C0. It keeps a longitude
//!   outside ±180 (-187 stays -187) and rejects a wrong length.
//! - PostGIS reads the EWKB `01` + type (1, with 0x20000000 for the SRID,
//!   0x80000000 for Z, 0x40000000 for M, little endian) + SRID + x
//!   (longitude), y (latitude), Z, M, and writes back the same doubles; it
//!   rejects SQL Server's form ("Invalid endian flag value encountered"),
//!   moves a longitude outside ±180 into range and accepts NaN.
//!
//! So only points both keep as they are have a constant here: SRID 4326,
//! finite coordinates, longitude within ±180 and latitude within ±90.

/// A point of geography: longitude `x`, latitude `y`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub srid: u32,
    pub x: f64,
    pub y: f64,
    pub z: Option<f64>,
    pub m: Option<f64>,
}

const SRID: u32 = 4326;

fn f64_at(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn kept(p: Point) -> Option<Point> {
    let finite = [Some(p.x), Some(p.y), p.z, p.m]
        .into_iter()
        .flatten()
        .all(f64::is_finite);
    (p.srid == SRID && finite && p.x.abs() <= 180.0 && p.y.abs() <= 90.0).then_some(p)
}

/// The point SQL Server's binary form `b` holds.
pub fn from_sqlserver(b: &[u8]) -> Option<Point> {
    let srid = u32_at(b, 0)?;
    if b.get(4) != Some(&1) {
        return None;
    }
    let props = *b.get(5)?;
    if props & !0x03 != 0x0C {
        return None;
    }
    let (has_z, has_m) = (props & 0x01 != 0, props & 0x02 != 0);
    if b.len() != 22 + 8 * (has_z as usize + has_m as usize) {
        return None;
    }
    let z = has_z.then(|| f64_at(b, 22)).flatten();
    let m = has_m.then(|| f64_at(b, 22 + 8 * has_z as usize)).flatten();
    kept(Point {
        srid,
        y: f64_at(b, 6)?,
        x: f64_at(b, 14)?,
        z,
        m,
    })
}

const EWKB_Z: u32 = 0x8000_0000;
const EWKB_M: u32 = 0x4000_0000;
const EWKB_SRID: u32 = 0x2000_0000;

/// The point of little-endian EWKB `b` with an SRID.
pub fn from_ewkb(b: &[u8]) -> Option<Point> {
    if b.first() != Some(&1) {
        return None;
    }
    let ty = u32_at(b, 1)?;
    if ty & !(EWKB_Z | EWKB_M) != EWKB_SRID | 1 {
        return None;
    }
    let (has_z, has_m) = (ty & EWKB_Z != 0, ty & EWKB_M != 0);
    if b.len() != 25 + 8 * (has_z as usize + has_m as usize) {
        return None;
    }
    let z = has_z.then(|| f64_at(b, 25)).flatten();
    let m = has_m.then(|| f64_at(b, 25 + 8 * has_z as usize)).flatten();
    kept(Point {
        srid: u32_at(b, 5)?,
        x: f64_at(b, 9)?,
        y: f64_at(b, 17)?,
        z,
        m,
    })
}

/// `p` as upper-case hex EWKB, the text PostGIS reads as a geography.
pub fn ewkb_hex(p: &Point) -> String {
    let mut ty = EWKB_SRID | 1;
    if p.z.is_some() {
        ty |= EWKB_Z;
    }
    if p.m.is_some() {
        ty |= EWKB_M;
    }
    let mut b = vec![1u8];
    b.extend(ty.to_le_bytes());
    b.extend(p.srid.to_le_bytes());
    for v in [Some(p.x), Some(p.y), p.z, p.m].into_iter().flatten() {
        b.extend(v.to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// Bytes of hex text (an even number of digits), or None.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.as_bytes();
    if s.len() % 2 != 0 {
        return None;
    }
    s.chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect()
}

/// What the well-known binary (WKB) of a geography value is, once it passes
/// [`wkb`]: a point, which has the one constant form of [`Point`], or any
/// other shape, which is kept as its bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum Wkb {
    Point(Point),
    /// Upper-case hex of the validated bytes. SQL Server's `STAsBinary()` and
    /// PostGIS's `ST_AsBinary` give these bytes back unchanged.
    Shape(String),
}

/// The geography an OGC WKB constant holds, only if SQL Server 2022
/// (`geography::STGeomFromWKB(wkb, 4326)`) and PostGIS 3.5
/// (`ST_GeomFromWKB(wkb, 4326)::geography`) both accept it, keep every
/// coordinate as the same double and write the same WKB back. Checked on
/// both engines on 2026-11-02 (`eval/geography/2026-11-02/`):
///
/// - Both read little-endian, two-dimensional types 1 to 7 (point, line
///   string, polygon, the three multi types, collection) and write the input
///   back byte for byte. Big-endian input, EWKB flags, ISO Z/M codes and
///   CircularString are not read alike (SQL Server drops the Z and M, writes
///   little endian; PostGIS keeps them), so they have no constant.
/// - Trailing bytes, a short buffer, a line with one point, a ring with
///   fewer than four points or one that is not closed, and a multi type
///   with the wrong child type are errors on both. NaN and infinity are
///   errors on SQL Server and accepted on PostGIS.
/// - A latitude beyond ±90 is an error on SQL Server and moved into range by
///   PostGIS; a longitude beyond ±180 is kept by SQL Server and moved by
///   PostGIS. Coordinates inside the ranges stay as they are.
/// - Neither engine checks validity at this point: a self-intersecting
///   polygon, a hole outside its shell or a clockwise exterior ring is
///   accepted by both unchanged (`STIsValid()` is 0 afterwards), so validity
///   is not asked here.
///
/// A geometry collection nested in a collection is accepted by both but not
/// read here.
pub fn wkb(b: &[u8]) -> Option<Wkb> {
    let mut r = Reader { b, at: 0 };
    r.geometry(true)?;
    if r.at != b.len() {
        return None;
    }
    if b.len() == 21 && b[1..5] == [1, 0, 0, 0] {
        let p = Point {
            srid: SRID,
            x: f64_at(b, 5)?,
            y: f64_at(b, 13)?,
            z: None,
            m: None,
        };
        return Some(Wkb::Point(p));
    }
    Some(Wkb::Shape(b.iter().map(|x| format!("{x:02X}")).collect()))
}

/// A plain decimal literal (digits, one optional point, optional minus) read
/// as the double both engines convert it to. SQL Server's decimal-to-float
/// and PostgreSQL's numeric-to-float8 both give the nearest double (606
/// literals of up to 27 digits checked equal on both engines,
/// `eval/geography/2026-11-02/pointprobe.py`); `-0.0` is a zero without a
/// sign in both. Exponents, other bases and longer literals are not read.
pub fn literal_double(e: &super::ast::Expr) -> Option<f64> {
    let super::ast::Expr::Num(s) = e else {
        return None;
    };
    let body = s.strip_prefix('-').unwrap_or(s);
    let digits = body.bytes().filter(u8::is_ascii_digit).count();
    let shape = body.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && body.bytes().filter(|&b| b == b'.').count() <= 1
        && body.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && !body.ends_with('.')
        && (1..=27).contains(&digits);
    let v: f64 = shape.then(|| s.parse().ok()).flatten()?;
    Some(if v == 0.0 { 0.0 } else { v })
}

/// The point `geography::Point(lat, lon, 4326)` (T-SQL) or
/// `ST_SetSRID(ST_MakePoint(lon, lat), 4326)` (PostGIS) builds from decimal
/// literals: SQL Server raises on a latitude beyond ±90 and PostGIS moves it
/// into range, so only coordinates inside ±90 / ±180 have a value.
pub fn point_of(lat: &super::ast::Expr, lon: &super::ast::Expr) -> Option<Point> {
    let (y, x) = (literal_double(lat)?, literal_double(lon)?);
    (y.abs() <= 90.0 && x.abs() <= 180.0).then_some(Point {
        srid: SRID,
        x,
        y,
        z: None,
        m: None,
    })
}

/// Whether T-SQL `geography::<method>(args)` is a call with a constant value
/// that cannot fail, given the bytes of a variable argument (`known`): the
/// same reading `Canon::geography_call` gives it.
pub fn tsql_constant(
    method: &str,
    args: &[super::ast::Expr],
    known: &dyn Fn(&super::ast::Expr) -> Option<String>,
) -> Option<Wkb> {
    use super::ast::Expr;
    let srid_4326 = |e: Option<&Expr>| matches!(e, Some(Expr::Num(s)) if s == "4326");
    match (method, args) {
        ("geography::stgeomfromwkb", [x, sr]) if srid_4326(Some(sr)) => {
            let hex = match x {
                Expr::Num(h) if h.len() >= 2 && h[..2].eq_ignore_ascii_case("0x") => {
                    h[2..].to_string()
                }
                _ => known(x)?,
            };
            wkb(&unhex(&hex)?)
        }
        ("geography::point", [la, lo, sr]) if srid_4326(Some(sr)) => {
            point_of(la, lo).map(Wkb::Point)
        }
        _ => None,
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Option<u32> {
        let v = u32_at(self.b, self.at)?;
        self.at += 4;
        Some(v)
    }

    fn coord(&mut self) -> Option<(f64, f64)> {
        let x = f64_at(self.b, self.at)?;
        let y = f64_at(self.b, self.at + 8)?;
        self.at += 16;
        (x.is_finite() && y.is_finite() && x.abs() <= 180.0 && y.abs() <= 90.0).then_some((x, y))
    }

    /// A count of items each at least `min` bytes wide that still fit.
    fn count(&mut self, min: usize) -> Option<u32> {
        let n = self.u32()?;
        (n as usize)
            .checked_mul(min)
            .is_some_and(|w| w <= self.b.len() - self.at)
            .then_some(n)
    }

    fn points(&mut self, n: u32) -> Option<Vec<(f64, f64)>> {
        (0..n).map(|_| self.coord()).collect()
    }

    /// One geometry; `top` allows the collection (no nesting).
    fn geometry(&mut self, top: bool) -> Option<u32> {
        if *self.b.get(self.at)? != 1 {
            return None;
        }
        self.at += 1;
        let ty = self.u32()?;
        match ty {
            1 => {
                self.coord()?;
            }
            2 => {
                let n = self.count(16)?;
                if n == 1 {
                    return None;
                }
                self.points(n)?;
            }
            3 => {
                let rings = self.count(4)?;
                for _ in 0..rings {
                    let n = self.count(16)?;
                    let p = self.points(n)?;
                    // Bitwise equal first and last point, as the engines
                    // compare them.
                    let closed = p.len() >= 4
                        && p[0].0.to_bits() == p[p.len() - 1].0.to_bits()
                        && p[0].1.to_bits() == p[p.len() - 1].1.to_bits();
                    if !closed {
                        return None;
                    }
                }
            }
            4..=6 => {
                let n = self.count(9)?;
                for _ in 0..n {
                    if self.geometry(false)? != ty - 3 {
                        return None;
                    }
                }
            }
            7 if top => {
                let n = self.count(9)?;
                for _ in 0..n {
                    if self.geometry(false)? == 7 {
                        return None;
                    }
                }
            }
            _ => return None,
        }
        Some(ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sql(h: &str) -> Option<Point> {
        from_sqlserver(&unhex(h).unwrap())
    }

    #[test]
    fn sql_server_points_as_checked() {
        // geography::Point(36.5822667, -87.0655564, 4326)
        let p = sql("E6100000010C610619B7874A4240C982781332C455C0").unwrap();
        assert_eq!((p.x, p.y, p.z, p.m), (-87.0655564, 36.5822667, None, None));
        assert_eq!(
            ewkb_hex(&p),
            "0101000020E6100000C982781332C455C0610619B7874A4240"
        );
        // POINT(-87 36 5 7), POINT(-87 36 5), POINT(-87 36 NULL 7) and the
        // EWKB PostGIS writes for them.
        for (s, e) in [
            (
                "E6100000010F00000000000042400000000000C055C000000000000014400000000000001C40",
                "01010000E0E61000000000000000C055C0000000000000424000000000000014400000000000001C40",
            ),
            (
                "E6100000010D00000000000042400000000000C055C00000000000001440",
                "01010000A0E61000000000000000C055C000000000000042400000000000001440",
            ),
            (
                "E6100000010E00000000000042400000000000C055C00000000000001C40",
                "0101000060E61000000000000000C055C000000000000042400000000000001C40",
            ),
        ] {
            let p = sql(s).unwrap();
            assert_eq!(ewkb_hex(&p), e);
            assert_eq!(from_ewkb(&unhex(e).unwrap()), Some(p));
        }
    }

    #[test]
    fn points_one_side_changes_or_rejects_have_none() {
        // Longitude -187: SQL Server keeps it, PostGIS moves it to 173.
        assert_eq!(sql("E6100000010C000000000000424000000000006067C0"), None);
        // SRID 4269.
        assert_eq!(sql("AD100000010C00000000000042400000000000C055C0"), None);
        // Wrong length, version or properties.
        assert_eq!(sql("E6100000010C610619B7874A4240C982781332C455"), None);
        assert_eq!(sql("E6100000020C610619B7874A4240C982781332C455C0"), None);
        assert_eq!(sql("E6100000010D610619B7874A4240C982781332C455C0"), None);
        assert_eq!(sql("E610000001040000000000004240C982781332C455C0"), None);
        // NaN latitude.
        assert_eq!(sql("E6100000010C000000000000F87F0000000000C055C0"), None);
        // Big-endian EWKB and WKB without an SRID are not read.
        assert_eq!(
            from_ewkb(&unhex("0101000000C982781332C455C0610619B7874A4240").unwrap()),
            None
        );
    }

    fn wk(h: &str) -> Option<Wkb> {
        unhex(h).and_then(|b| wkb(&b))
    }

    /// `eval/geography/2026-11-02/cases.txt`: name, hex, then whether both
    /// engines accepted the input and kept its bytes.
    #[test]
    fn wkb_as_probed_on_both_engines() {
        let cases = include_str!("../../../../eval/geography/2026-11-02/cases.txt");
        let mut n = 0;
        for l in cases.lines().filter(|l| !l.trim().is_empty()) {
            let [name, hex, both] = l.split_whitespace().collect::<Vec<_>>()[..] else {
                panic!("bad case line {l}");
            };
            // Both engines read a collection inside a collection alike; it
            // is left out on purpose.
            let read = both == "same" && name != "gc_nested";
            assert_eq!(wk(hex).is_some(), read, "{name}");
            n += 1;
        }
        assert!(n > 50);
    }

    #[test]
    fn a_point_in_wkb_is_the_point_constant() {
        let Some(Wkb::Point(p)) = wk("0101000000000000000000F03F0000000000000040") else {
            panic!("point");
        };
        assert_eq!((p.srid, p.x, p.y), (4326, 1.0, 2.0));
        assert!(matches!(wk("0103000000010000000400000000000000000000000000000000000000000000000000F03F0000000000000000000000000000F03F000000000000F03F00000000000000000000000000000000"), Some(Wkb::Shape(_))));
    }

    /// Reads the cases of `SQLDIFF_WKB_CASES` (name, hex) and writes
    /// `name accept|reject` to `SQLDIFF_WKB_OUT`: the fuzz run of
    /// `eval/geography/2026-11-02/` compares this with the engines.
    #[test]
    #[ignore]
    fn wkb_decisions_for_the_fuzz_run() {
        let (Ok(i), Ok(o)) = (
            std::env::var("SQLDIFF_WKB_CASES"),
            std::env::var("SQLDIFF_WKB_OUT"),
        ) else {
            return;
        };
        let out: String = std::fs::read_to_string(i)
            .unwrap()
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(k, h)| {
                let ok = unhex(h.trim()).and_then(|b| wkb(&b)).is_some();
                format!("{k} {}\n", if ok { "accept" } else { "reject" })
            })
            .collect();
        std::fs::write(o, out).unwrap();
    }
}
