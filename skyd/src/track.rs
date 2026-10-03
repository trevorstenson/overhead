// Tracks between polls. Feeds answer every few seconds; frames come 30 times
// a second, so each aircraft is dead-reckoned from its last fix along its
// ground track. When a new fix lands somewhere other than where we'd drawn it,
// the gap is eased out over a moment instead of jumping.

use crate::source::Aircraft;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Never extrapolate further than this past a fix: a track that stopped
/// reporting shouldn't fly off across the map, but one the feed is merely
/// slow about (a rate-limited minute) should keep moving
const MAX_RECKON: Duration = Duration::from_secs(90);
const DROP_AFTER: Duration = Duration::from_secs(60);
const EASE_SECONDS: f64 = 0.6;
const TRAIL_EVERY: Duration = Duration::from_secs(2);
const TRAIL_LENGTH: Duration = Duration::from_secs(60);

/// Local flat-earth position relative to home, in nautical miles: x east, y north.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn distance(self) -> f64 {
        self.x.hypot(self.y)
    }

    /// Compass bearing from home, 0 north, clockwise
    pub fn bearing(self) -> f64 {
        self.x.atan2(self.y).to_degrees().rem_euclid(360.0)
    }
}

#[derive(Clone, Copy)]
pub struct Home {
    pub lat: f64,
    pub lon: f64,
}

impl Home {
    pub fn project(&self, lat: f64, lon: f64) -> Point {
        Point {
            x: (lon - self.lon) * 60.0 * self.lat.to_radians().cos(),
            y: (lat - self.lat) * 60.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    Ground,
    Arrival,
    Departure,
    Cruise,
}

pub struct Track {
    pub aircraft: Aircraft,
    fix: Point,
    fix_at: Instant,
    last_seen: Instant,
    /// Where we were drawing it minus where the new fix says, eased to zero
    correction: Point,
    corrected_at: Instant,
    /// Where it was every couple of seconds, and how high
    pub trail: VecDeque<(Instant, Point, Option<i32>)>,
}

impl Track {
    pub fn status(&self) -> Status {
        let a = &self.aircraft;
        match a.alt_ft {
            None => Status::Ground,
            Some(alt) if alt < 12_000 && a.vrate_fpm <= -300 => Status::Arrival,
            Some(alt) if alt < 12_000 && a.vrate_fpm >= 300 => Status::Departure,
            _ => Status::Cruise,
        }
    }

    fn reckoned(&self, now: Instant) -> Point {
        let a = &self.aircraft;
        let Some(track) = a.track_deg.filter(|_| !a.is_on_ground()) else {
            return self.fix;
        };
        let elapsed = now.saturating_duration_since(self.fix_at).min(MAX_RECKON).as_secs_f64();
        let nm = a.gs_kt / 3600.0 * elapsed;
        let radians = track.to_radians();
        Point { x: self.fix.x + nm * radians.sin(), y: self.fix.y + nm * radians.cos() }
    }

    pub fn position(&self, now: Instant) -> Point {
        let p = self.reckoned(now);
        let since = now.saturating_duration_since(self.corrected_at).as_secs_f64();
        let k = (-since / EASE_SECONDS * 3.0).exp();
        Point { x: p.x + self.correction.x * k, y: p.y + self.correction.y * k }
    }

    /// When, within `horizon`, the aircraft comes closest to home on its
    /// current track and speed, and how close: (seconds from now, nm)
    pub fn closest_approach(&self, now: Instant, horizon: Duration) -> Option<(f64, f64)> {
        let a = &self.aircraft;
        let track = a.track_deg.filter(|_| self.is_moving())?;
        let p = self.position(now);
        let speed = a.gs_kt / 3600.0;
        let (vx, vy) = (speed * track.to_radians().sin(), speed * track.to_radians().cos());
        // Minimizes |p + v t|: t = -(p·v) / |v|², kept within the horizon
        let t = (-(p.x * vx + p.y * vy) / (vx * vx + vy * vy)).clamp(0.0, horizon.as_secs_f64());
        Some((t, (p.x + vx * t).hypot(p.y + vy * t)))
    }

    pub fn is_moving(&self) -> bool {
        !self.aircraft.is_on_ground() && self.aircraft.gs_kt > 1.0
    }
}

pub struct Store {
    home: Home,
    tracks: HashMap<String, Track>,
}

impl Store {
    pub fn new(home: Home) -> Self {
        Self { home, tracks: HashMap::new() }
    }

    pub fn update(&mut self, aircraft: Vec<Aircraft>, now: Instant) {
        for a in aircraft {
            let fix = self.home.project(a.lat, a.lon);
            let fix_at = now.checked_sub(Duration::from_secs_f64(a.seen_pos_s.max(0.0))).unwrap_or(now);
            match self.tracks.get_mut(&a.hex) {
                Some(track) => {
                    // Feeds repeat a position until a newer one arrives
                    if fix_at <= track.fix_at && fix == track.fix {
                        track.aircraft = a;
                        track.last_seen = now;
                        continue;
                    }
                    let drawn = track.position(now);
                    track.aircraft = a;
                    track.fix = fix;
                    track.fix_at = fix_at;
                    track.last_seen = now;
                    let lands = track.reckoned(now);
                    track.correction = Point { x: drawn.x - lands.x, y: drawn.y - lands.y };
                    track.corrected_at = now;
                }
                None => {
                    self.tracks.insert(
                        a.hex.clone(),
                        Track {
                            aircraft: a,
                            fix,
                            fix_at,
                            last_seen: now,
                            correction: Point::default(),
                            corrected_at: now,
                            trail: VecDeque::new(),
                        },
                    );
                }
            }
        }
        self.tracks.retain(|_, t| now.saturating_duration_since(t.last_seen) < DROP_AFTER);
    }

    /// Drops any easing still under way, so every track sits on its fix:
    /// what a replay of past polls wants
    pub fn settle(&mut self) {
        for track in self.tracks.values_mut() {
            track.correction = Point::default();
        }
    }

    /// Records trail points; call once a frame
    pub fn tick(&mut self, now: Instant) {
        for track in self.tracks.values_mut() {
            let p = track.position(now);
            if track.trail.back().is_none_or(|(at, _, _)| now.saturating_duration_since(*at) >= TRAIL_EVERY) {
                track.trail.push_back((now, p, track.aircraft.alt_ft));
            }
            while track.trail.front().is_some_and(|(at, _, _)| now.saturating_duration_since(*at) > TRAIL_LENGTH) {
                track.trail.pop_front();
            }
        }
    }

    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.values()
    }

    pub fn any_moving(&self) -> bool {
        self.tracks.values().any(Track::is_moving)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(lat: f64, lon: f64, track: f64, gs: f64) -> Aircraft {
        Aircraft {
            hex: "abc123".into(),
            callsign: Some("TEST1".into()),
            type_code: None,
            registration: None,
            lat,
            lon,
            alt_ft: Some(10_000),
            gs_kt: gs,
            track_deg: Some(track),
            vrate_fpm: 0,
            seen_pos_s: 0.0,
            squawk: None,
            emergency: None,
            is_military: false,
            is_rotorcraft: false,
            interest: None,
        }
    }

    #[test]
    fn projects_north_and_east() {
        let home = Home { lat: 42.39, lon: -71.10 };
        let p = home.project(42.39 + 1.0 / 60.0, -71.10);
        assert!((p.y - 1.0).abs() < 1e-9 && p.x.abs() < 1e-9);
        assert!((p.bearing() - 0.0).abs() < 1e-6);
        let east = home.project(42.39, -71.0);
        assert!((east.bearing() - 90.0).abs() < 1e-6);
    }

    #[test]
    fn reckons_along_track() {
        let home = Home { lat: 42.39, lon: -71.10 };
        let mut store = Store::new(home);
        let t0 = Instant::now();
        // Due east at 360 kt: 0.1 nm a second
        store.update(vec![plane(42.39, -71.10, 90.0, 360.0)], t0);
        let p = store.tracks().next().unwrap().position(t0 + Duration::from_secs(10));
        assert!((p.x - 1.0).abs() < 1e-6, "{p:?}");
        assert!(p.y.abs() < 1e-6);
    }

    #[test]
    fn eases_instead_of_jumping() {
        let home = Home { lat: 42.39, lon: -71.10 };
        let mut store = Store::new(home);
        let t0 = Instant::now();
        store.update(vec![plane(42.39, -71.10, 90.0, 360.0)], t0);
        let t1 = t0 + Duration::from_secs(5);
        let before = store.tracks().next().unwrap().position(t1);
        // The new fix is 0.5 nm north of where we'd reckoned it
        let mut moved = plane(42.39 + 0.5 / 60.0, -71.10, 90.0, 360.0);
        moved.lon = -71.10 + 0.5 / (60.0 * 42.39f64.to_radians().cos());
        store.update(vec![moved], t1);
        let track = store.tracks().next().unwrap();
        assert!((track.position(t1).y - before.y).abs() < 1e-6, "no jump at the moment of the fix");
        assert!((track.position(t1 + Duration::from_secs(2)).y - 0.5).abs() < 0.01, "settled on the fix");
    }

    #[test]
    fn predicts_a_pass_overhead() {
        let home = Home { lat: 42.39, lon: -71.10 };
        let mut store = Store::new(home);
        let t0 = Instant::now();
        // One nm south, heading north at 360 kt: overhead in 10 s
        store.update(vec![plane(42.39 - 1.0 / 60.0, -71.10, 0.0, 360.0)], t0);
        let (t, nm) = store.tracks().next().unwrap().closest_approach(t0, Duration::from_secs(60)).unwrap();
        assert!((t - 10.0).abs() < 0.01 && nm < 0.01, "{t} s, {nm} nm");
        // Heading away: closest is now
        let mut store = Store::new(home);
        store.update(vec![plane(42.39 - 1.0 / 60.0, -71.10, 180.0, 360.0)], t0);
        let (t, nm) = store.tracks().next().unwrap().closest_approach(t0, Duration::from_secs(60)).unwrap();
        assert!(t == 0.0 && (nm - 1.0).abs() < 0.01);
    }

    #[test]
    fn drops_stale_tracks() {
        let mut store = Store::new(Home { lat: 42.39, lon: -71.10 });
        let t0 = Instant::now();
        store.update(vec![plane(42.4, -71.1, 0.0, 200.0)], t0);
        store.update(vec![], t0 + Duration::from_secs(61));
        assert_eq!(store.tracks().count(), 0);
    }
}
