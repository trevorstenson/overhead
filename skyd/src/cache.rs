// Facts looked up by key over the network (a callsign's route, a hex's
// airframe), one at a time on a thread of their own, and kept on disk for
// every session on the machine. A miss is remembered too, for less long, so a
// key nobody knows isn't asked for again every poll.

use serde::{Serialize, de::DeserializeOwned};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(serde::Serialize, serde::Deserialize)]
struct Entry<T> {
    value: Option<T>,
    fetched_at: u64,
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn cache_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/overhead")
    } else {
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or(home.join(".cache")).join("overhead")
    })
}

pub struct Lookups<T> {
    entries: HashMap<String, Entry<T>>,
    pending: HashSet<String>,
    asks: mpsc::Sender<String>,
    answers: mpsc::Receiver<(String, Option<T>)>,
    path: Option<PathBuf>,
    found_ttl_s: u64,
    missing_ttl_s: u64,
    dirty: bool,
}

impl<T: Serialize + DeserializeOwned + Send + 'static> Lookups<T> {
    /// `file` names the cache under the cache directory; `fetch` runs on the
    /// lookup thread and answers None for a key it couldn't find
    pub fn new(file: &str, found_ttl: Duration, missing_ttl: Duration, fetch: fn(&str) -> Option<T>) -> Self {
        let path = cache_dir().map(|d| d.join(file));
        let entries = path.as_ref().map(|p| read(p)).unwrap_or_default();
        let (asks, asked) = mpsc::channel::<String>();
        let (answer, answers) = mpsc::channel();
        std::thread::spawn(move || {
            for key in asked {
                let value = fetch(&key);
                if answer.send((key, value)).is_err() {
                    return;
                }
                // Be gentle with free services
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        Self {
            entries,
            pending: HashSet::new(),
            asks,
            answers,
            path,
            found_ttl_s: found_ttl.as_secs(),
            missing_ttl_s: missing_ttl.as_secs(),
            dirty: false,
        }
    }

    /// Asks for the key in the background unless a fresh answer is cached
    pub fn want(&mut self, key: &str) {
        let now = unix_now();
        let fresh = self.entries.get(key).is_some_and(|e| {
            let ttl = if e.value.is_some() { self.found_ttl_s } else { self.missing_ttl_s };
            now.saturating_sub(e.fetched_at) < ttl
        });
        if !fresh && self.pending.insert(key.to_string()) {
            let _ = self.asks.send(key.to_string());
        }
    }

    /// The cached answer, fresh or not, without asking
    pub fn get(&self, key: &str) -> Option<&T> {
        self.entries.get(key)?.value.as_ref()
    }

    /// Takes in finished lookups; true when any arrived
    pub fn collect(&mut self) -> bool {
        let mut any = false;
        while let Ok((key, value)) = self.answers.try_recv() {
            self.pending.remove(&key);
            self.entries.insert(key, Entry { value, fetched_at: unix_now() });
            self.dirty = true;
            any = true;
        }
        any
    }

    /// Writes the cache when it changed, merging what other sessions wrote
    /// and dropping what's long expired
    pub fn save(&mut self) {
        let Some(path) = &self.path else { return };
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        let mut merged: HashMap<String, Entry<T>> = read(path);
        let now = unix_now();
        let keep = self.found_ttl_s * 4;
        merged.retain(|_, e| now.saturating_sub(e.fetched_at) < keep);
        for (key, entry) in self.entries.drain() {
            if merged.get(&key).is_none_or(|m| m.fetched_at < entry.fetched_at) {
                merged.insert(key, entry);
            }
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Whole or not at all, so a reader never sees half a file
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        if std::fs::write(&tmp, serde_json::to_string(&merged).unwrap()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
        self.entries = merged;
    }
}

fn read<T: DeserializeOwned>(path: &PathBuf) -> HashMap<String, Entry<T>> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
