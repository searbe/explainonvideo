//! The cameras and microphones there are, and how to open them in a pipeline.
use gstreamer as gst;
use gst::prelude::*;
use std::process::Command;

/// Camera formats to ask for, best first: 720p is plenty for a picture-in-picture, and MJPEG keeps USB light.
const CAMERA_CAPS: &[&str] = &[
    "image/jpeg,width=1280,height=720,framerate=30/1", "video/x-raw,width=1280,height=720,framerate=30/1",
    "image/jpeg,width=960,height=540,framerate=30/1", "image/jpeg,width=640,height=480,framerate=30/1",
    "video/x-raw,width=640,height=480,framerate=30/1", "image/jpeg,width=1920,height=1080,framerate=30/1",
    "video/x-raw,width=1920,height=1080"];

#[derive(Clone)]
pub struct Camera {
    pub name: String,
    pub path: String,
    caps: Option<gst::Caps>,
}

impl Camera {
    /// The pipeline description that gives this camera's picture, decoded.
    pub fn source(&self) -> String {
        for want in CAMERA_CAPS {
            let ok = match &self.caps {
                None => true,
                Some(c) => want.parse::<gst::Caps>().map(|w| c.can_intersect(&w)).unwrap_or(false),
            };
            if ok {
                let decode = if want.starts_with("image/jpeg") { "jpegdec" } else { "identity" };
                return format!("v4l2src device=\"{}\" ! {want} ! {decode}", self.path);
            }
        }
        format!("v4l2src device=\"{}\" ! decodebin", self.path)
    }
}

/// The cameras plugged in (V4L2), each once (not their metadata nodes).
pub fn cameras() -> Vec<Camera> {
    let Some(provider) = gst::DeviceProviderFactory::by_name("v4l2deviceprovider") else { return vec![] };
    let mut out = vec![];
    for d in provider.devices() {
        let path = d.properties().and_then(|p| {
            ["api.v4l2.path", "device.path"].iter().find_map(|k| p.get::<String>(*k).ok())
        });
        let caps = d.caps();
        if let (Some(path), Some(caps)) = (path, caps) {
            if !caps.is_empty() {
                out.push(Camera { name: d.display_name().to_string(), path, caps: Some(caps) });
            }
        }
    }
    out
}

/// The camera at path, or the first there is (path empty or gone).
pub fn camera(path: &str) -> Option<Camera> {
    let found = cameras();
    found.iter().find(|c| c.path == path).cloned().or_else(|| found.first().cloned())
}

/// [(name, description)] of the sound inputs (not other outputs' monitors), from PipeWire's Pulse side.
pub fn microphones() -> Vec<(String, String)> {
    let Ok(out) = Command::new("pactl").args(["-f", "json", "list", "sources"]).output() else { return vec![] };
    let Ok(sources) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return vec![] };
    sources.as_array().map(|a| a.iter().filter_map(|s| {
        let name = s["name"].as_str()?.to_string();
        if name.ends_with(".monitor") {
            return None;
        }
        let desc = s["description"].as_str().filter(|d| !d.is_empty()).unwrap_or(&name).to_string();
        Some((name, desc))
    }).collect()).unwrap_or_default()
}

/// The microphone to record from: name if it's there; else the desktop's default if that's there (""); else the
/// first there is; None if there's none.
pub fn microphone(name: &str) -> Option<String> {
    let mics: Vec<String> = microphones().into_iter().map(|(n, _)| n).collect();
    if !name.is_empty() && mics.iter().any(|m| m == name) {
        return Some(name.to_string());
    }
    let default = Command::new("pactl").arg("get-default-source").output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if mics.contains(&default) {
        return Some(String::new());
    }
    mics.first().cloned()
}
