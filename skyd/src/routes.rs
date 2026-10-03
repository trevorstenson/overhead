// Origin and destination by callsign, from adsb.lol's published route files,
// then adsbdb. Route databases go stale (a callsign moves to another city
// pair), so a route is only shown when the aircraft is plausibly flying one
// of its legs.

use crate::cache::Lookups;
use crate::geo::{bearing, distance_nm};
use crate::source::http_agent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Airport {
    /// IATA where it has one, else ICAO
    pub code: String,
    pub city: String,
    pub lat: f64,
    pub lon: f64,
}

/// Every stop a callsign makes, in order: two for most flights, more for a
/// regional that hops BOS-DCA-BOS under one number
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Route {
    pub stops: Vec<Airport>,
    pub airline: Option<String>,
}

/// The one leg an aircraft is flying, as the mod sees it
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Leg<'a> {
    pub from: &'a Airport,
    pub to: &'a Airport,
    pub airline: Option<&'a str>,
}

impl Route {
    /// The leg an aircraft at this position could be flying, if any is
    /// plausible at all: one it's headed toward the end of, when its track is
    /// known, then the one it detours least to be on. Near the hub of a
    /// BOS-DCA-BOS hop both legs fit; only the heading tells them apart.
    pub fn leg_at(&self, lat: f64, lon: f64, track_deg: Option<f64>) -> Option<Leg<'_>> {
        let heading_away = |to: &Airport| {
            track_deg.is_some_and(|track| {
                let diff = (bearing(lat, lon, to.lat, to.lon) - track).rem_euclid(360.0);
                diff > 90.0 && diff < 270.0
            })
        };
        self.stops
            .windows(2)
            .filter(|pair| pair[0].code != pair[1].code)
            .map(|pair| (detour(&pair[0], &pair[1], lat, lon), pair))
            .filter(|(excess, _)| *excess <= 0.0)
            .min_by(|a, b| (heading_away(&a.1[1]), a.0).partial_cmp(&(heading_away(&b.1[1]), b.0)).unwrap())
            .map(|(_, pair)| Leg { from: &pair[0], to: &pair[1], airline: self.airline.as_deref() })
    }
}

pub fn lookups() -> Lookups<Route> {
    Lookups::new("routes.json", Duration::from_secs(24 * 3600), Duration::from_secs(6 * 3600), lookup)
}

/// Airline flights look like `JBU1786`: three letters then a number. A
/// registration flown as a callsign (`N123AB`) has no route to find.
pub fn looks_like_flight(callsign: &str) -> bool {
    let b = callsign.as_bytes();
    b.len() >= 4 && b[..3].iter().all(u8::is_ascii_alphabetic) && b[3].is_ascii_digit()
}

fn get_json(url: &str) -> Option<Value> {
    let body = http_agent().get(url).call().ok()?.body_mut().read_to_string().ok()?;
    serde_json::from_str(&body).ok()
}

fn lookup(callsign: &str) -> Option<Route> {
    from_adsb_lol(callsign).or_else(|| from_adsbdb(callsign))
}

fn from_adsb_lol(callsign: &str) -> Option<Route> {
    let v = get_json(&format!("https://vrs-standing-data.adsb.lol/routes/{}/{callsign}.json", &callsign[..2]))?;
    let airport = |a: &Value| -> Option<Airport> {
        Some(Airport {
            code: a["iata"].as_str().filter(|s| !s.is_empty()).or(a["icao"].as_str())?.to_string(),
            city: a["location"].as_str().unwrap_or("").to_string(),
            lat: a["lat"].as_f64()?,
            lon: a["lon"].as_f64()?,
        })
    };
    let stops = v["_airports"].as_array()?.iter().map(airport).collect::<Option<Vec<_>>>()?;
    (stops.len() >= 2).then_some(Route { stops, airline: None })
}

fn from_adsbdb(callsign: &str) -> Option<Route> {
    let v = get_json(&format!("https://api.adsbdb.com/v0/callsign/{callsign}"))?;
    let r = &v["response"]["flightroute"];
    let airport = |a: &Value| -> Option<Airport> {
        Some(Airport {
            code: a["iata_code"].as_str().filter(|s| !s.is_empty()).or(a["icao_code"].as_str())?.to_string(),
            city: a["municipality"].as_str().unwrap_or("").to_string(),
            lat: a["latitude"].as_f64()?,
            lon: a["longitude"].as_f64()?,
        })
    };
    Some(Route {
        stops: vec![airport(&r["origin"])?, airport(&r["destination"])?],
        airline: r["airline"]["name"].as_str().map(String::from),
    })
}

/// How much further than allowed the trip is by way of this position:
/// zero or less when an aircraft here could plausibly be flying it
fn detour(from: &Airport, to: &Airport, lat: f64, lon: f64) -> f64 {
    let direct = distance_nm(from.lat, from.lon, to.lat, to.lon);
    let via = distance_nm(from.lat, from.lon, lat, lon) + distance_nm(lat, lon, to.lat, to.lon);
    via - (direct * 1.2 + 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn airport(code: &str, lat: f64, lon: f64) -> Airport {
        Airport { code: code.into(), city: String::new(), lat, lon }
    }

    #[test]
    fn plausible_only_between_the_airports() {
        let pit_bos = Route { stops: vec![airport("PIT", 40.49, -80.23), airport("BOS", 42.36, -71.01)], airline: None };
        // Descending into Boston from the west
        assert_eq!(pit_bos.leg_at(42.45, -71.2, None).unwrap().to.code, "BOS");
        // Over Miami: some other flight with a reused callsign
        assert!(pit_bos.leg_at(25.8, -80.3, None).is_none());
    }

    #[test]
    fn picks_the_leg_being_flown() {
        let hop = Route {
            stops: vec![airport("BOS", 42.36, -71.01), airport("BUF", 42.94, -78.73), airport("BOS", 42.36, -71.01)],
            airline: None,
        };
        // Near Rochester: one of the BOS-BUF legs, never BOS-BOS
        let leg = hop.leg_at(43.1, -77.6, None).unwrap();
        assert_ne!(leg.from.code, leg.to.code);
        // Just out of Boston: the heading says which way
        assert_eq!(hop.leg_at(42.5, -71.4, Some(280.0)).unwrap().to.code, "BUF");
        assert_eq!(hop.leg_at(42.5, -71.4, Some(100.0)).unwrap().to.code, "BOS");
    }

    #[test]
    fn only_airline_callsigns_are_looked_up() {
        assert!(looks_like_flight("JBU1786"));
        assert!(looks_like_flight("SWR52"));
        assert!(!looks_like_flight("N123AB"));
        assert!(!looks_like_flight("BOSRES1"));
    }
}
