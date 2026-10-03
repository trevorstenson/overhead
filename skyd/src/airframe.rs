// What an aircraft is, by its ICAO hex, from adsbdb: the model by name, its
// maker and who flies it. Feeds give only the type designator (`E545`), and
// not always that.

use crate::cache::Lookups;
use crate::source::http_agent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Airframe {
    /// The designator, such as `E545`
    pub icao_type: Option<String>,
    /// "Embraer Legacy 450"
    pub model: Option<String>,
    /// "Flexjet"
    pub owner: Option<String>,
    pub photo: Option<String>,
}

pub fn lookups() -> Lookups<Airframe> {
    // Airframes change hands rarely; a miss may be a new registration
    Lookups::new("airframes.json", Duration::from_secs(30 * 24 * 3600), Duration::from_secs(7 * 24 * 3600), lookup)
}

fn lookup(hex: &str) -> Option<Airframe> {
    let body = http_agent()
        .get(&format!("https://api.adsbdb.com/v0/aircraft/{hex}"))
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    let v: Value = serde_json::from_str(&body).ok()?;
    parse(&v)
}

fn parse(v: &Value) -> Option<Airframe> {
    let a = v["response"].get("aircraft")?;
    let text = |key: &str| a[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let model = match (text("manufacturer"), text("type")) {
        // "Boeing" + "737-8H4" reads well; "Airbus" + "Airbus A321" doesn't
        (Some(maker), Some(kind)) if !kind.to_lowercase().starts_with(&maker.to_lowercase()) => Some(format!("{maker} {kind}")),
        (_, kind) => kind,
    };
    Some(Airframe { icao_type: text("icao_type"), model, owner: text("registered_owner"), photo: text("url_photo") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_adsbdb_answer() {
        let v: Value = serde_json::from_str(
            r#"{"response":{"aircraft":{"type":"Legacy 450","icao_type":"E545","manufacturer":"Embraer","registered_owner":"Flexjet","url_photo":"https://example/p.jpg"}}}"#,
        )
        .unwrap();
        let a = parse(&v).unwrap();
        assert_eq!(a.model.as_deref(), Some("Embraer Legacy 450"));
        assert_eq!(a.owner.as_deref(), Some("Flexjet"));
    }

    #[test]
    fn unknown_is_none() {
        let v: Value = serde_json::from_str(r#"{"response":"unknown aircraft"}"#).unwrap();
        assert!(parse(&v).is_none());
    }
}
