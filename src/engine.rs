//! The pipeline that makes the video: the screen, cut to the area and scaled into the video (with the background
//! around it), the camera on top, and the sound (what the computer plays, and the microphone) mixed. From it: a
//! small preview, the virtual camera, and the recording (to ffmpeg, which encodes it).
//!
//! Everything that can change while recording changes in place: the area (crop and zoom), which windows show,
//! the camera (on, off, where, how big) and the microphone. A recording is made of parts, one per stretch between
//! pauses; stop() joins them into the finished MP4.
use crate::kwin_cast;
use crate::layout::{self, Rect};
use crate::{design, devices, export, portal, settings};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, OwnedFd};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const PREVIEW_W: i32 = 480; // the preview's width (its height follows the video's shape)
const CAMERA_PX: i32 = 480; // the camera picture's height before it's placed

/// Camera shapes, by how wide they are per tall.
pub fn shape_aspect(shape: &str) -> f64 {
    match shape {
        "rounded" => 4.0 / 3.0,
        "wide" => 16.0 / 9.0,
        _ => 1.0,
    }
}

/// Where the screen's picture comes from: the portal's PipeWire stream, or a test pattern of the same size (for
/// trying things without sharing the screen). rect: its place on the desktop.
pub struct Screen {
    pub rect: Rect,
    pub stream: Option<Rc<portal::Stream>>,
}

impl Screen {
    /// The screen's source, and the portal's PipeWire connection it uses (kept open while the pipeline is).
    fn description(&self, fps: i64) -> Result<(String, Option<OwnedFd>), String> {
        match &self.stream {
            Some(s) if s.kwin.is_some() => {
                Ok((format!("pipewiresrc path={} always-copy=true keepalive-time=100 do-timestamp=true", s.node), None))
            }
            Some(s) => {
                let fd = s.fd().map_err(|e| format!("the screen's PipeWire connection couldn't be opened ({e})"))?;
                Ok((format!("pipewiresrc fd={} path={} always-copy=true keepalive-time=100 do-timestamp=true",
                            fd.as_raw_fd(), s.node), Some(fd)))
            }
            None => Ok((format!("videotestsrc is-live=true pattern=smpte horizontal-speed=4 ! \
                                 video/x-raw,width={},height={},framerate={fps}/1", self.rect.2, self.rect.3), None)),
        }
    }
}

/// What the streaming threads tell the main loop.
enum Event {
    Frame(Vec<u8>, i32, i32, i32),
    Caps(i32, i32),
    PartEos(u64),
    Detach(gst::Element, gst::Element, gst::Pad),
}

/// A chosen window's own stream (KWin's), placed on the area canvas. branch: its source and canvas pad, once
/// KWin has made the stream.
struct WinSource {
    id: String,
    rect: Rect,
    z: u32,
    branch: Option<(gst::Element, gst::Pad)>,
    /// The stream's own size: the window is drawn at that, never stretched (KWin's word of where it is can come a
    /// moment before or after a new size arrives, as when it goes full screen).
    size: Arc<Mutex<Option<(i32, i32)>>>,
    _stream: kwin_cast::Stream,
}

#[derive(Default)]
struct CanvasFit {
    flowing: (i32, i32), // the size of the canvas's frames reaching the video
    want: (i32, i32),    // the size it's been set to
    target: Option<Rect>,
}

impl CanvasFit {
    fn apply(&self, pad: &gst::Pad) {
        if let (true, Some(r)) = (self.flowing == self.want, self.target) {
            pad.set_property("xpos", r.0);
            pad.set_property("ypos", r.1);
            pad.set_property("width", r.2);
            pad.set_property("height", r.3);
        }
    }
}

/// What the screen's mask (drawn on a streaming thread) needs.
struct Mask {
    shown: Option<Vec<Rect>>,
    scale: (f64, f64),
    background: (f64, f64, f64),
}

pub type ErrorFn = Box<dyn Fn(&str, bool, Option<&str>)>;

pub struct Engine {
    pub screen: Screen,
    pub out_w: i32,
    pub out_h: i32,
    pub fps: i64,
    pub on_frame: RefCell<Option<Box<dyn Fn(gdk_pixbuf::Pixbuf)>>>,
    pub on_level: RefCell<Option<Box<dyn Fn(f64)>>>,
    pub on_error: RefCell<Option<ErrorFn>>,
    me: RefCell<Weak<Engine>>,
    pipe: gst::Pipeline,
    mix: gst::Element,
    amix: gst::Element,
    vt: gst::Element,
    at: gst::Element,
    crop: gst::Element,
    screen_pad: gst::Pad,
    screen_valve: gst::Element,
    mask: Arc<Mutex<Mask>>,
    shown_given: RefCell<Option<Vec<Rect>>>,
    // the chosen windows' own streams, composed on a canvas the area's size (in place of the screen)
    wmix: gst::Element,
    wcaps: gst::Element,
    canvas_pad: gst::Pad,
    /// Where the canvas goes in the video, put there only once frames of the size it's for arrive (a queue keeps a
    /// few frames of the old size: placed early, they'd be stretched).
    canvas_fit: Arc<Mutex<CanvasFit>>,
    _portal_fd: Option<OwnedFd>,
    canvas_size: Cell<(i32, i32)>,
    wins: RefCell<Vec<WinSource>>,
    windows_on: Cell<bool>,
    cast_failed: Cell<bool>,
    /// Following windows: the space between them taken out (layout::Squeeze), and the area without it.
    pack: Cell<bool>,
    squeeze: RefCell<Option<layout::Squeeze>>,
    canvas_area: Cell<Rect>,
    area: Cell<Rect>,
    crop_now: Cell<(i32, i32, i32, i32)>,
    place: Cell<Rect>, // where the area is in the video (moving towards target)
    target: Cell<Rect>,
    anim: RefCell<Option<glib::SourceId>>,
    camera: RefCell<Option<(gst::Element, gst::Pad)>>,
    camera_aspect: Cell<f64>,
    mic: RefCell<Option<(gst::Element, gst::Pad)>>,
    system: RefCell<Option<(gst::Element, gst::Pad)>>,
    vcam: RefCell<Option<(gst::Element, gst::Pad)>>,
    failed: RefCell<HashSet<String>>,
    tx: glib::Sender<Event>,
    bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
    // recording
    recorder: RefCell<Option<Recorder>>,
    finishing: RefCell<Vec<Recorder>>,
    next_part_id: Cell<u64>,
    parts_dir: RefCell<Option<PathBuf>>,
    pub final_path: RefCell<Option<PathBuf>>,
    parts: RefCell<Vec<PathBuf>>,
    pending: Cell<usize>,
    stopping: Cell<bool>,
    on_saved: RefCell<Option<Box<dyn Fn(PathBuf, Option<String>)>>>,
}

fn background_rgb() -> (f64, f64, f64) {
    let c = design::rgba(&settings::string("background"));
    (c.red(), c.green(), c.blue())
}

impl Engine {
    /// out_w × out_h video at fps from screen.
    pub fn new(screen: Screen, out_w: i32, out_h: i32, fps: i64) -> Result<Rc<Engine>, String> {
        let (r, g, b) = background_rgb();
        let bg = format!("0xff{:02x}{:02x}{:02x}", (r * 255.0).round() as u8, (g * 255.0).round() as u8,
                         (b * 255.0).round() as u8);
        let (w, h) = (out_w, out_h);
        let preview_h = layout::even(PREVIEW_W as f64 * h as f64 / w as f64);
        let pw = PREVIEW_W / 4 * 4; // (RGB rows a whole number of 4-byte words, as GdkPixbuf wants them)
        let (src, portal_fd) = screen.description(fps)?;
        let desc = format!(r#"
            compositor name=mix background=black ignore-inactive-pads=true
              ! video/x-raw,format=AYUV,width={w},height={h},framerate={fps}/1,pixel-aspect-ratio=1/1
              ! videoconvert n-threads=4 ! video/x-raw,format=I420 ! tee name=vt allow-not-linked=true
            videotestsrc is-live=true pattern=solid-color foreground-color={bg}
              ! video/x-raw,width=64,height=64,framerate={fps}/1 ! mix.sink_0
            {src} ! valve name=sv drop=false ! queue max-size-buffers=2 leaky=downstream
              ! videoconvert ! video/x-raw,format=BGRx ! videocrop name=crop ! cairooverlay name=mask
              ! queue max-size-buffers=2 leaky=downstream ! mix.sink_1
            compositor name=wmix ignore-inactive-pads=true
              ! capsfilter name=wcaps caps=video/x-raw,format=BGRA,width=16,height=16,framerate={fps}/1,pixel-aspect-ratio=1/1
              ! queue max-size-buffers=2 leaky=downstream ! mix.sink_2
            videotestsrc is-live=true pattern=solid-color foreground-color={bg}
              ! video/x-raw,width=16,height=16,framerate={fps}/1 ! wmix.sink_0
            vt. ! queue max-size-buffers=1 leaky=downstream ! videorate drop-only=true
              ! video/x-raw,framerate=15/1 ! videoscale ! videoconvert
              ! video/x-raw,format=RGB,width={pw},height={preview_h}
              ! appsink name=preview max-buffers=1 drop=true sync=false
            audiomixer name=amix ! audio/x-raw,format=S16LE,rate=48000,channels=2,layout=interleaved
              ! tee name=at allow-not-linked=true
            audiotestsrc is-live=true wave=silence ! audio/x-raw,rate=48000,channels=2 ! amix.
        "#);
        let pipe = gst::parse_launch(&desc).map_err(|e| e.to_string())?.downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline")?;
        pipe.use_clock(Some(&gst::SystemClock::obtain())); // (sources come and go: the clock mustn't go with them)
        let get = |n: &str| pipe.by_name(n).unwrap();
        let (mix, amix, vt, at, crop) = (get("mix"), get("amix"), get("vt"), get("at"), get("crop"));
        let bgpad = mix.static_pad("sink_0").unwrap();
        bgpad.set_property("xpos", 0i32);
        bgpad.set_property("ypos", 0i32);
        bgpad.set_property("width", w);
        bgpad.set_property("height", h);
        bgpad.set_property("zorder", 0u32);
        let screen_pad = mix.static_pad("sink_1").unwrap();
        screen_pad.set_property("zorder", 1u32);
        let (wmix, wcaps, screen_valve) = (get("wmix"), get("wcaps"), get("sv"));
        let canvas_pad = mix.static_pad("sink_2").unwrap();
        canvas_pad.set_property("zorder", 1u32);
        canvas_pad.set_property("alpha", 0.0f64);
        let canvas_fit = Arc::new(Mutex::new(CanvasFit { want: (16, 16), ..Default::default() }));
        let fit = canvas_fit.clone();
        canvas_pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |pad, info| {
            let Some(gst::PadProbeData::Event(ev)) = &info.data else { return gst::PadProbeReturn::Ok };
            let gst::EventView::Caps(c) = ev.view() else { return gst::PadProbeReturn::Ok };
            if let Some((Ok(w), Ok(h))) = c.caps().structure(0).map(|st| (st.get::<i32>("width"), st.get::<i32>("height"))) {
                let mut f = fit.lock().unwrap();
                f.flowing = (w, h);
                f.apply(pad);
            }
            gst::PadProbeReturn::Ok
        });
        wmix.static_pad("sink_0").unwrap().set_property("zorder", 0u32);
        let mask = Arc::new(Mutex::new(Mask { shown: None, scale: (1.0, 1.0), background: (r, g, b) }));
        let (tx, rx) = crate::util::channel::<Event>();

        let m = mask.clone();
        get("mask").connect("draw", false, move |args| {
            let cr = args[1].get::<cairo::Context>().ok()?;
            draw_mask(&m.lock().unwrap(), &cr);
            None
        });
        let t = tx.clone();
        crop.static_pad("sink").unwrap().add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            if let Some(gst::PadProbeData::Event(ev)) = &info.data {
                if let gst::EventView::Caps(c) = ev.view() {
                    if let Some(s) = c.caps().structure(0) {
                        if let (Ok(w), Ok(h)) = (s.get::<i32>("width"), s.get::<i32>("height")) {
                            let _ = t.send(Event::Caps(w, h));
                        }
                    }
                }
            }
            gst::PadProbeReturn::Ok
        });
        let t = tx.clone();
        let sink = get("preview").downcast::<gst_app::AppSink>().unwrap();
        sink.set_callbacks(gst_app::AppSinkCallbacks::builder().new_sample(move |s| {
            let sample = s.pull_sample().map_err(|_| gst::FlowError::Eos)?;
            let (Some(buf), Some(caps)) = (sample.buffer(), sample.caps()) else { return Ok(gst::FlowSuccess::Ok) };
            let st = caps.structure(0).unwrap();
            let (w, h) = (st.get::<i32>("width").unwrap_or(0), st.get::<i32>("height").unwrap_or(0));
            if let Ok(map) = buf.map_readable() {
                if h > 0 {
                    let stride = (map.len() / h as usize) as i32;
                    let _ = t.send(Event::Frame(map.to_vec(), w, h, stride));
                }
            }
            Ok(gst::FlowSuccess::Ok)
        }).build());

        let engine = Rc::new(Engine {
            screen, out_w, out_h, fps,
            on_frame: RefCell::new(None), on_level: RefCell::new(None), on_error: RefCell::new(None),
            me: RefCell::new(Weak::new()),
            pipe, mix, amix, vt, at, crop, screen_pad, screen_valve, mask, shown_given: RefCell::new(None),
            wmix, wcaps, canvas_pad, canvas_fit, _portal_fd: portal_fd, canvas_size: Cell::new((16, 16)), wins: RefCell::new(vec![]),
            windows_on: Cell::new(false), cast_failed: Cell::new(false),
            pack: Cell::new(false), squeeze: RefCell::new(None), canvas_area: Cell::new((0, 0, 0, 0)),
            area: Cell::new((0, 0, 0, 0)), crop_now: Cell::new((0, 0, 0, 0)),
            place: Cell::new((0, 0, out_w, out_h)), target: Cell::new((0, 0, out_w, out_h)),
            anim: RefCell::new(None),
            camera: RefCell::new(None), camera_aspect: Cell::new(1.0),
            mic: RefCell::new(None), system: RefCell::new(None), vcam: RefCell::new(None),
            failed: RefCell::new(HashSet::new()), tx, bus_watch: RefCell::new(None),
            recorder: RefCell::new(None), finishing: RefCell::new(vec![]), next_part_id: Cell::new(0),
            parts_dir: RefCell::new(None), final_path: RefCell::new(None), parts: RefCell::new(vec![]),
            pending: Cell::new(0), stopping: Cell::new(false), on_saved: RefCell::new(None),
        });
        *engine.me.borrow_mut() = Rc::downgrade(&engine);
        engine.area.set(engine.screen.rect);
        let weak = Rc::downgrade(&engine);
        rx.attach(None, move |ev| {
            match weak.upgrade() {
                Some(e) => {
                    e.event(ev);
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            }
        });
        let weak = Rc::downgrade(&engine);
        let watch = engine.pipe.bus().unwrap().add_watch_local(move |_, msg| {
            if let Some(e) = weak.upgrade() {
                e.message(msg);
            }
            glib::ControlFlow::Continue
        }).map_err(|e| e.to_string())?;
        *engine.bus_watch.borrow_mut() = Some(watch);
        engine.apply_place(engine.place.get());
        Ok(engine)
    }

    fn rc(&self) -> Rc<Engine> {
        self.me.borrow().upgrade().unwrap()
    }

    pub fn start(&self) {
        let shown = self.shown_given.borrow().clone();
        self.set_area(self.area.get(), shown, false);
        if settings::bool("system_audio") {
            self.set_system_audio(true);
        }
        if settings::bool("mic") {
            self.set_mic(true);
        }
        if settings::bool("camera") {
            self.set_camera(true);
        }
        if settings::bool("virtual_camera") {
            self.set_virtual_camera(true);
        }
        if self.pipe.set_state(gst::State::Playing).is_err() {
            self.fail("The screen couldn't be captured.", true, None);
        }
    }

    pub fn close(&self) {
        if let Some(r) = self.recorder.borrow_mut().take() {
            r.abandon(self);
        }
        self.wins.borrow_mut().clear(); // (closes KWin's streams)
        if let Some(id) = self.anim.borrow_mut().take() {
            id.remove();
        }
        let _ = self.pipe.set_state(gst::State::Null);
        self.bus_watch.borrow_mut().take();
    }

    fn event(&self, ev: Event) {
        match ev {
            Event::Frame(data, w, h, stride) => {
                let pix = gdk_pixbuf::Pixbuf::from_bytes(&glib::Bytes::from_owned(data),
                                                         gdk_pixbuf::Colorspace::Rgb, false, 8, w, h, stride);
                if let Some(f) = self.on_frame.borrow().as_ref() {
                    f(pix);
                }
            }
            Event::Caps(pw, ph) => {
                let (_, _, sw, sh) = self.screen.rect;
                if pw > 0 && ph > 0 && sw > 0 && sh > 0 {
                    self.mask.lock().unwrap().scale = (pw as f64 / sw as f64, ph as f64 / sh as f64);
                    let shown = self.shown_given.borrow().clone();
                    self.set_area(self.area.get(), shown, false);
                }
            }
            Event::PartEos(id) => self.part_eos(id),
            Event::Detach(b, tee, pad) => {
                let _ = b.set_state(gst::State::Null);
                let _ = self.pipe.remove(&b);
                tee.release_request_pad(&pad);
            }
        }
    }

    fn message(&self, msg: &gst::Message) {
        match msg.view() {
            gst::MessageView::Error(err) => {
                let src = msg.src().cloned();
                if !inside(src.as_ref(), self.pipe.upcast_ref()) {
                    return; // (from a camera or microphone already taken out)
                }
                let win = self.wins.borrow().iter()
                    .find(|w| w.branch.as_ref().is_some_and(|(b, _)| inside(src.as_ref(), b.upcast_ref())))
                    .map(|w| w.id.clone());
                if let Some(id) = win { // (a window's stream ended: leave it out)
                    crate::warn!("window {id}'s stream: {}", err.error());
                    let me = self.rc();
                    glib::idle_add_local_once(move || me.window_event(&id, kwin_cast::Event::Closed));
                    return;
                }
                let which = [("camera", &self.camera), ("mic", &self.mic), ("system_audio", &self.system),
                             ("virtual_camera", &self.vcam)].into_iter()
                    .find_map(|(name, b)| {
                        b.borrow().as_ref().filter(|(bin, _)| inside(src.as_ref(), bin.upcast_ref()))
                            .map(|(bin, _)| (name, bin.name().to_string()))
                    });
                if let Some((_, bin)) = &which {
                    if !self.failed.borrow_mut().insert(bin.clone()) {
                        return; // (one notice per failure)
                    }
                }
                crate::warn!("pipeline: {} ({:?})", err.error(), err.debug());
                let me = self.rc();
                match which.map(|(n, _)| n) {
                    Some("camera") => {
                        glib::idle_add_local_once(move || { me.set_camera(false); });
                        self.fail("The camera couldn't be used.", false, Some("camera"));
                    }
                    Some("mic") => {
                        glib::idle_add_local_once(move || me.set_mic(false));
                        self.fail("The microphone couldn't be used.", false, Some("mic"));
                    }
                    Some("system_audio") => {
                        glib::idle_add_local_once(move || me.set_system_audio(false));
                        self.fail("The computer's sound couldn't be recorded.", false, Some("system_audio"));
                    }
                    Some(_) => {
                        glib::idle_add_local_once(move || me.set_virtual_camera(false));
                        self.fail("The virtual camera couldn't be started.", false, Some("virtual_camera"));
                    }
                    None => {
                        let name = src.map(|s| s.name().to_string()).unwrap_or_default();
                        self.fail(&format!("Recording stopped: {} ({name})", err.error()), true, None);
                    }
                }
            }
            gst::MessageView::Element(el) => {
                let Some(s) = el.structure() else { return };
                if s.name() != "level" {
                    return;
                }
                let Ok(rms) = s.get::<glib::ValueArray>("rms") else { return };
                let db = rms.iter().filter_map(|v| v.get::<f64>().ok()).fold(f64::MIN, f64::max);
                if db > f64::MIN {
                    if let Some(f) = self.on_level.borrow().as_ref() {
                        f(((db + 55.0) / 45.0).clamp(0.0, 1.0));
                    }
                }
            }
            _ => {}
        }
    }

    /// Tell the app something went wrong: fatal (the pipeline stopped), or what was turned off because of it
    /// (camera, mic, system_audio, virtual_camera).
    fn fail(&self, message: &str, fatal: bool, what: Option<&str>) {
        if let Some(f) = self.on_error.borrow().as_ref() {
            f(message, fatal, what);
        }
    }

    // --- the area ---
    /// Record area (on the desktop), showing only the rectangles shown of it (relative to it), or all of it
    /// (None). The area is zoomed to fill the video, up to the zoom limit.
    pub fn set_area(&self, area: Rect, shown: Option<Vec<Rect>>, animate: bool) {
        let area = layout::clamp_rect(area, self.screen.rect);
        self.area.set(area);
        *self.shown_given.borrow_mut() = shown.clone();
        let scale = {
            let mut m = self.mask.lock().unwrap();
            m.shown = if self.windows_on.get() { None } else { shown };
            m.scale
        };
        if self.windows_on.get() {
            self.squeeze_windows();
            self.size_canvas();
            self.place_windows();
        }
        let new = layout::crop(area, self.screen.rect, scale.0, scale.1);
        let old = self.crop_now.get();
        let (n, o) = ([new.0, new.1, new.2, new.3], [old.0, old.1, old.2, old.3]);
        let names = ["left", "right", "top", "bottom"];
        let mut order: Vec<usize> = (0..4).collect();
        order.sort_by_key(|&i| n[i] - o[i]); // shrink first: the crop never outgrows the frame
        for i in order {
            if n[i] != o[i] {
                self.crop.set_property(names[i], n[i]);
            }
        }
        self.crop_now.set(new);
        let (mut aw, mut ah) = if self.windows_on.get() { let c = self.canvas_area.get(); (c.2, c.3) }
                               else { (area.2, area.3) };
        if !settings::bool("hidpi") {
            aw = (aw as f64 * scale.0) as i32;
            ah = (ah as f64 * scale.1) as i32;
        }
        let target = layout::fit(aw, ah, self.out_w, self.out_h, settings::f64("max_zoom"));
        if target != self.target.get() {
            self.target.set(target);
            // (glide only to the same shape: the picture's new shape is there at once, and a glide between
            // shapes would stretch it on the way)
            let now = self.place.get();
            let same_shape = now.2 > 0 && now.3 > 0
                && ((now.2 as f64 / now.3 as f64) / (target.2 as f64 / target.3 as f64) - 1.0).abs() < 0.02;
            if animate && same_shape && design::animations() {
                self.animate(target);
            } else {
                self.apply_place(target);
            }
        }
    }

    fn animate(&self, target: Rect) {
        let start = self.place.get();
        let began = Instant::now();
        if let Some(id) = self.anim.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(&self.rc());
        let id = glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            let Some(me) = weak.upgrade() else { return glib::ControlFlow::Break };
            let p = (began.elapsed().as_secs_f64() / design::MORPH).min(1.0);
            me.apply_place(layout::lerp_rect(start, target, design::ease(p)));
            if p >= 1.0 {
                me.anim.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.anim.borrow_mut() = Some(id);
    }

    fn apply_place(&self, rect: Rect) {
        self.place.set(rect);
        self.screen_pad.set_property("xpos", rect.0);
        self.screen_pad.set_property("ypos", rect.1);
        self.screen_pad.set_property("width", rect.2);
        self.screen_pad.set_property("height", rect.3);
        let mut f = self.canvas_fit.lock().unwrap();
        f.target = Some(rect);
        f.apply(&self.canvas_pad);
    }

    // --- the chosen windows' own streams ---
    /// Record only these windows ([(KWin id, rect on the desktop)], bottom first), each from its own stream, so
    /// nothing else shows, whatever covers them. Empty (or without KWin's streams): the screen, as it is. pack:
    /// put them side by side, without the space between them (they're followed, not in a drawn area).
    pub fn set_windows(&self, list: Vec<(String, Rect)>, pack: bool) {
        self.pack.set(pack);
        let cast = if list.is_empty() || self.cast_failed.get() { None } else { kwin_cast::Cast::get() };
        let Some(cast) = cast else {
            self.windows_off();
            return;
        };
        let gone: Vec<WinSource> = {
            let mut ws = self.wins.borrow_mut();
            let (keep, gone) = ws.drain(..).partition(|w| list.iter().any(|(id, _)| *id == w.id));
            *ws = keep;
            gone
        };
        for w in gone {
            self.drop_window(w);
        }
        for (i, (id, rect)) in list.iter().enumerate() {
            let z = i as u32 + 1;
            let mut ws = self.wins.borrow_mut();
            if let Some(w) = ws.iter_mut().find(|w| w.id == *id) {
                w.rect = *rect;
                w.z = z;
                if let Some((_, pad)) = &w.branch {
                    pad.set_property("zorder", z);
                }
                continue;
            }
            let weak = self.me.borrow().clone();
            let idc = id.clone();
            let stream = cast.stream_window(id, move |ev| {
                if let Some(e) = weak.upgrade() {
                    e.window_event(&idc, ev);
                }
            });
            ws.push(WinSource { id: id.clone(), rect: *rect, z, branch: None, size: Arc::new(Mutex::new(None)),
                                _stream: stream });
        }
        if !self.windows_on.replace(true) {
            self.screen_valve.set_property("drop", true); // (the screen isn't needed meanwhile)
            self.screen_pad.set_property("alpha", 0.0f64);
            self.canvas_pad.set_property("alpha", 1.0f64);
            self.mask.lock().unwrap().shown = None;
        }
    }

    fn windows_off(&self) {
        let gone: Vec<WinSource> = self.wins.borrow_mut().drain(..).collect();
        for w in gone {
            self.drop_window(w);
        }
        if self.windows_on.replace(false) {
            self.screen_valve.set_property("drop", false);
            self.screen_pad.set_property("alpha", 1.0f64);
            self.canvas_pad.set_property("alpha", 0.0f64);
            self.mask.lock().unwrap().shown = self.shown_given.borrow().clone();
            self.canvas_size.set((0, 0));
            self.set_canvas_caps(16, 16);
        }
    }

    fn drop_window(&self, w: WinSource) {
        if let Some((b, pad)) = &w.branch {
            self.remove(b, &self.wmix, pad);
        }
        drop(w); // (and KWin's stream with it)
    }

    fn window_event(&self, id: &str, ev: kwin_cast::Event) {
        match ev {
            kwin_cast::Event::Created(node) => {
                let desc = format!("pipewiresrc path={node} always-copy=true keepalive-time=100 do-timestamp=true \
                                    ! queue max-size-buffers=2 leaky=downstream ! videoconvert");
                let b = match gst::parse_bin_from_description(&desc, true) {
                    Ok(b) => b,
                    Err(e) => return crate::warn!("window {id}: {e}"),
                };
                let mut ws = self.wins.borrow_mut();
                let Some(w) = ws.iter_mut().find(|w| w.id == id && w.branch.is_none()) else { return };
                let _ = self.pipe.add(&b);
                let pad = self.wmix.request_pad_simple("sink_%u").unwrap();
                pad.set_property("zorder", w.z);
                w.branch = Some((b.clone().upcast(), pad.clone()));
                drop(ws);
                self.place_windows();
                let src = b.static_pad("src").unwrap();
                let size = self.wins.borrow().iter().find(|w| w.id == id).map(|w| w.size.clone()).unwrap();
                let mix_pad = pad.clone();
                src.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
                    // (a new size: drawn at it from its first frame, before the main loop hears of it)
                    let Some(gst::PadProbeData::Event(ev)) = &info.data else { return gst::PadProbeReturn::Ok };
                    let gst::EventView::Caps(c) = ev.view() else { return gst::PadProbeReturn::Ok };
                    let st = c.caps().structure(0).map(|st| (st.get::<i32>("width"), st.get::<i32>("height")));
                    if let Some((Ok(w), Ok(h))) = st {
                        *size.lock().unwrap() = Some((w, h));
                        mix_pad.set_property("width", w);
                        mix_pad.set_property("height", h);
                    }
                    gst::PadProbeReturn::Ok
                });
                let _ = src.link(&pad);
                start_branch(&self.pipe, b.upcast_ref());
            }
            kwin_cast::Event::Failed(message) => {
                crate::warn!("KWin couldn't stream window {id} ({message}): covering the rest of the screen instead");
                self.cast_failed.set(true);
                self.windows_off();
                let shown = self.shown_given.borrow().clone();
                self.set_area(self.area.get(), shown, false);
            }
            kwin_cast::Event::Closed => {
                let w = {
                    let mut ws = self.wins.borrow_mut();
                    ws.iter().position(|w| w.id == id).map(|i| ws.remove(i))
                };
                if let Some(w) = w {
                    self.drop_window(w);
                }
            }
        }
    }

    fn set_canvas_caps(&self, w: i32, h: i32) {
        {
            let mut f = self.canvas_fit.lock().unwrap();
            f.want = (w, h);
            f.target = None; // (placed for this size once it's worked out, and frames of it arrive)
        }
        let caps = gst::Caps::builder("video/x-raw").field("format", "BGRA").field("width", w).field("height", h)
            .field("framerate", gst::Fraction::new(self.fps as i32, 1))
            .field("pixel-aspect-ratio", gst::Fraction::new(1, 1)).build();
        self.wcaps.set_property("caps", &caps);
        let bg = self.wmix.static_pad("sink_0").unwrap();
        bg.set_property("width", w);
        bg.set_property("height", h);
    }

    /// Where the windows go on the canvas: where they are in the area, or (pack) closed up.
    fn squeeze_windows(&self) {
        let sq = self.pack.get().then(|| {
            layout::Squeeze::new(&self.wins.borrow().iter().map(|w| w.rect).collect::<Vec<_>>())
        });
        let area = self.area.get();
        self.canvas_area.set(sq.as_ref().map_or(area, |s| s.rect(area)));
        *self.squeeze.borrow_mut() = sq;
    }

    /// The canvas: the area's size (without the space between windows, if packed) in the screen's pixels.
    fn size_canvas(&self) {
        let (_, _, aw, ah) = self.canvas_area.get();
        let (sx, sy) = self.mask.lock().unwrap().scale;
        let size = (layout::even(aw as f64 * sx), layout::even(ah as f64 * sy));
        if size != self.canvas_size.get() {
            self.canvas_size.set(size);
            self.set_canvas_caps(size.0, size.1);
        }
    }

    /// Put each window on the canvas where it is in the area (parts outside the area are cut off by its edges).
    fn place_windows(&self) {
        let (ax, ay, _, _) = self.canvas_area.get();
        let (sx, sy) = self.mask.lock().unwrap().scale;
        let sq = self.squeeze.borrow();
        for w in self.wins.borrow().iter() {
            let Some((_, pad)) = &w.branch else { continue };
            let (x, y, ww, wh) = sq.as_ref().map_or(w.rect, |s| s.rect(w.rect));
            pad.set_property("xpos", ((x - ax) as f64 * sx).round() as i32);
            pad.set_property("ypos", ((y - ay) as f64 * sy).round() as i32);
            // (the stream's pixels are the screen's: KWin streams a window at its screen's scale)
            let (pw, ph) = w.size.lock().unwrap()
                .unwrap_or(((ww as f64 * sx).round() as i32, (wh as f64 * sy).round() as i32));
            pad.set_property("width", pw);
            pad.set_property("height", ph);
        }
    }

    // --- the camera ---
    pub fn camera_on(&self) -> bool {
        self.camera.borrow().is_some()
    }

    /// Show the camera (opening it) or take it away (closing it).
    pub fn set_camera(&self, on: bool) -> bool {
        if on && self.camera.borrow().is_none() {
            let cam = devices::camera(&settings::string("camera_device"));
            if cam.is_none() && std::env::var_os("EOV_TEST_CAMERA").is_none() {
                self.fail("There's no camera plugged in.", false, Some("camera"));
                return false;
            }
            let source = cam.map(|c| c.source()).unwrap_or_else(|| {
                "videotestsrc is-live=true pattern=ball ! video/x-raw,width=1280,height=720,framerate=30/1".into()
            });
            let shape = settings::string("camera_shape");
            let aspect = shape_aspect(&shape);
            let (ch, cw) = (CAMERA_PX, layout::even(CAMERA_PX as f64 * aspect));
            let flip = if settings::bool("camera_mirror") { "videoflip method=horizontal-flip !" } else { "" };
            let desc = format!("{source} ! videoconvert ! {flip} aspectratiocrop aspect-ratio={cw}/{ch} ! videoscale \
                                ! video/x-raw,width={cw},height={ch},pixel-aspect-ratio=1/1 ! videoconvert \
                                ! video/x-raw,format=BGRA ! cairooverlay name=shape \
                                ! queue max-size-buffers=2 leaky=downstream");
            let b = match gst::parse_bin_from_description(&desc, true) {
                Ok(b) => b,
                Err(e) => {
                    self.fail(&format!("The camera couldn't be opened ({e})."), false, Some("camera"));
                    return false;
                }
            };
            b.by_name("shape").unwrap().connect("draw", false, move |args| {
                let cr = args[1].get::<cairo::Context>().ok()?;
                draw_shape(&cr, &shape);
                None
            });
            self.camera_aspect.set(aspect);
            let _ = self.pipe.add(&b);
            let pad = self.mix.request_pad_simple("sink_%u").unwrap();
            pad.set_property("zorder", 2u32);
            *self.camera.borrow_mut() = Some((b.clone().upcast(), pad.clone()));
            self.place_camera();
            let _ = b.static_pad("src").unwrap().link(&pad);
            start_branch(&self.pipe, b.upcast_ref());
        } else if !on {
            let taken = self.camera.borrow_mut().take();
            if let Some((b, pad)) = taken {
                self.remove(&b, &self.mix, &pad);
            }
        }
        true
    }

    /// Put the camera where the settings say (camera_x, camera_y, camera_size).
    pub fn place_camera(&self) {
        if let Some((_, pad)) = self.camera.borrow().as_ref() {
            let r = self.camera_rect();
            pad.set_property("xpos", r.0);
            pad.set_property("ypos", r.1);
            pad.set_property("width", r.2);
            pad.set_property("height", r.3);
        }
    }

    pub fn camera_rect(&self) -> Rect {
        layout::camera_rect(self.out_w, self.out_h, settings::f64("camera_x"), settings::f64("camera_y"),
                            settings::f64("camera_size"), self.camera_aspect.get())
    }

    // --- sound ---
    pub fn set_mic(&self, on: bool) {
        if on && self.mic.borrow().is_none() {
            let Some(device) = devices::microphone(&settings::string("mic_device")) else {
                self.fail("There's no microphone.", false, Some("mic"));
                return;
            };
            let dev = if device.is_empty() { String::new() } else { format!("device=\"{device}\"") };
            let b = self.add_audio(&format!("pulsesrc {dev} client-name=explainonvideo ! audioconvert ! audioresample \
                                             ! audio/x-raw,rate=48000,channels=2 ! level interval=100000000 \
                                             ! queue max-size-time=500000000 leaky=downstream"));
            *self.mic.borrow_mut() = b;
        } else if !on {
            let taken = self.mic.borrow_mut().take();
            if let Some((b, pad)) = taken {
                self.remove(&b, &self.amix, &pad);
                if let Some(f) = self.on_level.borrow().as_ref() {
                    f(0.0);
                }
            }
        }
    }

    pub fn set_system_audio(&self, on: bool) {
        if on && self.system.borrow().is_none() {
            let b = self.add_audio("pulsesrc device=@DEFAULT_MONITOR@ client-name=explainonvideo \
                                    ! audioconvert ! audioresample ! audio/x-raw,rate=48000,channels=2 \
                                    ! queue max-size-time=500000000 leaky=downstream");
            *self.system.borrow_mut() = b;
        } else if !on {
            let taken = self.system.borrow_mut().take();
            if let Some((b, pad)) = taken {
                self.remove(&b, &self.amix, &pad);
            }
        }
    }

    fn add_audio(&self, desc: &str) -> Option<(gst::Element, gst::Pad)> {
        let b = gst::parse_bin_from_description(desc, true).map_err(|e| crate::warn!("audio: {e}")).ok()?;
        let _ = self.pipe.add(&b);
        let pad = self.amix.request_pad_simple("sink_%u")?;
        let _ = b.static_pad("src")?.link(&pad);
        start_branch(&self.pipe, b.upcast_ref());
        Some((b.upcast(), pad))
    }

    /// Take a source out while playing: its mixer pad first (flushing it frees the source's thread, which may be
    /// waiting on the mixer), then the source.
    fn remove(&self, b: &gst::Element, mixer: &gst::Element, mixpad: &gst::Pad) {
        mixer.release_request_pad(mixpad);
        let _ = b.set_state(gst::State::Null);
        let _ = self.pipe.remove(b);
        if self.pipe.state(gst::ClockTime::ZERO).0.is_err() { // (it was what failed: carry on without it)
            let _ = self.pipe.set_state(gst::State::Playing);
        }
    }

    // --- the virtual camera ---
    /// Offer the video to other apps as a camera (a PipeWire camera: browsers, OBS, …).
    pub fn set_virtual_camera(&self, on: bool) {
        if on && self.vcam.borrow().is_none() {
            let desc = "queue max-size-buffers=2 leaky=downstream ! videoconvert ! video/x-raw,format=I420 \
                        ! pipewiresink mode=provide sync=false client-name=explainonvideo \
                        stream-properties=\"props,media.class=Video/Source,node.name=explainonvideo,\
                        node.description=Explain\\ on\\ video,media.role=Camera\"";
            let b = match gst::parse_bin_from_description(desc, true) {
                Ok(b) => b,
                Err(e) => {
                    self.fail(&format!("The virtual camera couldn't be started ({e})."), false,
                              Some("virtual_camera"));
                    return;
                }
            };
            let _ = self.pipe.add(&b);
            let pad = self.vt.request_pad_simple("src_%u").unwrap();
            let _ = pad.link(&b.static_pad("sink").unwrap());
            start_branch(&self.pipe, b.upcast_ref());
            *self.vcam.borrow_mut() = Some((b.upcast(), pad));
        } else if !on {
            let taken = self.vcam.borrow_mut().take();
            if let Some((b, pad)) = taken {
                self.detach(&self.vt, &pad, &b);
            }
        }
    }

    /// Take branch b off tee's pad (while it plays).
    fn detach(&self, tee: &gst::Element, pad: &gst::Pad, b: &gst::Element) {
        let tx = self.tx.clone();
        let (tee, b2) = (tee.clone(), b.clone());
        pad.add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |p, _| {
            if let Some(sink) = b2.static_pad("sink") {
                let _ = p.unlink(&sink);
            }
            let _ = tx.send(Event::Detach(b2.clone(), tee.clone(), p.clone()));
            gst::PadProbeReturn::Remove
        });
    }

    // --- recording ---
    /// Get ready to record into folder (during the countdown): where its parts and the finished file go.
    pub fn prepare(&self, folder: &std::path::Path) {
        let stamp = glib::DateTime::now_local().ok().and_then(|d| d.format("%Y-%m-%d %H.%M.%S").ok())
            .map(|s| s.to_string()).unwrap_or_else(|| "recording".into());
        let dir = folder.join(".parts").join(&stamp);
        let _ = std::fs::create_dir_all(&dir);
        *self.parts_dir.borrow_mut() = Some(dir);
        *self.final_path.borrow_mut() = Some(folder.join(format!("Recording {stamp}.mp4")));
        self.parts.borrow_mut().clear();
        self.pending.set(0);
        self.stopping.set(false);
    }

    pub fn unprepare(&self) {
        if let Some(d) = self.parts_dir.borrow_mut().take() {
            rmtree(&d);
        }
    }

    /// Start recording (a first part; pause() and resume() make more). on_saved(path, error) when stop() has
    /// made the finished file.
    pub fn record(&self, on_saved: impl Fn(PathBuf, Option<String>) + 'static) {
        *self.on_saved.borrow_mut() = Some(Box::new(on_saved));
        self.resume();
    }

    pub fn resume(&self) {
        let Some(dir) = self.parts_dir.borrow().clone() else { return };
        let part = dir.join(format!("part{:03}.mkv", self.parts.borrow().len()));
        self.parts.borrow_mut().push(part.clone());
        let id = self.next_part_id.get();
        self.next_part_id.set(id + 1);
        match Recorder::new(self, id, &part) {
            Ok(r) => *self.recorder.borrow_mut() = Some(r),
            Err(e) => self.fail(&format!("Recording couldn't start: {e}"), true, None),
        }
    }

    /// End the current part (the video carries on from here when resumed).
    pub fn pause(&self) {
        let taken = self.recorder.borrow_mut().take();
        if let Some(rec) = taken {
            self.pending.set(self.pending.get() + 1);
            rec.finish(self);
            self.finishing.borrow_mut().push(rec);
        }
    }

    fn part_eos(&self, id: u64) {
        let pos = self.finishing.borrow().iter().position(|r| r.id == id);
        if let Some(i) = pos {
            let rec = self.finishing.borrow_mut().remove(i);
            rec.close(self);
        }
    }

    fn part_done(&self, error: Option<String>) {
        self.pending.set(self.pending.get().saturating_sub(1));
        if let Some(e) = error {
            crate::warn!("a part didn't save: {e}");
        }
        if self.pending.get() == 0 && self.stopping.get() {
            self.join();
        }
    }

    /// Finish recording: the last part, then all of them joined into the MP4 (on_saved, later).
    pub fn stop(&self) {
        self.stopping.set(true);
        if self.recorder.borrow().is_some() {
            self.pause();
        } else if self.pending.get() == 0 {
            self.join();
        }
    }

    /// Stop recording and throw it away.
    pub fn discard(&self) {
        self.on_saved.borrow_mut().take();
        let taken = self.recorder.borrow_mut().take();
        if let Some(r) = taken {
            r.abandon(self);
        }
        if let Some(d) = self.parts_dir.borrow().clone() {
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || rmtree(&d));
        }
    }

    fn join(&self) {
        self.stopping.set(false);
        let parts = self.parts.borrow().clone();
        let Some(fin) = self.final_path.borrow().clone() else { return };
        let on_saved = self.on_saved.borrow_mut().take();
        let f2 = fin.clone();
        crate::util::in_thread(move || {
            let r = export::join(&parts, &f2);
            if r.is_ok() {
                if let Some(p) = parts.first().and_then(|p| p.parent()) {
                    rmtree(p);
                }
            }
            r.err()
        }, move |error| {
            if let Some(f) = on_saved {
                f(fin, error);
            }
        });
    }
}

/// Cover what isn't to be shown (parts of the area outside the chosen windows) with the background.
fn draw_mask(m: &Mask, cr: &cairo::Context) {
    let Some(shown) = &m.shown else { return };
    let (sx, sy) = m.scale;
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    cr.rectangle(-1.0, -1.0, 100000.0, 100000.0);
    for (x, y, w, h) in shown {
        cr.rectangle(*x as f64 * sx, *y as f64 * sy, *w as f64 * sx, *h as f64 * sy);
    }
    cr.set_source_rgb(m.background.0, m.background.1, m.background.2);
    let _ = cr.fill();
}

/// Cut the camera's picture to its shape (clear outside it), with a faint rim.
fn draw_shape(cr: &cairo::Context, shape: &str) {
    let Ok(target) = cairo::ImageSurface::try_from(cr.target()) else { return };
    let (w, h) = (target.width() as f64, target.height() as f64);
    cr.new_path();
    if shape == "circle" {
        cr.arc(w / 2.0, h / 2.0, w.min(h) / 2.0, 0.0, 2.0 * std::f64::consts::PI);
    } else {
        design::round_rect(cr, 0.0, 0.0, w, h, h * 0.08);
    }
    let Ok(path) = cr.copy_path() else { return };
    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    cr.rectangle(0.0, 0.0, w, h);
    cr.set_operator(cairo::Operator::Clear);
    let _ = cr.fill();
    cr.set_operator(cairo::Operator::Over);
    cr.append_path(&path);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.18);
    cr.set_line_width((h / 120.0).max(2.0));
    let _ = cr.stroke();
}

/// One part of a recording: the video and sound from the engine's tees, muxed (Matroska, raw) into ffmpeg, which
/// encodes it to path.
struct Recorder {
    id: u64,
    bin: gst::Bin,
    vpad: gst::Pad,
    apad: gst::Pad,
    write: RefCell<Option<std::io::PipeWriter>>,
    proc: Arc<Mutex<Option<std::process::Child>>>,
    closed: Cell<bool>,
}

impl Recorder {
    fn new(engine: &Engine, id: u64, path: &std::path::Path) -> Result<Recorder, String> {
        let (read, write) = std::io::pipe().map_err(|e| e.to_string())?;
        let cmd = export::record_command(path, engine.fps);
        crate::info!("recording: {}", cmd.join(" "));
        let child = std::process::Command::new(&cmd[0]).args(&cmd[1..]).stdin(read)
            .stderr(std::process::Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        let bin = gst::Bin::new();
        let make = |f: &str| gst::ElementFactory::make(f).build().map_err(|e| e.to_string());
        let (vq, aq) = (make("queue")?, make("queue")?);
        for q in [&vq, &aq] {
            q.set_property("max-size-time", 3 * gst::ClockTime::SECOND.nseconds());
            q.set_property("max-size-buffers", 0u32);
            q.set_property("max-size-bytes", 0u32);
        }
        let conv = make("audioconvert")?;
        let mux = make("matroskamux")?;
        mux.set_property("streamable", true);
        let sink = make("fdsink")?;
        sink.set_property("fd", write.as_raw_fd());
        sink.set_property("sync", false);
        bin.add_many([&vq, &aq, &conv, &mux, &sink]).map_err(|e| e.to_string())?;
        vq.link(&mux).map_err(|e| e.to_string())?;
        gst::Element::link_many([&aq, &conv, &mux, &sink]).map_err(|e| e.to_string())?;
        bin.add_pad(&gst::GhostPad::builder_with_target(&vq.static_pad("sink").unwrap()).unwrap().name("video").build())
            .map_err(|e| e.to_string())?;
        bin.add_pad(&gst::GhostPad::builder_with_target(&aq.static_pad("sink").unwrap()).unwrap().name("audio").build())
            .map_err(|e| e.to_string())?;
        let tx = engine.tx.clone();
        sink.static_pad("sink").unwrap().add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
            if let Some(gst::PadProbeData::Event(ev)) = &info.data {
                if ev.type_() == gst::EventType::Eos {
                    let _ = tx.send(Event::PartEos(id));
                }
            }
            gst::PadProbeReturn::Ok
        });
        engine.pipe.add(&bin).map_err(|e| e.to_string())?;
        let vpad = engine.vt.request_pad_simple("src_%u").ok_or("no video pad")?;
        let apad = engine.at.request_pad_simple("src_%u").ok_or("no audio pad")?;
        start_branch(&engine.pipe, bin.upcast_ref());
        vpad.link(&bin.static_pad("video").unwrap()).map_err(|e| format!("{e:?}"))?;
        apad.link(&bin.static_pad("audio").unwrap()).map_err(|e| format!("{e:?}"))?;
        Ok(Recorder { id, bin, vpad, apad, write: RefCell::new(Some(write)),
                      proc: Arc::new(Mutex::new(Some(child))), closed: Cell::new(false) })
    }

    /// Stop taking frames and let the muxer finish (then close(): ffmpeg).
    fn finish(&self, engine: &Engine) {
        for (pad, ghost) in [(&self.vpad, "video"), (&self.apad, "audio")] {
            let sinkpad = self.bin.static_pad(ghost).unwrap();
            pad.add_probe(gst::PadProbeType::IDLE, move |p, _| {
                if p.is_linked() {
                    let _ = p.unlink(&sinkpad);
                    sinkpad.send_event(gst::event::Eos::new());
                }
                gst::PadProbeReturn::Remove
            });
        }
        let (weak, id) = (Rc::downgrade(&engine.rc()), self.id);
        glib::timeout_add_local_once(std::time::Duration::from_secs(8), move || {
            if let Some(e) = weak.upgrade() {
                if e.finishing.borrow().iter().any(|r| r.id == id) {
                    crate::warn!("the muxer didn't finish in time; closing the part anyway");
                    e.part_eos(id);
                }
            }
        });
    }

    fn close(&self, engine: &Engine) {
        if self.closed.replace(true) {
            return;
        }
        let _ = self.bin.set_state(gst::State::Null);
        engine.vt.release_request_pad(&self.vpad);
        engine.at.release_request_pad(&self.apad);
        let _ = engine.pipe.remove(&self.bin);
        self.write.borrow_mut().take(); // (ffmpeg sees the end of its input)
        let proc = self.proc.clone();
        let weak = Rc::downgrade(&engine.rc());
        crate::util::in_thread(move || {
            let child = proc.lock().unwrap().take();
            let Some(child) = child else { return Some("abandoned".to_string()) };
            match child.wait_with_output() {
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr);
                    if !err.trim().is_empty() {
                        let tail: String = err.trim().chars().rev().take(500).collect::<Vec<_>>().into_iter().rev()
                            .collect();
                        crate::info!("ffmpeg: {tail}");
                    }
                    (!out.status.success()).then(|| format!("ffmpeg stopped ({})", out.status))
                }
                Err(e) => Some(e.to_string()),
            }
        }, move |error| {
            if let Some(e) = weak.upgrade() {
                e.part_done(error);
            }
        });
    }

    fn abandon(&self, engine: &Engine) {
        if let Some(mut c) = self.proc.lock().unwrap().take() { // (first: a write to it may be holding things up)
            let _ = c.kill();
            let _ = c.wait();
        }
        if !self.closed.get() {
            for pad in [&self.vpad, &self.apad] {
                if let Some(peer) = pad.peer() {
                    let _ = pad.unlink(&peer);
                }
            }
            self.closed.set(true);
            let _ = self.bin.set_state(gst::State::Null);
            engine.vt.release_request_pad(&self.vpad);
            engine.at.release_request_pad(&self.apad);
            let _ = engine.pipe.remove(&self.bin);
            self.write.borrow_mut().take();
        }
    }
}

/// Bring b, just added to pipe, to the state pipe is in. (Not sync_state_with_parent: after a source failed, the
/// pipeline's last state change reads as failed and a new branch would stop at PAUSED.)
fn start_branch(pipe: &gst::Pipeline, b: &gst::Element) {
    b.set_property("async-handling", true);
    let (_, current, pending) = pipe.state(gst::ClockTime::ZERO);
    let _ = b.set_state(if pending == gst::State::VoidPending { current } else { pending });
}

fn inside(obj: Option<&gst::Object>, b: &gst::Object) -> bool {
    let mut o = obj.cloned();
    while let Some(x) = o {
        if &x == b {
            return true;
        }
        o = x.parent();
    }
    false
}

fn rmtree(path: &std::path::Path) {
    let _ = std::fs::remove_dir_all(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent); // .parts, if that was the last
    }
}
