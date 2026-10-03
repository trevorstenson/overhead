// The map under the radar, from OpenStreetMap: coastline, lakes and rivers,
// runways and airports, for about 60 nm around home. Fetched once through the
// Overpass API, simplified, and cached on disk by place, then projected into
// the same nm-from-home plane the aircraft are in.
//
// The sea is never a polygon in OSM, only coastline ways with the land on
// their left. So the radar shades a point as sea when it lies on the right of
// the coastline nearest it, found through a grid of the coastline's segments.

use crate::cache::cache_dir;
use crate::track::{Home, Point};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How far the map reaches from home, in degrees of latitude (about 60 nm)
const REACH_DEG: f64 = 1.0;
/// Lines are simplified until no point is further than this off them (about 25 m)
const SIMPLIFY_DEG: f64 = 0.000_25;
const OVERPASS: [&str; 2] = ["https://overpass-api.de/api/interpreter", "https://overpass.kumi.systems/api/interpreter"];
/// The coastline grid's cell, in nm
const GRID_NM: f64 = 1.0;

type LatLon = (f64, f64);

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Raw {
    /// Coastline lines, land on the left as you walk them
    coast: Vec<Vec<LatLon>>,
    /// Closed rings of lakes and river water
    water: Vec<Vec<LatLon>>,
    /// Runway centrelines, with their width in metres
    runways: Vec<(LatLon, LatLon, f64)>,
    /// Airports by IATA code (ICAO where there's none)
    airports: Vec<(String, LatLon)>,
    /// Each runway's designators ("4R/22L"), as `runways` lists them
    #[serde(default)]
    runway_refs: Vec<String>,
    /// Each airport's ICAO code, as `airports` lists them: what METARs go by
    #[serde(default)]
    airport_icaos: Vec<Option<String>>,
}

/// A runway, for working out which way an airport is using it
#[derive(Clone, Debug)]
pub struct Runway {
    pub a: Point,
    pub b: Point,
    /// "4R/22L", or empty where OSM doesn't say
    pub refs: String,
}

/// One coastline segment, with the points either side of it where the line
/// goes on: what settles which side a point near a corner is on
#[derive(Clone, Copy)]
pub struct Segment {
    pub a: Point,
    pub b: Point,
    before: Option<Point>,
    after: Option<Point>,
}

/// The map in nm from home, ready to draw
pub struct Map {
    pub coast: Vec<Vec<Point>>,
    pub water: Vec<Vec<Point>>,
    pub runways: Vec<(Point, Point, f64)>,
    pub airports: Vec<(String, Point)>,
    /// ICAO codes, as `airports` lists them
    pub airport_icaos: Vec<Option<String>>,
    /// Every runway, short ones too, with designators
    pub all_runways: Vec<Runway>,
    grid: HashMap<(i64, i64), Vec<Segment>>,
}

fn cross(o: Point, a: Point, b: Point) -> f64 {
    (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
}

impl Map {
    fn new(raw: &Raw, home: Home) -> Self {
        let project = |line: &Vec<LatLon>| line.iter().map(|(lat, lon)| home.project(*lat, *lon)).collect::<Vec<_>>();
        let coast: Vec<Vec<Point>> = raw.coast.iter().map(project).collect();
        let mut grid: HashMap<(i64, i64), Vec<Segment>> = HashMap::new();
        for line in &coast {
            let n = line.len();
            // An island's ring goes on round: its last point leads into its first
            let closed = n > 3 && line[0] == line[n - 1];
            for i in 0..n.saturating_sub(1) {
                let segment = Segment {
                    a: line[i],
                    b: line[i + 1],
                    before: i.checked_sub(1).map(|j| line[j]).or(if closed { Some(line[n - 2]) } else { None }),
                    after: line.get(i + 2).copied().or(if closed { Some(line[1]) } else { None }),
                };
                // Every grid cell the segment's box touches
                let (x0, x1) = (segment.a.x.min(segment.b.x), segment.a.x.max(segment.b.x));
                let (y0, y1) = (segment.a.y.min(segment.b.y), segment.a.y.max(segment.b.y));
                for gx in (x0 / GRID_NM).floor() as i64..=(x1 / GRID_NM).floor() as i64 {
                    for gy in (y0 / GRID_NM).floor() as i64..=(y1 / GRID_NM).floor() as i64 {
                        grid.entry((gx, gy)).or_default().push(segment);
                    }
                }
            }
        }
        Map {
            coast,
            water: raw.water.iter().map(project).collect(),
            // Airliner runways only: grass strips scatter marks at radar scale
            runways: raw
                .runways
                .iter()
                .map(|(a, b, w)| (home.project(a.0, a.1), home.project(b.0, b.1), *w))
                .filter(|(a, b, _)| (a.x - b.x).hypot(a.y - b.y) > 1000.0 / 1852.0)
                .collect(),
            airports: raw.airports.iter().map(|(code, p)| (code.clone(), home.project(p.0, p.1))).collect(),
            airport_icaos: (0..raw.airports.len()).map(|i| raw.airport_icaos.get(i).cloned().flatten()).collect(),
            all_runways: raw
                .runways
                .iter()
                .enumerate()
                .map(|(i, (a, b, _))| Runway {
                    a: home.project(a.0, a.1),
                    b: home.project(b.0, b.1),
                    refs: raw.runway_refs.get(i).cloned().unwrap_or_default(),
                })
                .collect(),
            grid,
        }
    }

    /// The map as plain data for a surface that draws it itself (the Desktop
    /// app's SVG radar): coastline simplified to a tenth of a mile, runways
    /// and airports, in nm east and north of home, to two decimals
    pub fn outline_json(&self) -> String {
        let round = |v: f64| (v * 100.0).round() / 100.0;
        let coast: Vec<Vec<[f64; 2]>> = self
            .coast
            .iter()
            .map(|line| {
                let pairs: Vec<LatLon> = line.iter().map(|p| (p.y, p.x)).collect();
                simplify(&pairs, 0.1).into_iter().map(|(y, x)| [round(x), round(y)]).collect::<Vec<_>>()
            })
            .filter(|line| line.len() > 1)
            .collect();
        let runways: Vec<[f64; 4]> = self.runways.iter().map(|(a, b, _)| [round(a.x), round(a.y), round(b.x), round(b.y)]).collect();
        let airports: Vec<(&str, f64, f64)> = self.airports.iter().map(|(code, p)| (code.as_str(), round(p.x), round(p.y))).collect();
        serde_json::json!({ "coast": coast, "runways": runways, "airports": airports }).to_string()
    }

    pub fn has_coast(&self) -> bool {
        !self.grid.is_empty()
    }

    /// Whether a point is sea: on the water side of the nearest coastline.
    /// With no coastline in reach it's land.
    pub fn is_sea(&self, q: Point) -> bool {
        let (gx, gy) = ((q.x / GRID_NM).floor() as i64, (q.y / GRID_NM).floor() as i64);
        // The nearest segment, and the nearest whose nearest point isn't a
        // loose end: a coastline cut off at the edge of the fetched area has
        // no next segment to say which way it turns, so near its end the
        // side test would sweep a wedge of sea into land
        let mut best: Option<(f64, Segment, f64)> = None;
        let mut best_joined: Option<(f64, Segment, f64)> = None;
        for ring in 0..120i64 {
            // Nothing in a further ring can beat what's found by more than a cell
            if let Some((d, _, _)) = best_joined {
                if (ring - 1) as f64 * GRID_NM > d {
                    break;
                }
            }
            for gxi in gx - ring..=gx + ring {
                for gyi in gy - ring..=gy + ring {
                    if (gxi - gx).abs() != ring && (gyi - gy).abs() != ring {
                        continue;
                    }
                    let Some(segments) = self.grid.get(&(gxi, gyi)) else { continue };
                    for s in segments {
                        let (dx, dy) = (s.b.x - s.a.x, s.b.y - s.a.y);
                        let len2 = dx * dx + dy * dy;
                        let t = if len2 > 0.0 { (((q.x - s.a.x) * dx + (q.y - s.a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                        let d = (q.x - s.a.x - t * dx).hypot(q.y - s.a.y - t * dy);
                        if best.is_none_or(|(bd, _, _)| d < bd) {
                            best = Some((d, *s, t));
                        }
                        let loose = (t <= 0.0 && s.before.is_none()) || (t >= 1.0 && s.after.is_none());
                        if !loose && best_joined.is_none_or(|(bd, _, _)| d < bd) {
                            best_joined = Some((d, *s, t));
                        }
                    }
                }
            }
        }
        let Some((_, s, t)) = best_joined.or(best) else { return false };
        // Nearest at a corner: which side depends on which way the line turns
        let corner = if t <= 0.0 {
            s.before.map(|p| (p, s.a, s.b))
        } else if t >= 1.0 {
            s.after.map(|n| (s.a, s.b, n))
        } else {
            None
        };
        let land = match corner {
            Some((p, v, n)) => {
                let (left_in, left_out) = (cross(p, v, q) > 0.0, cross(v, n, q) > 0.0);
                // A left turn wraps the land in a wedge: inside both. A right
                // turn opens it: inside either.
                if cross(p, v, n) > 0.0 { left_in && left_out } else { left_in || left_out }
            }
            None => cross(s.a, s.b, q) > 0.0,
        };
        !land
    }
}

/// The map around home, arriving in the background
pub struct Basemap {
    map: Arc<Mutex<Option<Arc<Map>>>>,
}

impl Basemap {
    pub fn start(home: Home) -> Self {
        let map = Arc::new(Mutex::new(None));
        let slot = map.clone();
        std::thread::spawn(move || {
            // A map cached by an older version draws at once, while the
            // current one (which knows more) downloads
            if let Some(raw) = load_older(home) {
                *slot.lock().unwrap() = Some(Arc::new(Map::new(&raw, home)));
            }
            if let Some(raw) = load(home) {
                *slot.lock().unwrap() = Some(Arc::new(Map::new(&raw, home)));
            }
        });
        Self { map }
    }

    pub fn get(&self) -> Option<Arc<Map>> {
        self.map.lock().unwrap().clone()
    }
}

/// The rounded place a map is cached under
fn cache_key(home: Home) -> (f64, f64) {
    ((home.lat * 10.0).round() / 10.0, (home.lon * 10.0).round() / 10.0)
}

/// A map an older version of skyd cached here, if there's no current one yet
fn load_older(home: Home) -> Option<Raw> {
    let (lat, lon) = cache_key(home);
    let dir = cache_dir()?;
    if dir.join(format!("basemap-v4-{lat:.1}-{lon:.1}.json")).exists() {
        return None;
    }
    ["v3", "v2"].iter().find_map(|v| {
        let text = std::fs::read_to_string(dir.join(format!("basemap-{v}-{lat:.1}-{lon:.1}.json"))).ok()?;
        serde_json::from_str(&text).ok()
    })
}

fn load(home: Home) -> Option<Raw> {
    // Cached by the tenth of a degree, so nearby homes share a map
    let (lat, lon) = cache_key(home);
    let path = cache_dir().map(|d| d.join(format!("basemap-v4-{lat:.1}-{lon:.1}.json")));
    if let Some(raw) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()) {
        return Some(raw);
    }
    let lon_reach = REACH_DEG / lat.to_radians().cos().max(0.2);
    let bbox = format!("{:.3},{:.3},{:.3},{:.3}", lat - REACH_DEG, lon - lon_reach, lat + REACH_DEG, lon + lon_reach);
    let query = format!(
        "[out:json][timeout:120];(\
         way[\"natural\"=\"coastline\"]({bbox});\
         way[\"natural\"=\"water\"](if:length()>4000)({bbox});\
         relation[\"natural\"=\"water\"]({bbox});\
         way[\"waterway\"=\"riverbank\"]({bbox});\
         way[\"aeroway\"=\"runway\"]({bbox});\
         nwr[\"aeroway\"=\"aerodrome\"][\"iata\"]({bbox});\
         );out geom qt;"
    );
    // A busy server, then its mirror, a few times over
    let mut json = None;
    for attempt in 0..6 {
        let url = OVERPASS[attempt % OVERPASS.len()];
        // Its own agent: the shared one's 10 s is too short for this query
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(150)))
            .user_agent(concat!("overhead-skyd/", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        let answer = agent
            .post(url)
            .send_form([("data", query.as_str())])
            .ok()
            .and_then(|mut r| r.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().ok())
            .and_then(|body| serde_json::from_str::<Value>(&body).ok());
        if answer.as_ref().is_some_and(|v| v["elements"].is_array()) {
            json = answer;
            break;
        }
        std::thread::sleep(Duration::from_secs(30 * (attempt as u64 + 1)));
    }
    let raw = parse(&json?);
    if let Some(path) = path {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string(&raw).unwrap());
    }
    Some(raw)
}

fn geometry(v: &Value) -> Vec<LatLon> {
    v.as_array()
        .map(|points| points.iter().filter_map(|p| Some((p["lat"].as_f64()?, p["lon"].as_f64()?))).collect())
        .unwrap_or_default()
}

fn parse(json: &Value) -> Raw {
    let mut raw = Raw::default();
    let mut coast_ways = Vec::new();
    for e in json["elements"].as_array().into_iter().flatten() {
        let tags = &e["tags"];
        let kind = e["type"].as_str().unwrap_or("");
        if tags["natural"] == "coastline" {
            coast_ways.push(geometry(&e["geometry"]));
        } else if tags["aeroway"] == "runway" {
            let g = geometry(&e["geometry"]);
            let width = tags["width"].as_str().and_then(|w| w.trim_end_matches(" m").parse().ok()).unwrap_or(45.0);
            if let (Some(a), Some(b)) = (g.first(), g.last()) {
                raw.runways.push((*a, *b, width));
                raw.runway_refs.push(tags["ref"].as_str().unwrap_or("").to_string());
            }
        } else if tags["aeroway"] == "aerodrome" {
            let code = tags["iata"].as_str().or(tags["icao"].as_str()).unwrap_or("").to_string();
            let at = match kind {
                "node" => e["lat"].as_f64().zip(e["lon"].as_f64()),
                _ => {
                    let b = &e["bounds"];
                    b["minlat"].as_f64().zip(b["maxlat"].as_f64()).zip(b["minlon"].as_f64().zip(b["maxlon"].as_f64()))
                        .map(|((a, b), (c, d))| ((a + b) / 2.0, (c + d) / 2.0))
                }
            };
            if let (false, Some(at)) = (code.is_empty(), at) {
                raw.airports.push((code, at));
                raw.airport_icaos.push(tags["icao"].as_str().map(String::from));
            }
        } else if kind == "relation" {
            // A big lake or river: its outer ways, joined into rings
            let outers: Vec<Vec<LatLon>> = e["members"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m["role"] == "outer")
                .map(|m| geometry(&m["geometry"]))
                .filter(|g| g.len() > 1)
                .collect();
            raw.water.extend(join(outers, true).into_iter().filter(|r| r.first() == r.last()));
        } else {
            let g = geometry(&e["geometry"]);
            if g.len() > 3 && g.first() == g.last() {
                raw.water.push(g);
            }
        }
    }
    raw.coast = join(coast_ways, false);
    for line in raw.coast.iter_mut().chain(raw.water.iter_mut()) {
        *line = simplify(line, SIMPLIFY_DEG);
    }
    // Ponds are clutter at radar scale: keep water a few hundred metres across
    raw.water.retain(|r| {
        let (lo, hi) = r.iter().fold(((f64::MAX, f64::MAX), (f64::MIN, f64::MIN)), |(lo, hi), p| {
            ((lo.0.min(p.0), lo.1.min(p.1)), (hi.0.max(p.0), hi.1.max(p.1)))
        });
        r.len() > 3 && (hi.0 - lo.0).max(hi.1 - lo.1) > 0.004
    });
    raw
}

/// Joins lines that meet into the longest runs they make: OSM splits a
/// coastline or a river's bank into many. Coastline only ever joins head to
/// tail, since which way it runs says which side is land; a river's banks may
/// meet end to end, and one is turned round to join them.
fn join(mut lines: Vec<Vec<LatLon>>, may_reverse: bool) -> Vec<Vec<LatLon>> {
    let mut joined = Vec::new();
    while let Some(mut line) = lines.pop() {
        loop {
            let (first, last) = (line[0], *line.last().unwrap());
            if first == last {
                break;
            }
            let Some(i) = lines.iter().position(|l| {
                l[0] == last || *l.last().unwrap() == first || (may_reverse && (*l.last().unwrap() == last || l[0] == first))
            }) else {
                break;
            };
            let mut next = lines.swap_remove(i);
            if next[0] == last {
                line.extend_from_slice(&next[1..]);
            } else if *next.last().unwrap() == last {
                // Only river banks meet end to end; coastline always runs on
                next.reverse();
                line.extend_from_slice(&next[1..]);
            } else if *next.last().unwrap() == first {
                next.extend_from_slice(&line[1..]);
                line = next;
            } else {
                next.reverse();
                next.extend_from_slice(&line[1..]);
                line = next;
            }
        }
        joined.push(line);
    }
    joined
}

/// Douglas–Peucker: the fewest points that keep a line within `tolerance`
fn simplify(line: &[LatLon], tolerance: f64) -> Vec<LatLon> {
    if line.len() < 3 {
        return line.to_vec();
    }
    let mut keep = vec![false; line.len()];
    keep[0] = true;
    keep[line.len() - 1] = true;
    let mut stack = vec![(0usize, line.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        let (a, b) = (line[start], line[end]);
        let (dx, dy) = (b.1 - a.1, b.0 - a.0);
        let len = dx.hypot(dy);
        let mut worst = (0.0, start);
        for i in start + 1..end {
            let p = line[i];
            let d = if len > 0.0 { ((p.1 - a.1) * dy - (p.0 - a.0) * dx).abs() / len } else { (p.1 - a.1).hypot(p.0 - a.0) };
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((start, worst.1));
            stack.push((worst.1, end));
        }
    }
    line.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_of(coast: Vec<Vec<LatLon>>) -> Map {
        Map::new(&Raw { coast, ..Raw::default() }, Home { lat: 0.0, lon: 0.0 })
    }

    #[test]
    fn sea_is_right_of_the_coastline() {
        // A coast running north along the meridian: land to the west (left)
        let map = map_of(vec![vec![(-0.5, 0.0), (0.5, 0.0)]]);
        assert!(map.is_sea(Point { x: 3.0, y: 0.0 }));
        assert!(!map.is_sea(Point { x: -3.0, y: 0.0 }));
    }

    #[test]
    fn an_island_is_land_inside_and_sea_around() {
        // Counter-clockwise ring, so the land is inside on the left
        let ring = vec![(-0.1, -0.1), (-0.1, 0.1), (0.1, 0.1), (0.1, -0.1), (-0.1, -0.1)];
        let map = map_of(vec![ring]);
        assert!(!map.is_sea(Point { x: 0.0, y: 0.0 }));
        // Off every corner, including the diagonals a naive side test gets
        // wrong, and the corner where the ring starts and ends
        for (x, y) in [(10.0, 10.0), (-10.0, 10.0), (10.0, -10.0), (-10.0, -10.0), (0.0, 12.0), (-9.0, -6.0), (-6.0, -9.0)] {
            assert!(map.is_sea(Point { x, y }), "{x},{y}");
        }
    }

    #[test]
    fn joins_split_ways_and_simplifies() {
        let joined = join(vec![vec![(0.0, 0.0), (0.0, 1.0)], vec![(0.0, 1.0), (0.0, 2.0)], vec![(5.0, 5.0), (6.0, 6.0)]], false);
        assert_eq!(joined.len(), 2);
        assert!(joined.iter().any(|l| l.len() == 3));
        let straight: Vec<LatLon> = (0..10).map(|i| (0.0, i as f64)).collect();
        assert_eq!(simplify(&straight, 0.001).len(), 2);
        // Coastline never turns a piece round; banks do
        let facing = vec![vec![(0.0, 0.0), (0.0, 1.0)], vec![(0.0, 2.0), (0.0, 1.0)]];
        assert_eq!(join(facing.clone(), false).len(), 2);
        assert_eq!(join(facing, true).len(), 1);
    }
}

