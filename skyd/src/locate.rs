// Where home is. By default the public IP's city, from a keyless lookup; the
// person can name a place or give coordinates instead. IP location is
// city-level and lands on the VPN's exit when one is on, so the mod always
// says which it used and how to set it exactly.

use crate::source::http_agent;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Debug)]
pub struct Place {
    pub lat: f64,
    pub lon: f64,
    /// "Somerville, Massachusetts, US"
    pub label: String,
    /// `ip` (approximate) or `place` (a named place's centre)
    pub source: &'static str,
}

fn get_json(url: &str) -> Result<Value, String> {
    let body = http_agent()
        .get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("{url}: bad JSON: {e}"))
}

fn label(parts: &[Option<&str>]) -> String {
    parts.iter().flatten().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(", ")
}

/// The public IP's approximate location: ipinfo.io, then ipwho.is
pub fn locate() -> Result<Place, String> {
    let ipinfo = || -> Result<Place, String> {
        let v = get_json("https://ipinfo.io/json")?;
        let loc = v["loc"].as_str().ok_or("ipinfo.io: no loc")?;
        let (lat, lon) = loc.split_once(',').ok_or("ipinfo.io: bad loc")?;
        Ok(Place {
            lat: lat.parse().map_err(|_| "ipinfo.io: bad lat")?,
            lon: lon.parse().map_err(|_| "ipinfo.io: bad lon")?,
            label: label(&[v["city"].as_str(), v["region"].as_str(), v["country"].as_str()]),
            source: "ip",
        })
    };
    let ipwho = || -> Result<Place, String> {
        let v = get_json("https://ipwho.is/")?;
        if v["success"].as_bool() != Some(true) {
            return Err(format!("ipwho.is: {}", v["message"].as_str().unwrap_or("failed")));
        }
        Ok(Place {
            lat: v["latitude"].as_f64().ok_or("ipwho.is: no latitude")?,
            lon: v["longitude"].as_f64().ok_or("ipwho.is: no longitude")?,
            label: label(&[v["city"].as_str(), v["region"].as_str(), v["country_code"].as_str()]),
            source: "ip",
        })
    };
    ipinfo().or_else(|first| ipwho().map_err(|second| format!("{first}; {second}")))
}

/// A named place, such as "Somerville, MA" or "Lyon, France", or "42.39,-71.1"
pub fn geocode(query: &str) -> Result<Place, String> {
    if let Some(place) = coordinates(query) {
        return Ok(place);
    }
    let mut parts = query.split(',').map(str::trim);
    let name = parts.next().filter(|s| !s.is_empty()).ok_or("name a place")?;
    let qualifier = parts.collect::<Vec<_>>().join(" ").to_lowercase();
    let url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?count=10&format=json&name={}",
        encode(name)
    );
    let v = get_json(&url)?;
    let results = v["results"].as_array().filter(|r| !r.is_empty()).ok_or(format!("no place called {name}"))?;
    // Results come most-populous first; a qualifier picks among them
    let pick = results
        .iter()
        .find(|r| qualifier.is_empty() || matches_qualifier(r, &qualifier))
        .ok_or(format!("no {name} in {qualifier}"))?;
    Ok(Place {
        lat: pick["latitude"].as_f64().ok_or("no latitude")?,
        lon: pick["longitude"].as_f64().ok_or("no longitude")?,
        label: label(&[pick["name"].as_str(), pick["admin1"].as_str(), pick["country_code"].as_str()]),
        source: "place",
    })
}

fn coordinates(query: &str) -> Option<Place> {
    let (lat, lon) = query.split_once(|c| c == ',' || c == ' ')?;
    let (lat, lon): (f64, f64) = (lat.trim().parse().ok()?, lon.trim().parse().ok()?);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    Some(Place { lat, lon, label: format!("{lat:.4}, {lon:.4}"), source: "place" })
}

fn matches_qualifier(result: &Value, qualifier: &str) -> bool {
    let fields = ["admin1", "admin2", "country", "country_code"]
        .map(|k| result[k].as_str().unwrap_or("").to_lowercase());
    let state = us_state(qualifier).map(str::to_lowercase);
    fields.iter().any(|f| !f.is_empty() && (f == qualifier || state.as_deref() == Some(f.as_str())))
}

fn us_state(code: &str) -> Option<&'static str> {
    const STATES: [(&str, &str); 51] = [
        ("al", "Alabama"), ("ak", "Alaska"), ("az", "Arizona"), ("ar", "Arkansas"), ("ca", "California"),
        ("co", "Colorado"), ("ct", "Connecticut"), ("de", "Delaware"), ("dc", "District of Columbia"),
        ("fl", "Florida"), ("ga", "Georgia"), ("hi", "Hawaii"), ("id", "Idaho"), ("il", "Illinois"),
        ("in", "Indiana"), ("ia", "Iowa"), ("ks", "Kansas"), ("ky", "Kentucky"), ("la", "Louisiana"),
        ("me", "Maine"), ("md", "Maryland"), ("ma", "Massachusetts"), ("mi", "Michigan"), ("mn", "Minnesota"),
        ("ms", "Mississippi"), ("mo", "Missouri"), ("mt", "Montana"), ("ne", "Nebraska"), ("nv", "Nevada"),
        ("nh", "New Hampshire"), ("nj", "New Jersey"), ("nm", "New Mexico"), ("ny", "New York"),
        ("nc", "North Carolina"), ("nd", "North Dakota"), ("oh", "Ohio"), ("ok", "Oklahoma"), ("or", "Oregon"),
        ("pa", "Pennsylvania"), ("ri", "Rhode Island"), ("sc", "South Carolina"), ("sd", "South Dakota"),
        ("tn", "Tennessee"), ("tx", "Texas"), ("ut", "Utah"), ("vt", "Vermont"), ("va", "Virginia"),
        ("wa", "Washington"), ("wv", "West Virginia"), ("wi", "Wisconsin"), ("wy", "Wyoming"),
    ];
    STATES.iter().find(|(c, _)| *c == code).map(|(_, name)| *name)
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_coordinates() {
        let p = geocode("42.39, -71.1").unwrap();
        assert_eq!((p.lat, p.lon), (42.39, -71.1));
        assert!(coordinates("Somerville, MA").is_none());
    }

    #[test]
    fn qualifies_by_state_code_or_name() {
        let r: Value = serde_json::json!({ "admin1": "Massachusetts", "country_code": "US" });
        assert!(matches_qualifier(&r, "ma"));
        assert!(matches_qualifier(&r, "massachusetts"));
        assert!(matches_qualifier(&r, "us"));
        assert!(!matches_qualifier(&r, "nj"));
    }
}
