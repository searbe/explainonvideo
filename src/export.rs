//! ffmpeg: what the computer can encode with, the commands that encode a recording as it's made, put its parts
//! together, and export an edit; and reading a video's length and size.
use crate::edit::Edit;
use crate::settings;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

const RENDER_NODE: &str = "/dev/dri/renderD128";
const AUDIO: [&str; 4] = ["-c:a", "aac", "-b:a", "160k"];

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Whether H.264 can be encoded on the GPU (VA-API): tried once, on a few frames.
pub fn hardware_works() -> bool {
    static WORKS: OnceLock<bool> = OnceLock::new();
    *WORKS.get_or_init(|| {
        Path::new(RENDER_NODE).exists() && Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-vaapi_device", RENDER_NODE, "-f", "lavfi", "-i",
                   "color=c=black:s=256x144:r=30", "-frames:v", "5", "-vf", "format=nv12,hwupload", "-c:v",
                   "h264_vaapi", "-f", "null", "-"])
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
    })
}

pub fn use_hardware() -> bool {
    settings::string("encoder") != "software" && hardware_works()
}

/// ffmpeg's video encoding arguments, (before the inputs, after them): the GPU when it works, else x264; live:
/// fast enough to keep up with a recording (else a better-compressing pace for an export).
pub fn video_args(fps: i64, live: bool, hardware: bool) -> (Vec<String>, Vec<String>) {
    let gop = vec!["-g".to_string(), (fps * 2).to_string()]; // a keyframe every 2 s: quick to seek in the editor
    if hardware {
        let mut post = s(&["-vf", "format=nv12,hwupload", "-c:v", "h264_vaapi", "-qp", if live { "20" } else { "22" }]);
        post.extend(gop);
        return (s(&["-vaapi_device", RENDER_NODE]), post);
    }
    let mut post = s(&["-c:v", "libx264", "-preset", if live { "veryfast" } else { "medium" }, "-crf",
                       if live { "20" } else { "21" }, "-pix_fmt", "yuv420p"]);
    post.extend(gop);
    (vec![], post)
}

/// Encode a stream of raw video and audio in Matroska on stdin (from the pipeline) into out.
pub fn record_command(out: &Path, fps: i64) -> Vec<String> {
    let (pre, post) = video_args(fps, true, use_hardware());
    let mut cmd = s(&["ffmpeg", "-hide_banner", "-loglevel", "warning", "-nostdin", "-y"]);
    cmd.extend(pre);
    cmd.extend(s(&["-thread_queue_size", "512", "-f", "matroska", "-i", "pipe:0"]));
    cmd.extend(post);
    cmd.extend(s(&["-fps_mode", "cfr", "-r", &fps.to_string()]));
    cmd.extend(s(&AUDIO));
    cmd.push(out.to_string_lossy().into());
    cmd
}

/// Put a recording's parts (one per stretch between pauses) together into one MP4, without re-encoding.
pub fn join(parts: &[PathBuf], out: &Path) -> Result<(), String> {
    let parts: Vec<&PathBuf> = parts.iter().filter(|p| p.metadata().map(|m| m.len() > 0).unwrap_or(false))
        .collect();
    if parts.is_empty() {
        return Err("nothing was recorded".into());
    }
    let mut cmd = s(&["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    if parts.len() == 1 {
        cmd.extend(["-i".to_string(), parts[0].to_string_lossy().into()]);
    } else {
        let listing = parts[0].with_extension("txt");
        let text: String = parts.iter().map(|p| format!("file '{}'\n", p.file_name().unwrap().to_string_lossy()))
            .collect();
        std::fs::write(&listing, text).map_err(|e| e.to_string())?;
        cmd.extend(s(&["-f", "concat", "-safe", "0", "-i"]));
        cmd.push(listing.to_string_lossy().into());
    }
    cmd.extend(s(&["-c", "copy", "-movflags", "+faststart"]));
    cmd.push(out.to_string_lossy().into());
    let run = Command::new(&cmd[0]).args(&cmd[1..]).output().map_err(|e| e.to_string())?;
    if !run.status.success() {
        let err = String::from_utf8_lossy(&run.stderr);
        return Err(err.trim().lines().last().unwrap_or("ffmpeg failed").to_string());
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Info {
    pub duration: f64,
    pub width: i32,
    pub height: i32,
    pub fps: f64,
    pub audio: bool,
}

/// A video's duration, size, frame rate and whether it has sound; None if it can't be read.
pub fn probe(path: &Path) -> Option<Info> {
    let out = Command::new("ffprobe").args(["-v", "error", "-show_entries",
        "format=duration:stream=codec_type,width,height,r_frame_rate", "-of", "json"]).arg(path).output().ok()?;
    let data: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let streams = data["streams"].as_array()?;
    let video = streams.iter().find(|s| s["codec_type"] == "video")?;
    let rate = video["r_frame_rate"].as_str().unwrap_or("30/1");
    let (num, den) = rate.split_once('/').unwrap_or((rate, "1"));
    let fps = num.parse::<f64>().unwrap_or(30.0) / den.parse::<f64>().unwrap_or(1.0).max(1e-9);
    Some(Info {
        duration: data["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0),
        width: video["width"].as_i64()? as i32,
        height: video["height"].as_i64()? as i32,
        fps: if (1.0..=240.0).contains(&fps) { fps } else { 30.0 },
        audio: streams.iter().any(|s| s["codec_type"] == "audio"),
    })
}

fn even_down(n: f64) -> i32 {
    ((n as i32) / 2 * 2).max(2)
}

/// The ffmpeg command that makes out from src with edit applied: its clips in order, cropped.
pub fn edit_command(src: &Path, info: &Info, edit: &Edit, out: &Path, hardware: bool) -> Vec<String> {
    let mut chains = vec![];
    let mut labels = String::new();
    for (i, c) in edit.clips.iter().enumerate() {
        chains.push(format!("[0:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS[v{i}]", c.start, c.end));
        labels += &format!("[v{i}]");
        if info.audio {
            chains.push(format!("[0:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS[a{i}]", c.start, c.end));
            labels += &format!("[a{i}]");
        }
    }
    let a = if info.audio { 1 } else { 0 };
    chains.push(format!("{labels}concat=n={}:v=1:a={a}[cv]{}", edit.clips.len(), if info.audio { "[ca]" } else { "" }));
    let mut video = "[cv]".to_string();
    let (mut fw, mut fh) = (info.width, info.height);
    if let Some((x, y, w, h)) = edit.crop {
        let (iw, ih) = (info.width as f64, info.height as f64);
        let (cx, cy) = (even_down(x * iw), even_down(y * ih));
        let cw = even_down(w * iw).min(even_down(iw - cx as f64));
        let ch = even_down(h * ih).min(even_down(ih - cy as f64));
        chains.push(format!("[cv]crop={cw}:{ch}:{cx}:{cy}[crop]"));
        video = "[crop]".into();
        (fw, fh) = (cw, ch);
    }
    let min = crate::layout::MIN_VIDEO;
    if hardware && (fw < min || fh < min) { // (the GPU's encoder can't take a picture this small: pad it)
        chains.push(format!("{video}pad={}:{}:(ow-iw)/2:(oh-ih)/2[pad]", fw.max(min), fh.max(min)));
        video = "[pad]".into();
    }
    let (pre, mut post) = video_args(info.fps.round() as i64, false, hardware);
    if post[0] == "-vf" { // the GPU's upload goes at the end of the filter graph
        chains.push(format!("{video}{}[out]", post[1]));
        video = "[out]".into();
        post.drain(..2);
    }
    let mut cmd = s(&["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    cmd.extend(pre);
    cmd.extend(["-i".to_string(), src.to_string_lossy().into(), "-filter_complex".into(), chains.join(";"),
                "-map".into(), video]);
    if info.audio {
        cmd.extend(s(&["-map", "[ca]"]));
        cmd.extend(s(&AUDIO));
    }
    cmd.extend(post);
    cmd.extend(s(&["-movflags", "+faststart", "-progress", "pipe:1", "-nostats"]));
    cmd.push(out.to_string_lossy().into());
    cmd
}

pub enum Progress {
    Done(f64),
    Finished(Option<String>), // the error, if any
}

/// Runs a command (an export) in the background; reports progress (share done, from ffmpeg's -progress) and its
/// end on the main loop.
pub struct Export {
    child: Arc<Mutex<Option<std::process::Child>>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl Export {
    pub fn start(cmd: Vec<String>, length: f64, on: impl Fn(Progress) + 'static) -> Export {
        let (tx, rx) = crate::util::channel::<Progress>();
        let child = Arc::new(Mutex::new(None));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (child2, cancelled2) = (child.clone(), cancelled.clone());
        let length = length.max(0.01);
        std::thread::spawn(move || {
            let spawned = Command::new(&cmd[0]).args(&cmd[1..]).stdout(Stdio::piped()).stderr(Stdio::piped())
                .spawn();
            let mut proc = match spawned {
                Ok(p) => p,
                Err(e) => return { let _ = tx.send(Progress::Finished(Some(e.to_string()))); },
            };
            let stdout = proc.stdout.take().unwrap();
            let mut stderr = proc.stderr.take().unwrap();
            *child2.lock().unwrap() = Some(proc);
            let err_thread = std::thread::spawn(move || {
                let mut e = String::new();
                let _ = stderr.read_to_string(&mut e);
                e
            });
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(v) = line.trim().strip_prefix("out_time_us=") {
                    if let Ok(us) = v.parse::<f64>() {
                        let _ = tx.send(Progress::Done((us / 1e6 / length).min(1.0)));
                    }
                }
            }
            let err = err_thread.join().unwrap_or_default();
            let code = child2.lock().unwrap().as_mut().and_then(|c| c.wait().ok());
            let result = if cancelled2.load(std::sync::atomic::Ordering::SeqCst) {
                Some("cancelled".to_string())
            } else if !code.map(|c| c.success()).unwrap_or(false) {
                Some(err.trim().lines().last().map(|l| l.to_string()).unwrap_or_else(|| "ffmpeg stopped".into()))
            } else {
                None
            };
            let _ = tx.send(Progress::Finished(result));
        });
        rx.attach(None, move |p| {
            let end = matches!(p, Progress::Finished(_));
            on(p);
            if end { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
        });
        Export { child, cancelled }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            let _ = c.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Clip;

    #[test]
    fn edit_command_shape() {
        let e = Edit::with(10.0, vec![Clip::new(5.0, 10.0), Clip::new(0.0, 2.0)], Some((0.25, 0.25, 0.5, 0.5)));
        let info = Info { duration: 10.0, width: 1920, height: 1080, fps: 30.0, audio: true };
        let cmd = edit_command(Path::new("in.mp4"), &info, &e, Path::new("out.mp4"), false);
        let graph = &cmd[cmd.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("trim=start=5.000:end=10.000") && graph.contains("concat=n=2:v=1:a=1"));
        assert!(graph.contains("crop=960:540:480:270"));
        assert!(!graph.contains("pad="));
        assert_eq!(cmd.last().unwrap(), "out.mp4");
        let hw = edit_command(Path::new("in.mp4"), &info, &e, Path::new("out.mp4"), true);
        assert!(hw.iter().any(|a| a.contains("format=nv12,hwupload[out]")));
        let small = Info { width: 400, height: 140, ..info };
        let hw = edit_command(Path::new("in.mp4"), &small, &e, Path::new("out.mp4"), true);
        assert!(hw.iter().any(|a| a.contains("crop=200:70:100:34[crop];[crop]pad=200:128:(ow-iw)/2:(oh-ih)/2[pad];[pad]format=nv12")));
    }
}
