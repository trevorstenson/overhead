// A photo of the picked aircraft: Planespotters' thumbnail of that exact
// airframe, else the one adsbdb links to. Fetched when an aircraft is picked,
// never in bulk, and cached on disk. Planespotters asks for a descriptive
// User-Agent, and for every photo to credit its photographer and link back.

use crate::cache::cache_dir;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

const USER_AGENT: &str = concat!("overhead-skyd/", env!("CARGO_PKG_VERSION"), " (+https://github.com/trevorstenson/overhead)");
const KEEP: Duration = Duration::from_secs(30 * 24 * 3600);

#[derive(Clone, Serialize, Deserialize)]
pub struct Credit {
    /// "Photo: Ethan Yang / Planespotters.net"
    pub text: String,
    pub link: Option<String>,
}

pub struct Photo {
    pub hex: String,
    pub credit: Credit,
    pub image: image::RgbImage,
}

pub struct Photos {
    asks: mpsc::Sender<(String, Option<String>)>,
    answers: mpsc::Receiver<(String, Option<Photo>)>,
    asked: Option<String>,
}

impl Photos {
    pub fn new() -> Self {
        let (asks, asked) = mpsc::channel::<(String, Option<String>)>();
        let (answer, answers) = mpsc::channel();
        std::thread::spawn(move || {
            for (hex, fallback) in asked {
                let photo = load(&hex, fallback.as_deref());
                if answer.send((hex, photo)).is_err() {
                    return;
                }
            }
        });
        Self { asks, answers, asked: None }
    }

    /// Asks for the picked aircraft's photo, once per pick; `fallback` is
    /// adsbdb's photo URL for it, where known
    pub fn want(&mut self, hex: Option<&str>, fallback: Option<&str>) {
        if self.asked.as_deref() == hex {
            return;
        }
        self.asked = hex.map(String::from);
        if let Some(hex) = hex {
            let _ = self.asks.send((hex.to_string(), fallback.map(String::from)));
        }
    }

    /// A photo (or word that there's none) for what's picked now
    pub fn arrived(&mut self) -> Option<(String, Option<Photo>)> {
        let mut latest = None;
        while let Ok((hex, photo)) = self.answers.try_recv() {
            if self.asked.as_deref() == Some(hex.as_str()) {
                latest = Some((hex, photo));
            }
        }
        latest
    }
}

fn dir() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("photos"))
}

fn get(url: &str) -> Option<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(USER_AGENT)
        .build()
        .into();
    let mut bytes = Vec::new();
    agent.get(url).call().ok()?.body_mut().as_reader().take(8 * 1024 * 1024).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The cached photo, else Planespotters', else adsbdb's
fn load(hex: &str, fallback: Option<&str>) -> Option<Photo> {
    let hex = hex.to_lowercase();
    let dir = dir()?;
    let (jpeg_path, credit_path) = (dir.join(format!("{hex}.jpg")), dir.join(format!("{hex}.json")));
    let fresh = std::fs::metadata(&credit_path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age < KEEP);
    if fresh {
        let credit: Option<Credit> = std::fs::read_to_string(&credit_path).ok().and_then(|s| serde_json::from_str(&s).ok());
        // A credit with no picture beside it means there's none to be had
        let image = std::fs::read(&jpeg_path).ok().and_then(|b| image::load_from_memory(&b).ok());
        return match (credit, image) {
            (Some(credit), Some(image)) => Some(Photo { hex, credit, image: image.to_rgb8() }),
            _ => None,
        };
    }
    let (bytes, credit) = from_planespotters(&hex).or_else(|| {
        let url = fallback?;
        let bytes = get(url)?;
        Some((bytes, Credit { text: "Photo: airport-data.com".into(), link: Some(url.to_string()) }))
    }).unzip();
    let _ = std::fs::create_dir_all(&dir);
    let decoded = bytes.as_ref().and_then(|b| image::load_from_memory(b).ok());
    match (bytes, credit, decoded) {
        (Some(bytes), Some(credit), Some(image)) => {
            let _ = std::fs::write(&jpeg_path, &bytes);
            let _ = std::fs::write(&credit_path, serde_json::to_string(&credit).unwrap());
            Some(Photo { hex, credit, image: image.to_rgb8() })
        }
        _ => {
            // Remember there's none, so it isn't asked for again for a while
            let _ = std::fs::remove_file(&jpeg_path);
            let _ = std::fs::write(&credit_path, "null");
            None
        }
    }
}

fn from_planespotters(hex: &str) -> Option<(Vec<u8>, Credit)> {
    let body = get(&format!("https://api.planespotters.net/pub/photos/hex/{hex}"))?;
    let v: Value = serde_json::from_slice(&body).ok()?;
    let photo = v["photos"].as_array()?.first()?;
    let src = photo["thumbnail_large"]["src"].as_str().or(photo["thumbnail"]["src"].as_str())?;
    let photographer = photo["photographer"].as_str().unwrap_or("unknown");
    Some((
        get(src)?,
        Credit { text: format!("Photo: {photographer} / Planespotters.net"), link: photo["link"].as_str().map(String::from) },
    ))
}

/// What the mod draws a photo from: a raw RGB file for kitty and Ghostty,
/// quadrant cells for every other terminal, and a small JPEG for the SVG of
/// the Desktop app, with the credit
pub fn describe(photo: &Photo, columns: usize) -> String {
    let (w, h) = photo.image.dimensions();
    let file = dir().map(|d| d.join(format!("{}.rgb", photo.hex)));
    if let Some(file) = &file {
        let _ = std::fs::write(file, photo.image.as_raw());
    }
    // Cells are twice as tall as wide and hold 2×2 pixels each
    let columns = columns.clamp(8, 120);
    let rows = ((columns as f64 * h as f64 / w as f64) / 2.0).round().max(1.0) as usize;
    let small = image::imageops::resize(&photo.image, (columns * 2) as u32, (rows * 2) as u32, image::imageops::FilterType::Triangle);
    let mut canvas = crate::render::Canvas::cells(columns, rows);
    canvas.rgb.copy_from_slice(small.as_raw());
    let desktop = image::imageops::resize(&photo.image, 320, (320.0 * h as f64 / w as f64).round() as u32, image::imageops::FilterType::Triangle);
    let mut jpeg = Vec::new();
    let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 72).encode_image(&desktop);
    serde_json::json!({
        "hex": photo.hex,
        "credit": photo.credit.text,
        "link": photo.credit.link,
        "file": file.map(|f| f.to_string_lossy().into_owned()),
        "width": w,
        "height": h,
        "cells": { "columns": columns, "rows": rows, "data": crate::render::raster_cells(&canvas) },
        "jpeg": base64::engine::general_purpose::STANDARD.encode(&jpeg),
        "jpeg_width": desktop.width(),
        "jpeg_height": desktop.height(),
    })
    .to_string()
}
