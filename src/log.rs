//! A log: ~/.cache/explainonvideo/log.txt (started afresh each run) and stderr.
use std::io::Write;
use std::sync::Mutex;

static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn start() {
    let dir = crate::settings::cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    *FILE.lock().unwrap() = std::fs::File::create(dir.join("log.txt")).ok();
}

pub fn write(level: &str, message: &str) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let line = format!("{:.3} {} {}\n", now % 86400.0, level, message);
    eprint!("{line}");
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! info { ($($t:tt)*) => { $crate::log::write("INFO", &format!($($t)*)) } }
#[macro_export]
macro_rules! warn { ($($t:tt)*) => { $crate::log::write("WARNING", &format!($($t)*)) } }
