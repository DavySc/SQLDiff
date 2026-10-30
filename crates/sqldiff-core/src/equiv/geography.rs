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
}
