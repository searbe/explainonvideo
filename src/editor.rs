//! The editor: a few quick changes to a recording before sharing it. Split it into clips at the playhead (S), take
//! clips out (Delete), drag clips into another order, drag a clip's ends to trim it, crop the picture; undo any of
//! it (Ctrl+Z). Export makes a new MP4 beside the recording, which is never changed. The edits are kept beside it
//! too (recording.mp4.edit.json), so they're still there next time.
//!
//! Playback plays the edited video: the clips in their order, jumping over what was taken out.
use crate::controls::clock;
use crate::design::{self as d, Icon};
use crate::edit::{Crop, Edit};
use crate::export::{self, Export, Info, Progress};
use crate::settings;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const THUMB_H: f64 = 56.0; // the timeline's pictures
const RULER_H: f64 = 22.0;
const WAVE_H: f64 = 22.0;
const GAP: f64 = 4.0; // between clips
const EDGE: f64 = 8.0; // how near a clip's end a press takes hold of it
const STAGE_W: i32 = 1280; // the largest the picture is decoded for playback

pub fn choose_file(parent: Option<&gtk::Window>) -> Option<PathBuf> {
    let dialog = gtk::FileChooserNative::new(Some("Edit a recording"), parent, gtk::FileChooserAction::Open, None, None);
    dialog.set_current_folder(settings::folder());
    let f = gtk::FileFilter::new();
    f.set_name(Some("Videos"));
    for p in ["*.mp4", "*.mkv", "*.webm", "*.mov"] {
        f.add_pattern(p);
    }
    dialog.add_filter(f);
    let path = if dialog.run() == gtk::ResponseType::Accept { dialog.filename() } else { None };
    dialog.destroy();
    path
}

fn fmt(seconds: f64, tenths: bool) -> String {
    if tenths {
        let s = seconds.max(0.0);
        return format!("{}:{:04.1}", (s as i64) / 60, s % 60.0);
    }
    clock(seconds.max(0.0))
}

/// Whether GStreamer here can play path's video and sound itself. (SteamOS's has no H.264 or AAC decoders.)
fn can_decode(path: &Path) -> bool {
    let Ok(out) = std::process::Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=codec_name",
                                                             "-of", "csv=p=0"]).arg(path).output() else { return false };
    let decoders = gst::ElementFactory::factories_with_type(gst::ElementFactoryType::DECODER, gst::Rank::Marginal);
    for codec in String::from_utf8_lossy(&out.stdout).split_whitespace() {
        if codec.starts_with("pcm_") {
            continue;
        }
        let caps = match codec {
            "h264" => "video/x-h264", "aac" => "audio/mpeg, mpegversion=(int)4", "hevc" => "video/x-h265",
            "vp8" => "video/x-vp8", "vp9" => "video/x-vp9", "opus" => "audio/x-opus", "mjpeg" => "image/jpeg",
            "av1" => "video/x-av1", _ => "unknown/unknown",
        };
        let want: gst::Caps = caps.parse().unwrap();
        if !decoders.iter().any(|f| f.can_sink_any_caps(&want)) {
            return false;
        }
    }
    true
}

/// A copy of path that GStreamer here can play, for editing: smaller, every frame a JPEG (so it seeks exactly),
/// the sound uncompressed. Same timing as the original.
fn make_proxy(path: &Path, info: &Info, out: &Path, progress: &std::sync::atomic::AtomicU32) -> Result<(), String> {
    use std::io::BufRead;
    let w = info.width.min(960) / 2 * 2;
    let mut child = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"]).arg(path)
        .args(["-vf", &format!("scale={w}:-2"), "-c:v", "mjpeg", "-q:v", "5", "-fps_mode", "passthrough",
               "-c:a", "pcm_s16le", "-progress", "pipe:1", "-nostats"]).arg(out)
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some(v) = line.trim().strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
            let p = (v / 1e6 / info.duration.max(0.01)).min(1.0);
            progress.store((p * 1000.0) as u32, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err.trim().chars().rev().take(300).collect::<String>().chars().rev().collect());
    }
    Ok(())
}


/// Plays the recording (sound and a scaled-down picture); each picture to on_frame.
/// A button's way back to its own editor (the editor is made after its buttons).
#[derive(Clone, Default)]
struct Me(Rc<std::cell::OnceCell<std::rc::Weak<Editor>>>);

impl Me {
    fn act(&self, f: impl FnOnce(&Editor)) {
        if let Some(e) = self.0.get().and_then(std::rc::Weak::upgrade) {
            f(&e);
        }
    }

    fn on(&self, f: impl Fn(&Editor) + 'static) -> impl Fn() + 'static {
        let me = self.clone();
        move || me.act(&f)
    }
}

struct Player {
    bin: gst::Element,
    _watch: Option<gst::bus::BusWatchGuard>,
}

impl Player {
    fn new(path: &Path, info: &Info, on_frame: impl Fn(gdk_pixbuf::Pixbuf) + 'static) -> Option<Player> {
        let w = STAGE_W.min(info.width) / 4 * 4;
        let h = ((w as f64 * info.height as f64 / info.width as f64).round() as i32 / 2 * 2).max(2);
        let sink = gst::parse_bin_from_description(&format!(
            "videoconvert ! videoscale ! video/x-raw,format=RGB,width={w},height={h},pixel-aspect-ratio=1/1 \
             ! appsink name=frames max-buffers=2 drop=true"), true).ok()?;
        let appsink = sink.by_name("frames")?.downcast::<gst_app::AppSink>().ok()?;
        let (tx, rx) = crate::util::channel::<(Vec<u8>, i32, i32, i32)>();
        let send = move |sample: gst::Sample| {
            let (Some(buf), Some(caps)) = (sample.buffer(), sample.caps()) else { return };
            let s = caps.structure(0).unwrap();
            let (w, h) = (s.get::<i32>("width").unwrap_or(0), s.get::<i32>("height").unwrap_or(0));
            if let (Ok(map), true) = (buf.map_readable(), h > 0) {
                let _ = tx.send((map.to_vec(), w, h, (map.len() / h as usize) as i32));
            }
        };
        let send2 = send.clone();
        appsink.set_callbacks(gst_app::AppSinkCallbacks::builder()
            .new_sample(move |s| {
                if let Ok(sample) = s.pull_sample() { send(sample) }
                Ok(gst::FlowSuccess::Ok)
            })
            .new_preroll(move |s| {
                if let Ok(sample) = s.pull_preroll() { send2(sample) }
                Ok(gst::FlowSuccess::Ok)
            }).build());
        rx.attach(None, move |(data, w, h, stride)| {
            on_frame(gdk_pixbuf::Pixbuf::from_bytes(&glib::Bytes::from_owned(data), gdk_pixbuf::Colorspace::Rgb,
                                                     false, 8, w, h, stride));
            glib::ControlFlow::Continue
        });
        let bin = gst::ElementFactory::make("playbin").build().ok()?;
        let uri = glib::filename_to_uri(std::fs::canonicalize(path).ok()?, None).ok()?;
        bin.set_property("uri", uri.as_str());
        bin.set_property("video-sink", &sink);
        let watch = bin.bus()?.add_watch_local(|_, msg| { // (and so the bus doesn't fill up)
            if let gst::MessageView::Error(e) = msg.view() {
                crate::warn!("editor playback: {} ({:?})", e.error(), e.debug());
            }
            glib::ControlFlow::Continue
        }).ok();
        let _ = bin.set_state(gst::State::Paused);
        let _ = bin.state(gst::ClockTime::from_seconds(5));
        Some(Player { bin, _watch: watch })
    }

    fn seek(&self, t: f64) {
        let _ = self.bin.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                                     gst::ClockTime::from_nseconds((t.max(0.0) * 1e9) as u64));
    }

    fn position(&self) -> Option<f64> {
        self.bin.query_position::<gst::ClockTime>().map(|p| p.nseconds() as f64 / 1e9)
    }

    fn play(&self) {
        let _ = self.bin.set_state(gst::State::Playing);
    }

    fn pause(&self) {
        let _ = self.bin.set_state(gst::State::Paused);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.bin.set_state(gst::State::Null);
    }
}

#[derive(Clone)]
struct TlPress {
    what: &'static str, // scrub, move, start, end
    clip: usize,
    x0: f64,
    x: f64,
    moved: bool,
    grab: f64,
    orig: (f64, f64),
}

pub struct Editor {
    pub win: gtk::Window,
    path: PathBuf,
    info: Info,
    sidecar: PathBuf,
    pub edit: RefCell<Edit>,
    t: Cell<f64>, // the playhead, in the edited video
    pub clip: Cell<usize>, // the clip selected
    playing: Cell<bool>,
    playing_clip: Cell<usize>,
    frame: RefCell<Option<gdk_pixbuf::Pixbuf>>,
    cropping: Cell<bool>,
    crop_ratio: Cell<Option<f64>>,
    exporting: RefCell<Option<Export>>,
    out: RefCell<Option<PathBuf>>,
    thumbs: RefCell<Vec<gdk_pixbuf::Pixbuf>>,
    thumb_step: Cell<f64>,
    wave: RefCell<Option<(Vec<f64>, f64)>>,
    player: RefCell<Option<Player>>,
    preparing: std::sync::Arc<std::sync::atomic::AtomicU32>, // per mille; FAILED, or READY once playing
    tl_press: RefCell<Option<TlPress>>,
    tl_hover: Cell<Option<usize>>,
    stage_press: RefCell<Option<(f64, f64, &'static str, Crop)>>,
    // widgets
    subtitle: gtk::Label,
    undo_btn: gtk::Button,
    redo_btn: gtk::Button,
    export_btn: gtk::Button,
    export_row: gtk::Box,
    export_label: gtk::Label,
    progress: gtk::ProgressBar,
    export_cancel: gtk::Button,
    export_extra: RefCell<Vec<gtk::Widget>>,
    stage: gtk::DrawingArea,
    play_btn: (gtk::Button, Icon),
    time: gtk::Label,
    crop_tools: gtk::Box,
    ratio_buttons: RefCell<Vec<gtk::Button>>,
    hint: gtk::Label,
    delete_btn: gtk::Button,
    crop_btn: d::Button,
    timeline: gtk::DrawingArea,
}

const FAILED: u32 = 2000;
const READY: u32 = 3000;

thread_local! {
    static OPEN: RefCell<Vec<Rc<Editor>>> = const { RefCell::new(vec![]) };
}

fn cache_for(path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::canonicalize(path).unwrap_or(path.to_path_buf()).hash(&mut h);
    std::fs::metadata(path).and_then(|m| m.modified()).ok().hash(&mut h);
    settings::cache_dir().join("thumbs").join(format!("{:016x}", h.finish()))
}

fn keyhint(b: &gtk::Button, key: &str) {
    if let Some(bx) = b.child().and_then(|c| c.downcast::<gtk::Box>().ok()) {
        bx.pack_start(&d::classes(gtk::Label::new(Some(key)), &["keyhint"]), false, false, 0);
    }
}

impl Editor {
    /// Open path in an editor window.
    pub fn open(path: &Path) {
        let Some(info) = export::probe(path).filter(|i| i.duration > 0.0) else {
            let win = d::classes(gtk::Window::builder().title("Explain on video").default_width(600)
                .default_height(300).build(), &["eov", "editor"]);
            let bx = d::classes(gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8)
                .halign(gtk::Align::Center).valign(gtk::Align::Center).vexpand(true).build(), &["empty-state"]);
            bx.pack_start(&d::classes(Icon::new("film", 32).widget, &["faint"]), false, false, 0);
            bx.pack_start(&d::text("This video can't be opened.", "body"), false, false, 0);
            bx.pack_start(&d::text(&path.to_string_lossy(), "small"), false, false, 0);
            win.add(&bx);
            quit_with(&win);
            win.show_all();
            return;
        };
        let e = Editor::new(path, info);
        OPEN.with(|o| o.borrow_mut().push(e.clone()));
        let me = Rc::downgrade(&e);
        e.win.connect_destroy(move |_| {
            if let Some(me) = me.upgrade() {
                me.closed();
                OPEN.with(|o| o.borrow_mut().retain(|x| !Rc::ptr_eq(x, &me)));
            }
        });
        quit_with(&e.win);
        e.win.present();
    }

    fn new(path: &Path, info: Info) -> Rc<Editor> {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let win = d::classes(gtk::Window::builder().title(format!("{name} · Explain on video")).default_width(1180)
            .default_height(820).build(), &["eov", "editor"]);
        let sidecar = PathBuf::from(format!("{}.edit.json", path.display()));
        let edit = std::fs::read_to_string(&sidecar).map(|t| Edit::from_json(&t, info.duration))
            .unwrap_or_else(|_| Edit::new(info.duration));
        let me = Me::default();
        let page = d::page_box();
        // the header: name, what's been done, undo, redo, export
        let subtitle = d::text("", "small");
        let undo = d::icon_button("undo", "Undo (Ctrl+Z)", false, me.on(|e| e.undo()));
        let redo = d::icon_button("redo", "Redo (Ctrl+Shift+Z)", false, me.on(|e| e.redo()));
        let export_btn = d::button("Export…", "primary", Some("export"), None, me.on(|e| e.export())).button;
        let header = d::classes(gtk::Box::builder().spacing(8).build(), &["page-header"]);
        let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
        let title = d::text(&path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), "headline");
        title.set_ellipsize(pango::EllipsizeMode::End);
        words.pack_start(&title, false, false, 0);
        words.pack_start(&subtitle, false, false, 0);
        header.pack_start(&words, true, true, 0);
        header.pack_end(&export_btn, false, false, 0);
        header.pack_end(&redo.0, false, false, 0);
        header.pack_end(&undo.0, false, false, 0);
        page.pack_start(&header, false, false, 0);
        let export_row = d::classes(gtk::Box::builder().spacing(12).margin_bottom(12).build(), &["export-bar"]);
        let export_label = d::text("", "small");
        let progress = gtk::ProgressBar::builder().valign(gtk::Align::Center).hexpand(true).build();
        let export_cancel = d::button("Cancel", "quiet", None, None, me.on(|e| e.cancel_export())).button;
        export_row.pack_start(&export_label, false, false, 0);
        export_row.pack_start(&progress, true, true, 0);
        export_row.pack_start(&export_cancel, false, false, 0);
        page.pack_start(&export_row, false, false, 0);
        // the stage
        let stage = d::classes(gtk::DrawingArea::builder().vexpand(true).hexpand(true).build(), &["editor-stage"]);
        stage.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK
                         | gdk::EventMask::POINTER_MOTION_MASK);
        page.pack_start(&stage, true, true, 0);
        // the transport
        let bar = d::classes(gtk::Box::builder().spacing(6).build(), &["transport"]);
        let play_btn = d::icon_button("play", "Play (Space)", false, me.on(|e| e.play_pause()));
        bar.pack_start(&play_btn.0, false, false, 0);
        let time = d::classes(gtk::Label::builder().label("0:00").xalign(0.0).width_chars(14).build(),
                              &["strong", "tabular"]);
        bar.pack_start(&time, false, false, 6);
        let crop_tools = gtk::Box::builder().spacing(6).build();
        let (ratios, ratio_buttons) = d::segmented(&["Free", "16:9", "1:1", "9:16"], 0,
                                                   {
                                                       let me = me.clone();
                                                       move |i| me.act(|e| e.ratio(["Free", "16:9", "1:1", "9:16"][i]))
                                                   });
        crop_tools.pack_start(&ratios, false, false, 0);
        let reset = d::button("Reset", "quiet", None, None, me.on(|e| e.reset_crop())).button;
        d::classes(reset.clone(), &["small"]);
        crop_tools.pack_start(&reset, false, false, 0);
        bar.pack_start(&crop_tools, false, false, 0);
        let hint = d::text("", "caption");
        hint.set_ellipsize(pango::EllipsizeMode::End);
        bar.pack_start(&hint, true, true, 8);
        let split_btn = d::button("Split", "secondary", Some("split"), Some("Cut the clip in two at the playhead"),
                                  me.on(|e| e.split())).button;
        keyhint(&split_btn, "S");
        let delete_btn = d::button("Delete", "danger", Some("trash"), Some("Take the selected clip out"),
                                   me.on(|e| e.delete())).button;
        keyhint(&delete_btn, "Del");
        let crop_btn = d::button("Crop", "secondary", Some("crop"), Some("Choose the part of the picture to keep"),
                                 me.on(|e| e.toggle_crop()));
        keyhint(&crop_btn.button, "C");
        for b in [&split_btn, &delete_btn, &crop_btn.button] {
            bar.pack_start(b, false, false, 0);
        }
        page.pack_start(&bar, false, false, 0);
        // the timeline
        let tl_box = d::classes(gtk::Box::builder().orientation(gtk::Orientation::Vertical).build(), &["timeline-box"]);
        let timeline = gtk::DrawingArea::builder().height_request((RULER_H + THUMB_H + WAVE_H + 14.0) as i32)
            .can_focus(true).build();
        timeline.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK
                            | gdk::EventMask::POINTER_MOTION_MASK | gdk::EventMask::LEAVE_NOTIFY_MASK);
        tl_box.pack_start(&timeline, false, false, 0);
        page.pack_start(&tl_box, false, false, 0);
        win.add(&page);

        let e = Rc::new(Editor {
            win: win.clone(), path: path.to_path_buf(), info: info.clone(), sidecar, edit: RefCell::new(edit),
            t: Cell::new(0.0), clip: Cell::new(0), playing: Cell::new(false), playing_clip: Cell::new(0),
            frame: RefCell::new(None), cropping: Cell::new(false), crop_ratio: Cell::new(None),
            exporting: RefCell::new(None), out: RefCell::new(None), thumbs: RefCell::new(vec![]),
            thumb_step: Cell::new(1.0), wave: RefCell::new(None), player: RefCell::new(None),
            preparing: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)), tl_press: RefCell::new(None),
            tl_hover: Cell::new(None), stage_press: RefCell::new(None),
            subtitle, undo_btn: undo.0, redo_btn: redo.0, export_btn, export_row, export_label, progress,
            export_cancel, export_extra: RefCell::new(vec![]), stage: stage.clone(), play_btn, time, crop_tools,
            ratio_buttons: RefCell::new(ratio_buttons), hint, delete_btn, crop_btn, timeline: timeline.clone(),
        });
        let _ = me.0.set(Rc::downgrade(&e));
        // (the buttons' callbacks find this editor as the one last focused)
        let me = Rc::downgrade(&e);
        win.connect_focus_in_event(move |_, _| {
            if let Some(me) = me.upgrade() {
                OPEN.with(|o| {
                    let mut o = o.borrow_mut();
                    o.retain(|x| !Rc::ptr_eq(x, &me));
                    o.push(me);
                });
            }
            glib::Propagation::Proceed
        });
        let me = e.clone();
        stage.connect_draw(move |w, cr| {
            me.draw_stage(w, cr);
            glib::Propagation::Proceed
        });
        let me = e.clone();
        stage.connect_button_press_event(move |_, ev| {
            let (x, y) = ev.position();
            me.stage_press_at(x, y, ev.button());
            glib::Propagation::Stop
        });
        let me = e.clone();
        stage.connect_motion_notify_event(move |_, ev| {
            let (x, y) = ev.position();
            me.stage_motion(x, y);
            glib::Propagation::Stop
        });
        let me = e.clone();
        stage.connect_button_release_event(move |_, _| {
            me.stage_release();
            glib::Propagation::Stop
        });
        let me = e.clone();
        timeline.connect_draw(move |w, cr| {
            me.draw_timeline(w, cr);
            glib::Propagation::Proceed
        });
        let me = e.clone();
        timeline.connect_button_press_event(move |w, ev| {
            let (x, y) = ev.position();
            if ev.button() == 1 && me.exporting.borrow().is_none() {
                w.grab_focus();
                me.tl_press_at(x, y);
            }
            glib::Propagation::Stop
        });
        let me = e.clone();
        timeline.connect_motion_notify_event(move |w, ev| {
            let (x, y) = ev.position();
            me.tl_motion(w, x, y);
            glib::Propagation::Stop
        });
        let me = e.clone();
        timeline.connect_button_release_event(move |_, ev| {
            me.tl_release(ev.position().0);
            glib::Propagation::Stop
        });
        let me = e.clone();
        win.connect_key_press_event(move |_, ev| me.key(ev));
        win.show_all();
        e.crop_tools.hide();
        e.export_row.hide();
        e.changed(false);
        e.seek(0.0);
        e.start_player();
        e.make_thumbs();
        e.make_wave();
        let weak = Rc::downgrade(&e);
        glib::timeout_add_local(std::time::Duration::from_millis(30), move || match weak.upgrade() {
            Some(me) => {
                me.tick();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        e
    }

    fn closed(&self) {
        if let Some(x) = self.exporting.borrow().as_ref() {
            x.cancel();
        }
        self.player.borrow_mut().take();
    }

    fn preparing(&self) -> u32 {
        self.preparing.load(std::sync::atomic::Ordering::Relaxed)
    }

    // --- the edits ---
    fn changed(&self, save: bool) {
        {
            let e = self.edit.borrow();
            self.clip.set(self.clip.get().min(e.clips.len() - 1));
            self.t.set(self.t.get().min(e.length()));
            let n = e.clips.len();
            let cut = e.duration - e.length();
            let mut bits = vec![format!("{} long", fmt(e.length(), false)), format!("{n} clip{}", if n != 1 { "s" } else { "" })];
            if cut > 0.05 {
                bits.push(format!("{} taken out", fmt(cut, false)));
            }
            if let Some(c) = e.crop {
                bits.push(format!("cropped to {} × {}", (c.2 * self.info.width as f64).round(),
                                  (c.3 * self.info.height as f64).round()));
            }
            self.subtitle.set_text(&bits.join(" · "));
            self.undo_btn.set_sensitive(e.can_undo());
            self.redo_btn.set_sensitive(e.can_redo());
            self.delete_btn.set_sensitive(n > 1);
            self.hint.set_text(if n > 1 { "Drag clips to reorder them; drag their ends to trim them." }
                               else { "Split at the playhead to cut out or move a part." });
            if save {
                let _ = std::fs::write(&self.sidecar, e.to_json());
            }
        }
        self.timeline.queue_draw();
        self.stage.queue_draw();
        self.show_time();
    }

    pub fn split(&self) {
        let r = self.edit.borrow_mut().split(self.t.get());
        if let Some(i) = r {
            self.clip.set(i);
            self.changed(true);
        }
    }

    pub fn delete(&self) {
        let ok = self.edit.borrow_mut().delete(self.clip.get());
        if ok {
            let t = {
                let e = self.edit.borrow();
                e.starts()[self.clip.get().min(e.clips.len() - 1)]
            };
            self.t.set(t);
            self.changed(true);
            self.seek(t);
        }
    }

    fn undo(&self) {
        let ok = self.edit.borrow_mut().undo();
        if ok {
            self.changed(true);
            self.seek(self.t.get());
        }
    }

    fn redo(&self) {
        let ok = self.edit.borrow_mut().redo();
        if ok {
            self.changed(true);
            self.seek(self.t.get());
        }
    }

    pub fn move_clip(&self, i: usize, j: usize) {
        let ok = self.edit.borrow_mut().move_clip(i, j);
        if ok {
            self.clip.set(j);
            self.changed(true);
        }
    }

    // --- playing ---
    pub fn seek(&self, t: f64) {
        let (t, loc) = {
            let e = self.edit.borrow();
            let t = t.max(0.0).min(e.length());
            (t, e.locate(t))
        };
        self.t.set(t);
        if let Some((i, src)) = loc {
            self.playing_clip.set(i);
            if let Some(p) = self.player.borrow().as_ref() {
                p.seek(src);
            }
        }
        self.show_time();
        self.timeline.queue_draw();
    }

    pub fn length(&self) -> f64 {
        self.edit.borrow().length()
    }

    fn play_pause(&self) {
        if self.playing.get() {
            self.playing.set(false);
            if let Some(p) = self.player.borrow().as_ref() {
                p.pause();
            }
        } else {
            if self.t.get() >= self.length() - 0.05 {
                self.seek(0.0);
            }
            self.playing.set(true);
            self.seek(self.t.get());
            if let Some(p) = self.player.borrow().as_ref() {
                p.play();
            }
        }
        let playing = self.playing.get();
        self.play_btn.1.set_icon(if playing { "pause" } else { "play" });
        self.play_btn.0.set_tooltip_text(Some(if playing { "Pause (Space)" } else { "Play (Space)" }));
    }

    fn tick(&self) {
        if self.preparing() < READY {
            self.stage.queue_draw();
        }
        if !self.playing.get() {
            return;
        }
        let Some(pos) = self.player.borrow().as_ref().and_then(|p| p.position()) else { return };
        let i = self.playing_clip.get();
        let (clips, starts, len) = {
            let e = self.edit.borrow();
            (e.clips.clone(), e.starts(), e.length())
        };
        if i >= clips.len() {
            return;
        }
        let c = clips[i];
        if pos >= c.end - 0.02 || pos < c.start - 0.5 {
            if i + 1 < clips.len() {
                self.playing_clip.set(i + 1);
                if let Some(p) = self.player.borrow().as_ref() {
                    p.seek(clips[i + 1].start);
                }
                self.t.set(starts[i + 1]);
            } else {
                self.t.set(len);
                self.play_pause();
            }
        } else {
            self.t.set(self.edit.borrow().timeline_time(i, pos));
        }
        self.show_time();
        self.timeline.queue_draw();
    }

    fn show_time(&self) {
        self.time.set_text(&format!("{} / {}", fmt(self.t.get(), true), fmt(self.length(), false)));
    }

    fn key(&self, ev: &gdk::EventKey) -> glib::Propagation {
        use gdk::keys::constants as k;
        let key = ev.keyval();
        let ctrl = ev.state().contains(gdk::ModifierType::CONTROL_MASK);
        let shift = ev.state().contains(gdk::ModifierType::SHIFT_MASK);
        if self.exporting.borrow().is_some() {
            return glib::Propagation::Proceed;
        }
        if key == k::space {
            self.play_pause();
        } else if (key == k::s || key == k::S) && !ctrl {
            self.split();
        } else if key == k::Delete || key == k::BackSpace {
            self.delete();
        } else if (key == k::c || key == k::C) && !ctrl {
            self.toggle_crop();
        } else if (key == k::z || key == k::Z) && ctrl {
            if shift { self.redo() } else { self.undo() }
        } else if (key == k::y || key == k::Y) && ctrl {
            self.redo();
        } else if key == k::Left || key == k::Right {
            let step = if shift { 1.0 / self.info.fps } else { 1.0 };
            self.seek(self.t.get() + if key == k::Right { step } else { -step });
        } else if key == k::Home || key == k::End {
            self.seek(if key == k::Home { 0.0 } else { self.length() });
        } else if key == k::Escape && self.cropping.get() {
            self.toggle_crop();
        } else {
            return glib::Propagation::Proceed;
        }
        glib::Propagation::Stop
    }

    // --- the stage (the picture, and cropping it) ---
    fn picture_rect(&self) -> (f64, f64, f64, f64) {
        let a = self.stage.allocation();
        let (iw, ih) = (self.info.width as f64, self.info.height as f64);
        let pad = 24.0;
        let z = ((a.width() as f64 - 2.0 * pad) / iw).min((a.height() as f64 - 2.0 * pad) / ih);
        let (w, h) = (iw * z, ih * z);
        ((a.width() as f64 - w) / 2.0, (a.height() as f64 - h) / 2.0, w, h)
    }

    fn draw_stage(&self, widget: &gtk::DrawingArea, cr: &cairo::Context) {
        let a = widget.allocation();
        let (aw, ah) = (a.width() as f64, a.height() as f64);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        let _ = cr.paint();
        let (x, y, w, h) = self.picture_rect();
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let crop = self.edit.borrow().crop;
        let cropping = self.cropping.get();
        if let Some(frame) = self.frame.borrow().as_ref() {
            let _ = cr.save();
            let (fw, fh) = (frame.width() as f64, frame.height() as f64);
            match crop.filter(|_| !cropping) {
                Some((cx, cy, cw, ch)) => { // show what the export will be: the crop, as big as fits
                    let pad = 24.0;
                    let z = ((aw - 2.0 * pad) / (cw * w)).min((ah - 2.0 * pad) / (ch * h));
                    let (vw, vh) = (cw * w * z, ch * h * z);
                    let (vx, vy) = ((aw - vw) / 2.0, (ah - vh) / 2.0);
                    cr.rectangle(vx, vy, vw, vh);
                    cr.clip();
                    cr.translate(vx - cx * w * z, vy - cy * h * z);
                    cr.scale(w * z / fw, h * z / fh);
                }
                None => {
                    cr.translate(x, y);
                    cr.scale(w / fw, h / fh);
                }
            }
            cr.set_source_pixbuf(frame, 0.0, 0.0);
            let _ = cr.source().set_filter(cairo::Filter::Good);
            let _ = cr.paint();
            let _ = cr.restore();
        } else if self.preparing() < READY {
            let p = self.preparing();
            let words = if p == FAILED { "This video can't be played here.".to_string() }
                        else { format!("Getting the video ready to play… {} %", (p as f64 / 10.0).round()) };
            let (lay, tw, th) = d::layout(cr, &words, "small");
            d::set_source(cr, "text-2", Some(1.0));
            cr.move_to((aw - tw as f64) / 2.0, (ah - th as f64) / 2.0);
            d::show_layout(cr, &lay);
        }
        if cropping {
            let (cx, cy, cw, ch) = crop.unwrap_or((0.0, 0.0, 1.0, 1.0));
            let (rx, ry, rw, rh) = (x + cx * w, y + cy * h, cw * w, ch * h);
            cr.set_fill_rule(cairo::FillRule::EvenOdd);
            cr.rectangle(x, y, w, h);
            cr.rectangle(rx, ry, rw, rh);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
            let _ = cr.fill();
            let accent = d::rgba("accent");
            cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), 1.0);
            cr.set_line_width(2.0);
            cr.rectangle(rx, ry, rw, rh);
            let _ = cr.stroke();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.25); // thirds
            cr.set_line_width(1.0);
            for k in [1.0, 2.0] {
                cr.move_to(rx + rw * k / 3.0, ry);
                cr.line_to(rx + rw * k / 3.0, ry + rh);
                cr.move_to(rx, ry + rh * k / 3.0);
                cr.line_to(rx + rw, ry + rh * k / 3.0);
            }
            let _ = cr.stroke();
            for (_, (hx, hy)) in crop_handles((rx, ry, rw, rh)) {
                cr.arc(hx, hy, 6.0, 0.0, 2.0 * PI);
                cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), 1.0);
                cr.set_line_width(2.0);
                let _ = cr.stroke();
            }
            let words = format!("{} × {}", (cw * self.info.width as f64).round(), (ch * self.info.height as f64).round());
            let (lay, tw, th) = d::layout(cr, &words, "caption");
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.7);
            d::round_rect(cr, rx + 8.0, ry + 8.0, tw as f64 + 16.0, th as f64 + 8.0, d::R_SM);
            let _ = cr.fill();
            cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
            cr.move_to(rx + 16.0, ry + 12.0);
            d::show_layout(cr, &lay);
        }
    }

    pub fn toggle_crop(&self) {
        let c = !self.cropping.get();
        self.cropping.set(c);
        self.crop_btn.label.set_text(if c { "Done" } else { "Crop" });
        d::style(&self.crop_btn.button, "primary", c);
        self.crop_tools.set_visible(c);
        self.hint.set_visible(!c);
        self.stage.queue_draw();
    }

    pub fn ratio(&self, label: &str) {
        let r = match label { "16:9" => Some(16.0 / 9.0), "1:1" => Some(1.0), "9:16" => Some(9.0 / 16.0), _ => None };
        self.crop_ratio.set(r);
        if let Some(r) = r {
            let want = r * self.info.height as f64 / self.info.width as f64; // width share per height share
            let (cx, cy, cw, ch) = self.edit.borrow().crop.unwrap_or((0.0, 0.0, 1.0, 1.0));
            let (nw, nh) = if cw / ch > want { (ch * want, ch) } else { (cw, cw / want) };
            self.edit.borrow_mut().set_crop(Some((cx + (cw - nw) / 2.0, cy + (ch - nh) / 2.0, nw, nh)));
            self.changed(true);
        }
    }

    fn reset_crop(&self) {
        for (i, b) in self.ratio_buttons.borrow().iter().enumerate() {
            d::style(b, "on", i == 0);
        }
        self.crop_ratio.set(None);
        let ok = self.edit.borrow_mut().set_crop(None);
        if ok {
            self.changed(true);
        }
    }

    fn stage_press_at(&self, ex: f64, ey: f64, button: u32) {
        if !self.cropping.get() || button != 1 {
            if button == 1 && !self.cropping.get() {
                self.play_pause();
            }
            return;
        }
        let (x, y, w, h) = self.picture_rect();
        let crop = self.edit.borrow().crop.unwrap_or((0.0, 0.0, 1.0, 1.0));
        let r = (x + crop.0 * w, y + crop.1 * h, crop.2 * w, crop.3 * h);
        let mut what = crop_handles(r).into_iter().find(|(_, (hx, hy))| (ex - hx).hypot(ey - hy) < 12.0).map(|(k, _)| k);
        if what.is_none() && r.0 <= ex && ex <= r.0 + r.2 && r.1 <= ey && ey <= r.1 + r.3 {
            what = Some("move");
        }
        *self.stage_press.borrow_mut() = Some((ex, ey, what.unwrap_or("new"), crop)); // (else a new crop from here)
        self.edit.borrow_mut().begin_change();
    }

    fn stage_motion(&self, ex: f64, ey: f64) {
        let Some((px, py, what, (cx, cy, cw, ch))) = *self.stage_press.borrow() else { return };
        let (x, y, w, h) = self.picture_rect();
        let (dx, dy) = ((ex - px) / w, (ey - py) / h);
        let nx = ((ex - x) / w).clamp(0.0, 1.0);
        let ny = ((ey - y) / h).clamp(0.0, 1.0);
        let rect = match what {
            "move" => ((cx + dx).clamp(0.0, 1.0 - cw), (cy + dy).clamp(0.0, 1.0 - ch), cw, ch),
            "new" => {
                let (sx, sy) = (((px - x) / w).clamp(0.0, 1.0), ((py - y) / h).clamp(0.0, 1.0));
                let r = (sx.min(nx), sy.min(ny), (nx - sx).abs(), (ny - sy).abs());
                if self.crop_ratio.get().is_some() { self.fit_ratio(r, "se") } else { r }
            }
            _ => {
                let (mut x0, mut y0, mut x1, mut y1) = (cx, cy, cx + cw, cy + ch);
                if what.contains('w') { x0 = nx.min(x1 - 0.05) }
                if what.contains('e') { x1 = nx.max(x0 + 0.05) }
                if what.contains('n') { y0 = ny.min(y1 - 0.05) }
                if what.contains('s') { y1 = ny.max(y0 + 0.05) }
                let r = (x0, y0, x1 - x0, y1 - y0);
                if self.crop_ratio.get().is_some() { self.fit_ratio(r, what) } else { r }
            }
        };
        if rect.2 > 0.01 && rect.3 > 0.01 {
            self.edit.borrow_mut().crop = Some(rect); // (live; the undo point was made on press)
            self.stage.queue_draw();
        }
    }

    fn fit_ratio(&self, rect: Crop, what: &str) -> Crop {
        let (x, y, mut w, mut h) = rect;
        let want = self.crop_ratio.get().unwrap_or(1.0) * self.info.height as f64 / self.info.width as f64;
        match what {
            "n" | "s" => w = h * want,
            "e" | "w" => h = w / want,
            _ => {
                w = w.max(h * want);
                h = w / want;
            }
        }
        if x + w > 1.0 {
            w = 1.0 - x;
            h = w / want;
        }
        if y + h > 1.0 {
            h = 1.0 - y;
            w = h * want;
        }
        (x, y, w, h)
    }

    fn stage_release(&self) {
        if self.stage_press.borrow_mut().take().is_some() {
            {
                let mut e = self.edit.borrow_mut();
                if let Some(c) = e.crop {
                    if c.2 < 0.02 || c.3 < 0.02 || (c.2 > 0.995 && c.3 > 0.995) {
                        e.crop = None;
                    }
                }
            }
            self.changed(true);
        }
    }

    // --- the timeline ---
    /// Pixels per second, so the whole edited video fits the width.
    fn pps(&self) -> f64 {
        let a = self.timeline.allocation();
        let e = self.edit.borrow();
        let usable = (a.width() as f64 - GAP * (e.clips.len() as f64 - 1.0) - 2.0).max(50.0);
        usable / e.length().max(0.01)
    }

    /// Each clip's (x, width) on the timeline.
    fn blocks(&self) -> Vec<(f64, f64)> {
        let pps = self.pps();
        let mut x = 1.0;
        self.edit.borrow().clips.iter().map(|c| {
            let w = (c.length() * pps).max(2.0);
            let b = (x, w);
            x += w + GAP;
            b
        }).collect()
    }

    /// The x of time t (edited video) on the timeline.
    fn x_of(&self, t: f64) -> f64 {
        let pps = self.pps();
        let blocks = self.blocks();
        let e = self.edit.borrow();
        for (((x, _), c), s) in blocks.iter().zip(&e.clips).zip(e.starts()) {
            if t <= s + c.length() + 1e-6 {
                return x + (t - s) * pps;
            }
        }
        blocks.last().map(|(x, w)| x + w).unwrap_or(0.0)
    }

    /// The time (edited video) at x on the timeline, and the clip there.
    fn t_of(&self, px: f64) -> (f64, usize) {
        let pps = self.pps();
        let blocks = self.blocks();
        let e = self.edit.borrow();
        for (i, (((x, w), c), s)) in blocks.iter().zip(&e.clips).zip(e.starts()).enumerate() {
            if px <= x + w + GAP / 2.0 {
                return (s + ((px - x) / pps).clamp(0.0, c.length()), i);
            }
        }
        (e.length(), blocks.len() - 1)
    }

    fn draw_timeline(&self, widget: &gtk::DrawingArea, cr: &cairo::Context) {
        let a = widget.allocation();
        let pps = self.pps();
        let top = RULER_H;
        let len = self.length();
        // the ruler: a tick every so often
        let step = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 1e9].into_iter()
            .find(|s| s * pps >= 70.0).unwrap_or(1e9);
        let t3 = d::rgba("text-3");
        let mut t = 0.0;
        while t <= len + 1e-6 && step < 1e9 {
            let x = self.x_of(t);
            cr.set_source_rgba(t3.red(), t3.green(), t3.blue(), 0.6);
            cr.rectangle(x.floor(), top - 6.0, 1.0, 5.0);
            let _ = cr.fill();
            let (lay, tw, _) = d::layout(cr, &fmt(t, false), "caption");
            cr.set_source_rgba(t3.red(), t3.green(), t3.blue(), 1.0);
            cr.move_to((x + 3.0).min(a.width() as f64 - tw as f64), 2.0);
            d::show_layout(cr, &lay);
            t += step;
        }
        // the clips
        let blocks = self.blocks();
        let h = THUMB_H + WAVE_H;
        let accent = d::rgba("accent");
        let press = self.tl_press.borrow().clone();
        let dragging = press.as_ref().filter(|p| p.what == "move" && p.moved);
        let clips = self.edit.borrow().clips.clone();
        for (i, ((x, w), c)) in blocks.iter().zip(&clips).enumerate() {
            if dragging.is_some_and(|p| p.clip == i) {
                d::set_source(cr, "surface-3", Some(0.5));
                d::round_rect(cr, *x, top, *w, h, d::R_MD);
                let _ = cr.fill();
                continue;
            }
            self.draw_clip(cr, *x, top, *w, h, c.start, pps);
            if i == self.clip.get() {
                cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), 1.0);
                cr.set_line_width(2.0);
                d::round_rect(cr, x + 1.0, top + 1.0, w - 2.0, h - 2.0, d::R_MD);
                let _ = cr.stroke();
                for ex in [x + 3.0, x + w - 7.0] { // the ends, to trim by
                    cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
                    d::round_rect(cr, ex, top + h / 2.0 - 10.0, 4.0, 20.0, 2.0);
                    let _ = cr.fill();
                }
            }
        }
        if let Some(p) = dragging { // the clip being moved, under the pointer, and where it would go
            let c = clips[p.clip];
            let w = (c.length() * pps).max(2.0);
            cr.push_group();
            self.draw_clip(cr, p.x - p.grab, top - 4.0, w, h, c.start, pps);
            let _ = cr.pop_group_to_source();
            let _ = cr.paint_with_alpha(0.85);
            let j = self.drop_index(p.x, p.clip);
            let mut mx = if j < blocks.len() { blocks[j].0 - GAP / 2.0 }
                         else { blocks.last().unwrap().0 + blocks.last().unwrap().1 + GAP / 2.0 };
            if j > p.clip {
                mx = blocks[j].0 + blocks[j].1 + GAP / 2.0;
            }
            cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), 1.0);
            cr.rectangle(mx - 1.5, top - 6.0, 3.0, h + 12.0);
            let _ = cr.fill();
        }
        // the playhead
        let x = self.x_of(self.t.get());
        d::set_source(cr, "text", Some(1.0));
        cr.rectangle(x.floor() - 1.0, top - 8.0, 2.0, h + 14.0);
        let _ = cr.fill();
        cr.move_to(x - 6.0, top - 14.0);
        cr.line_to(x + 6.0, top - 14.0);
        cr.line_to(x, top - 6.0);
        cr.close_path();
        let _ = cr.fill();
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_clip(&self, cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, start: f64, pps: f64) {
        let _ = cr.save();
        d::round_rect(cr, x, y, w, h, d::R_MD);
        cr.clip();
        d::set_source(cr, "surface-3", Some(1.0));
        let _ = cr.paint();
        let thumbs = self.thumbs.borrow();
        if let Some(first) = thumbs.first() { // pictures
            let tw = first.width() as f64 * THUMB_H / first.height() as f64;
            let n = (w / tw).ceil().max(1.0) as usize;
            for k in 0..n {
                let src = start + (k as f64 * tw + tw / 2.0) / pps;
                let idx = ((src / self.thumb_step.get()) as usize).min(thumbs.len() - 1);
                let pix = &thumbs[idx];
                let _ = cr.save();
                cr.translate(x + k as f64 * tw, y);
                cr.scale(tw / pix.width() as f64, THUMB_H / pix.height() as f64);
                cr.set_source_pixbuf(pix, 0.0, 0.0);
                let _ = cr.paint();
                let _ = cr.restore();
            }
        }
        let base = y + THUMB_H; // sound
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.35);
        cr.rectangle(x, base, w, WAVE_H);
        let _ = cr.fill();
        if let Some((peaks, rate)) = self.wave.borrow().as_ref() {
            d::set_source(cr, "text-2", Some(0.8));
            let mid = base + WAVE_H / 2.0;
            for px in 0..(w as usize) {
                let i = ((start + px as f64 / pps) * rate) as usize;
                if let Some(p) = peaks.get(i) {
                    let v = p * (WAVE_H / 2.0 - 2.0);
                    cr.rectangle(x + px as f64, mid - v, 1.0, (v * 2.0).max(1.0));
                }
            }
            let _ = cr.fill();
        }
        let _ = cr.restore();
    }

    /// What a press on the timeline takes hold of: an end of a clip (to trim), a clip (to select or move), or the
    /// time (to move the playhead).
    fn tl_what(&self, ex: f64, ey: f64) -> (&'static str, Option<usize>) {
        if ey < RULER_H {
            return ("scrub", None);
        }
        for (i, (x, w)) in self.blocks().into_iter().enumerate() {
            if i == self.clip.get() || self.tl_hover.get() == Some(i) {
                if (ex - x).abs() <= EDGE && w > 2.0 * EDGE {
                    return ("start", Some(i));
                }
                if (ex - (x + w)).abs() <= EDGE && w > 2.0 * EDGE {
                    return ("end", Some(i));
                }
            }
            if x <= ex && ex <= x + w {
                return ("move", Some(i));
            }
        }
        ("scrub", None)
    }

    pub fn tl_press_at(&self, ex: f64, ey: f64) {
        let (what, i) = self.tl_what(ex, ey);
        let mut p = TlPress { what, clip: i.unwrap_or(0), x0: ex, x: ex, moved: false, grab: 0.0, orig: (0.0, 0.0) };
        match (what, i) {
            ("start" | "end", Some(i)) => {
                self.edit.borrow_mut().begin_change();
                let c = self.edit.borrow().clips[i];
                p.orig = (c.start, c.end);
                self.clip.set(i);
            }
            ("move", Some(i)) => {
                self.clip.set(i);
                p.grab = ex - self.blocks()[i].0;
                let (t, _) = self.t_of(ex);
                self.seek(t);
            }
            _ => {
                let (t, i) = self.t_of(ex);
                self.clip.set(i);
                p.clip = i;
                self.seek(t);
            }
        }
        *self.tl_press.borrow_mut() = Some(p);
        self.timeline.queue_draw();
    }

    fn tl_motion(&self, widget: &gtk::DrawingArea, ex: f64, ey: f64) {
        let press = self.tl_press.borrow().clone();
        let Some(mut p) = press else {
            let hover = self.blocks().into_iter().enumerate().filter(|(_, (x, w))| x - EDGE <= ex && ex <= x + w + EDGE)
                .map(|(i, _)| i).last();
            self.tl_hover.set(hover);
            let cursor = match self.tl_what(ex, ey).0 { "start" | "end" => "col-resize", "move" => "grab", _ => "text" };
            if let Some(g) = widget.window() {
                g.set_cursor(gdk::Cursor::from_name(&widget.display(), cursor).as_ref());
            }
            return;
        };
        p.x = ex;
        if (ex - p.x0).abs() > 4.0 {
            p.moved = true;
        }
        let pps = self.pps();
        let n = self.edit.borrow().clips.len();
        if p.what == "scrub" || (p.what == "move" && n == 1) {
            let (t, i) = self.t_of(ex);
            self.clip.set(i);
            self.seek(t);
        } else if (p.what == "start" || p.what == "end") && p.moved {
            let dt = (ex - p.x0) / pps;
            if p.what == "start" {
                self.edit.borrow_mut().trim(p.clip, Some(p.orig.0 + dt), None, false);
                let s = self.edit.borrow().starts()[p.clip];
                self.seek(s);
            } else {
                self.edit.borrow_mut().trim(p.clip, None, Some(p.orig.1 + dt), false);
                let (s, l) = {
                    let e = self.edit.borrow();
                    (e.starts()[p.clip], e.clips[p.clip].length())
                };
                self.seek(s + l - 0.001);
            }
        }
        *self.tl_press.borrow_mut() = Some(p);
        self.timeline.queue_draw();
    }

    fn drop_index(&self, px: f64, from: usize) -> usize {
        let blocks = self.blocks();
        for (j, (x, w)) in blocks.iter().enumerate() {
            if px < x + w / 2.0 {
                return if j <= from { j } else { j - 1 };
            }
        }
        blocks.len() - 1
    }

    fn tl_release(&self, ex: f64) {
        let Some(p) = self.tl_press.borrow_mut().take() else { return };
        let n = self.edit.borrow().clips.len();
        if p.what == "move" && p.moved && n > 1 {
            let j = self.drop_index(ex, p.clip);
            let ok = self.edit.borrow_mut().move_clip(p.clip, j);
            if ok {
                self.clip.set(j);
                let s = self.edit.borrow().starts()[j];
                self.seek(s);
            }
        } else if p.what == "start" || p.what == "end" {
            let c = self.edit.borrow().clips[p.clip];
            if (c.start, c.end) == p.orig { // (nothing changed: no undo point)
                self.edit.borrow_mut().undo();
                self.edit.borrow_mut().clear_redo();
            }
        }
        self.changed(true);
    }

    // --- pictures and sound for the timeline ---
    fn start_player(self: &Rc<Self>) {
        let (path, info, cache, progress) = (self.path.clone(), self.info.clone(), cache_for(&self.path),
                                              self.preparing.clone());
        let weak = Rc::downgrade(self);
        crate::util::in_thread(move || -> Option<PathBuf> {
            if can_decode(&path) {
                return Some(path);
            }
            let proxy = cache.join("proxy.mkv");
            if !cache.join("proxy.done").exists() {
                let _ = std::fs::create_dir_all(&cache);
                if let Err(e) = make_proxy(&path, &info, &proxy, &progress) {
                    crate::warn!("no playable copy: {e}");
                    return None;
                }
                let _ = std::fs::write(cache.join("proxy.done"), "1");
            }
            Some(proxy)
        }, move |playable| {
            let Some(me) = weak.upgrade() else { return };
            let Some(p) = playable else {
                me.preparing.store(FAILED, std::sync::atomic::Ordering::Relaxed);
                me.stage.queue_draw();
                return;
            };
            let weak2 = Rc::downgrade(&me);
            let player = Player::new(&p, &me.info, move |pix| {
                if let Some(me) = weak2.upgrade() {
                    *me.frame.borrow_mut() = Some(pix);
                    me.stage.queue_draw();
                }
            });
            if player.is_none() {
                me.preparing.store(FAILED, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            *me.player.borrow_mut() = player;
            me.preparing.store(READY, std::sync::atomic::Ordering::Relaxed);
            me.seek(me.t.get());
        });
    }

    fn make_thumbs(self: &Rc<Self>) {
        let (path, dur, folder) = (self.path.clone(), self.info.duration, cache_for(&self.path));
        let weak = Rc::downgrade(self);
        crate::util::in_thread(move || -> Option<(Vec<PathBuf>, f64)> {
            let mut step = (dur / 160.0).max(0.5);
            if !folder.join("done").exists() {
                let _ = std::fs::create_dir_all(&folder);
                for f in std::fs::read_dir(&folder).ok()?.flatten() {
                    if f.path().extension().is_some_and(|e| e == "jpg") {
                        let _ = std::fs::remove_file(f.path());
                    }
                }
                let ok = std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
                    .arg(&path).args(["-vf", &format!("fps=1/{step:.3},scale=-2:{}", THUMB_H as i32 * 2), "-q:v", "5"])
                    .arg(folder.join("%05d.jpg")).status().ok()?.success();
                if !ok {
                    return None;
                }
                std::fs::write(folder.join("done"), step.to_string()).ok()?;
            }
            step = std::fs::read_to_string(folder.join("done")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(step);
            let mut files: Vec<PathBuf> = std::fs::read_dir(&folder).ok()?.flatten().map(|f| f.path())
                .filter(|p| p.extension().is_some_and(|e| e == "jpg")).collect();
            files.sort();
            Some((files, step))
        }, move |got| {
            let (Some(me), Some((files, step))) = (weak.upgrade(), got) else { return };
            *me.thumbs.borrow_mut() = files.iter().filter_map(|f| gdk_pixbuf::Pixbuf::from_file(f).ok()).collect();
            me.thumb_step.set(step);
            me.timeline.queue_draw();
        });
    }

    fn make_wave(self: &Rc<Self>) {
        if !self.info.audio {
            return;
        }
        let path = self.path.clone();
        let weak = Rc::downgrade(self);
        crate::util::in_thread(move || -> Option<Vec<f64>> {
            let (rate_in, rate) = (8000usize, 100usize); // decoded at 8 kHz; a peak per 10 ms
            let out = std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
                .arg(&path).args(["-vn", "-ac", "1", "-ar", &rate_in.to_string(), "-f", "s16le", "-"]).output().ok()?;
            let samples: Vec<i16> = out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
            let mut peaks: Vec<f64> = samples.chunks(rate_in / rate)
                .map(|c| c.iter().map(|s| (*s as f64).abs()).fold(0.0, f64::max) / 32768.0).collect();
            let top = peaks.iter().cloned().fold(0.0, f64::max);
            if top > 0.0 {
                peaks.iter_mut().for_each(|p| *p = (*p / top).sqrt()); // (quieter speech still shows)
            }
            Some(peaks)
        }, move |peaks| {
            if let (Some(me), Some(p)) = (weak.upgrade(), peaks) {
                *me.wave.borrow_mut() = Some((p, 100.0));
                me.timeline.queue_draw();
            }
        });
    }

    // --- exporting ---
    pub fn export(&self) {
        if self.exporting.borrow().is_some() {
            return;
        }
        if self.cropping.get() {
            self.toggle_crop();
        }
        if self.playing.get() {
            self.play_pause();
        }
        let out = self.out_path();
        let cmd = export::edit_command(&self.path, &self.info, &self.edit.borrow(), &out, export::use_hardware());
        crate::info!("export: {}", cmd.join(" "));
        self.export_label.set_text(&format!("Exporting {}…", out.file_name().unwrap().to_string_lossy()));
        *self.out.borrow_mut() = Some(out);
        self.progress.set_fraction(0.0);
        for w in self.export_extra.borrow_mut().drain(..) {
            self.export_row.remove(&w);
        }
        self.export_row.show_all();
        self.export_btn.set_sensitive(false);
        let (bar, path) = (self.progress.clone(), self.path.clone());
        let x = Export::start(cmd, self.length(), move |p| match p {
            Progress::Done(f) => bar.set_fraction(f),
            Progress::Finished(err) => {
                OPEN.with(|o| o.borrow().iter().find(|e| e.path == path).cloned()).map(|e| e.exported(err));
            }
        });
        *self.exporting.borrow_mut() = Some(x);
    }

    fn out_path(&self) -> PathBuf {
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        for n in 1..1000 {
            let suffix = if n == 1 { String::new() } else { format!(" {n}") };
            let out = self.path.with_file_name(format!("{stem} (edited{suffix}).mp4"));
            if !out.exists() {
                return out;
            }
        }
        self.path.with_file_name(format!("{stem} (edited).mp4"))
    }

    fn cancel_export(&self) {
        if let Some(x) = self.exporting.borrow().as_ref() {
            x.cancel();
        }
    }

    fn exported(&self, err: Option<String>) {
        self.exporting.borrow_mut().take();
        self.export_btn.set_sensitive(true);
        let out = self.out.borrow().clone().unwrap_or_default();
        match err.as_deref() {
            Some("cancelled") => {
                let _ = std::fs::remove_file(&out);
                self.export_row.hide();
            }
            Some(e) => {
                self.export_label.set_text("The export didn't work.");
                self.export_label.set_tooltip_text(Some(e));
                self.progress.set_fraction(0.0);
                self.export_cancel.hide();
                crate::warn!("export: {e}");
            }
            None => {
                self.progress.set_fraction(1.0);
                self.export_label.set_text(&format!("Saved {}", out.file_name().unwrap_or_default().to_string_lossy()));
                let folder = out.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let show = d::button("Show in folder", "quiet", None, None, move || crate::app::open(&folder)).button;
                d::classes(show.clone(), &["small"]);
                let row = self.export_row.clone();
                let close = d::icon_button("close", "Hide", false, move || row.hide()).0;
                self.export_row.pack_start(&show, false, false, 0);
                self.export_row.pack_start(&close, false, false, 0);
                *self.export_extra.borrow_mut() = vec![show.upcast(), close.upcast()];
                self.export_row.show_all();
                self.export_cancel.hide();
            }
        }
    }
}

fn crop_handles(r: (f64, f64, f64, f64)) -> [(&'static str, (f64, f64)); 8] {
    let (x, y, w, h) = r;
    [("nw", (x, y)), ("n", (x + w / 2.0, y)), ("ne", (x + w, y)), ("e", (x + w, y + h / 2.0)), ("se", (x + w, y + h)),
     ("s", (x + w / 2.0, y + h)), ("sw", (x, y + h)), ("w", (x, y + h / 2.0))]
}

/// The editor last used (the one its buttons act on).
fn with_last(f: impl FnOnce(&Editor)) {
    let e = OPEN.with(|o| o.borrow().last().cloned());
    if let Some(e) = e {
        f(&e);
    }
}

/// When the editor is the whole program (--edit), closing it quits.
fn quit_with(win: &gtk::Window) {
    if !crate::app::exists() {
        win.connect_destroy(|_| {
            if OPEN.with(|o| o.borrow().is_empty()) {
                gtk::main_quit();
            }
        });
    }
}

/// For the UI drive: split, delete, move and crop in the last editor.
pub fn drive_edits() {
    with_last(|e| {
        e.seek(e.length() * 0.3);
        e.split();
        e.seek(e.length() * 0.6);
        e.split();
        e.clip.set(1);
        e.delete();
        e.move_clip(1, 0);
        e.toggle_crop();
        e.ratio("16:9");
    });
}

pub fn drive_export() {
    with_last(|e| {
        e.toggle_crop();
        e.export();
    });
}
