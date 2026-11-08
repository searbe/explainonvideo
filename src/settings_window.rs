//! Settings: one page of setting rows, saved as they change and applied at once where they can be.
use crate::app::app;
use crate::design as d;
use crate::{devices, export, settings};
use gtk::prelude::*;
use serde_json::{json, Value};

fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

fn set(name: &str, value: Value) {
    if name == "virtual_camera" {
        if settings::bool(name) != value.as_bool().unwrap_or(false) {
            app().toggle(name);
        }
        return;
    }
    settings::set(name, value);
    app().settings_changed(name);
}

fn combo(name: &'static str, options: Vec<(Value, String)>) -> gtk::ComboBoxText {
    let c = gtk::ComboBoxText::new();
    for (_, label) in &options {
        c.append_text(label);
    }
    let current = settings::get(name);
    c.set_active(Some(options.iter().position(|(v, _)| same(v, &current)).unwrap_or(0) as u32));
    c.connect_changed(move |w| {
        if let Some(i) = w.active() {
            set(name, options[i as usize].0.clone());
        }
    });
    c
}

fn opts(list: &[(Value, &str)]) -> Vec<(Value, String)> {
    list.iter().map(|(v, l)| (v.clone(), l.to_string())).collect()
}

fn switch(name: &'static str) -> gtk::Switch {
    d::switch(settings::bool(name), move |v| set(name, v.into()))
}

pub fn window() -> gtk::Window {
    let win = d::classes(gtk::Window::builder().title("Settings · Explain on video").default_width(680)
        .default_height(760).build(), &["eov"]);
    let page = d::page_box();
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).width_request(640)
        .halign(gtk::Align::Center).build();
    let header = d::classes(gtk::Box::builder().spacing(8).build(), &["page-header"]);
    header.pack_start(&d::text("Settings", "headline"), true, true, 0);
    column.pack_start(&header, false, false, 0);
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_start(24).margin_end(24)
        .margin_bottom(24).build();
    column.pack_start(&body, false, false, 0);
    page.pack_start(&d::scrolled(&column), true, true, 0);
    win.add(&page);

    let group = |title: &str| body.pack_start(&d::group_header(title), false, false, 0);
    let row = |title: &str, desc: Option<&str>, control: &gtk::Widget| {
        let last_is_label = body.children().last().is_some_and(|w| w.is::<gtk::Label>());
        if !body.children().is_empty() && !last_is_label {
            body.pack_start(&d::hairline(false), false, false, 0);
        }
        body.pack_start(&d::setting_row(title, desc, control), false, false, 0);
    };

    group("Video");
    row("Size", Some("The video's size. “Same as the area” starts at the size of what you chose."),
        combo("output", opts(&[(json!("area"), "Same as the area"), (json!("720"), "720p (1280 × 720)"),
                               (json!("1080"), "1080p (1920 × 1080)"), (json!("1440"), "1440p (2560 × 1440)")]))
            .upcast_ref());
    row("When the area gets smaller", Some("How far a smaller area is enlarged to fill the video. Beyond that, the \
         background shows around it."),
        combo("max_zoom", opts(&[(json!(1.0), "Don't enlarge it"), (json!(1.5), "Enlarge up to 1.5×"),
                                 (json!(2.0), "Enlarge up to 2×"), (json!(3.0), "Enlarge up to 3×"),
                                 (json!(100.0), "Always fill the video")])).upcast_ref());
    row("Background", Some("Around an area that doesn't fill the video, and behind windows not recorded."),
        combo("background", opts(&[(json!("#111214"), "Dark grey"), (json!("#000000"), "Black"),
                                   (json!("#f6f6f7"), "Light grey"), (json!("#ffffff"), "White")])).upcast_ref());
    row("Frames per second", None, combo("fps", opts(&[(json!(30), "30"), (json!(60), "60")])).upcast_ref());
    row("Even out scaled screens", Some("On a HiDPI screen, zoom by the desktop's size rather than its pixels, so \
         text looks the same size as on a standard screen."), switch("hidpi").upcast_ref());
    row("Countdown", Some("Before recording starts."),
        combo("countdown", opts(&[(json!(0), "None"), (json!(3), "3 seconds"), (json!(5), "5 seconds")]))
            .upcast_ref());
    let enc_desc = format!("Using the graphics card keeps the computer free while recording.{}",
                           if export::hardware_works() { "" } else { " (Not available on this computer.)" });
    row("Encoding", Some(&enc_desc),
        combo("encoder", opts(&[(json!("auto"), "Automatic"), (json!("hardware"), "Graphics card"),
                                (json!("software"), "Processor")])).upcast_ref());

    group("Sound");
    let mut mics = vec![(json!(""), "The desktop's default".to_string())];
    mics.extend(devices::microphones().into_iter().map(|(n, desc)| (json!(n), desc)));
    row("Microphone", None, combo("mic_device", mics).upcast_ref());

    group("Camera");
    let mut cams = vec![(json!(""), "The first one".to_string())];
    cams.extend(devices::cameras().into_iter().map(|c| (json!(c.path), c.name)));
    let none = cams.len() == 1;
    row("Camera", none.then_some("No camera is plugged in."), combo("camera_device", cams).upcast_ref());
    row("Shape", None, combo("camera_shape", opts(&[(json!("circle"), "Circle"), (json!("rounded"), "Rounded (4:3)"),
                                                    (json!("wide"), "Wide (16:9)")])).upcast_ref());
    row("Mirror", Some("Show the camera as a mirror would."), switch("camera_mirror").upcast_ref());

    group("Virtual camera");
    row("Offer the video as a camera", Some("Other apps can pick “Explain on video” as their camera: browsers \
         (Chrome, Firefox), OBS and apps that use PipeWire cameras."), switch("virtual_camera").upcast_ref());

    group("Saving");
    let folder = d::text(&settings::string("folder"), "small");
    folder.set_ellipsize(pango::EllipsizeMode::Middle);
    folder.set_max_width_chars(40);
    let bx = gtk::Box::builder().spacing(8).build();
    bx.pack_start(&folder, true, true, 0);
    let (w2, f2) = (win.clone(), folder.clone());
    bx.pack_start(&d::button("Change…", "secondary", None, None, move || {
        let dialog = gtk::FileChooserNative::new(Some("Save recordings in"), Some(&w2),
                                                 gtk::FileChooserAction::SelectFolder, None, None);
        dialog.set_current_folder(settings::folder());
        if dialog.run() == gtk::ResponseType::Accept {
            if let Some(p) = dialog.filename() {
                settings::set("folder", json!(p.to_string_lossy()));
                f2.set_text(&p.to_string_lossy());
            }
        }
    }).button, false, false, 0);
    row("Folder", None, bx.upcast_ref());

    group("Screen");
    let forget = || {
        settings::set("restore_token", json!(""));
        app().settings_changed("restore_token");
    };
    if crate::kwin_cast::Cast::get().is_some_and(|c| c.can_stream_screens()) {
        row("The screen recorded", Some("The one the pointer was on when recording first started. Switch to record \
             the one it's on now."), d::button("Switch", "secondary", None, None, forget).button.upcast_ref());
    } else {
        row("The screen shared", Some("The desktop asked which screen to share and remembers it. Forget it to \
             choose again next time."), d::button("Forget", "danger", None, None, forget).button.upcast_ref());
    }
    win.show_all();
    win
}
