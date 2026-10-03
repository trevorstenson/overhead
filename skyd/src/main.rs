// skyd: Overhead's helper. It owns everything heavy: polling the ADS-B feed,
// tracking and dead-reckoning aircraft, and drawing the radar. The mod only
// relays what it prints.
//
//   skyd locate              one line of JSON: home from the public IP
//   skyd geocode <place>     one line of JSON: a named place or "lat,lon"
//   skyd run --lat --lon …   the radar, one message per line:
//       @ready
//       @frame <shm-name> <w> <h>     --mode shm: a shared-memory RGB frame
//       @cells <cols> <rows> <b64>    --mode cells: a Raster's cells
//       @aircraft <json>              after each poll, nearest first
//       @overhead <json>              an aircraft will pass within 1 km of
//                                     home in the next minute; once per pass
//       @alert <json>                 an emergency (7500, 7600, 7700, or
//                                     declared), a military aircraft, or one
//                                     spotters tagged (plane-alert-db); once
//       @pan <east> <north>           where follow mode has moved the map
//       @outline <json>               the map, once it's loaded, for a surface
//                                     that draws it itself (Desktop's SVG)
//       @trace <json>                 the picked aircraft's flight so far, as
//                                     [east, north, altitude] points
//       @history <seconds>            how far back `rewind` can go
//       @ops <json>                   the busiest nearby airport: runways in
//                                     use and its METAR, when they change
//       @photo <json>                 the picked aircraft's photo, as a file,
//                                     cells and a JPEG, with its credit; or
//                                     {"hex": …, "none": true}
//       @error <message>
//   skyd run --mode ansi …   waits for one poll and prints the frame, to look
//                            at in any terminal
//   skyd run --mode ppm …    the same, as a full-resolution PPM image
//
// `--input <path>` names a control file the mod rewrites whole (the mod can't
// write to a spawned child's stdin). skyd reads it every frame and applies
// what changed, as space-separated pairs:
//   columns 120 rows 34 range 15 paused 0 select a39fe4 view window face 135
//   click 3 0.42 0.61 pan -2.5 1.0 colors altitude follow 1
// `colors` is `altitude` (tar1090's scale) or `status`; `follow 1` keeps the
// picked aircraft in the middle of the radar. `rewind 300` draws the sky as
// it was five minutes ago, replayed from the polls skyd keeps (30 minutes).
// `pan` is where the middle of the radar is, in nm east and north of home.
// skyd reports `@scale <nm across> <nm down>`, the radar picture's span, when
// it changes, and `@face <degrees>` when the window turns by itself.
// A click (numbered, so a repeat is a new one) is a point on the picture as
// fractions of its width and height; skyd answers `@select <hex>` with the
// aircraft drawn nearest it, or `@select -` when none is near.
// Paused, skyd draws nothing but keeps polling, more slowly, so the status
// line stays live while the pane is closed.

mod airframe;
mod basemap;
mod cache;
mod env;
mod geo;
mod interest;
mod locate;
mod ops;
mod photo;
mod render;
mod routes;
mod shm;
mod source;
mod trace;
mod track;
mod window;

use render::{Canvas, Radar};
use serde::Serialize;
use source::{AdsbLol, Aircraft, FileSource, Source};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use track::{Home, Status, Store};

static STOP: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

enum Mode {
    Shm { width: usize, height: usize, prefix: String },
    Cells { columns: usize, rows: usize },
    Ansi { columns: usize, rows: usize },
    Ppm { width: usize, height: usize },
}

struct Args {
    mode: Mode,
    fps: u32,
    home: Home,
    range_nm: f64,
    source: String,
    poll: Duration,
    input: Option<String>,
    view: String,
    at: Option<std::time::SystemTime>,
    cloud: Option<f64>,
    select: Option<String>,
}

/// What the control file asks for; None leaves a setting as it is
#[derive(Default, PartialEq, Clone)]
struct Control {
    columns: Option<usize>,
    rows: Option<usize>,
    range_nm: Option<f64>,
    paused: Option<bool>,
    /// An aircraft's hex, or `-` for none
    select: Option<String>,
    /// `radar` or `window`
    view: Option<String>,
    /// The bearing the window faces, or `auto`
    face: Option<String>,
    /// The latest click: its number, then where, as fractions
    click: Option<(u64, f64, f64)>,
    /// The radar's middle, nm east and north of home
    pan: Option<(f64, f64)>,
    /// `altitude` or `status`
    colors: Option<String>,
    follow: Option<bool>,
    /// Seconds into the past to draw; 0 is now
    rewind: Option<u64>,
    /// How many cells wide to make the picked aircraft's photo
    photo: Option<usize>,
    /// Degrees of sky across the window view
    fov: Option<f64>,
}

fn parse_control(text: &str) -> Control {
    let mut control = Control::default();
    let mut words: Vec<&str> = text.split_whitespace().collect();
    // `pan` takes two values; pull it out so the rest stay pairs
    if let Some(i) = words.iter().position(|w| *w == "pan") {
        let number = |k: usize| words.get(i + k).and_then(|w| w.parse::<f64>().ok());
        if let (Some(x), Some(y)) = (number(1), number(2)) {
            control.pan = Some((x, y));
        }
        words.drain(i..(i + 3).min(words.len()));
    }
    // The one entry with three values; the rest are pairs
    if let Some(i) = words.iter().position(|w| *w == "click") {
        let number = |k: usize| words.get(i + k).and_then(|w| w.parse::<f64>().ok());
        if let (Some(n), Some(x), Some(y)) = (number(1), number(2), number(3)) {
            control.click = Some((n as u64, x, y));
        }
        let mut rest = words.clone();
        rest.drain(i..(i + 4).min(words.len()));
        let mut without = parse_control(&rest.join(" "));
        without.click = control.click;
        without.pan = control.pan;
        return without;
    }
    for pair in words.chunks(2) {
        let [key, value] = pair else { continue };
        match *key {
            "columns" => control.columns = value.parse().ok().map(|c: usize| c.clamp(1, 512)),
            "rows" => control.rows = value.parse().ok().map(|r: usize| r.clamp(1, 256)),
            "range" => control.range_nm = value.parse().ok().map(|r: f64| r.clamp(2.0, 100.0)),
            "paused" => control.paused = Some(*value == "1"),
            "select" => control.select = Some(value.to_string()),
            "view" => control.view = Some(value.to_string()),
            "face" => control.face = Some(value.to_string()),
            "colors" => control.colors = Some(value.to_string()),
            "follow" => control.follow = Some(*value == "1"),
            "rewind" => control.rewind = value.parse().ok(),
            "photo" => control.photo = value.parse().ok(),
            "fov" => control.fov = value.parse().ok().map(|f: f64| f.clamp(20.0, 140.0)),
            "click" => {}
            _ => {}
        }
    }
    control
}

fn parse_run_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut mode = String::from("shm");
    let (mut width, mut height) = (960usize, 540usize);
    let (mut columns, mut rows) = (80usize, 22usize);
    let mut prefix = format!("/ov{}-", std::process::id());
    let mut fps = 30u32;
    let (mut lat, mut lon) = (None, None);
    let mut range_nm = 15.0;
    let mut source = String::from("adsb.lol");
    let mut poll = 5.0;
    let mut input = None;
    let mut view = String::from("radar");
    let (mut at, mut cloud) = (None, None);
    let mut select = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        let int = |s: String| s.parse::<usize>().map_err(|e| format!("{s}: {e}"));
        let float = |s: String| s.parse::<f64>().map_err(|e| format!("{s}: {e}"));
        match flag.as_str() {
            "--mode" => mode = value()?,
            "--width" => width = int(value()?)?,
            "--height" => height = int(value()?)?,
            "--columns" => columns = int(value()?)?,
            "--rows" => rows = int(value()?)?,
            "--prefix" => prefix = value()?,
            "--fps" => fps = int(value()?)? as u32,
            "--lat" => lat = Some(float(value()?)?),
            "--lon" => lon = Some(float(value()?)?),
            "--range" => range_nm = float(value()?)?.clamp(2.0, 100.0),
            "--source" => source = value()?,
            "--poll" => poll = float(value()?)?.max(1.0),
            "--input" => input = Some(value()?),
            "--view" => view = value()?,
            // Previews: the sky at a unix time, and a cloud cover from 0 to 1
            "--at" => at = Some(std::time::UNIX_EPOCH + Duration::from_secs_f64(float(value()?)?)),
            "--cloud" => cloud = Some(float(value()?)?.clamp(0.0, 1.0)),
            // Previews: draw with this aircraft picked
            "--select" => select = Some(value()?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    let (Some(lat), Some(lon)) = (lat, lon) else {
        return Err("--lat and --lon are required (see `skyd locate`)".into());
    };
    let (columns, rows) = (columns.clamp(1, 512), rows.clamp(1, 256));
    let mode = match mode.as_str() {
        "shm" => Mode::Shm { width, height, prefix },
        "cells" => Mode::Cells { columns, rows },
        "ansi" => Mode::Ansi { columns, rows },
        "ppm" => Mode::Ppm { width, height },
        other => return Err(format!("unknown mode {other}")),
    };
    Ok(Args { mode, fps: fps.clamp(1, 120), home: Home { lat, lon }, range_nm, source, poll: Duration::from_secs_f64(poll), input, view, at, cloud, select })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("locate") => print_place(locate::locate()),
        Some("geocode") => print_place(locate::geocode(&args.collect::<Vec<_>>().join(" "))),
        Some("run") => parse_run_args(args).and_then(run),
        _ => Err("usage: skyd locate | skyd geocode <place> | skyd run --lat <lat> --lon <lon> [flags]".into()),
    };
    if let Err(message) = result {
        println!("@error {message}");
        std::process::exit(2);
    }
}

fn print_place(place: Result<locate::Place, String>) -> Result<(), String> {
    println!("{}", serde_json::to_string(&place?).unwrap());
    Ok(())
}

/// Polls on its own thread so a slow feed never stalls a frame
fn spawn_poller(mut source: Box<dyn Source>) -> mpsc::Receiver<Result<Vec<Aircraft>, String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut backoff = source.interval();
        loop {
            let result = source.fetch();
            let wait = if result.is_ok() {
                backoff = source.interval();
                // Nobody is watching the map, only the status line
                if PAUSED.load(Ordering::Relaxed) { source.interval() * 3 } else { source.interval() }
            } else {
                // Twice as long each failure in a row, and never sooner than
                // the server asked
                backoff = (backoff * 2).min(Duration::from_secs(120)).max(source.retry_after().unwrap_or_default());
                backoff
            };
            if tx.send(result).is_err() {
                return;
            }
            std::thread::sleep(wait);
        }
    });
    rx
}

#[derive(Serialize)]
struct Summary<'a> {
    hex: &'a str,
    callsign: Option<&'a str>,
    #[serde(rename = "type")]
    type_code: Option<&'a str>,
    registration: Option<&'a str>,
    alt_ft: Option<i32>,
    gs_kt: f64,
    track_deg: Option<f64>,
    vrate_fpm: i32,
    status: &'static str,
    distance_nm: f64,
    bearing_deg: f64,
    route: Option<routes::Leg<'a>>,
    /// "Embraer Legacy 450"
    model: Option<&'a str>,
    owner: Option<&'a str>,
    squawk: Option<&'a str>,
    /// In words: `emergency`, `hijack`, `radio failure`, `medical`…
    emergency: Option<&'static str>,
    military: bool,
    rotorcraft: bool,
    /// Spotters tagged it: plane-alert-db's group, category, operator and notes
    interest: Option<interest::Interest>,
    /// Along the route's great circle to the destination, and how long at
    /// this ground speed
    remaining_nm: Option<f64>,
    eta_min: Option<f64>,
}

/// What skyd looks up about the aircraft it tracks, beyond what the feed says
struct Facts {
    routes: cache::Lookups<routes::Route>,
    airframes: cache::Lookups<airframe::Airframe>,
    interests: interest::Interests,
}

impl Facts {
    fn new() -> Self {
        Self { routes: routes::lookups(), airframes: airframe::lookups(), interests: interest::Interests::start() }
    }

    /// Takes in finished lookups and saves them; true when any arrived
    fn collect(&mut self) -> bool {
        let routes = self.routes.collect();
        let airframes = self.airframes.collect();
        self.routes.save();
        self.airframes.save();
        routes || airframes
    }
}

fn summarize(store: &Store, facts: &mut Facts, now: Instant) -> String {
    // Ask about every airborne aircraft first; the borrows below only read
    for t in store.tracks().filter(|t| !t.aircraft.is_on_ground()) {
        if let Some(callsign) = t.aircraft.callsign.as_deref().filter(|c| routes::looks_like_flight(c)) {
            facts.routes.want(callsign);
        }
        facts.airframes.want(&t.aircraft.hex);
    }
    let facts = &*facts;
    let mut list: Vec<Summary> = store
        .tracks()
        .map(|t| {
            let a = &t.aircraft;
            let p = t.position(now);
            let airframe = facts.airframes.get(&a.hex);
            let route = a
                .callsign
                .as_deref()
                .and_then(|c| facts.routes.get(c))
                .and_then(|r| r.leg_at(a.lat, a.lon, a.track_deg));
            Summary {
                hex: &a.hex,
                callsign: a.callsign.as_deref(),
                type_code: a.type_code.as_deref().or(airframe.and_then(|f| f.icao_type.as_deref())),
                registration: a.registration.as_deref(),
                alt_ft: a.alt_ft,
                gs_kt: a.gs_kt,
                track_deg: a.track_deg,
                vrate_fpm: a.vrate_fpm,
                status: match t.status() {
                    Status::Ground => "ground",
                    Status::Arrival => "arrival",
                    Status::Departure => "departure",
                    Status::Cruise => "cruise",
                },
                distance_nm: (p.distance() * 10.0).round() / 10.0,
                bearing_deg: p.bearing().round(),
                remaining_nm: route.as_ref().map(|leg| (geo::distance_nm(a.lat, a.lon, leg.to.lat, leg.to.lon) * 10.0).round() / 10.0),
                eta_min: route
                    .as_ref()
                    .filter(|_| a.gs_kt > 60.0)
                    .map(|leg| (geo::distance_nm(a.lat, a.lon, leg.to.lat, leg.to.lon) / a.gs_kt * 60.0).round()),
                route,
                model: airframe.and_then(|f| f.model.as_deref()),
                owner: airframe.and_then(|f| f.owner.as_deref()),
                squawk: a.squawk.as_deref(),
                emergency: a.emergency_kind(),
                military: a.is_military,
                rotorcraft: a.is_rotorcraft,
                interest: facts.interests.get(&a.hex),
            }
        })
        .collect();
    list.sort_by(|a, b| a.distance_nm.total_cmp(&b.distance_nm));
    serde_json::to_string(&list).unwrap()
}

/// Facing the window: toward the airport that the most nearby flights are
/// coming from or going to, where the traffic is
fn busiest_airport_bearing(store: &Store, facts: &Facts, home: Home) -> Option<f64> {
    busiest_airport(store, facts, home).map(|(_, bearing)| bearing)
}

/// The airport within 40 nm that the most nearby flights are coming from or
/// going to: its code and its bearing from home
fn busiest_airport(store: &Store, facts: &Facts, home: Home) -> Option<(String, f64)> {
    let mut counts: HashMap<&str, (usize, f64)> = HashMap::new();
    for t in store.tracks() {
        let a = &t.aircraft;
        let Some(leg) = a.callsign.as_deref().and_then(|c| facts.routes.get(c)).and_then(|r| r.leg_at(a.lat, a.lon, a.track_deg)) else {
            continue;
        };
        for airport in [leg.from, leg.to] {
            let p = home.project(airport.lat, airport.lon);
            if p.distance() < 40.0 {
                counts.entry(airport.code.as_str()).or_insert((0, p.bearing())).0 += 1;
            }
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(b.0.cmp(a.0)))
        .map(|(code, (_, bearing))| (code.to_string(), bearing))
}

/// "Overhead" is within this of home, horizontally: about a kilometre
const OVERHEAD_NM: f64 = 0.55;
const OVERHEAD_HORIZON: Duration = Duration::from_secs(60);

#[derive(Serialize)]
struct Pass<'a> {
    hex: &'a str,
    callsign: Option<&'a str>,
    #[serde(rename = "type")]
    type_code: Option<&'a str>,
    model: Option<&'a str>,
    alt_ft: Option<i32>,
    in_s: u32,
    closest_nm: f64,
    route: Option<routes::Leg<'a>>,
}

/// Aircraft about to pass overhead that haven't been announced lately
fn passes(store: &Store, facts: &Facts, announced: &mut HashMap<String, Instant>, now: Instant) -> Vec<String> {
    announced.retain(|_, at| now.saturating_duration_since(*at) < Duration::from_secs(600));
    let mut lines = Vec::new();
    for t in store.tracks() {
        let a = &t.aircraft;
        let Some((in_s, closest_nm)) = t.closest_approach(now, OVERHEAD_HORIZON) else { continue };
        if closest_nm > OVERHEAD_NM || announced.contains_key(&a.hex) {
            continue;
        }
        announced.insert(a.hex.clone(), now);
        let airframe = facts.airframes.get(&a.hex);
        let pass = Pass {
            hex: &a.hex,
            callsign: a.callsign.as_deref(),
            type_code: a.type_code.as_deref().or(airframe.and_then(|f| f.icao_type.as_deref())),
            model: airframe.and_then(|f| f.model.as_deref()),
            alt_ft: a.alt_ft,
            in_s: in_s.round() as u32,
            closest_nm: (closest_nm * 100.0).round() / 100.0,
            route: a.callsign.as_deref().and_then(|c| facts.routes.get(c)).and_then(|r| r.leg_at(a.lat, a.lon, a.track_deg)),
        };
        lines.push(format!("@overhead {}", serde_json::to_string(&pass).unwrap()));
    }
    lines
}

#[derive(Serialize)]
struct Alert<'a> {
    /// `emergency` or `military`
    kind: &'static str,
    hex: &'a str,
    callsign: Option<&'a str>,
    #[serde(rename = "type")]
    type_code: Option<&'a str>,
    model: Option<&'a str>,
    squawk: Option<&'a str>,
    /// For an emergency, what kind, in words
    what: Option<&'static str>,
    /// What spotters say about it, for military and interesting aircraft
    interest: Option<interest::Interest>,
    alt_ft: Option<i32>,
    distance_nm: f64,
    bearing_deg: f64,
}

/// Emergencies, each kind once per aircraft, and military aircraft, once each
fn alerts(store: &Store, facts: &Facts, said: &mut std::collections::HashSet<String>, now: Instant) -> Vec<String> {
    let mut lines = Vec::new();
    for t in store.tracks() {
        let a = &t.aircraft;
        let (kind, what) = match (a.emergency_kind(), a.is_military, a.interest) {
            (Some(what), _, _) => ("emergency", Some(what)),
            (None, true, _) => ("military", None),
            (None, false, Some(_)) => ("interesting", None),
            _ => continue,
        };
        if !said.insert(format!("{}:{kind}:{}", a.hex, what.unwrap_or(""))) {
            continue;
        }
        let airframe = facts.airframes.get(&a.hex);
        let p = t.position(now);
        let alert = Alert {
            kind,
            hex: &a.hex,
            callsign: a.callsign.as_deref(),
            type_code: a.type_code.as_deref().or(airframe.and_then(|f| f.icao_type.as_deref())),
            model: airframe.and_then(|f| f.model.as_deref()),
            squawk: a.squawk.as_deref(),
            what,
            interest: facts.interests.get(&a.hex),
            alt_ft: a.alt_ft,
            distance_nm: (p.distance() * 10.0).round() / 10.0,
            bearing_deg: p.bearing().round(),
        };
        lines.push(format!("@alert {}", serde_json::to_string(&alert).unwrap()));
    }
    lines
}

/// The picked aircraft's track, route on and airports, in the radar's plane
fn picked_view(store: &Store, facts: &Facts, traces: &trace::Traces, selected: Option<&str>, home: Home, now: Instant) -> render::Picked {
    let mut picked = render::Picked::default();
    let Some(track) = selected.and_then(|hex| store.tracks().find(|t| t.aircraft.hex == hex)) else { return picked };
    let a = &track.aircraft;
    picked.trace = traces.fixes.iter().map(|(lat, lon, alt)| (home.project(*lat, *lon), *alt)).collect();
    // The trace ends at the last report; carry it on to where it's drawn now
    picked.trace.push((track.position(now), a.alt_ft));
    let leg = a.callsign.as_deref().and_then(|c| facts.routes.get(c)).and_then(|r| r.leg_at(a.lat, a.lon, a.track_deg));
    if let Some(leg) = leg {
        picked.ahead = geo::great_circle(a.lat, a.lon, leg.to.lat, leg.to.lon, 64).into_iter().map(|(lat, lon)| home.project(lat, lon)).collect();
        picked.from = Some((leg.from.code.clone(), home.project(leg.from.lat, leg.from.lon)));
        picked.to = Some((leg.to.code.clone(), home.project(leg.to.lat, leg.to.lon)));
    }
    picked
}

/// How far back rewind reaches
const HISTORY: Duration = Duration::from_secs(30 * 60);

/// The sky `rewind` ago: a fresh store fed the minute of polls before that
/// moment, so trails and positions come out as they were then. Returns the
/// store and the instant it shows.
fn replay(history: &std::collections::VecDeque<(Instant, Vec<Aircraft>)>, home: Home, rewind: Duration, now: Instant) -> (Store, Instant) {
    let at = now.checked_sub(rewind).unwrap_or(now);
    let mut store = Store::new(home);
    for (when, aircraft) in history.iter().filter(|(when, _)| *when <= at && at.saturating_duration_since(*when) <= Duration::from_secs(70)) {
        store.update(aircraft.clone(), *when);
        store.tick(*when);
    }
    store.settle();
    store.tick(at);
    (store, at)
}

fn run(args: Args) -> Result<(), String> {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_signal as *const () as libc::sighandler_t);
        // A closed stdout shows up as a write error instead of killing us
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }

    let home = args.home;
    let source: Box<dyn Source> = match args.source.strip_prefix("file:") {
        Some(path) => Box::new(FileSource::new(path.to_string())),
        // Ask a little past the drawn range, so arrivals show as they enter
        None => Box::new(AdsbLol::new(home.lat, home.lon, args.range_nm * 1.5, args.poll)),
    };
    let polls = spawn_poller(source);
    let mut store = Store::new(home);
    let mut facts = Facts::new();
    let mut announced = HashMap::new();
    let mut passes_checked = Instant::now();
    let mut radar = Radar::new(args.range_nm);
    let ops = ops::Ops::start();
    let mut ops_said = String::new();
    let mut photos = photo::Photos::new();
    // The photo held for the picked aircraft, and the width its cells were made at
    let mut photo_held: Option<photo::Photo> = None;
    let mut photo_columns = 36usize;
    let basemap = basemap::Basemap::start(home);
    let mut traces = trace::Traces::new();
    let mut said = std::collections::HashSet::new();
    let mut following = false;
    let mut history: std::collections::VecDeque<(Instant, Vec<Aircraft>)> = std::collections::VecDeque::new();
    let mut rewind = Duration::ZERO;

    let mut outline_sent = false;
    let mut pan_reported = Instant::now();
    let mut reported_scale = (0.0, 0.0);
    let mut reported_face = f64::NAN;
    let mut window = window::Window { face_deg: 180.0, fov_deg: window::DEFAULT_FOV_DEG, selected: None, at: args.at, cloud: args.cloud };
    let mut is_window = false;
    let mut face_auto = true;
    let env = env::Env::start(home.lat, home.lon);

    let snapshot = match args.mode {
        Mode::Ansi { columns, rows } => Some(Canvas::cells(columns, rows)),
        Mode::Ppm { width, height } => Some(Canvas::pixels(width, height)),
        _ => None,
    };
    if let Some(mut canvas) = snapshot {
        // A preview waits for the map, which is cached after the first time
        for _ in 0..1800 {
            if basemap.get().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let aircraft = polls.recv().map_err(|e| e.to_string())??;
        let now = Instant::now();
        store.update(aircraft, now);
        if args.view == "window" {
            // Give weather and the skyline a moment to arrive
            std::thread::sleep(Duration::from_secs(3));
            if let Some(deg) = busiest_airport_bearing(&store, &facts, home) {
                window.face_deg = deg;
            }
            window.draw(&mut canvas, &store, &env, (home.lat, home.lon), now);
        } else {
            // The busiest airport's runways, as the live view marks them
            if let Some(map) = basemap.get() {
                summarize(&store, &mut facts, now);
                std::thread::sleep(Duration::from_secs(2));
                facts.collect();
                let code = busiest_airport(&store, &facts, home).map(|(code, _)| code);
                if let Some(report) = ops.report(&map, &store, code.as_deref(), now) {
                    radar.landing = report.landing_lines;
                }
            }
            // A picked aircraft brings its route and its track, given a moment
            radar.selected = args.select.clone();
            if radar.selected.is_some() {
                for _ in 0..100 {
                    traces.follow(radar.selected.as_deref());
                    facts.collect();
                    if !traces.fixes.is_empty() {
                        break;
                    }
                    summarize(&store, &mut facts, now);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            let picked = picked_view(&store, &facts, &traces, radar.selected.as_deref(), home, now);
            radar.draw(&mut canvas, &store, basemap.get().as_deref(), &picked, now);
        }
        if canvas.is_cells() {
            print!("{}", render::ansi(&canvas));
        } else {
            let mut out = std::io::stdout().lock();
            let _ = write!(out, "P6\n{} {}\n255\n", canvas.width, canvas.height);
            let _ = out.write_all(&canvas.rgb);
        }
        return Ok(());
    }

    let mut canvas = match &args.mode {
        Mode::Shm { width, height, .. } => Canvas::pixels(*width, *height),
        Mode::Cells { columns, rows } => Canvas::cells(*columns, *rows),
        Mode::Ansi { .. } | Mode::Ppm { .. } => unreachable!(),
    };
    let mut writer = match &args.mode {
        Mode::Shm { prefix, .. } => Some(shm::FrameWriter::new(prefix.clone())),
        _ => None,
    };

    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "@ready");
    let _ = out.flush();

    let moving = Duration::from_secs_f64(1.0 / args.fps as f64);
    let still = Duration::from_millis(500);
    let mut next_at = Instant::now();
    let mut control = Control::default();
    while !STOP.load(Ordering::SeqCst) {
        let mut lines = Vec::new();
        if let Some(text) = args.input.as_deref().and_then(|path| std::fs::read_to_string(path).ok()) {
            let next = parse_control(&text);
            if next != control {
                if let (Mode::Cells { .. }, Some(columns), Some(rows)) = (&args.mode, next.columns, next.rows) {
                    if (columns, rows) != (canvas.columns(), canvas.height / 2) {
                        canvas = Canvas::cells(columns, rows);
                    }
                }
                if let Some(range) = next.range_nm {
                    radar.range_nm = range;
                }
                if next.click.is_some() && next.click.map(|c| c.0) != control.click.map(|c| c.0) {
                    let (_, x, y) = next.click.unwrap();
                    lines.push(format!("@select {}", canvas.nearest_hit(x, y).unwrap_or("-")));
                }
                PAUSED.store(next.paused == Some(true), Ordering::Relaxed);
                radar.selected = next.select.clone().filter(|hex| hex != "-");
                let (px, py) = next.pan.unwrap_or_default();
                radar.pan = track::Point { x: px, y: py };
                radar.by_altitude = next.colors.as_deref() != Some("status");
                following = next.follow == Some(true);
                rewind = Duration::from_secs(next.rewind.unwrap_or(0).min(HISTORY.as_secs()));
                window.selected = radar.selected.clone();
                is_window = next.view.as_deref() == Some("window");
                window.fov_deg = next.fov.unwrap_or(window::DEFAULT_FOV_DEG);
                match next.face.as_deref().map(str::parse::<f64>) {
                    Some(Ok(deg)) => {
                        window.face_deg = deg.rem_euclid(360.0);
                        face_auto = false;
                    }
                    _ => face_auto = true,
                }
                control = next;
            }
        }
        let paused = PAUSED.load(Ordering::Relaxed);
        let now = Instant::now();
        while let Ok(result) = polls.try_recv() {
            match result {
                Ok(mut aircraft) => {
                    // Spotters' tags, and their air forces as military too
                    for a in &mut aircraft {
                        a.interest = facts.interests.get(&a.hex).map(|i| i.group);
                        a.is_military |= a.interest == Some("military");
                    }
                    history.push_back((now, aircraft.clone()));
                    while history.front().is_some_and(|(when, _)| now.saturating_duration_since(*when) > HISTORY) {
                        history.pop_front();
                    }
                    if let Some((oldest, _)) = history.front() {
                        lines.push(format!("@history {}", now.saturating_duration_since(*oldest).as_secs()));
                    }
                    store.update(aircraft, now);
                    if let Some(map) = basemap.get() {
                        let code = busiest_airport(&store, &facts, home).map(|(code, _)| code);
                        if let Some(report) = ops.report(&map, &store, code.as_deref(), now) {
                            radar.landing = report.landing_lines.clone();
                            let json = serde_json::to_string(&report).unwrap();
                            if json != ops_said {
                                lines.push(format!("@ops {json}"));
                                ops_said = json;
                            }
                        }
                    }
                    if face_auto {
                        if let Some(deg) = busiest_airport_bearing(&store, &facts, home) {
                            window.face_deg = deg;
                        }
                    }
                    lines.push(format!("@aircraft {}", summarize(&store, &mut facts, now)));
                }
                Err(message) => lines.push(format!("@error {message}")),
            }
        }
        // Routes found since the last poll reach the mod without waiting for the next
        if facts.collect() {
            if lines.is_empty() {
                lines.push(format!("@aircraft {}", summarize(&store, &mut facts, now)));
            }
        }
        store.tick(now);
        if now.saturating_duration_since(passes_checked) >= Duration::from_secs(1) {
            passes_checked = now;
            lines.extend(passes(&store, &facts, &mut announced, now));
            lines.extend(alerts(&store, &facts, &mut said, now));
        }
        if traces.follow(radar.selected.as_deref()) {
            // At most a few hundred points: enough for a line on any screen
            let every = (traces.fixes.len() / 300).max(1);
            let points: Vec<(f64, f64, Option<i32>)> = traces
                .fixes
                .iter()
                .step_by(every)
                .map(|(lat, lon, alt)| {
                    let p = home.project(*lat, *lon);
                    ((p.x * 100.0).round() / 100.0, (p.y * 100.0).round() / 100.0, *alt)
                })
                .collect();
            lines.push(format!("@trace {}", serde_json::to_string(&points).unwrap()));
        }
        // The picked aircraft's photo: asked for on a pick, described again if
        // the pane wants it another width
        let wanted_columns = control.photo.unwrap_or(36);
        let fallback = radar.selected.as_deref().and_then(|hex| facts.airframes.get(hex)).and_then(|f| f.photo.clone());
        photos.want(radar.selected.as_deref(), fallback.as_deref());
        if let Some((hex, photo)) = photos.arrived() {
            photo_held = photo;
            photo_columns = wanted_columns;
            lines.push(match &photo_held {
                Some(p) => format!("@photo {}", photo::describe(p, photo_columns)),
                None => format!("@photo {}", serde_json::json!({ "hex": hex, "none": true })),
            });
        } else if let (Some(p), true) = (&photo_held, wanted_columns != photo_columns) {
            photo_columns = wanted_columns;
            lines.push(format!("@photo {}", photo::describe(p, photo_columns)));
        }
        if radar.selected.is_none() {
            photo_held = None;
        }
        if !outline_sent {
            if let Some(map) = basemap.get() {
                outline_sent = true;
                lines.push(format!("@outline {}", map.outline_json()));
            }
        }
        // Follow mode: the picked aircraft stays in the middle, and the mod
        // hears where that's taken the map, so letting go leaves it there
        if following {
            if let Some(t) = radar.selected.as_deref().and_then(|hex| store.tracks().find(|t| t.aircraft.hex == hex)) {
                radar.pan = t.position(now);
                if now.saturating_duration_since(pan_reported) >= Duration::from_secs(1) {
                    pan_reported = now;
                    lines.push(format!("@pan {:.3} {:.3}", radar.pan.x, radar.pan.y));
                }
            }
        }
        let span = radar.span_nm(&canvas);
        if (span.0 - reported_scale.0).abs() > 1e-6 || (span.1 - reported_scale.1).abs() > 1e-6 {
            reported_scale = span;
            lines.push(format!("@scale {:.4} {:.4}", span.0, span.1));
        }
        if (window.face_deg - reported_face).abs() > 0.01 {
            reported_face = window.face_deg;
            lines.push(format!("@face {:.1}", window.face_deg));
        }
        if !paused {
            // Rewound: the same views, of the sky as it was
            let replayed = (!rewind.is_zero()).then(|| replay(&history, home, rewind, now));
            let (sky, at) = match &replayed {
                Some((store, at)) => (store, *at),
                None => (&store, now),
            };
            if is_window {
                window.draw(&mut canvas, sky, &env, (home.lat, home.lon), at);
            } else {
                let picked = picked_view(sky, &facts, &traces, radar.selected.as_deref(), home, at);
                radar.draw(&mut canvas, sky, basemap.get().as_deref(), &picked, at);
            }
            if replayed.is_some() {
                canvas.stamp_rewind(rewind.as_secs());
            }
            lines.push(match writer.as_mut() {
                Some(writer) => match writer.write(&canvas.rgb, canvas.width, canvas.height) {
                    Ok(name) => format!("@frame {name} {} {}", canvas.width, canvas.height),
                    Err(error) => format!("@error {error}"),
                },
                None => format!("@cells {} {} {}", canvas.columns(), canvas.height / 2, render::raster_cells(&canvas)),
            });
        }
        if lines.iter().try_for_each(|line| writeln!(out, "{line}")).and_then(|_| out.flush()).is_err() {
            break;
        }

        // Nothing moving: a couple of frames a second keeps the view alive
        next_at += if store.any_moving() && !paused { moving } else { still };
        let now = Instant::now();
        if next_at > now {
            std::thread::sleep(next_at - now);
        } else {
            next_at = now;
        }
    }

    if let Some(writer) = writer.as_mut() {
        writer.cleanup();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_the_sky_as_it_was() {
        let home = Home { lat: 42.39, lon: -71.10 };
        let plane = |lat: f64| {
            let body = format!(r#"{{"ac":[{{"hex":"abc","lat":{lat},"lon":-71.10,"alt_baro":5000,"gs":0,"track":0}}]}}"#);
            source::parse_readsb(&body).unwrap()
        };
        let t0 = Instant::now();
        let history: std::collections::VecDeque<_> =
            [(t0, plane(42.39)), (t0 + Duration::from_secs(5), plane(42.40)), (t0 + Duration::from_secs(10), plane(42.41))].into();
        let now = t0 + Duration::from_secs(10);
        // Ten seconds back: where it was first seen, a mile below where it is now
        let (store, at) = replay(&history, home, Duration::from_secs(10), now);
        let p = store.tracks().next().unwrap().position(at);
        assert!(p.y.abs() < 0.01, "{p:?}");
        let (store, at) = replay(&history, home, Duration::from_secs(5), now);
        assert!((store.tracks().next().unwrap().position(at).y - 0.6).abs() < 0.01);
    }

    #[test]
    fn reads_a_control_line_with_a_click() {
        let c = parse_control("columns 120 rows 34 click 3 0.42 0.61 range 15 pan -2.5 1 select - view window face 135 colors status follow 1");
        assert_eq!(c.colors.as_deref(), Some("status"));
        assert_eq!(c.follow, Some(true));
        assert_eq!(c.pan, Some((-2.5, 1.0)));
        assert_eq!((c.columns, c.rows, c.range_nm), (Some(120), Some(34), Some(15.0)));
        assert_eq!(c.click, Some((3, 0.42, 0.61)));
        assert_eq!(c.select.as_deref(), Some("-"));
        assert_eq!(c.view.as_deref(), Some("window"));
        assert_eq!(c.face.as_deref(), Some("135"));
    }
}
