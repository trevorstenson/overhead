// The whole of the picked aircraft's flight so far, from adsb.lol's trace
// files: every position it has reported today, cut back to this flight.
// Fetched on its own thread when the pick changes, and again every minute
// while it stays picked.

use crate::source::http_agent;
use serde_json::Value;
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// One position: latitude, longitude, altitude (None on the ground)
pub type Fix = (f64, f64, Option<i32>);

/// A gap longer than this between positions starts a new flight
const NEW_FLIGHT_GAP_S: f64 = 30.0 * 60.0;
const REFRESH: Duration = Duration::from_secs(60);

pub struct Traces {
    asks: mpsc::Sender<String>,
    answers: mpsc::Receiver<(String, Vec<Fix>)>,
    /// The hex whose trace is held, and when it was asked for
    current: Option<(String, Instant)>,
    pub fixes: Vec<Fix>,
}

impl Traces {
    pub fn new() -> Self {
        let (asks, asked) = mpsc::channel::<String>();
        let (answer, answers) = mpsc::channel();
        std::thread::spawn(move || {
            for hex in asked {
                let fixes = fetch(&hex).unwrap_or_default();
                if answer.send((hex, fixes)).is_err() {
                    return;
                }
            }
        });
        Self { asks, answers, current: None, fixes: Vec::new() }
    }

    /// Follows the pick: asks for a new one's trace, refreshes a held one,
    /// drops it when nothing is picked. True when the held trace changed.
    pub fn follow(&mut self, picked: Option<&str>) -> bool {
        let mut changed = false;
        while let Ok((hex, fixes)) = self.answers.try_recv() {
            if self.current.as_ref().is_some_and(|(h, _)| *h == hex) {
                self.fixes = fixes;
                changed = true;
            }
        }
        match (picked, &self.current) {
            (None, Some(_)) => {
                self.current = None;
                self.fixes.clear();
                changed = true;
            }
            (Some(hex), current) if current.as_ref().is_none_or(|(h, at)| h != hex || at.elapsed() >= REFRESH) => {
                if current.as_ref().is_some_and(|(h, _)| h != hex) {
                    self.fixes.clear();
                    changed = true;
                }
                self.current = Some((hex.to_string(), Instant::now()));
                let _ = self.asks.send(hex.to_string());
            }
            _ => {}
        }
        changed
    }
}

fn fetch(hex: &str) -> Option<Vec<Fix>> {
    let hex = hex.to_lowercase();
    let url = format!("https://globe.adsb.lol/data/traces/{}/trace_full_{hex}.json", &hex[hex.len().saturating_sub(2)..]);
    let mut bytes = Vec::new();
    http_agent().get(&url).call().ok()?.body_mut().as_reader().read_to_end(&mut bytes).ok()?;
    // Served as a gzip file, whether or not the server says so
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut plain = Vec::new();
        flate2::read::GzDecoder::new(&bytes[..]).read_to_end(&mut plain).ok()?;
        bytes = plain;
    }
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    Some(this_flight(&v))
}

/// The positions since the aircraft last took off: after its last time on
/// the ground, a long gap, or a point readsb marks as starting a new leg
pub fn this_flight(v: &Value) -> Vec<Fix> {
    let Some(points) = v["trace"].as_array() else { return Vec::new() };
    let mut start = 0;
    let mut last_time = f64::NAN;
    for (i, p) in points.iter().enumerate() {
        let time = p[0].as_f64().unwrap_or(0.0);
        let on_ground = p[3].as_str() == Some("ground");
        let new_leg = p[6].as_u64().is_some_and(|flags| flags & 2 != 0);
        if on_ground || new_leg || time - last_time > NEW_FLIGHT_GAP_S {
            start = i;
        }
        last_time = time;
    }
    points[start..]
        .iter()
        .filter_map(|p| Some((p[1].as_f64()?, p[2].as_f64()?, p[3].as_f64().map(|a| a as i32))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_this_flight() {
        let v: Value = serde_json::from_str(
            r#"{"trace":[
                [0, 32.9, -97.0, "ground", 5, null, 0],
                [600, 33.0, -96.9, 9000, 300, 45, 0],
                [9000, 40.0, -75.0, 30000, 450, 60, 0],
                [9100, 40.1, -74.9, "ground", 10, null, 0],
                [9700, 40.2, -74.8, 2000, 160, 50, 0],
                [10000, 40.4, -74.5, 6000, 250, 50, 0]
            ]}"#,
        )
        .unwrap();
        let flight = this_flight(&v);
        // From the last time on the ground at Newark onward
        assert_eq!(flight.len(), 3);
        assert_eq!(flight[0].2, None);
        assert_eq!(flight[2].2, Some(6000));
    }

    #[test]
    fn a_long_gap_starts_a_new_flight() {
        let v: Value = serde_json::from_str(
            r#"{"trace":[[0, 1, 1, 30000, 400, 0, 0], [100, 1.1, 1, 30000, 400, 0, 0], [5000, 2, 2, 8000, 300, 0, 0]]}"#,
        )
        .unwrap();
        assert_eq!(this_flight(&v).len(), 1);
    }
}
