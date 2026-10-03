// Where aircraft positions come from. Every feed normalizes into `Aircraft`,
// so the track store and renderer never see a feed's own field names.

use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Aircraft {
    pub hex: String,
    pub callsign: Option<String>,
    pub type_code: Option<String>,
    pub registration: Option<String>,
    pub lat: f64,
    pub lon: f64,
    /// Barometric altitude; None on the ground
    pub alt_ft: Option<i32>,
    pub gs_kt: f64,
    pub track_deg: Option<f64>,
    pub vrate_fpm: i32,
    /// How old the position was when the feed answered
    pub seen_pos_s: f64,
    pub squawk: Option<String>,
    /// What the transponder declares: `general`, `lifeguard`, `minfuel`,
    /// `nordo`, `unlawful`, `downed`; None for no emergency
    pub emergency: Option<String>,
    /// The feed's database marks it military
    pub is_military: bool,
    /// Emitter category A7: a helicopter or gyrocopter
    pub is_rotorcraft: bool,
    /// plane-alert-db's group for it, when spotters have tagged it
    pub interest: Option<&'static str>,
}

impl Aircraft {
    pub fn is_on_ground(&self) -> bool {
        self.alt_ft.is_none()
    }

    /// The emergency it's in, in words, from what it declares or squawks
    pub fn emergency_kind(&self) -> Option<&'static str> {
        let declared = match self.emergency.as_deref() {
            Some("unlawful") => Some("hijack"),
            Some("nordo") => Some("radio failure"),
            Some("minfuel") => Some("minimum fuel"),
            Some("lifeguard") => Some("medical"),
            Some("downed") => Some("downed"),
            Some(_) => Some("emergency"),
            None => None,
        };
        declared.or(match self.squawk.as_deref() {
            Some("7500") => Some("hijack"),
            Some("7600") => Some("radio failure"),
            Some("7700") => Some("emergency"),
            _ => None,
        })
    }
}

pub trait Source: Send {
    fn fetch(&mut self) -> Result<Vec<Aircraft>, String>;
    /// How long to wait between successful polls
    fn interval(&self) -> Duration;
    /// After a failed fetch: how long the server asked us to wait, if it did
    fn retry_after(&self) -> Option<Duration> {
        None
    }
}

/// adsb.lol's keyless point-radius query. Every session on the machine
/// running the same query shares one answer: whoever asks writes it to the
/// cache, and the rest read it there while it's fresh, so three open sessions
/// poll no more than one.
pub struct AdsbLol {
    agent: ureq::Agent,
    url: String,
    interval: Duration,
    shared: Option<std::path::PathBuf>,
    /// What the last refusal's Retry-After asked for
    retry_after: Option<Duration>,
}

/// One HTTP setup for every request skyd makes
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(concat!("overhead-skyd/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

impl AdsbLol {
    pub fn new(lat: f64, lon: f64, radius_nm: f64, interval: Duration) -> Self {
        // Statuses come back as answers, so a 429's Retry-After can be read
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent(concat!("overhead-skyd/", env!("CARGO_PKG_VERSION")))
            .http_status_as_error(false)
            .build()
            .into();
        let radius = radius_nm.clamp(1.0, 250.0).round();
        let url = format!("https://api.adsb.lol/v2/point/{lat:.4}/{lon:.4}/{radius}");
        let shared = crate::cache::cache_dir().map(|d| d.join(format!("feed-{lat:.4}-{lon:.4}-{radius}.json")));
        Self { agent, url, interval, shared, retry_after: None }
    }
}

impl AdsbLol {
    /// Another session's answer, if it's newer than most of an interval,
    /// and how old it is
    fn shared_answer(&self) -> Option<(String, Duration)> {
        let path = self.shared.as_ref()?;
        let age = std::fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()?;
        (age < self.interval.mul_f64(0.8)).then(|| std::fs::read_to_string(path).ok().map(|body| (body, age))).flatten()
    }

    fn share(&self, body: &str) {
        let Some(path) = &self.shared else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Whole or not at all, so a reader never sees half an answer
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

impl Source for AdsbLol {
    fn fetch(&mut self) -> Result<Vec<Aircraft>, String> {
        if let Some((body, age)) = self.shared_answer() {
            if let Ok(mut aircraft) = parse_readsb(&body) {
                // Its positions were already this much older when we read them
                for a in &mut aircraft {
                    a.seen_pos_s += age.as_secs_f64();
                }
                return Ok(aircraft);
            }
        }
        self.retry_after = None;
        let mut response = self.agent.get(&self.url).call().map_err(|e| format!("couldn't reach adsb.lol: {e}"))?;
        let status = response.status().as_u16();
        if status == 429 || status == 503 {
            // Asked to slow down: wait as long as it says, a minute if it doesn't
            let wait = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(Duration::from_secs(60))
                .clamp(Duration::from_secs(10), Duration::from_secs(600));
            self.retry_after = Some(wait);
            return Err(format!("adsb.lol is rate-limiting this IP; trying again in {} s", wait.as_secs()));
        }
        if status != 200 {
            return Err(format!("adsb.lol answered {status}"));
        }
        let body = response.body_mut().read_to_string().map_err(|e| format!("adsb.lol's answer was cut off: {e}"))?;
        let aircraft = parse_readsb(&body)?;
        self.share(&body);
        Ok(aircraft)
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

/// Replays one recorded response on every poll, for offline work and tests.
pub struct FileSource {
    path: String,
}

impl FileSource {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

impl Source for FileSource {
    fn fetch(&mut self) -> Result<Vec<Aircraft>, String> {
        let body = std::fs::read_to_string(&self.path).map_err(|e| format!("{}: {e}", self.path))?;
        parse_readsb(&body)
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(5)
    }
}

/// The readsb JSON shape adsb.lol, airplanes.live and a local readsb share:
/// `{ "ac": [...] }` from the APIs, `{ "aircraft": [...] }` from readsb itself.
pub fn parse_readsb(body: &str) -> Result<Vec<Aircraft>, String> {
    let json: Value = serde_json::from_str(body).map_err(|e| format!("bad JSON: {e}"))?;
    let list = json
        .get("ac")
        .or_else(|| json.get("aircraft"))
        .and_then(Value::as_array)
        .ok_or("no aircraft list in response")?;
    Ok(list.iter().filter_map(parse_one).collect())
}

fn parse_one(v: &Value) -> Option<Aircraft> {
    let text = |key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let number = |key: &str| v.get(key).and_then(Value::as_f64);
    let alt_ft = match v.get("alt_baro") {
        Some(Value::String(s)) if s == "ground" => None,
        Some(alt) => Some(alt.as_f64()? as i32),
        // No barometric altitude: the GPS one, or unknown (drawn as if on the
        // ground) rather than dropping the aircraft
        None => number("alt_geom").map(|a| a as i32),
    };
    Some(Aircraft {
        hex: text("hex")?,
        callsign: text("flight"),
        type_code: text("t"),
        registration: text("r"),
        lat: number("lat")?,
        lon: number("lon")?,
        alt_ft,
        gs_kt: number("gs").unwrap_or(0.0),
        track_deg: number("track").or_else(|| number("true_heading")),
        vrate_fpm: number("baro_rate").or_else(|| number("geom_rate")).unwrap_or(0.0) as i32,
        seen_pos_s: number("seen_pos").unwrap_or(0.0),
        squawk: text("squawk"),
        emergency: text("emergency").filter(|e| e != "none"),
        // dbFlags bit 0: military
        is_military: v.get("dbFlags").and_then(Value::as_u64).is_some_and(|f| f & 1 == 1),
        is_rotorcraft: text("category").as_deref() == Some("A7"),
        interest: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_recorded_fixture() {
        let body = std::fs::read_to_string("tests/fixtures/boston.json").unwrap();
        let aircraft = parse_readsb(&body).unwrap();
        assert!(aircraft.len() > 5);
        assert!(aircraft.iter().all(|a| !a.hex.is_empty()));
        // Callsigns arrive space-padded
        assert!(aircraft.iter().filter_map(|a| a.callsign.as_ref()).all(|c| c.trim() == c));
    }

    #[test]
    fn reads_emergencies_and_flags() {
        let body = r#"{"ac":[
            {"hex":"a1","lat":42,"lon":-71,"squawk":"7700","emergency":"none"},
            {"hex":"a2","lat":42,"lon":-71,"squawk":"1200","emergency":"nordo"},
            {"hex":"a3","lat":42,"lon":-71,"squawk":"4321","dbFlags":1,"category":"A7"}]}"#;
        let a = parse_readsb(body).unwrap();
        assert_eq!(a[0].emergency_kind(), Some("emergency"));
        assert_eq!(a[1].emergency_kind(), Some("radio failure"));
        assert_eq!(a[2].emergency_kind(), None);
        assert!(a[2].is_military && a[2].is_rotorcraft);
    }

    #[test]
    fn ground_has_no_altitude() {
        let one = r#"{"ac":[{"hex":"abc123","alt_baro":"ground","lat":42.3,"lon":-71.0}]}"#;
        let aircraft = parse_readsb(one).unwrap();
        assert!(aircraft[0].is_on_ground());
    }
}
