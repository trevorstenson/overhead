// Aircraft worth a second look, from plane-alert-db: about 17,000 airframes
// spotters have tagged by hand (air forces, police, air ambulances,
// governments, historic aircraft, firefighters…). Downloaded once a week and
// cached; looked up by ICAO hex.
//
// The database's categories are many and playful ("Toy Soldiers", "Dogs with
// Jobs"); each is folded into a plain group for the radar and the toasts,
// keeping the original for the detail line.

use crate::cache::cache_dir;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const URL: &str = "https://raw.githubusercontent.com/sdr-enthusiasts/plane-alert-db/main/plane-alert-db.csv";
const REFRESH: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Interest {
    /// A plain group: military, police, air ambulance, government, coast
    /// guard, firefighting, historic, or notable
    pub group: &'static str,
    /// The database's own category, as written
    pub category: String,
    pub operator: Option<String>,
    /// What spotters say about it: its role, joined
    pub note: Option<String>,
}

/// The database's categories, folded into groups people read at a glance
fn group_of(category: &str) -> &'static str {
    let c = category.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| c.contains(w));
    if has(&["police"]) {
        "police"
    } else if has(&["doctor", "medic", "hospital", "ambulance"]) {
        "air ambulance"
    } else if has(&["coastguard", "coast guard"]) {
        "coast guard"
    } else if has(&["firefight"]) {
        "firefighting"
    } else if has(&["historic", "vintage", "warbird"]) {
        "historic"
    } else if has(&["government", "dictator", "head of state", "royal"]) {
        "government"
    } else if has(&[
        "usaf", "air force", "navy", "marine", "army", "raf", "gaf", "toy soldiers", "gunship", "special forces", "zoomies", "uav",
        "nuclear", "oxcart", "military",
    ]) {
        "military"
    } else {
        "notable"
    }
}

/// Splits one CSV line, honouring quoted fields with commas in them
fn split_csv(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    fields.push(field);
    fields
}

pub fn parse(csv: &str) -> HashMap<String, Interest> {
    let mut lines = csv.lines();
    let Some(header) = lines.next() else { return HashMap::new() };
    // Columns are named with $ and # markers ("$ICAO", "$#Tag 2"); match on the name
    let names: Vec<String> = split_csv(header).iter().map(|h| h.trim_start_matches(['$', '#']).trim_start_matches('#').to_lowercase()).collect();
    let column = |name: &str| names.iter().position(|n| n == name);
    let (Some(icao), Some(category)) = (column("icao"), column("category")) else { return HashMap::new() };
    let operator = column("operator");
    let tags: Vec<usize> = ["tag 1", "tag 2", "tag 3"].iter().filter_map(|t| column(t)).collect();
    let mut found = HashMap::new();
    for line in lines {
        let fields = split_csv(line);
        let get = |i: usize| fields.get(i).map(|f| f.trim()).filter(|f| !f.is_empty());
        let (Some(hex), Some(cat)) = (get(icao), get(category)) else { continue };
        let note = tags.iter().filter_map(|i| get(*i)).collect::<Vec<_>>().join(", ");
        found.insert(
            hex.to_lowercase(),
            Interest {
                group: group_of(cat),
                category: cat.to_string(),
                operator: operator.and_then(get).map(String::from),
                note: (!note.is_empty()).then_some(note),
            },
        );
    }
    found
}

/// The database, arriving in the background
pub struct Interests {
    table: Arc<Mutex<Option<Arc<HashMap<String, Interest>>>>>,
}

impl Interests {
    pub fn start() -> Self {
        let table = Arc::new(Mutex::new(None));
        let slot = table.clone();
        std::thread::spawn(move || {
            if let Some(csv) = load() {
                *slot.lock().unwrap() = Some(Arc::new(parse(&csv)));
            }
        });
        Self { table }
    }

    pub fn get(&self, hex: &str) -> Option<Interest> {
        self.table.lock().unwrap().as_ref()?.get(&hex.to_lowercase()).cloned()
    }
}

fn load() -> Option<String> {
    let path = cache_dir().map(|d| d.join("plane-alert-db.csv"));
    let cached = path.as_ref().and_then(|p| {
        let age = std::fs::metadata(p).ok()?.modified().ok()?.elapsed().ok()?;
        Some((std::fs::read_to_string(p).ok()?, age))
    });
    if let Some((csv, age)) = &cached {
        if *age < REFRESH {
            return Some(csv.clone());
        }
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(90)))
        .user_agent(concat!("overhead-skyd/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let fresh = agent
        .get(URL)
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string().ok())
        .filter(|csv| csv.len() > 10_000);
    match fresh {
        Some(csv) => {
            if let Some(path) = path {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = std::fs::write(path, &csv);
            }
            Some(csv)
        }
        // A stale list beats none
        None => cached.map(|(csv, _)| csv),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_database_and_groups_its_categories() {
        let csv = "$ICAO,$Registration,$Operator,$Type,$ICAO Type,#CMPG,$Tag 1,$#Tag 2,$#Tag 3,Category,$#Link\n\
                   AE1234,N1,Massachusetts State Police,Bell 407,B407,Pol,Patrol,\"Eye, in the sky\",,Police Forces,https://x\n\
                   000004,FAC1282,Colombian Aerospace Force,CASA C-295 M,C295,Mil,Cargo,Tactical Transport,,Other Air Forces,https://x\n\
                   a00001,N2,Boston MedFlight,EC135,EC35,Civ,HEMS,,,Flying Doctors,https://x\n";
        let table = parse(csv);
        let police = &table["ae1234"];
        assert_eq!(police.group, "police");
        assert_eq!(police.operator.as_deref(), Some("Massachusetts State Police"));
        assert_eq!(police.note.as_deref(), Some("Patrol, Eye, in the sky"));
        assert_eq!(table["000004"].group, "military");
        assert_eq!(table["a00001"].group, "air ambulance");
    }

    #[test]
    fn groups_the_playful_names() {
        assert_eq!(group_of("Toy Soldiers"), "military");
        assert_eq!(group_of("Dictator Alert"), "government");
        assert_eq!(group_of("Aerial Firefighter"), "firefighting");
        assert_eq!(group_of("As Seen on TV"), "notable");
    }
}
