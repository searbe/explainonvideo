//! Explain on video: a small screen recorder for SteamOS (KDE Plasma desktop mode).
#[macro_use]
mod log;
mod app;
mod controls;
mod design;
mod devices;
mod drive;
mod edit;
mod editor;
mod engine;
mod export;
mod floating;
mod frame;
mod kwin;
mod kwin_cast;
mod layout;
mod portal;
mod preview;
mod selector;
mod settings;
mod settings_window;
mod smoke;
mod util;
use gstreamer as gst;

fn main() {
    std::env::set_var("GDK_BACKEND", "x11"); // (XWayland: windows that can be placed and kept on top)
    let args: Vec<String> = std::env::args().collect();
    glib::set_prgname(Some("explainonvideo"));
    glib::set_application_name("Explain on video");
    gst::init().expect("GStreamer");
    gtk::init().expect("GTK");
    log::start();
    // (the test modes: see the README)
    let path = |i: usize| args.get(i).cloned().unwrap_or_else(|| {
        eprintln!("{} needs more arguments (see the README)", args[1]);
        std::process::exit(2)
    });
    match args.get(1).map(String::as_str) {
        Some("--save-icon") => {
            let _ = design::save_app_icon(std::path::Path::new(&path(2)), 256);
            return;
        }
        Some("--engine-smoke") => return smoke::run(&path(2)),
        Some("--resize-test") => return smoke::resize_test(),
        _ => {}
    }
    design::install();
    match args.get(1).map(String::as_str) {
        Some("--ui-drive") => drive::run(&path(2)),
        Some("--window-drive") => drive::windows(&path(2), &path(3)),
        Some("--try-preview") => drive::try_preview(&path(2), args.get(3).cloned()),
        Some("--edit") => {
            let file = args.get(2).map(std::path::PathBuf::from).or_else(|| editor::choose_file(None));
            if let Some(p) = file {
                editor::Editor::open(&p);
                gtk::main();
            }
        }
        _ => app::App::new(args.get(1).map(String::as_str) != Some("--no-choose")).run(),
    }
    settings::save();
}
