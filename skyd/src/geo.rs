// Distances and paths on the earth, for routes and the lines drawn along them.

const EARTH_RADIUS_NM: f64 = 3440.065;

/// Great-circle distance in nautical miles
pub fn distance_nm(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    EARTH_RADIUS_NM * 2.0 * a.sqrt().asin()
}

/// Initial great-circle bearing from one point to another, 0 north
pub fn bearing(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = dl.sin() * p2.cos();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}

/// Points along the great circle from one place to another, both ends
/// included: the path an airliner flies, curved on a flat map
pub fn great_circle(lat1: f64, lon1: f64, lat2: f64, lon2: f64, steps: usize) -> Vec<(f64, f64)> {
    let to_vec = |lat: f64, lon: f64| {
        let (p, l) = (lat.to_radians(), lon.to_radians());
        [p.cos() * l.cos(), p.cos() * l.sin(), p.sin()]
    };
    let (a, b) = (to_vec(lat1, lon1), to_vec(lat2, lon2));
    let omega = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0).acos();
    if omega < 1e-9 {
        return vec![(lat1, lon1), (lat2, lon2)];
    }
    (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let (wa, wb) = (((1.0 - t) * omega).sin() / omega.sin(), (t * omega).sin() / omega.sin());
            let v = [wa * a[0] + wb * b[0], wa * a[1] + wb * b[1], wa * a[2] + wb * b[2]];
            (v[2].atan2(v[0].hypot(v[1])).to_degrees(), v[1].atan2(v[0]).to_degrees())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boston_to_london_bends_north() {
        let path = great_circle(42.36, -71.01, 51.47, -0.45, 32);
        assert_eq!(path.len(), 33);
        assert!((path[0].0 - 42.36).abs() < 1e-6 && (path[32].1 + 0.45).abs() < 1e-6);
        // The midpoint is well north of both ends' average latitude
        assert!(path[16].0 > 52.0, "{:?}", path[16]);
        assert!((distance_nm(42.36, -71.01, 51.47, -0.45) - 2834.0).abs() < 20.0);
    }
}
