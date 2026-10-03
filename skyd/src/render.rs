// The radar view, drawn into a Canvas: RGB pixels, plus, when the frame is
// headed for a Raster, a layer of text cells over them. A Raster cell holds a
// 2×2 block of pixels, drawn as whichever quadrant character (▘▝▖▗▚▞▙▛▜▟…)
// with two colors matches it best: far too coarse for a sprite or a word, so
// aircraft and labels go in as real characters and stay sharp in any terminal.
// Cells are about twice as tall as wide, so those pixels are too: a cell
// canvas's `aspect` (pixel width over height) is 0.5, and shapes allow for it.

use crate::basemap::Map;
use crate::track::{Point, Status, Store, Track};
use base64::Engine;
use std::time::Instant;

pub(crate) const BACKGROUND: u32 = 0x0a0f1a;
const RING: u32 = 0x1f4a3d;
const AXIS: u32 = 0x132230;
const HOME: u32 = 0x4fd1ff;
const RING_LABEL: u32 = 0x3f7a66;
pub(crate) const SELECTED: u32 = 0xfacc15;

pub(crate) fn status_color(status: Status) -> u32 {
    match status {
        Status::Arrival => 0x4ade80,
        Status::Departure => 0xfb923c,
        Status::Cruise => 0xe5e7eb,
        Status::Ground => 0x6b7280,
    }
}

#[derive(Clone, Copy)]
struct TextCell {
    ch: char,
    fg: u32,
    /// None keeps the pixels' own color behind the character
    bg: Option<u32>,
}

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
    /// A pixel's width over its height: 1 for real pixels, 0.5 in cells
    pub aspect: f64,
    columns: usize,
    /// One per cell, `columns` by `height / 2` rows; None for a pixel canvas
    text: Option<Vec<Option<TextCell>>>,
    /// Where each aircraft landed in this frame, by hex, in pixels: what a
    /// click is matched against
    hits: Vec<(String, f64, f64)>,
}

impl Canvas {
    pub fn pixels(width: usize, height: usize) -> Self {
        Self { width, height, rgb: vec![0; width * height * 3], aspect: 1.0, columns: width, text: None, hits: Vec::new() }
    }

    pub fn cells(columns: usize, rows: usize) -> Self {
        Self {
            width: columns * 2,
            height: rows * 2,
            rgb: vec![0; columns * rows * 12],
            aspect: 0.5,
            columns,
            text: Some(vec![None; columns * rows]),
            hits: Vec::new(),
        }
    }

    pub fn is_cells(&self) -> bool {
        self.text.is_some()
    }

    pub(crate) fn rows(&self) -> usize {
        self.height / 2
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    /// The text cell a pixel falls in
    pub(crate) fn cell_of(&self, (x, y): (f64, f64)) -> (i64, i64) {
        ((x / 2.0).floor() as i64, (y / 2.0).floor() as i64)
    }

    /// Every pixel one color, leaving text and hits alone
    pub(crate) fn fill(&mut self, color: u32) {
        let [_, r, g, b] = color.to_be_bytes();
        for px in self.rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&[r, g, b]);
        }
    }

    pub(crate) fn clear(&mut self, color: u32) {
        let [_, r, g, b] = color.to_be_bytes();
        for px in self.rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&[r, g, b]);
        }
        if let Some(text) = &mut self.text {
            text.fill(None);
        }
        self.hits.clear();
    }

    pub(crate) fn hit(&mut self, hex: &str, (x, y): (f64, f64)) {
        self.hits.push((hex.to_string(), x, y));
    }

    /// The aircraft drawn nearest a point given as fractions of the frame,
    /// if one is close enough to have been meant
    pub fn nearest_hit(&self, fx: f64, fy: f64) -> Option<&str> {
        let (x, y) = (fx * self.width as f64, fy * self.height as f64);
        let reach = self.width.max(self.height) as f64 * 0.05;
        self.hits
            .iter()
            .map(|(hex, hx, hy)| (hex, (hx - x).hypot(hy - y)))
            .filter(|(_, d)| *d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(hex, _)| hex.as_str())
    }

    pub(crate) fn get(&self, x: usize, y: usize) -> u32 {
        let i = (y * self.width + x) * 3;
        (self.rgb[i] as u32) << 16 | (self.rgb[i + 1] as u32) << 8 | self.rgb[i + 2] as u32
    }

    /// Blends `color` over the pixel at `alpha` (0..1); off-canvas is ignored
    pub(crate) fn plot(&mut self, x: i64, y: i64, color: u32, alpha: f64) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        let i = (y as usize * self.width + x as usize) * 3;
        let [_, r, g, b] = color.to_be_bytes();
        for (c, v) in [r, g, b].into_iter().enumerate() {
            let old = self.rgb[i + c] as f64;
            self.rgb[i + c] = (old + (v as f64 - old) * alpha.clamp(0.0, 1.0)).round() as u8;
        }
    }

    pub(crate) fn line(&mut self, (x0, y0): (f64, f64), (x1, y1): (f64, f64), color: u32, alpha: f64) {
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0) as i64;
        for s in 0..=steps {
            let t = s as f64 / steps as f64;
            self.plot((x0 + (x1 - x0) * t).round() as i64, (y0 + (y1 - y0) * t).round() as i64, color, alpha);
        }
    }

    /// A ring `radius` pixels tall, as wide as that looks on screen
    pub(crate) fn circle(&mut self, (cx, cy): (f64, f64), radius: f64, color: u32) {
        let steps = (radius * 8.0 / self.aspect).max(32.0) as usize;
        for s in 0..steps {
            let a = s as f64 / steps as f64 * std::f64::consts::TAU;
            let x = cx + radius * a.cos() / self.aspect;
            self.plot(x.round() as i64, (cy + radius * a.sin()).round() as i64, color, 1.0);
        }
    }

    pub(crate) fn disc(&mut self, (cx, cy): (f64, f64), radius: f64, color: u32) {
        let (rx, ry) = ((radius / self.aspect).ceil() as i64, radius.ceil() as i64);
        for dy in -ry..=ry {
            for dx in -rx..=rx {
                let wide = dx as f64 * self.aspect;
                if wide * wide + (dy * dy) as f64 <= radius * radius {
                    self.plot(cx.round() as i64 + dx, cy.round() as i64 + dy, color, 1.0);
                }
            }
        }
    }

    /// Fills a polygon, even-odd, by scanlines through pixel centres
    pub(crate) fn fill_polygon(&mut self, points: &[(f64, f64)], color: u32) {
        if points.len() < 3 {
            return;
        }
        let (y0, y1) = points.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p.1), hi.max(p.1)));
        let (y0, y1) = (y0.max(0.0).floor() as i64, y1.min(self.height as f64 - 1.0).ceil() as i64);
        let mut crossings = Vec::new();
        for y in y0..=y1 {
            let sy = y as f64 + 0.5;
            crossings.clear();
            for i in 0..points.len() {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                if (a.1 <= sy) != (b.1 <= sy) {
                    crossings.push(a.0 + (sy - a.1) / (b.1 - a.1) * (b.0 - a.0));
                }
            }
            crossings.sort_by(f64::total_cmp);
            for pair in crossings.chunks_exact(2) {
                let (from, to) = (pair[0].max(0.0).round() as i64, pair[1].min(self.width as f64).round() as i64);
                for x in from..to {
                    self.plot(x, y, color, 1.0);
                }
            }
        }
    }

    /// Marks a rewound frame: "⏪ 5:00 ago" in the top right corner of a cell
    /// picture, a bar along the top of a pixel one
    pub fn stamp_rewind(&mut self, seconds: u64) {
        let text = format!(" ⏪ {}:{:02} ago ", seconds / 60, seconds % 60);
        if self.is_cells() {
            let start = self.columns as i64 - text.chars().count() as i64 - 1;
            self.put_text_on(start, 0, &text, 0x0a0f1a, Some(0xfacc15));
        } else {
            for x in 0..self.width as i64 {
                for y in 0..4 {
                    self.plot(x, y, 0xfacc15, 0.9);
                }
            }
        }
    }

    pub(crate) fn is_text_free(&self, column: i64, row: i64, len: usize) -> bool {
        let Some(text) = &self.text else { return false };
        if row < 0 || column < 0 || row as usize >= self.rows() || column as usize + len > self.columns {
            return false;
        }
        (0..len).all(|i| text[row as usize * self.columns + column as usize + i].is_none())
    }

    pub(crate) fn put_text(&mut self, column: i64, row: i64, s: &str, color: u32) {
        self.put_text_on(column, row, s, color, None);
    }

    pub(crate) fn put_text_on(&mut self, column: i64, row: i64, s: &str, fg: u32, bg: Option<u32>) {
        let (width, rows) = (self.columns, self.rows());
        let Some(text) = &mut self.text else { return };
        for (i, ch) in s.chars().enumerate() {
            let x = column + i as i64;
            if row >= 0 && x >= 0 && (row as usize) < rows && (x as usize) < width {
                text[row as usize * width + x as usize] = Some(TextCell { ch, fg, bg });
            }
        }
    }
}

/// What the radar shows about the aircraft the person picked, beyond the
/// aircraft itself: where it has flown and where it's going
#[derive(Default)]
pub struct Picked {
    /// Its flight so far, oldest first, with altitude
    pub trace: Vec<(Point, Option<i32>)>,
    /// The rest of its route: a great-circle line on to the destination
    pub ahead: Vec<Point>,
    /// The airports at either end, code and place
    pub from: Option<(String, Point)>,
    pub to: Option<(String, Point)>,
}

pub struct Radar {
    pub range_nm: f64,
    /// The hex of the aircraft the person picked
    pub selected: Option<String>,
    /// Where the middle of the picture is, from home, once the map is dragged
    pub pan: Point,
    /// tar1090's altitude colors, or the status colors (arrival, departure…)
    pub by_altitude: bool,
    /// Runways landing traffic is using, threshold to far end
    pub landing: Vec<(Point, Point)>,
    /// An old ATC scope: green phosphor, a sweep, blips that fade between passes
    pub scope: bool,
    /// Where the scope's sweep points this frame, degrees from north
    sweep_deg: f64,
    /// The map drawn for the current view, kept until the view changes
    layer: Option<(LayerKey, Vec<u8>)>,
}

/// What the map layer depends on
#[derive(PartialEq, Clone, Copy)]
struct LayerKey {
    width: usize,
    height: usize,
    range_milli: i64,
    pan_milli: (i64, i64),
    map: usize,
    scope: bool,
}

/// The colors of the map under the radar, for each look
struct Palette {
    land: u32,
    sea: u32,
    coast: u32,
    runway: u32,
    ring: u32,
    axis: u32,
}

const MAP_PALETTE: Palette = Palette { land: LAND, sea: SEA, coast: COAST, runway: RUNWAY, ring: RING, axis: AXIS };
const SCOPE_PALETTE: Palette = Palette { land: 0x020a05, sea: 0x03110a, coast: 0x1f6b3a, runway: 0x2f8f50, ring: 0x135c2c, axis: 0x0c3a1c };
/// A scope's phosphor: dim between sweeps, near white as the sweep crosses
const PHOSPHOR_DIM: u32 = 0x0f5c2a;
const PHOSPHOR_BRIGHT: u32 = 0xc8ffd8;
/// One turn of the sweep, in seconds
const SWEEP_PERIOD_S: f64 = 4.0;

const LAND: u32 = 0x0d121b;
const SEA: u32 = 0x0a1a2c;
const COAST: u32 = 0x23415c;
const RUNWAY: u32 = 0x5d6b7c;
const AIRPORT: u32 = 0x6f8aa6;
const EMERGENCY: u32 = 0xef4444;
const MILITARY: u32 = 0xc4a5fa;
const INTERESTING: u32 = 0x22d3ee;
const LANDING: u32 = 0x4ade80;
const ROUTE: u32 = 0xb8c4d6;

/// tar1090's altitude scale: hue climbs from orange near the ground through
/// greens and blues to magenta at cruise
pub(crate) fn altitude_color(alt: Option<i32>) -> u32 {
    const STOPS: [(f64, f64); 8] =
        [(0.0, 20.0), (2000.0, 32.5), (4000.0, 43.0), (6000.0, 54.0), (8000.0, 72.0), (9000.0, 85.0), (11000.0, 140.0), (40000.0, 300.0)];
    let Some(alt) = alt else { return 0x6b7280 };
    let a = (alt as f64).clamp(0.0, 40000.0);
    let i = STOPS.windows(2).position(|w| a <= w[1].0).unwrap_or(6);
    let (lo, hi) = (STOPS[i], STOPS[i + 1]);
    let hue = lo.1 + (a - lo.0) / (hi.0 - lo.0) * (hi.1 - lo.1);
    hsl(hue, 0.85, 0.55)
}

fn hsl(h: f64, s: f64, l: f64) -> u32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h / 60.0).rem_euclid(6.0);
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u32;
    f(r) << 16 | f(g) << 8 | f(b)
}

impl Radar {
    pub fn new(range_nm: f64) -> Self {
        Self { range_nm, selected: None, pan: Point::default(), by_altitude: true, landing: Vec::new(), scope: false, sweep_deg: 0.0, layer: None }
    }

    /// Pixels per nm, counted in pixel heights; across, a pixel is narrower
    fn scale(&self, canvas: &Canvas) -> f64 {
        (canvas.height as f64 / 2.0).min(canvas.width as f64 / 2.0 * canvas.aspect) / self.range_nm * 0.95
    }

    /// How many nm the whole picture spans, across and down: what turns a
    /// drag on it into a distance
    pub fn span_nm(&self, canvas: &Canvas) -> (f64, f64) {
        let scale = self.scale(canvas);
        (canvas.width as f64 * canvas.aspect / scale, canvas.height as f64 / scale)
    }

    fn palette(&self) -> &'static Palette {
        if self.scope { &SCOPE_PALETTE } else { &MAP_PALETTE }
    }

    /// How brightly a point glows on the scope: full as the sweep crosses
    /// its bearing, fading over the turn after
    fn glow(&self, p: Point) -> f64 {
        let behind = (self.sweep_deg - p.bearing()).rem_euclid(360.0) / 360.0 * SWEEP_PERIOD_S;
        (-behind / 1.6).exp()
    }

    /// An aircraft's color on this radar: phosphor on the scope, else by
    /// altitude or status
    fn blip_color(&self, track: &Track, p: Point) -> u32 {
        if self.scope {
            return mix_colors(PHOSPHOR_DIM, PHOSPHOR_BRIGHT, self.glow(p));
        }
        self.color_of(track, track.aircraft.alt_ft)
    }

    fn color_of(&self, track: &Track, alt: Option<i32>) -> u32 {
        if self.by_altitude && track.status() != Status::Ground { altitude_color(alt) } else { status_color(track.status()) }
    }

    /// Land, sea, lakes, coastline and runways for this view: slow enough to
    /// keep until the view changes, which is every frame only while dragging
    fn paint_map(&mut self, canvas: &mut Canvas, map: Option<&Map>, to_px: &dyn Fn(Point) -> (f64, f64), from_px: &dyn Fn(f64, f64) -> Point, scale: f64) {
        let key = LayerKey {
            width: canvas.width,
            height: canvas.height,
            range_milli: (self.range_nm * 1000.0) as i64,
            pan_milli: ((self.pan.x * 1000.0) as i64, (self.pan.y * 1000.0) as i64),
            map: map.map(|m| m as *const Map as usize).unwrap_or(0),
            scope: self.scope,
        };
        let palette = self.palette();
        if let Some((k, rgb)) = &self.layer {
            if *k == key {
                canvas.rgb.copy_from_slice(rgb);
                return;
            }
        }
        canvas.fill(palette.land);
        if let Some(map) = map {
            if map.has_coast() {
                paint_sea(canvas, map, to_px, from_px, palette.sea);
            }
            for ring in &map.water {
                let points: Vec<(f64, f64)> = ring.iter().map(|p| to_px(*p)).collect();
                canvas.fill_polygon(&points, palette.sea);
            }
            for line in &map.coast {
                for pair in line.windows(2) {
                    canvas.line(to_px(pair[0]), to_px(pair[1]), palette.coast, 1.0);
                }
            }
            for (a, b, width_m) in &map.runways {
                // As wide as the runway really is, at least a pixel
                let half = (width_m / 1852.0 * scale / 2.0).max(0.5);
                let (pa, pb) = (to_px(*a), to_px(*b));
                let (dx, dy) = (pb.0 - pa.0, pb.1 - pa.1);
                let len = dx.hypot(dy).max(1e-9);
                let (nx, ny) = (-dy / len / canvas.aspect, dx / len);
                let steps = (half * 2.0).ceil().max(1.0) as i64;
                for i in 0..=steps {
                    let o = -half + i as f64 * (2.0 * half / steps as f64);
                    canvas.line((pa.0 + nx * o, pa.1 + ny * o), (pb.0 + nx * o, pb.1 + ny * o), palette.runway, 1.0);
                }
            }
        }
        self.layer = Some((key, canvas.rgb.clone()));
    }

    pub fn draw(&mut self, canvas: &mut Canvas, store: &Store, map: Option<&Map>, picked: &Picked, now: Instant) {
        canvas.clear(BACKGROUND);
        self.sweep_deg = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() % SWEEP_PERIOD_S / SWEEP_PERIOD_S * 360.0)
            .unwrap_or(0.0);
        let aspect = canvas.aspect;
        let scale = self.scale(canvas);
        let (w, h) = (canvas.width as f64, canvas.height as f64);
        // Home on the picture: the middle, less however far the map was dragged
        let (hx, hy) = (w / 2.0 - self.pan.x * scale / aspect, h / 2.0 + self.pan.y * scale);
        let to_px = move |p: Point| (hx + p.x * scale / aspect, hy - p.y * scale);
        let from_px = move |x: f64, y: f64| Point { x: (x - hx) * aspect / scale, y: (hy - y) / scale };
        self.paint_map(canvas, map, &to_px, &from_px, scale);

        // Rings out to the corner furthest from home
        let reach = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)]
            .iter()
            .map(|(x, y)| ((x - hx) * aspect).hypot(y - hy) / scale)
            .fold(0.0, f64::max);
        let palette = self.palette();
        canvas.line((0.0, hy), (w, hy), palette.axis, 0.6);
        canvas.line((hx, 0.0), (hx, h), palette.axis, 0.6);
        let step = ring_step(self.range_nm);
        let mut ring = step;
        while ring < reach {
            canvas.circle((hx, hy), ring * scale, palette.ring);
            ring += step;
        }

        // The sweep, with a fading wake behind it
        if self.scope {
            let radius = reach * scale;
            // Dense enough near the edge that the wake reads as one glow
            let steps = ((radius * 0.42).ceil() as usize).clamp(16, 160);
            for i in 0..steps {
                let t = i as f64 / steps as f64;
                let b = (self.sweep_deg - t * 24.0).to_radians();
                let end = (hx + b.sin() * radius / aspect, hy - b.cos() * radius);
                // Lines overlap near the centre, so each adds only a little
                canvas.line((hx, hy), end, 0x3cff7a, 0.07 * (1.0 - t).powf(1.5));
            }
            let b = self.sweep_deg.to_radians();
            canvas.line((hx, hy), (hx + b.sin() * radius / aspect, hy - b.cos() * radius), 0x8cffb0, 0.85);
        }

        // Runways in use for landing, green, with the approach dashed out 4 nm
        // from the threshold: where the arrivals line up
        for (threshold, far) in &self.landing {
            let (dx, dy) = (far.x - threshold.x, far.y - threshold.y);
            let len = dx.hypot(dy).max(1e-9);
            canvas.line(to_px(*threshold), to_px(*far), LANDING, 1.0);
            for i in 0..8 {
                if i % 2 == 0 {
                    let at = |k: f64| Point { x: threshold.x - dx / len * k, y: threshold.y - dy / len * k };
                    canvas.line(to_px(at(i as f64 * 0.5)), to_px(at(i as f64 * 0.5 + 0.5)), LANDING, 0.6);
                }
            }
        }

        // The picked aircraft's flight so far, colored by height, and the rest
        // of its route dashed on to the destination
        for pair in picked.trace.windows(2) {
            let color = if self.by_altitude { altitude_color(pair[1].1) } else { ROUTE };
            canvas.line(to_px(pair[0].0), to_px(pair[1].0), color, 0.8);
        }
        for (i, pair) in picked.ahead.windows(2).enumerate() {
            if i % 2 == 0 {
                canvas.line(to_px(pair[0]), to_px(pair[1]), ROUTE, 0.7);
            }
        }
        for (_, at) in picked.from.iter().chain(picked.to.iter()) {
            canvas.circle(to_px(*at), if canvas.is_cells() { 1.5 } else { 6.0 }, ROUTE);
        }

        let mut tracks: Vec<(&Track, Point)> = store.tracks().map(|t| (t, t.position(now))).collect();
        // Nearest drawn last, so it lands on top and claims label space first
        tracks.sort_by(|a, b| b.1.distance().total_cmp(&a.1.distance()));

        for (track, _) in &tracks {
            let n = track.trail.len();
            for (i, pair) in track.trail.iter().collect::<Vec<_>>().windows(2).enumerate() {
                let alpha = 0.15 + 0.45 * (i as f64 / n.max(1) as f64);
                // On the scope a trail is the phosphor's afterglow
                let color = if self.scope { PHOSPHOR_DIM } else { self.color_of(track, pair[1].2) };
                canvas.line(to_px(pair[0].1), to_px(pair[1].1), color, alpha);
            }
        }

        let flash = flash_on();
        for (track, p) in &tracks {
            let a = &track.aircraft;
            let color = self.blip_color(track, *p);
            canvas.hit(&a.hex, to_px(*p));
            // A leader line: where it will be in a minute
            if let (Some(heading), false) = (a.track_deg, a.is_on_ground()) {
                let nm = a.gs_kt / 60.0;
                let r = heading.to_radians();
                let ahead = Point { x: p.x + nm * r.sin(), y: p.y + nm * r.cos() };
                canvas.line(to_px(*p), to_px(ahead), color, 0.45);
            }
            if !canvas.is_cells() {
                canvas.disc(to_px(*p), 2.5, color);
            }
            if a.emergency_kind().is_some() {
                canvas.circle(to_px(*p), if canvas.is_cells() { 3.0 } else { 12.0 }, if flash { EMERGENCY } else { 0x7f1d1d });
            }
            if self.selected.as_deref() == Some(a.hex.as_str()) {
                canvas.circle(to_px(*p), if canvas.is_cells() { 2.5 } else { 9.0 }, SELECTED);
            }
        }

        canvas.disc((hx, hy), if canvas.is_cells() { 0.6 } else { 3.0 }, if self.scope { PHOSPHOR_BRIGHT } else { HOME });

        if canvas.is_cells() {
            self.label(canvas, (hx, hy), scale, reach, &tracks, map, picked, flash);
        } else if !self.scope {
            self.legend_bar(canvas);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn label(
        &self,
        canvas: &mut Canvas,
        (cx, cy): (f64, f64),
        scale: f64,
        reach: f64,
        tracks: &[(&Track, Point)],
        map: Option<&Map>,
        picked: &Picked,
        flash: bool,
    ) {
        let aspect = canvas.aspect;
        let cell = |canvas: &Canvas, p: Point| canvas.cell_of((cx + p.x * scale / aspect, cy - p.y * scale));

        // Aircraft glyphs first, nearest last so it wins a shared cell
        for (track, p) in tracks {
            let (column, row) = cell(canvas, *p);
            let color = if track.aircraft.emergency_kind().is_some() { EMERGENCY } else { self.blip_color(track, *p) };
            canvas.put_text(column, row, &glyph(track).to_string(), color);
        }
        let (hc, hr) = canvas.cell_of((cx, cy));
        canvas.put_text(hc, hr, "⌂", HOME);
        canvas.put_text(canvas.columns() as i64 / 2, 0, "N", RING_LABEL);

        // Airports before aircraft labels, so a busy one keeps its name:
        // below it, else above, else beside. The picked route's two always.
        for (code, at) in picked.from.iter().chain(picked.to.iter()) {
            let (column, row) = cell(canvas, *at);
            canvas.put_text_on(column - code.len() as i64 / 2, row + 1, code, BACKGROUND, Some(ROUTE));
        }
        if let Some(map) = map {
            for (code, at) in &map.airports {
                let (column, row) = cell(canvas, *at);
                let len = code.len() as i64;
                let spots = [(column - len / 2, row + 1), (column - len / 2, row - 1), (column + 1, row), (column - len - 1, row)];
                if let Some((x, y)) = spots.into_iter().find(|(x, y)| canvas.is_text_free(*x, *y, code.len())) {
                    canvas.put_text(x, y, code, AIRPORT);
                }
            }
        }

        // Emergencies first, then the picked aircraft, so both always get
        // their label, then the rest nearest first. Ground traffic at a busy
        // airport is a cluster of names nobody reads, unless it's picked.
        let is_selected = |t: &Track| self.selected.as_deref() == Some(t.aircraft.hex.as_str());
        let urgent = tracks.iter().filter(|(t, _)| t.aircraft.emergency_kind().is_some());
        let picked_one = tracks.iter().filter(|(t, _)| is_selected(t) && t.aircraft.emergency_kind().is_none());
        let rest = tracks
            .iter()
            .rev()
            .filter(|(t, _)| !is_selected(t) && t.aircraft.emergency_kind().is_none() && t.status() != Status::Ground);
        for (track, p) in urgent.chain(picked_one).chain(rest) {
            let a = &track.aircraft;
            let (column, row) = cell(canvas, *p);
            let mut text = label_text(track);
            if let Some(kind) = a.emergency_kind() {
                text = format!("{text} {}", a.squawk.as_deref().filter(|s| s.starts_with("75") || s.starts_with("76") || s.starts_with("77")).unwrap_or(kind));
            } else if a.is_military {
                text.push_str(" MIL");
            } else if let Some(group) = a.interest {
                text.push_str(&format!(" ★{group}"));
            } else if a.is_rotorcraft {
                text.push_str(" heli");
            }
            let len = text.chars().count() as i64;
            let start_right = if column + 1 + len <= canvas.columns() as i64 { column + 1 } else { column - len - 1 };
            if a.emergency_kind().is_some() {
                let (fg, bg) = if flash { (0xffffff, EMERGENCY) } else { (EMERGENCY, 0x2a0a0a) };
                canvas.put_text_on(start_right, row, &text, fg, Some(bg));
                continue;
            }
            if is_selected(track) {
                canvas.put_text_on(start_right, row, &text, BACKGROUND, Some(SELECTED));
                canvas.put_text(column, row, &glyph(track).to_string(), SELECTED);
                continue;
            }
            let color = if self.scope {
                dim(self.blip_color(track, *p), 0.9)
            } else if a.is_military {
                MILITARY
            } else if a.interest.is_some() {
                INTERESTING
            } else {
                dim(self.color_of(track, a.alt_ft), 0.85)
            };
            for start in [column + 1, column - len - 1] {
                if canvas.is_text_free(start, row, len as usize) {
                    canvas.put_text(start, row, &text, color);
                    break;
                }
            }
        }


        let step = ring_step(self.range_nm);
        let mut ring = step;
        while ring < reach {
            let text = format!("{ring:.0}");
            let (column, row) = canvas.cell_of((cx + 2.0, cy - ring * scale));
            if canvas.is_text_free(column, row, text.len()) {
                canvas.put_text(column, row, &text, RING_LABEL);
            }
            ring += step;
        }

        if self.by_altitude && !self.scope {
            self.legend_text(canvas);
        }
    }

    /// The altitude scale along the bottom left, each figure in its color
    fn legend_text(&self, canvas: &mut Canvas) {
        let row = canvas.rows() as i64 - 1;
        let mut column = 1i64;
        for (label, alt) in [("0", 0), ("2", 2000), ("5", 5000), ("10", 10000), ("20", 20000), ("30", 30000), ("40k", 40000)] {
            if canvas.is_text_free(column, row, label.len() + 1) {
                canvas.put_text(column, row, label, altitude_color(Some(alt)));
            }
            column += label.len() as i64 + 1;
        }
    }

    /// The altitude scale as a bar along the bottom left of a pixel frame
    fn legend_bar(&self, canvas: &mut Canvas) {
        if !self.by_altitude {
            return;
        }
        let (x0, y0, width) = (12i64, canvas.height as i64 - 14, 160i64);
        for i in 0..width {
            let alt = (i as f64 / width as f64 * 40000.0) as i32;
            for dy in 0..6 {
                canvas.plot(x0 + i, y0 + dy, altitude_color(Some(alt)), 1.0);
            }
        }
    }
}

/// Shades the sea. The coastline is drawn as walls on a grid of samples (a
/// pixel in cells, a 2×2 block on a big frame), the grid is flood-filled
/// into regions, and each region is sea or land by a vote of the side test
/// taken only right beside the coast, where it's reliable. OSM coastline has
/// spikes (a line out to a point and straight back) and a few loose ends, and
/// the side test alone sweeps wedges of wrong color out from them; here
/// they're a few outvoted samples.
fn paint_sea(canvas: &mut Canvas, map: &Map, to_px: &dyn Fn(Point) -> (f64, f64), from_px: &dyn Fn(f64, f64) -> Point, sea: u32) {
    let stride = if canvas.is_cells() { 1 } else { 2 };
    let (gw, gh) = (canvas.width.div_ceil(stride), canvas.height.div_ceil(stride));
    let mut wall = vec![false; gw * gh];
    for line in &map.coast {
        for pair in line.windows(2) {
            let (a, b) = (to_px(pair[0]), to_px(pair[1]));
            let (a, b) = ((a.0 / stride as f64, a.1 / stride as f64), (b.0 / stride as f64, b.1 / stride as f64));
            let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil().max(1.0) as usize;
            // Off the grid entirely: nothing to wall
            if steps > 4 * (gw + gh) && !(0.0..gw as f64).contains(&a.0) && !(0.0..gw as f64).contains(&b.0) {
                continue;
            }
            for i in 0..=steps {
                let t = i as f64 / steps as f64;
                let (x, y) = ((a.0 + (b.0 - a.0) * t).floor(), (a.1 + (b.1 - a.1) * t).floor());
                if x >= 0.0 && y >= 0.0 && (x as usize) < gw && (y as usize) < gh {
                    wall[y as usize * gw + x as usize] = true;
                }
            }
        }
    }
    let center = |i: usize| from_px(((i % gw) * stride) as f64 + stride as f64 / 2.0, ((i / gw) * stride) as f64 + stride as f64 / 2.0);
    // Regions, four-connected, so a diagonal step of a wall still holds
    let mut region = vec![usize::MAX; gw * gh];
    let mut is_sea_region = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    for start in 0..gw * gh {
        if wall[start] || region[start] != usize::MAX {
            continue;
        }
        let id = is_sea_region.len();
        let (mut sea, mut land, mut seen) = (0u32, 0u32, 0u32);
        region[start] = id;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            let (x, y) = (i % gw, i / gw);
            let beside_wall = [(-1i64, 0i64), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)].iter().any(|(dx, dy)| {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                nx >= 0 && ny >= 0 && (nx as usize) < gw && (ny as usize) < gh && wall[ny as usize * gw + nx as usize]
            });
            // Votes from beside the coast; a few hundred settle any region
            if beside_wall {
                seen += 1;
                if seen % 3 == 0 && sea + land < 400 {
                    if map.is_sea(center(i)) { sea += 1 } else { land += 1 }
                }
            }
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < gw && ny < gh {
                    let n = ny * gw + nx;
                    if !wall[n] && region[n] == usize::MAX {
                        region[n] = id;
                        queue.push_back(n);
                    }
                }
            }
        }
        // No coast beside it: the side test from its first sample decides
        is_sea_region.push(if sea + land == 0 { map.is_sea(center(start)) } else { sea > land });
    }
    for i in 0..gw * gh {
        // A wall cell takes the color of a region beside it; the coastline is drawn over it
        let id = if wall[i] {
            let (x, y) = (i % gw, i / gw);
            [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)]
                .iter()
                .filter(|(nx, ny)| *nx < gw && *ny < gh)
                .map(|(nx, ny)| region[ny * gw + nx])
                .find(|r| *r != usize::MAX)
        } else {
            Some(region[i])
        };
        if id.is_some_and(|id| is_sea_region[id]) {
            let (x, y) = ((i % gw) * stride, (i / gw) * stride);
            for dy in 0..stride {
                for dx in 0..stride {
                    canvas.plot((x + dx) as i64, (y + dy) as i64, sea, 1.0);
                }
            }
        }
    }
}

fn mix_colors(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xff) as f64, ((b >> shift) & 0xff) as f64);
        ((x + (y - x) * t).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Range rings a readable distance apart for the zoom: about four across
fn ring_step(range_nm: f64) -> f64 {
    match range_nm {
        r if r <= 6.0 => 2.0,
        r if r <= 20.0 => 5.0,
        r if r <= 50.0 => 10.0,
        _ => 20.0,
    }
}

/// Emergencies flash, twice a second
fn flash_on() -> bool {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() % 1000 < 500).unwrap_or(true)
}

pub(crate) fn glyph(track: &Track) -> char {
    const ARROWS: [char; 8] = ['↑', '↗', '→', '↘', '↓', '↙', '←', '↖'];
    match (track.status(), track.aircraft.track_deg) {
        (Status::Ground, _) => '·',
        (_, Some(t)) => ARROWS[((t / 45.0).round() as usize) % 8],
        (_, None) => '+',
    }
}

/// `RPA4726 107`: callsign, then altitude in hundreds of feet
pub(crate) fn label_text(track: &Track) -> String {
    let a = &track.aircraft;
    let name = a.callsign.as_deref().or(a.registration.as_deref()).unwrap_or(&a.hex);
    match a.alt_ft {
        Some(alt) => format!("{name} {:03}", (alt.max(0) + 50) / 100),
        None => name.to_string(),
    }
}

pub(crate) fn dim(color: u32, k: f64) -> u32 {
    let [_, r, g, b] = color.to_be_bytes();
    let f = |v: u8| ((v as f64) * k).round() as u32;
    f(r) << 16 | f(g) << 8 | f(b)
}

/// The quadrant characters by which of a cell's four pixels the foreground
/// paints: bit 0 upper left, 1 upper right, 2 lower left, 3 lower right
const QUADRANTS: [char; 16] = [' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█'];

fn mean(colors: &[u32]) -> u32 {
    if colors.is_empty() {
        return 0;
    }
    let n = colors.len() as u32;
    let channel = |shift: u32| colors.iter().map(|c| (c >> shift) & 0xff).sum::<u32>() / n;
    channel(16) << 16 | channel(8) << 8 | channel(0)
}

fn distance2(a: u32, b: u32) -> i64 {
    let d = |shift: u32| ((a >> shift) & 0xff) as i64 - ((b >> shift) & 0xff) as i64;
    d(16) * d(16) + d(8) * d(8) + d(0) * d(0)
}

/// The quadrant character and two colors that best stand for four pixels:
/// each split of the four into foreground and background, colored by their
/// means, scored by how far each pixel ends up from its color
fn best_quadrant(px: [u32; 4]) -> (u32, u32, u32) {
    if px.iter().all(|c| *c == px[0]) {
        return (' ' as u32, px[0], px[0]);
    }
    let mut best = (i64::MAX, 15usize, px[0], px[0]);
    // Masks and their complements are the same split, colors swapped
    for mask in 1..8usize {
        let (fg, bg): (Vec<u32>, Vec<u32>) = (0..4).map(|i| (mask >> i & 1 == 1, px[i])).fold((vec![], vec![]), |(mut f, mut b), (on, c)| {
            if on { f.push(c) } else { b.push(c) }
            (f, b)
        });
        let (fc, bc) = (mean(&fg), mean(&bg));
        let error: i64 = fg.iter().map(|c| distance2(*c, fc)).sum::<i64>() + bg.iter().map(|c| distance2(*c, bc)).sum::<i64>();
        if error < best.0 {
            best = (error, mask, fc, bc);
        }
    }
    (QUADRANTS[best.1] as u32, best.2, best.3)
}

/// Each cell as `(character, foreground, background)`: a text cell over the
/// mean of its four pixels, otherwise the quadrant that fits them best
fn cell_words(canvas: &Canvas) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
    let text = canvas.text.as_ref().expect("a cell canvas");
    (0..canvas.rows()).flat_map(move |row| {
        (0..canvas.columns).map(move |column| {
            let (x, y) = (column * 2, row * 2);
            let px = [canvas.get(x, y), canvas.get(x + 1, y), canvas.get(x, y + 1), canvas.get(x + 1, y + 1)];
            match text[row * canvas.columns + column] {
                Some(TextCell { ch, fg, bg }) => (ch as u32, fg, bg.unwrap_or_else(|| mean(&px))),
                None => best_quadrant(px),
            }
        })
    })
}

/// A Raster's `cells`: base64 of little-endian u32 triplets, row-major
pub fn raster_cells(canvas: &Canvas) -> String {
    let mut bytes = Vec::with_capacity(canvas.columns * canvas.rows() * 12);
    for (ch, fg, bg) in cell_words(canvas) {
        for word in [ch, fg, bg] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The same cells as truecolor escapes, to look at a frame in any terminal
pub fn ansi(canvas: &Canvas) -> String {
    let mut out = String::new();
    for (i, (ch, fg, bg)) in cell_words(canvas).enumerate() {
        if i > 0 && i % canvas.columns == 0 {
            out.push_str("\x1b[0m\n");
        }
        let [_, fr, fgc, fb] = fg.to_be_bytes();
        let [_, br, bgc, bb] = bg.to_be_bytes();
        out.push_str(&format!("\x1b[38;2;{fr};{fgc};{fb};48;2;{br};{bgc};{bb}m{}", char::from_u32(ch).unwrap_or('?')));
    }
    out.push_str("\x1b[0m\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadrants_pick_the_shape_of_the_pixels() {
        let (on, off) = (0xffffff, 0x000000);
        // Left column lit: ▌ with white over black
        assert_eq!(best_quadrant([on, off, on, off]), ('▌' as u32, on, off));
        // Any two-color block comes back exactly, whichever of a split and its
        // complement was picked
        for bits in 0..16usize {
            let px: [u32; 4] = std::array::from_fn(|i| if bits >> i & 1 == 1 { on } else { off });
            let (ch, fg, bg) = best_quadrant(px);
            let mask = QUADRANTS.iter().position(|q| *q as u32 == ch).unwrap();
            let drawn: [u32; 4] = std::array::from_fn(|i| if mask >> i & 1 == 1 { fg } else { bg });
            assert_eq!(drawn, px, "bits {bits:04b}");
        }
        // Flat color: a blank cell on that background
        assert_eq!(best_quadrant([on, on, on, on]), (' ' as u32, on, on));
    }

    #[test]
    fn a_cell_canvas_has_two_by_two_pixels_per_cell() {
        let canvas = Canvas::cells(10, 4);
        assert_eq!((canvas.width, canvas.height, canvas.columns()), (20, 8, 10));
        assert_eq!(canvas.cell_of((5.0, 7.0)), (2, 3));
    }
}
