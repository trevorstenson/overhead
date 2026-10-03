// The window view: the sky from home as you'd see it out of a window facing
// one bearing. A pinhole camera at eye height, tilted a little up, so the
// skyline sits low and the sky fills the frame. Aircraft are lit points where
// they really are in the sky (bearing and elevation angle, the earth's curve
// included), with a label where the frame is made of cells.

use crate::env::{Env, Sun, Weather};
use crate::render::{Canvas, SELECTED, dim, label_text, status_color};
use crate::track::{Point, Status, Store, Track};
use std::time::{Instant, SystemTime};

const FOV_DEG: f64 = 90.0;
const PITCH_DEG: f64 = 20.0;
const NM_M: f64 = 1852.0;
const FT_M: f64 = 0.3048;
const EARTH_RADIUS_M: f64 = 6_371_000.0;
const EYE_M: f64 = 30.0;

const COMPASS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];

pub struct Window {
    /// The bearing the window faces, 0 north
    pub face_deg: f64,
    pub selected: Option<String>,
    /// Previews only: draw the sky at this time, and with this cloud cover
    pub at: Option<SystemTime>,
    pub cloud: Option<f64>,
}

/// The camera's frame: where a direction lands on the canvas
struct Camera {
    forward: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
    /// Pixels per unit across and down; they differ where pixels aren't square
    focal_x: f64,
    focal_y: f64,
    cx: f64,
    cy: f64,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl Camera {
    fn new(face_deg: f64, width: f64, height: f64, aspect: f64) -> Self {
        let (b, p) = (face_deg.to_radians(), PITCH_DEG.to_radians());
        // East, north, up
        let forward = [b.sin() * p.cos(), b.cos() * p.cos(), p.sin()];
        let right = [b.cos(), -b.sin(), 0.0];
        let up = [
            right[1] * forward[2] - right[2] * forward[1],
            right[2] * forward[0] - right[0] * forward[2],
            right[0] * forward[1] - right[1] * forward[0],
        ];
        let focal_x = (width / 2.0) / (FOV_DEG / 2.0).to_radians().tan();
        Self { forward, right, up, focal_x, focal_y: focal_x * aspect, cx: width / 2.0, cy: height / 2.0 }
    }

    /// A direction (east, north, up) onto the canvas; None behind the camera
    fn project(&self, d: [f64; 3]) -> Option<(f64, f64)> {
        let z = dot(d, self.forward);
        (z > 1e-6).then(|| (self.cx + self.focal_x * dot(d, self.right) / z, self.cy - self.focal_y * dot(d, self.up) / z))
    }

    /// The direction through a canvas point, as (bearing, elevation) radians
    fn ray(&self, x: f64, y: f64) -> (f64, f64) {
        let (u, v) = ((x - self.cx) / self.focal_x, (self.cy - y) / self.focal_y);
        let d: Vec<f64> = (0..3).map(|i| self.forward[i] + u * self.right[i] + v * self.up[i]).collect();
        let horizontal = d[0].hypot(d[1]);
        (d[0].atan2(d[1]), d[2].atan2(horizontal))
    }
}

/// Where an aircraft is from the eye: east, north, up in metres, with the
/// earth's curve taken off its height
fn eye_vector(p: Point, alt_ft: f64) -> [f64; 3] {
    let (e, n) = (p.x * NM_M, p.y * NM_M);
    let ground = e.hypot(n);
    [e, n, alt_ft * FT_M - EYE_M - ground * ground / (2.0 * EARTH_RADIUS_M)]
}

type Rgb = (f64, f64, f64);

fn rgb(c: u32) -> Rgb {
    (((c >> 16) & 0xff) as f64, ((c >> 8) & 0xff) as f64, (c & 0xff) as f64)
}

fn pack((r, g, b): Rgb) -> u32 {
    let f = |v: f64| v.round().clamp(0.0, 255.0) as u32;
    f(r) << 16 | f(g) << 8 | f(b)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t)
}

/// Zenith and horizon colors for a sun elevation, between keyframes from
/// deep night through twilight to midday
fn sky_colors(sun_deg: f64) -> (Rgb, Rgb) {
    const KEYS: [(f64, u32, u32); 6] = [
        (-18.0, 0x03050e, 0x0c1022),
        (-8.0, 0x0a102d, 0x2d2850),
        (-2.0, 0x192d64, 0xeb825a),
        (4.0, 0x3264aa, 0xf5b982),
        (15.0, 0x2d6ec3, 0xa5cdf0),
        (90.0, 0x2864be, 0xaad2f0),
    ];
    let s = sun_deg.clamp(KEYS[0].0, KEYS[5].0);
    let i = KEYS.windows(2).position(|w| s <= w[1].0).unwrap_or(4);
    let (a, b) = (KEYS[i], KEYS[i + 1]);
    let t = (s - a.0) / (b.0 - a.0);
    (mix(rgb(a.1), rgb(b.1), t), mix(rgb(a.2), rgb(b.2), t))
}

/// A cheap, stable hash of a cell of the sky, for stars, lights and clouds
fn hash(a: i64, b: i64, seed: u64) -> f64 {
    let mut h = (a as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (b as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed;
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Smooth value noise over the sky, by angle
fn noise(x: f64, y: f64, seed: u64) -> f64 {
    let (xi, yi) = (x.floor() as i64, y.floor() as i64);
    let (tx, ty) = (x - x.floor(), y - y.floor());
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let (a, b) = (hash(xi, yi, seed), hash(xi + 1, yi, seed));
    let (c, d) = (hash(xi, yi + 1, seed), hash(xi + 1, yi + 1, seed));
    let top = a + (b - a) * s(tx);
    let bottom = c + (d - c) * s(tx);
    top + (bottom - top) * s(ty)
}

impl Window {
    pub fn draw(&self, canvas: &mut Canvas, store: &Store, env: &Env, home: (f64, f64), now: Instant) {
        let sun = crate::env::sun_at(home.0, home.1, self.at.unwrap_or_else(SystemTime::now));
        let mut weather = env.weather();
        if let Some(cloud) = self.cloud {
            weather.cloud = cloud;
        }
        let camera = Camera::new(self.face_deg, canvas.width as f64, canvas.height as f64, canvas.aspect);
        // Wall-clock seconds: clouds drift and the strobe blinks by it
        let t = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        self.paint_sky(canvas, &camera, env, sun, weather, t);
        self.draw_aircraft(canvas, &camera, store, env, sun, now, t);
        if canvas.is_cells() {
            self.compass(canvas, &camera);
        }
    }

    fn paint_sky(&self, canvas: &mut Canvas, camera: &Camera, env: &Env, sun: Sun, weather: Weather, t: f64) {
        let (zenith, horizon) = sky_colors(sun.elevation_deg);
        let night = ((-sun.elevation_deg - 6.0) / 10.0).clamp(0.0, 1.0);
        let daylight = ((sun.elevation_deg + 6.0) / 16.0).clamp(0.15, 1.0);
        let grey = |c: Rgb| {
            let l = (c.0 * 0.3 + c.1 * 0.59 + c.2 * 0.11) * if weather.is_wet { 0.7 } else { 0.9 };
            (l, l, l * 1.04)
        };
        let ground_day = (52.0, 60.0, 54.0);
        let ground_night = (9.0, 11.0, 15.0);
        let (sun_b, sun_e) = (sun.azimuth_deg.to_radians(), sun.elevation_deg.to_radians());
        let skyline: Vec<f64> = (0..canvas.width)
            .map(|x| env.horizon_at(camera.ray(x as f64 + 0.5, camera.cy).0.to_degrees()))
            .collect();

        for y in 0..canvas.height {
            for x in 0..canvas.width {
                let (bearing, elevation) = camera.ray(x as f64 + 0.5, y as f64 + 0.5);
                let color = if elevation < skyline[x] {
                    let mut c = mix(ground_night, ground_day, daylight);
                    // Distant lights just under the skyline at night, thinning
                    // out below it, fixed to the land
                    let below = skyline[x] - elevation;
                    // Haze: land far off takes on the sky's color
                    c = mix(c, horizon, (1.0 - below / 0.12).clamp(0.0, 1.0) * 0.35);
                    let (lb, le) = ((bearing * 300.0).floor() as i64, (elevation * 300.0).floor() as i64);
                    if night > 0.3 && below < 0.02 && hash(lb, le, 7) < 0.03 * (1.0 - below / 0.02) {
                        c = mix(c, (255.0, 196.0, 120.0), night * (0.4 + 0.5 * hash(lb, le, 8)));
                    }
                    c
                } else {
                    let up = (elevation / 1.1).clamp(0.0, 1.0).powf(0.6);
                    let mut c = mix(horizon, zenith, up);
                    // The sun's glow, and the sun itself
                    if sun.elevation_deg > -8.0 {
                        let cos_angle = elevation.sin() * sun_e.sin() + elevation.cos() * sun_e.cos() * (bearing - sun_b).cos();
                        let angle = cos_angle.clamp(-1.0, 1.0).acos();
                        let glow = (1.0 - angle / 0.6).max(0.0).powi(2) * (1.0 - weather.cloud * 0.6);
                        c = mix(c, (255.0, 220.0, 170.0), glow * 0.6);
                        if angle < 0.02 && sun.elevation_deg > -1.0 && weather.cloud < 0.8 {
                            c = (255.0, 245.0, 225.0);
                        }
                    }
                    // Clouds: noise over bearing and height, thicker with cover,
                    // drifting slowly
                    if weather.cloud > 0.05 {
                        let (u, v) = (bearing * 6.0 + t / 600.0, elevation * 14.0);
                        let n = noise(u, v, 11) * 0.65 + noise(u * 2.3, v * 2.3, 23) * 0.35;
                        // Full cover closes every gap; broken cloud leaves sky between
                        let cover = ((n - (1.0 - weather.cloud) * 1.1 + 0.15) / 0.25).clamp(0.0, 1.0);
                        let mut lit = mix(grey(c), (235.0, 235.0, 240.0), daylight * 0.5);
                        // At night, cloud glows with the lights of the land under it
                        lit = mix(lit, (70.0, 55.0, 52.0), night * (1.0 - up) * 0.8);
                        c = mix(c, lit, cover * 0.85);
                    }
                    // Stars where it's dark and clear, fixed to the sky
                    if night > 0.2 && weather.cloud < 0.7 {
                        let (sb, se) = ((bearing * 400.0).floor() as i64, (elevation * 400.0).floor() as i64);
                        let h = hash(sb, se, 3);
                        if h < 0.0025 {
                            c = mix(c, (230.0, 235.0, 255.0), night * (0.4 + h * 200.0));
                        }
                    }
                    c
                };
                canvas.plot(x as i64, y as i64, pack(color), 1.0);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_aircraft(&self, canvas: &mut Canvas, camera: &Camera, store: &Store, env: &Env, sun: Sun, now: Instant, t: f64) {
        let night = sun.elevation_deg < -4.0;
        let mut seen: Vec<(&Track, (f64, f64), f64)> = Vec::new();
        let mut nearest_hidden: Option<(&Track, f64, f64)> = None;
        for track in store.tracks() {
            let Some(alt) = track.aircraft.alt_ft.filter(|_| track.status() != Status::Ground) else { continue };
            let p = track.position(now);
            let d = eye_vector(p, alt as f64);
            let elevation = d[2].atan2(d[0].hypot(d[1]));
            // Behind the hills
            if elevation < env.horizon_at(p.bearing()) {
                continue;
            }
            match camera.project(d).filter(|(x, y)| *x >= 0.0 && *y >= 0.0 && *x < canvas.width as f64 && *y < canvas.height as f64) {
                Some(at) => seen.push((track, at, p.distance())),
                None => {
                    if nearest_hidden.is_none_or(|(_, nm, _)| p.distance() < nm) {
                        let rel = (p.bearing() - self.face_deg + 540.0).rem_euclid(360.0) - 180.0;
                        nearest_hidden = Some((track, p.distance(), rel));
                    }
                }
            }
        }
        // Far first, so near ones land on top and label first
        seen.sort_by(|a, b| b.2.total_cmp(&a.2));

        for (track, _, _) in &seen {
            let Some(alt) = track.aircraft.alt_ft else { continue };
            let points: Vec<(f64, f64)> =
                track.trail.iter().filter_map(|(_, p, _)| camera.project(eye_vector(*p, alt as f64))).collect();
            for pair in points.windows(2) {
                canvas.line(pair[0], pair[1], 0xd8dde8, if night { 0.12 } else { 0.3 });
            }
        }

        // The nearest one's strobe: a flash a second
        let blink = t.fract() < 0.12;
        let nearest = seen.last().map(|(t, _, _)| t.aircraft.hex.clone());
        for (track, at, nm) in &seen {
            canvas.hit(&track.aircraft.hex, *at);
            let body = if night { 0xfff4d6 } else { 0x1d2330 };
            let radius = if canvas.is_cells() { 0.5 } else { (3.5 - nm / 4.0).clamp(1.0, 3.5) };
            canvas.disc(*at, radius, body);
            if nearest.as_deref() == Some(track.aircraft.hex.as_str()) && blink {
                canvas.disc(*at, radius + 1.0, 0xffffff);
            }
            if self.selected.as_deref() == Some(track.aircraft.hex.as_str()) {
                canvas.circle(*at, if canvas.is_cells() { 2.5 } else { 9.0 }, SELECTED);
            }
        }

        if !canvas.is_cells() {
            return;
        }
        // Labels: the picked one first, then nearest first
        let is_picked = |t: &Track| self.selected.as_deref() == Some(t.aircraft.hex.as_str());
        let order = seen.iter().filter(|(t, _, _)| is_picked(t)).chain(seen.iter().rev().filter(|(t, _, _)| !is_picked(t)));
        for (track, (x, y), nm) in order {
            let (column, row) = canvas.cell_of((*x, *y));
            let text = format!("{} {:.1}nm", label_text(track), nm);
            let len = text.chars().count() as i64;
            if is_picked(track) {
                let start = if column + 2 + len <= canvas.columns() as i64 { column + 2 } else { column - len - 1 };
                canvas.put_text_on(start, row, &text, 0x0a0f1a, Some(SELECTED));
                continue;
            }
            let color = dim(status_color(track.status()), if night { 0.85 } else { 0.55 });
            let color = if night { color } else { 0x0b1020 };
            for start in [column + 2, column - len - 1] {
                if canvas.is_text_free(start, row, len as usize) {
                    canvas.put_text(start, row, &text, color);
                    break;
                }
            }
        }
        // Nobody in the window: point to the nearest one out of it
        if let (true, Some((track, nm, rel))) = (seen.is_empty(), nearest_hidden) {
            let row = (canvas.rows() / 3) as i64;
            let text = format!("{} {:.1}nm", label_text(track), nm);
            if rel < 0.0 {
                canvas.put_text(1, row, &format!("← {text}"), 0xe5e7eb);
            } else {
                let len = text.chars().count() as i64 + 2;
                canvas.put_text(canvas.columns() as i64 - len - 1, row, &format!("{text} →"), 0xe5e7eb);
            }
        }
    }

    /// Compass points along the bottom row, where their bearings fall
    fn compass(&self, canvas: &mut Canvas, camera: &Camera) {
        let row = canvas.rows() as i64 - 1;
        for (i, name) in COMPASS.iter().enumerate() {
            let b = (i as f64 * 45.0).to_radians();
            let Some((x, _)) = camera.project([b.sin(), b.cos(), 0.0]) else { continue };
            let column = canvas.cell_of((x, 0.0)).0 - name.len() as i64 / 2;
            if column >= 0 && column + (name.len() as i64) <= canvas.columns() as i64 {
                canvas.put_text(column, row, name, 0x9aa3b2);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_ahead_lands_mid_frame_and_up_lands_higher() {
        let camera = Camera::new(90.0, 200.0, 100.0, 1.0);
        // Due east, tilted up by the camera's pitch: dead centre
        let p = PITCH_DEG.to_radians();
        let (x, y) = camera.project([p.cos(), 0.0, p.sin()]).unwrap();
        assert!((x - 100.0).abs() < 1e-6 && (y - 50.0).abs() < 1e-6);
        // A little north of east is left of centre
        let (x, _) = camera.project([1.0, 0.2, 0.2]).unwrap();
        assert!(x < 100.0);
        // Due west is behind
        assert!(camera.project([-1.0, 0.0, 0.1]).is_none());
    }

    #[test]
    fn rays_invert_projection() {
        let camera = Camera::new(200.0, 160.0, 90.0, 0.5);
        let (bearing, elevation) = camera.ray(40.0, 20.0);
        let d = [bearing.sin() * elevation.cos(), bearing.cos() * elevation.cos(), elevation.sin()];
        let (x, y) = camera.project(d).unwrap();
        assert!((x - 40.0).abs() < 1e-6 && (y - 20.0).abs() < 1e-6);
    }

    #[test]
    fn a_far_low_plane_sinks_below_the_curve() {
        let near = eye_vector(Point { x: 0.0, y: 2.0 }, 2000.0);
        let far = eye_vector(Point { x: 0.0, y: 120.0 }, 2000.0);
        assert!(near[2] > 0.0);
        assert!(far[2] < 0.0, "{far:?}");
    }
}
