// What the window view needs besides aircraft: where the sun is, what the
// weather is doing, and how high the land rises along the horizon. The sun is
// computed; weather and terrain come from Open-Meteo, keyless, on threads of
// their own, so the view draws a plain sky and a flat horizon until they land.

use crate::cache::cache_dir;
use crate::source::http_agent;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Horizon samples, one every this many degrees of bearing
pub const HORIZON_STEP_DEG: f64 = 5.0;
const HORIZON_SAMPLES: usize = (360.0 / HORIZON_STEP_DEG) as usize;
/// How far out the land is sampled, in metres
const HORIZON_DISTANCES_M: [f64; 4] = [1_000.0, 3_000.0, 8_000.0, 20_000.0];
/// Eyes at an upper window
const EYE_HEIGHT_M: f64 = 10.0;
const EARTH_RADIUS_M: f64 = 6_371_000.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct Weather {
    /// 0..1
    pub cloud: f64,
    /// Rain, drizzle, snow or storms falling now
    pub is_wet: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Sun {
    pub elevation_deg: f64,
    pub azimuth_deg: f64,
}

/// The sun's position for a place and time, to a fraction of a degree
pub fn sun_at(lat: f64, lon: f64, time: SystemTime) -> Sun {
    let unix = time.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let d = unix / 86_400.0 + 2_440_587.5 - 2_451_545.0;
    let rad = f64::to_radians;
    let g = rad((357.529 + 0.985_600_28 * d).rem_euclid(360.0));
    let q = (280.459 + 0.985_647_36 * d).rem_euclid(360.0);
    let l = rad(q + 1.915 * g.sin() + 0.020 * (2.0 * g).sin());
    let e = rad(23.439 - 0.000_000_36 * d);
    let ra = (e.cos() * l.sin()).atan2(l.cos());
    let dec = (e.sin() * l.sin()).asin();
    let gmst_deg = (18.697_374_558 + 24.065_709_824_419_08 * d).rem_euclid(24.0) * 15.0;
    let ha = rad(gmst_deg + lon) - ra;
    let phi = rad(lat);
    let elevation = (phi.sin() * dec.sin() + phi.cos() * dec.cos() * ha.cos()).asin();
    let azimuth = (-ha.sin()).atan2(dec.tan() * phi.cos() - phi.sin() * ha.cos());
    Sun { elevation_deg: elevation.to_degrees(), azimuth_deg: azimuth.to_degrees().rem_euclid(360.0) }
}

pub struct Env {
    weather: Arc<Mutex<Option<Weather>>>,
    /// Elevation angle of the skyline in radians, by bearing step
    horizon: Arc<Mutex<Option<Vec<f64>>>>,
}

impl Env {
    pub fn start(lat: f64, lon: f64) -> Self {
        let weather = Arc::new(Mutex::new(None));
        let horizon = Arc::new(Mutex::new(None));
        {
            let weather = weather.clone();
            std::thread::spawn(move || {
                loop {
                    let wait = match fetch_weather(lat, lon) {
                        Some(w) => {
                            *weather.lock().unwrap() = Some(w);
                            15 * 60
                        }
                        None => 2 * 60,
                    };
                    std::thread::sleep(Duration::from_secs(wait));
                }
            });
        }
        {
            let horizon = horizon.clone();
            std::thread::spawn(move || {
                if let Some(h) = load_horizon(lat, lon) {
                    *horizon.lock().unwrap() = Some(h);
                }
            });
        }
        Self { weather, horizon }
    }

    pub fn weather(&self) -> Weather {
        self.weather.lock().unwrap().unwrap_or_default()
    }

    /// The skyline's elevation angle at a bearing, in radians: the land, or
    /// the curve of the earth on open water and flat ground
    pub fn horizon_at(&self, bearing_deg: f64) -> f64 {
        let dip = -(2.0 * EYE_HEIGHT_M / EARTH_RADIUS_M).sqrt();
        let guard = self.horizon.lock().unwrap();
        let Some(h) = guard.as_ref() else { return dip };
        let pos = bearing_deg.rem_euclid(360.0) / HORIZON_STEP_DEG;
        let (i, t) = (pos.floor() as usize % HORIZON_SAMPLES, pos.fract());
        let j = (i + 1) % HORIZON_SAMPLES;
        (h[i] * (1.0 - t) + h[j] * t).max(dip)
    }
}

fn get_json(url: &str) -> Option<Value> {
    let body = http_agent().get(url).call().ok()?.body_mut().read_to_string().ok()?;
    serde_json::from_str(&body).ok()
}

fn fetch_weather(lat: f64, lon: f64) -> Option<Weather> {
    let v = get_json(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}&current=cloud_cover,weather_code"
    ))?;
    let current = &v["current"];
    let code = current["weather_code"].as_i64().unwrap_or(0);
    Some(Weather {
        cloud: current["cloud_cover"].as_f64().unwrap_or(0.0) / 100.0,
        // WMO codes: 51+ is drizzle, rain, snow, showers and storms
        is_wet: code >= 51,
    })
}

/// The point `distance` metres from home along a bearing
fn offset(lat: f64, lon: f64, bearing_deg: f64, distance: f64) -> (f64, f64) {
    let (b, d) = (bearing_deg.to_radians(), distance / EARTH_RADIUS_M);
    let (p1, l1) = (lat.to_radians(), lon.to_radians());
    let p2 = (p1.sin() * d.cos() + p1.cos() * d.sin() * b.cos()).asin();
    let l2 = l1 + (b.sin() * d.sin() * p1.cos()).atan2(d.cos() - p1.sin() * p2.sin());
    (p2.to_degrees(), l2.to_degrees())
}

/// The skyline around home, cached on disk by place: the steepest angle up
/// to the land along each bearing, less the earth's curve
fn load_horizon(lat: f64, lon: f64) -> Option<Vec<f64>> {
    let path = cache_dir().map(|d| d.join(format!("horizon-{lat:.3}-{lon:.3}.json")));
    if let Some(cached) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
        if let Ok(h) = serde_json::from_str::<Vec<f64>>(&cached) {
            if h.len() == HORIZON_SAMPLES {
                return Some(h);
            }
        }
    }
    let mut points = vec![(lat, lon)];
    for i in 0..HORIZON_SAMPLES {
        for d in HORIZON_DISTANCES_M {
            points.push(offset(lat, lon, i as f64 * HORIZON_STEP_DEG, d));
        }
    }
    let mut elevations = Vec::with_capacity(points.len());
    // The API takes up to 100 points a call, and counts each point against a
    // per-minute limit: over it, wait the minute out and ask again
    for chunk in points.chunks(100) {
        let lats = chunk.iter().map(|p| format!("{:.5}", p.0)).collect::<Vec<_>>().join(",");
        let lons = chunk.iter().map(|p| format!("{:.5}", p.1)).collect::<Vec<_>>().join(",");
        let url = format!("https://api.open-meteo.com/v1/elevation?latitude={lats}&longitude={lons}");
        let mut answer = None;
        for _ in 0..4 {
            answer = get_json(&url).filter(|v| v["elevation"].is_array());
            if answer.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_secs(65));
        }
        elevations.extend(answer?["elevation"].as_array()?.iter().map(|e| e.as_f64().unwrap_or(0.0)));
        std::thread::sleep(Duration::from_secs(2));
    }
    if elevations.len() != points.len() {
        return None;
    }
    let eye = elevations[0] + EYE_HEIGHT_M;
    let horizon: Vec<f64> = (0..HORIZON_SAMPLES)
        .map(|i| {
            HORIZON_DISTANCES_M
                .iter()
                .enumerate()
                .map(|(k, d)| {
                    let rise = elevations[1 + i * HORIZON_DISTANCES_M.len() + k] - eye - d * d / (2.0 * EARTH_RADIUS_M);
                    rise.atan2(*d)
                })
                .fold(f64::MIN, f64::max)
        })
        .collect();
    if let Some(path) = path {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string(&horizon).unwrap());
    }
    Some(horizon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_is_high_at_noon_in_june_and_down_at_midnight() {
        // 2026-06-21 16:45 UTC is about solar noon in Boston
        let noon = UNIX_EPOCH + Duration::from_secs(1_782_060_300);
        let sun = sun_at(42.39, -71.10, noon);
        assert!((sun.elevation_deg - 71.0).abs() < 1.5, "{sun:?}");
        assert!((sun.azimuth_deg - 180.0).abs() < 10.0, "{sun:?}");
        let midnight = noon + Duration::from_secs(12 * 3600);
        assert!(sun_at(42.39, -71.10, midnight).elevation_deg < -20.0);
    }

    #[test]
    fn offsets_north_by_a_degree() {
        let (lat, lon) = offset(42.0, -71.0, 0.0, 111_195.0);
        assert!((lat - 43.0).abs() < 0.01 && (lon + 71.0).abs() < 1e-6);
    }
}
