// What the nearest busy airport is doing: which runways it's landing and
// taking off on, read from the aircraft lined up with them, and its weather
// from the latest METAR (aviationweather.gov, keyless, worldwide).
//
// An arrival is low, descending, lined up with a runway's centreline and
// heading, and short of its threshold; a departure is low, climbing, lined up
// and past the far end. Each votes for that end of that runway.

use crate::basemap::{Map, Runway};
use crate::source::http_agent;
use crate::track::{Point, Store};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const METAR_EVERY: Duration = Duration::from_secs(10 * 60);
/// How far from the airport an aircraft still counts toward its runways
const NEAR_NM: f64 = 12.0;
const LOW_FT: i32 = 5000;

#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct Metar {
    pub icao: String,
    pub name: Option<String>,
    /// VFR, MVFR, IFR or LIFR
    pub category: Option<String>,
    /// Degrees, or None when variable or calm
    pub wind_dir: Option<i64>,
    pub wind_kt: Option<i64>,
    pub gust_kt: Option<i64>,
    pub visibility: Option<String>,
    /// The lowest broken or overcast layer, in feet
    pub ceiling_ft: Option<i64>,
    pub temp_c: Option<f64>,
    pub raw: Option<String>,
}

/// Where the traffic for a runway in use passes home: arrivals down the
/// approach, departures out along the climb
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct OverHome {
    /// `arrivals` or `departures`
    pub kind: &'static str,
    pub runway: String,
    /// How far from home the path passes
    pub offset_nm: f64,
    /// Which way from home the path is: N, NE…
    pub toward: &'static str,
    /// Roughly how high they are there
    pub alt_ft: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Default)]
pub struct Report {
    /// IATA code where there is one
    pub airport: String,
    pub icao: Option<String>,
    /// Runway ends in use, busiest first: "4R", "33L"
    pub landing: Vec<String>,
    pub departing: Vec<String>,
    pub metar: Option<Metar>,
    /// The paths in use that pass near home, nearest first
    pub over_home: Vec<OverHome>,
    /// The landing runway ends, threshold to far end, for the radar to mark
    #[serde(skip)]
    pub landing_lines: Vec<(Point, Point)>,
}

pub struct Ops {
    wanted: Arc<Mutex<Option<String>>>,
    metar: Arc<Mutex<Option<Metar>>>,
}

impl Ops {
    pub fn start() -> Self {
        let wanted: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let metar = Arc::new(Mutex::new(None));
        let (w, m) = (wanted.clone(), metar.clone());
        std::thread::spawn(move || {
            let mut last: Option<(String, Instant)> = None;
            let mut failed: Option<Instant> = None;
            loop {
                let icao = w.lock().unwrap().clone();
                if let Some(icao) = icao {
                    let due = last.as_ref().is_none_or(|(i, at)| *i != icao || at.elapsed() >= METAR_EVERY);
                    // After a failure, a minute before asking again
                    let resting = failed.is_some_and(|at| at.elapsed() < Duration::from_secs(60));
                    if due && !resting {
                        match fetch_metar(&icao) {
                            Some(metar) => {
                                *m.lock().unwrap() = Some(metar);
                                last = Some((icao.clone(), Instant::now()));
                                failed = None;
                            }
                            None => failed = Some(Instant::now()),
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        Self { wanted, metar }
    }

    /// The airport `code` (or, failing that, the nearest with runways) and what it's doing now
    pub fn report(&self, map: &Map, store: &Store, code: Option<&str>, now: Instant) -> Option<Report> {
        let airport = code
            .and_then(|c| map.airports.iter().position(|(a, _)| a == c))
            .or_else(|| nearest_with_runways(map))?;
        let (name, at) = &map.airports[airport];
        // OSM's ICAO code, else the US convention: K and the IATA code (BOS → KBOS)
        let icao = map
            .airport_icaos
            .get(airport)
            .cloned()
            .flatten()
            .or_else(|| (name.len() == 3 && name.chars().all(|c| c.is_ascii_uppercase())).then(|| format!("K{name}")));
        *self.wanted.lock().unwrap() = icao.clone();
        let runways: Vec<&Runway> = map.all_runways.iter().filter(|r| mid(r).sub(*at).len() < 4.0).collect();
        let (landing, departing) = runway_use(&runways, *at, store, now);
        let metar = self.metar.lock().unwrap().clone().filter(|m| Some(&m.icao) == icao.as_ref());
        let landing_lines = runways
            .iter()
            .flat_map(|r| ends(r))
            .filter(|(_, _, _, name)| landing.contains(name))
            .map(|(threshold, far, _, _)| (threshold, far))
            .collect();
        let over_home = over_home(&runways, &landing, &departing);
        Some(Report { airport: name.clone(), icao, landing, departing, metar, over_home, landing_lines })
    }
}

fn nearest_with_runways(map: &Map) -> Option<usize> {
    map.airports
        .iter()
        .enumerate()
        .filter(|(_, (_, at))| at.len() < 30.0 && map.all_runways.iter().any(|r| mid(r).sub(*at).len() < 4.0))
        .min_by(|a, b| a.1 .1.len().total_cmp(&b.1 .1.len()))
        .map(|(i, _)| i)
}

trait Vector {
    fn sub(self, other: Point) -> Point;
    fn len(self) -> f64;
}

impl Vector for Point {
    fn sub(self, o: Point) -> Point {
        Point { x: self.x - o.x, y: self.y - o.y }
    }
    fn len(self) -> f64 {
        self.x.hypot(self.y)
    }
}

fn mid(r: &Runway) -> Point {
    Point { x: (r.a.x + r.b.x) / 2.0, y: (r.a.y + r.b.y) / 2.0 }
}

fn heading(from: Point, to: Point) -> f64 {
    (to.x - from.x).atan2(to.y - from.y).to_degrees().rem_euclid(360.0)
}

fn angle_between(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// A runway's two ends as (threshold, far end, heading, designator): its
/// designators matched to directions by number, or made from the heading
fn ends(r: &Runway) -> [(Point, Point, f64, String); 2] {
    let forward = heading(r.a, r.b);
    let backward = (forward + 180.0).rem_euclid(360.0);
    let made = |h: f64| format!("{:02}", ((h / 10.0).round() as i64 - 1).rem_euclid(36) + 1);
    let parts: Vec<&str> = r.refs.split('/').map(str::trim).filter(|p| !p.is_empty()).collect();
    let number = |d: &str| d.trim_start_matches('0').trim_end_matches(['L', 'R', 'C']).parse::<f64>().ok();
    let (f, b) = match parts.as_slice() {
        [one, two] => match (number(one), number(two)) {
            (Some(n1), Some(_)) if angle_between(n1 * 10.0, forward) <= angle_between(n1 * 10.0, backward) => (one.to_string(), two.to_string()),
            (Some(_), Some(_)) => (two.to_string(), one.to_string()),
            _ => (made(forward), made(backward)),
        },
        _ => (made(forward), made(backward)),
    };
    [(r.a, r.b, forward, f), (r.b, r.a, backward, b)]
}

/// Votes from the aircraft lined up with each runway end, busiest first
pub fn runway_use(runways: &[&Runway], airport: Point, store: &Store, now: Instant) -> (Vec<String>, Vec<String>) {
    let mut landing: BTreeMap<String, usize> = BTreeMap::new();
    let mut departing: BTreeMap<String, usize> = BTreeMap::new();
    for t in store.tracks() {
        let a = &t.aircraft;
        let (Some(alt), Some(track)) = (a.alt_ft, a.track_deg) else { continue };
        let p = t.position(now);
        if alt > LOW_FT || p.sub(airport).len() > NEAR_NM {
            continue;
        }
        let (arriving, leaving) = (a.vrate_fpm <= -200, a.vrate_fpm >= 200);
        if !arriving && !leaving {
            continue;
        }
        for r in runways {
            for (start, end, h, name) in ends(r) {
                if angle_between(track, h) > 25.0 {
                    continue;
                }
                // Where it is along the runway's line (0 at the threshold, 1 at
                // the far end) and how far off the centreline
                let (dx, dy) = (end.x - start.x, end.y - start.y);
                let len2 = (dx * dx + dy * dy).max(1e-9);
                let along = ((p.x - start.x) * dx + (p.y - start.y) * dy) / len2;
                let off = ((p.x - start.x) * dy - (p.y - start.y) * dx).abs() / len2.sqrt();
                if off > 1.5 {
                    continue;
                }
                if arriving && along < 0.3 {
                    *landing.entry(name).or_default() += 1;
                } else if leaving && along > 0.5 {
                    *departing.entry(name).or_default() += 1;
                }
            }
        }
    }
    let busiest = |votes: BTreeMap<String, usize>| {
        let mut v: Vec<(String, usize)> = votes.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter().map(|(name, _)| name).collect()
    };
    (busiest(landing), busiest(departing))
}

const COMPASS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
/// A three-degree glidepath: about 318 ft for every nm out
const GLIDE_FT_PER_NM: f64 = 318.0;
/// A typical airliner's early climb
const CLIMB_FT_PER_NM: f64 = 700.0;

/// Where the arrivals and departures for the runways in use pass home (the
/// origin): arrivals down the extended centreline up to 20 nm out on a
/// three-degree glidepath, departures straight out for 10 nm (they turn
/// after that, so further out would be a guess). Only paths within 3 nm of
/// home, and at least 1.5 nm out from the runway: beside the field itself is
/// no prediction.
pub fn over_home(runways: &[&Runway], landing: &[String], departing: &[String]) -> Vec<OverHome> {
    let mut found = Vec::new();
    for r in runways {
        for (threshold, far, _, name) in ends(r) {
            let (dx, dy) = (far.x - threshold.x, far.y - threshold.y);
            let len = dx.hypot(dy).max(1e-9);
            let (ux, uy) = (dx / len, dy / len);
            // Home measured along the runway's line and off it
            let path = |from: Point, outward: f64, reach: f64, ft_per_nm: f64, kind: &'static str| {
                let (rx, ry) = (-from.x, -from.y);
                let along = (rx * ux + ry * uy) * outward;
                let off = rx * uy - ry * ux;
                // Out on the path, not at the field, and near enough to be overhead-ish
                if along < 1.5 || along > reach || off.abs() > 3.0 {
                    return None;
                }
                // The nearest point of the path, seen from home
                let (px, py) = (from.x + ux * along * outward, from.y + uy * along * outward);
                let toward = COMPASS[((px.atan2(py).to_degrees().rem_euclid(360.0) / 45.0).round() as usize) % 8];
                Some(OverHome {
                    kind,
                    runway: name.clone(),
                    offset_nm: (off.abs() * 10.0).round() / 10.0,
                    toward,
                    alt_ft: ((along * ft_per_nm / 100.0).round() * 100.0) as i64,
                })
            };
            if landing.contains(&name) {
                // Arrivals come in toward the threshold: out is backward
                found.extend(path(threshold, -1.0, 20.0, GLIDE_FT_PER_NM, "arrivals"));
            }
            if departing.contains(&name) {
                found.extend(path(far, 1.0, 10.0, CLIMB_FT_PER_NM, "departures"));
            }
        }
    }
    found.sort_by(|a, b| a.offset_nm.total_cmp(&b.offset_nm));
    found
}

fn fetch_metar(icao: &str) -> Option<Metar> {
    let body = http_agent()
        .get(&format!("https://aviationweather.gov/api/data/metar?ids={icao}&format=json"))
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    let v: Value = serde_json::from_str(&body).ok()?;
    parse_metar(v.as_array()?.first()?)
}

pub fn parse_metar(m: &Value) -> Option<Metar> {
    let ceiling_ft = m["clouds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| matches!(c["cover"].as_str(), Some("BKN" | "OVC" | "OVX")))
        .filter_map(|c| c["base"].as_i64())
        .min();
    Some(Metar {
        icao: m["icaoId"].as_str()?.to_string(),
        name: m["name"].as_str().map(String::from),
        category: m["fltCat"].as_str().map(String::from),
        wind_dir: m["wdir"].as_i64(),
        wind_kt: m["wspd"].as_i64(),
        gust_kt: m["wgst"].as_i64(),
        visibility: match &m["visib"] {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        },
        ceiling_ft,
        temp_c: m["temp"].as_f64(),
        raw: m["rawOb"].as_str().map(String::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::parse_readsb;
    use crate::track::Home;

    #[test]
    fn names_runway_ends_by_heading() {
        // A runway laid north to south in OSM: its 36 end faces north
        let r = Runway { a: Point { x: 0.0, y: 1.0 }, b: Point { x: 0.0, y: 0.0 }, refs: "18/36".into() };
        let [(_, _, h1, n1), (_, _, h2, n2)] = ends(&r);
        assert!((h1 - 180.0).abs() < 1e-6 && n1 == "18");
        assert!(h2.abs() < 1e-6 && n2 == "36");
        // No designators: made from the heading
        let bare = Runway { refs: String::new(), ..r };
        assert_eq!(ends(&bare)[1].3, "36");
    }

    #[test]
    fn reads_landings_from_lined_up_arrivals() {
        let home = Home { lat: 42.36, lon: -71.0 };
        // An east-west runway at home; one aircraft 4 nm west of it, low,
        // descending, heading east: landing on 09
        let r = Runway { a: Point { x: -0.5, y: 0.0 }, b: Point { x: 0.5, y: 0.0 }, refs: "9/27".into() };
        let lon = -71.0 - 4.0 / (60.0 * 42.36f64.to_radians().cos());
        let body = format!(r#"{{"ac":[{{"hex":"a1","lat":42.36,"lon":{lon},"alt_baro":1500,"gs":140,"track":90,"baro_rate":-700}}]}}"#);
        let mut store = Store::new(home);
        let now = Instant::now();
        store.update(parse_readsb(&body).unwrap(), now);
        let (landing, departing) = runway_use(&[&r], Point::default(), &store, now);
        assert_eq!(landing, vec!["9".to_string()]);
        assert!(departing.is_empty());
    }

    #[test]
    fn finds_where_arrivals_pass_home() {
        // Runway 9 runs east from 10 nm east of home; home sits 6 nm out on
        // its approach, half a mile north of the centreline
        let r = Runway { a: Point { x: 6.0, y: -0.5 }, b: Point { x: 7.5, y: -0.5 }, refs: "9/27".into() };
        let paths = over_home(&[&r], &["9".to_string()], &[]);
        assert_eq!(paths.len(), 1);
        let p = &paths[0];
        assert_eq!((p.kind, p.runway.as_str(), p.offset_nm, p.toward), ("arrivals", "9", 0.5, "S"));
        // 6 nm out on a three-degree path
        assert_eq!(p.alt_ft, 1900);
        // Landing the other way, the approach is on the far side: no pass
        assert!(over_home(&[&r], &["27".to_string()], &[]).is_empty());
    }

    #[test]
    fn reads_a_metar() {
        let v: Value = serde_json::from_str(
            r#"{"icaoId":"KBOS","name":"Boston/Logan Intl","fltCat":"MVFR","wdir":350,"wspd":7,"wgst":18,"visib":"10+","temp":22.2,
                "clouds":[{"cover":"SCT","base":1200},{"cover":"BKN","base":2500},{"cover":"OVC","base":8000}],"rawOb":"METAR KBOS"}"#,
        )
        .unwrap();
        let m = parse_metar(&v).unwrap();
        assert_eq!((m.category.as_deref(), m.wind_dir, m.gust_kt, m.ceiling_ft), (Some("MVFR"), Some(350), Some(18), Some(2500)));
        assert_eq!(m.visibility.as_deref(), Some("10+"));
    }
}
