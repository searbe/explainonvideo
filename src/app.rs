//! The app: what's chosen, whether it's recording, and the windows that show it.
//!
//! Starting shows the controls and the selection layer at once. Once something is chosen, its outline stays on
//! screen; recording starts after a short countdown, and the outline (and the controls) can be used throughout:
//! resize the area, follow windows, turn the microphone or camera on and off, move the camera in the preview.
//! Stop saves an MP4 and offers the editor.
use crate::controls::Controls;
use crate::engine::{self as eng, Engine};
use crate::floating::{action, close_action, notice, Countdown};
use crate::frame::Frame;
use crate::kwin::KWin;
use crate::layout::{self, Rect};
use crate::portal::{Portal, Stream};
use crate::preview::Preview;
use crate::selector::{Selection, Selector};
use crate::settings;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub fn test_screen() -> bool {
    std::env::var_os("EOV_TEST_SCREEN").is_some_and(|v| !v.is_empty() && v != "0")
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Whether the app (not only the editor) is running.
pub fn exists() -> bool {
    APP.with(|a| a.borrow().is_some())
}

/// The app (there's one).
pub fn app() -> Rc<App> {
    APP.with(|a| a.borrow().clone()).expect("the app")
}

type Waiting = Box<dyn FnOnce()>;

/// What the app is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    Countdown,
    Recording,
    Paused,
    Saving,
}

pub struct App {
    pub kwin: Rc<KWin>,
    portal: Option<Rc<Portal>>,
    stream: RefCell<Option<Rc<Stream>>>,
    pub selection: RefCell<Option<Selection>>,
    pub engine: RefCell<Option<Rc<Engine>>>,
    state: Cell<State>,
    choose_first: bool,
    pub selector: RefCell<Option<Rc<Selector>>>,
    pub preview: RefCell<Option<Rc<Preview>>>,
    countdown: RefCell<Option<Rc<Countdown>>>,
    settings_win: RefCell<Option<gtk::Window>>,
    pub frame: RefCell<Option<Rc<Frame>>>,
    controls: RefCell<Option<Rc<Controls>>>,
    waiting_for_stream: RefCell<Vec<Waiting>>,
    use_portal: Cell<bool>, // (KWin wouldn't stream the screen)
    pub on_saved: RefCell<Option<Box<dyn Fn(&Path, Option<&str>)>>>, // (for tests)
}

impl App {
    pub fn new(choose_first: bool) -> Rc<App> {
        let a = Rc::new(App {
            kwin: KWin::new(), portal: Portal::new(), stream: RefCell::new(None), selection: RefCell::new(None),
            engine: RefCell::new(None), state: Cell::new(State::Ready), choose_first,
            selector: RefCell::new(None), preview: RefCell::new(None), countdown: RefCell::new(None),
            settings_win: RefCell::new(None), frame: RefCell::new(None), controls: RefCell::new(None),
            waiting_for_stream: RefCell::new(vec![]), use_portal: Cell::new(false), on_saved: RefCell::new(None),
        });
        APP.with(|x| *x.borrow_mut() = Some(a.clone()));
        a
    }

    pub fn controls(&self) -> Rc<Controls> {
        self.controls.borrow().clone().expect("controls")
    }

    pub fn frame(&self) -> Rc<Frame> {
        self.frame.borrow().clone().expect("frame")
    }

    pub fn state(&self) -> State {
        self.state.get()
    }

    fn engine(&self) -> Option<Rc<Engine>> {
        self.engine.borrow().clone()
    }

    // --- starting and quitting ---
    pub fn run(self: &Rc<Self>) {
        self.kwin.start(|| app().started(), || {
            crate::info!("already running: asked it to show itself");
            gtk::main_quit();
        });
        gtk::main();
        self.close_engine();
        self.kwin.unload();
    }

    fn started(&self) {
        std::thread::spawn(crate::export::hardware_works); // (tried now, not when recording starts)
        self.kwin.on_activate(|| app().activate());
        self.kwin.on_change(|| app().windows_changed());
        *self.controls.borrow_mut() = Some(Controls::new());
        *self.frame.borrow_mut() = Some(Frame::new(self.screen_rect(), |r| app().area_dragged(r),
                                                   |r| app().area_dropped(r), || app().follow(),
                                                   || app().choose()));
        // (the window list arrives from KWin in a moment: wait for it, so windows can be chosen at once)
        let wait = if self.kwin.available.get() { 250 } else { 0 };
        glib::timeout_add_local_once(std::time::Duration::from_millis(wait), || {
            let a = app();
            a.controls().place(a.screen_rect());
            if a.choose_first {
                a.choose();
            }
        });
    }

    /// Launched again: show the controls, and the selection layer if nothing's being recorded.
    fn activate(&self) {
        self.controls().win.present();
        if self.state() == State::Ready && self.selector.borrow().is_none() {
            self.choose();
        }
    }

    pub fn quit(&self) {
        if self.state() != State::Ready {
            return;
        }
        self.close_engine();
        let stream = self.stream.borrow_mut().take();
        if let Some(s) = stream {
            s.close();
        }
        gtk::main_quit();
    }

    // --- the screen ---
    /// The screen being recorded (on the desktop): the shared one, else the pointer's.
    pub fn screen_rect(&self) -> Rect {
        if let Some(s) = self.stream.borrow().as_ref().filter(|s| s.w > 0) {
            return (s.x, s.y, s.w, s.h);
        }
        let display = gdk::Display::default().unwrap();
        let (px, py) = display.default_seat().and_then(|s| s.pointer()).map(|p| {
            let (_, x, y) = p.position();
            (x, y)
        }).unwrap_or((0, 0));
        let screens = self.kwin.screen_rects();
        if let Some(r) = screens.iter().find(|r| r.0 <= px && px < r.0 + r.2 && r.1 <= py && py < r.1 + r.3) {
            return *r;
        }
        if let Some(r) = screens.first() {
            return *r;
        }
        display.monitor_at_point(px, py).map(|m| {
            let g = m.geometry();
            (g.x(), g.y(), g.width(), g.height())
        }).unwrap_or((0, 0, 1920, 1080))
    }

    /// Get the screen's stream (from KWin; else the portal, whose dialog asks the first time), then then().
    fn with_stream(&self, then: impl FnOnce() + 'static) {
        if test_screen() || self.stream.borrow().is_some() {
            then();
            return;
        }
        self.waiting_for_stream.borrow_mut().push(Box::new(then));
        if self.waiting_for_stream.borrow().len() > 1 {
            return;
        }
        if !self.use_portal.get() && self.kwin_screen() {
            return;
        }
        let Some(portal) = self.portal.clone().filter(|p| p.available()) else {
            self.waiting_for_stream.borrow_mut().clear();
            self.problem("Screen recording isn't available",
                         Some("The desktop's screen-cast portal is missing or too old (xdg-desktop-portal-kde)."),
                         None);
            return;
        };
        portal.open(|stream| app().stream_ready(stream), |why| app().stream_failed(why));
    }

    /// Ask KWin for the screen directly (no dialog): true if it's been asked; stream_ready follows. If KWin
    /// refuses, the portal is asked instead.
    fn kwin_screen(&self) -> bool {
        let Some(cast) = crate::kwin_cast::Cast::get().filter(|c| c.can_stream_screens()) else { return false };
        if self.kwin.screen_rects().is_empty() {
            return false; // (KWin's script hasn't said where the screens are)
        }
        let rect = self.screen_rect();
        let handle: Rc<RefCell<Option<crate::kwin_cast::Stream>>> = Rc::default();
        let h = handle.clone();
        let s = cast.stream_region(rect, move |ev| match ev {
            crate::kwin_cast::Event::Created(node) => {
                let Some(kwin) = h.borrow_mut().take() else { return };
                crate::info!("KWin's stream of the screen: node {node}, {rect:?}");
                app().stream_ready(Stream { kwin: Some(kwin), session: String::new(), node,
                                            x: rect.0, y: rect.1, w: rect.2, h: rect.3 });
            }
            crate::kwin_cast::Event::Failed(why) => {
                crate::warn!("KWin couldn't stream the screen ({why}): asking the portal");
                h.borrow_mut().take();
                let waiting: Vec<Waiting> = app().waiting_for_stream.borrow_mut().drain(..).collect();
                app().use_portal.set(true);
                for f in waiting {
                    app().with_stream(f);
                }
            }
            crate::kwin_cast::Event::Closed => app().screen_closed(),
        });
        *handle.borrow_mut() = Some(s);
        true
    }

    /// KWin closed the screen's stream (the screens changed, say): forget it, and start again if nothing's being
    /// recorded (while recording, the pipeline's own error ends it, keeping what was recorded).
    fn screen_closed(&self) {
        crate::info!("KWin closed the screen's stream");
        self.stream.borrow_mut().take();
        if self.state() == State::Ready && self.engine.borrow().is_some() {
            self.restart_engine();
        }
    }

    fn stream_ready(&self, stream: Stream) {
        let waiting: Vec<Waiting> = self.waiting_for_stream.borrow_mut().drain(..).collect();
        let screens = self.kwin.screen_rects();
        let got = (stream.x, stream.y, stream.w, stream.h);
        if !screens.is_empty() && !screens.contains(&got) {
            // A region or a window was shared, not a whole screen: the app does its own area, so ask again.
            crate::info!("shared {got:?}, not a whole screen {screens:?}: asking again");
            stream.close();
            settings::set("restore_token", "".into());
            if self.state() == State::Countdown {
                self.set_state(State::Ready);
            }
            let waiting = Rc::new(RefCell::new(Some(waiting)));
            notice("Share the whole screen", Some("Only part of the screen was shared. Explain on video chooses the \
                    area itself, so in the next window pick your monitor at the top, not \"Share region\" or a \
                    window."), None,
                   vec![action("Cancel", "quiet", || {
                            let a = app();
                            if a.preview_open() {
                                a.toggle_preview();
                            }
                        }),
                        action("Share again", "primary", move || {
                            if let Some(ws) = waiting.borrow_mut().take() {
                                for f in ws {
                                    app().with_stream(f);
                                }
                            }
                        })],
                   Some("screen"), false, None);
            return;
        }
        *self.stream.borrow_mut() = Some(Rc::new(stream));
        let sel = self.selection.borrow().clone();
        if let Some(sel) = sel.filter(|s| !s.screen) {
            if let Some(r) = sel.rect(&self.kwin, self.screen_rect()) {
                if !inside(r, self.screen_rect()) {
                    self.problem("That's on another screen", Some("The screen shared isn't the one the area is on. \
                                 Choose the area again, or share the other screen (Settings → Forget the screen \
                                 chosen)."), None);
                }
            }
        }
        self.frame().screen.set(self.screen_rect());
        for f in waiting {
            f();
        }
    }

    fn stream_failed(&self, why: String) {
        self.waiting_for_stream.borrow_mut().clear();
        if why == "cancelled" {
            notice("Screen sharing was cancelled", Some("Nothing can be recorded without it."), None,
                   vec![action("Close", "quiet", || {
                            let a = app();
                            if a.preview_open() {
                                a.toggle_preview();
                            }
                        }),
                        action("Try again", "primary", || app().sync_engine(None))],
                   None, false, None);
        } else {
            self.problem("The screen couldn't be shared", Some(&why), None);
        }
        if self.state() == State::Countdown {
            self.set_state(State::Ready);
        }
    }

    fn screen(&self) -> eng::Screen {
        eng::Screen { rect: self.screen_rect(),
                      stream: if test_screen() { None } else { self.stream.borrow().clone() } }
    }

    // --- choosing ---
    /// What the frame's Follow button says: shown when the area is drawn rather than following windows (it was
    /// dragged off them, or drawn). With windows still chosen it follows those again; without, one is picked.
    fn follow_words(&self, sel: &Selection) -> Option<String> {
        if sel.screen || sel.follows() || !self.kwin.available.get() {
            return None;
        }
        let alive: Vec<_> = sel.windows.iter().filter_map(|w| self.kwin.window(w)).collect();
        Some(match alive.as_slice() {
            [] => "Follow a window…".into(),
            [w] => {
                let app = w.app.rsplit('.').next().unwrap_or("");
                let mut c = app.chars();
                match c.next() {
                    Some(f) => format!("Follow {}{}", f.to_uppercase(), c.as_str()),
                    None => format!("Follow {}", w.title),
                }
            }
            ws => format!("Follow the {} windows", ws.len()),
        })
    }

    /// The Follow button: attach the area to its windows again, or pick a window to follow.
    pub fn follow(&self) {
        let Some(mut sel) = self.selection.borrow().clone() else { return };
        sel.windows.retain(|w| self.kwin.window(w).is_some());
        if sel.windows.is_empty() {
            self.choose();
            let selector = self.selector.borrow().clone();
            if let Some(s) = selector {
                s.pick_window();
            }
            return;
        }
        sel.area = None;
        *self.selection.borrow_mut() = Some(sel);
        self.apply_selection(true);
    }

    pub fn choose(&self) {
        let st = self.state();
        if self.selector.borrow().is_some() || st == State::Countdown || st == State::Saving {
            return;
        }
        self.frame().hide();
        let recording = st == State::Recording || st == State::Paused;
        let sel = self.selection.borrow().clone();
        let before = sel.clone();
        let s = Selector::new(self.kwin.clone(), self.screen_rect(), sel, !recording, move |chosen, record| {
            let a = app();
            a.selector.borrow_mut().take();
            // (cancelled: back to how it was, as it may have changed live while recording)
            *a.selection.borrow_mut() = chosen.or(before);
            a.apply_selection(true);
            if record {
                a.start();
            }
        });
        if recording { // (windows clicked in or out are recorded at once)
            s.set_live(|sel| app().live_selection(sel));
        }
        *self.selector.borrow_mut() = Some(s);
        self.controls().win.present();
    }

    /// While recording, the chooser changed what's chosen: record it now (the outline comes back when it's closed).
    fn live_selection(&self, sel: Selection) {
        let Some(rect) = sel.rect(&self.kwin, self.screen_rect()) else { return };
        let rect = layout::clamp_rect(rect, self.screen_rect());
        *self.selection.borrow_mut() = Some(sel);
        if let Some(e) = self.engine() {
            self.engine_area(&e, rect, true);
        }
    }

    pub fn apply_selection(&self, animate: bool) {
        let Some(sel) = self.selection.borrow().clone() else { return };
        let screen = self.screen_rect();
        let Some(rect) = sel.rect(&self.kwin, screen) else { return };
        let rect = layout::clamp_rect(rect, screen);
        let frame = self.frame();
        frame.screen.set(screen);
        frame.set_area(rect, Some(&sel.describe(&self.kwin)));
        frame.set_follow(self.follow_words(&sel).as_deref());
        frame.set_pick(self.kwin.available.get() && !sel.screen);
        frame.set_state(self.state());
        frame.win.show();
        if let Some(e) = self.engine() {
            if self.state() == State::Ready && self.out_size() != (e.out_w, e.out_h) {
                self.restart_engine();
            } else {
                self.engine_area(&e, rect, animate);
            }
        }
    }

    /// Windows moved, resized, opened or closed: follow the chosen ones.
    fn windows_changed(&self) {
        let Some(mut sel) = self.selection.borrow().clone() else { return };
        if sel.windows.is_empty() || self.selector.borrow().is_some() {
            return;
        }
        let alive: Vec<String> = sel.windows.iter().filter(|w| self.kwin.window(w).is_some()).cloned().collect();
        if alive.is_empty() && sel.follows() { // all closed: keep recording where they were
            sel = Selection { area: self.frame().area.get(), ..Default::default() };
        } else {
            sel.windows = alive;
        }
        *self.selection.borrow_mut() = Some(sel);
        self.apply_selection(true);
    }

    /// The outline is being dragged: the area is drawn from now on (no longer following windows).
    pub fn area_dragged(&self, rect: Rect) {
        let sel = self.selection.borrow().clone();
        let sel = match sel {
            Some(mut s) if !s.screen && !s.follows() => {
                s.area = Some(rect);
                s
            }
            other => Selection { area: Some(rect), windows: other.map(|s| s.windows).unwrap_or_default(),
                                 screen: false },
        };
        *self.selection.borrow_mut() = Some(sel);
        if let Some(e) = self.engine() {
            self.engine_area(&e, rect, false);
        }
    }

    /// Tell the engine the area and the chosen windows (bottom first): it records their own streams if it can,
    /// else covers what isn't them.
    fn engine_area(&self, e: &Engine, rect: Rect, animate: bool) {
        let sel = self.selection.borrow().clone();
        let windows = sel.as_ref().filter(|s| !s.screen)
            .map(|s| self.kwin.with_popups(&s.windows))
            .unwrap_or_default();
        e.set_windows(windows, sel.as_ref().is_some_and(|s| s.follows()));
        e.set_area(rect, sel.as_ref().and_then(|s| s.shown(&self.kwin, Some(rect))), animate);
    }

    fn area_dropped(&self, rect: Rect) {
        self.area_dragged(rect);
        let desc = self.selection.borrow().as_ref().map(|s| s.describe(&self.kwin));
        self.frame().set_area(rect, desc.as_deref());
        let words = self.selection.borrow().as_ref().and_then(|s| self.follow_words(s));
        self.frame().set_follow(words.as_deref());
        if let Some(e) = self.engine() {
            if self.state() == State::Ready && self.out_size() != (e.out_w, e.out_h) {
                self.restart_engine();
            }
        }
    }

    // --- the engine ---
    fn out_size(&self) -> (i32, i32) {
        let r = self.video_rect();
        layout::output_size(&settings::string("output"), r.2, r.3)
    }

    pub fn rect(&self) -> Rect {
        let screen = self.screen_rect();
        match self.selection.borrow().as_ref() {
            None => screen,
            Some(s) => layout::clamp_rect(s.rect(&self.kwin, screen).unwrap_or(screen), screen),
        }
    }

    /// The area's size in the video: followed windows recorded by their own streams are put side by side, without
    /// the space between them (as the engine does).
    pub fn video_rect(&self) -> Rect {
        let rect = self.rect();
        let sel = self.selection.borrow().clone();
        match sel {
            Some(s) if s.follows() && crate::kwin_cast::Cast::get().is_some() => {
                let rects: Vec<Rect> = self.kwin.with_popups(&s.windows).into_iter().map(|(_, r)| r).collect();
                layout::Squeeze::new(&rects).rect(rect)
            }
            _ => rect,
        }
    }

    fn engine_needed(&self) -> bool {
        self.state() != State::Ready || self.preview_open()
            || settings::bool("virtual_camera")
    }

    /// Start the engine if something needs it (recording, the preview, the virtual camera), or stop it.
    pub fn sync_engine(&self, then: Option<Waiting>) {
        if self.engine_needed() && self.engine.borrow().is_none() {
            self.with_stream(move || app().open_engine(then));
            return;
        }
        if !self.engine_needed() && self.engine.borrow().is_some() {
            self.close_engine();
        }
        if let Some(f) = then {
            f();
        }
    }

    fn open_engine(&self, then: Option<Waiting>) {
        if self.engine.borrow().is_some() || !self.engine_needed() {
            if let Some(f) = then {
                f();
            }
            return;
        }
        let (w, h) = self.out_size();
        let e = match Engine::new(self.screen(), w, h, settings::i64("fps")) {
            Ok(e) => e,
            Err(err) => {
                crate::warn!("engine: {err}");
                self.problem("The screen couldn't be captured", Some(&err), None);
                if self.state() == State::Countdown {
                    self.set_state(State::Ready);
                }
                return;
            }
        };
        *e.on_frame.borrow_mut() = Some(Box::new(|pix| {
            if let Some(p) = app().preview.borrow().as_ref() {
                p.show_frame(pix);
            }
        }));
        *e.on_level.borrow_mut() = Some(Box::new(|l| app().controls().set_level(l)));
        *e.on_error.borrow_mut() = Some(Box::new(|m, fatal, what| {
            let (m, what) = (m.to_string(), what.map(|s| s.to_string()));
            glib::idle_add_local_once(move || app().engine_error(&m, fatal, what.as_deref()));
        }));
        self.engine_area(&e, self.rect(), false);
        *self.engine.borrow_mut() = Some(e.clone());
        e.start();
        if let Some(p) = self.preview.borrow().as_ref() {
            p.set_size(w, h);
        }
        if let Some(f) = then {
            f();
        }
    }

    fn close_engine(&self) {
        let e = self.engine.borrow_mut().take();
        if let Some(e) = e {
            e.close();
        }
        if let Some(c) = self.controls.borrow().as_ref() {
            c.set_level(0.0);
        }
    }

    fn restart_engine(&self) {
        self.close_engine();
        self.sync_engine(None);
    }

    fn engine_error(&self, message: &str, fatal: bool, what: Option<&str>) {
        if let Some(w) = what {
            settings::set(w, false.into());
            self.controls().update();
            if let Some(p) = self.preview.borrow().as_ref() {
                p.update_hint();
            }
        }
        if fatal {
            match self.state() {
                State::Recording | State::Paused => self.stop(), // keep what was recorded
                st => {
                    // (not left waiting: a countdown would end into no engine, a save would never be told it's done)
                    let countdown = self.countdown.borrow_mut().take();
                    if let Some(c) = countdown {
                        c.stop(true);
                    }
                    self.close_engine();
                    if st != State::Ready {
                        self.set_state(State::Ready);
                    }
                }
            }
        }
        if fatal {
            self.problem("Something went wrong", Some(message), Some(8));
        } else {
            self.problem(message, None, Some(8));
        }
    }

    fn problem(&self, title: &str, body: Option<&str>, timeout: Option<u32>) {
        let near = self.frame.borrow().as_ref().and_then(|f| f.area.get()).map(|_| self.rect());
        notice(title, body, near, vec![], None, true, timeout);
    }

    // --- recording ---
    pub fn start(&self) {
        if self.state() != State::Ready {
            return;
        }
        if self.selection.borrow().is_none() {
            *self.selection.borrow_mut() = Some(Selection { screen: true, ..Default::default() });
            self.apply_selection(true);
        }
        self.set_state(State::Countdown);
        self.sync_engine(Some(Box::new(|| app().count_down())));
    }

    fn count_down(&self) {
        let Some(e) = self.engine() else { return };
        if self.state() != State::Countdown {
            return;
        }
        e.prepare(&settings::folder());
        let seconds = settings::i64("countdown") as i32;
        if seconds <= 0 {
            self.begin();
        } else {
            let c = Countdown::new(seconds, self.rect(), |cancelled| {
                let a = app();
                a.countdown.borrow_mut().take();
                if cancelled {
                    if let Some(e) = a.engine() {
                        e.unprepare();
                    }
                    a.set_state(State::Ready);
                    a.sync_engine(None);
                } else {
                    a.begin();
                }
            });
            *self.countdown.borrow_mut() = Some(c);
        }
    }

    fn begin(&self) {
        let Some(e) = self.engine() else { return };
        if self.state() != State::Countdown {
            return;
        }
        e.record(|path, error| app().saved(path, error));
        self.set_state(State::Recording);
    }

    pub fn pause_or_resume(&self) {
        let Some(e) = self.engine() else { return };
        match self.state() {
            State::Recording => {
                e.pause();
                self.set_state(State::Paused);
            }
            State::Paused => {
                e.resume();
                self.set_state(State::Recording);
            }
            _ => {}
        }
    }

    pub fn stop(&self) {
        let st = self.state();
        let Some(e) = self.engine() else { return };
        if st != State::Recording && st != State::Paused {
            return;
        }
        self.set_state(State::Saving);
        e.stop();
    }

    fn saved(&self, path: PathBuf, error: Option<String>) {
        self.set_state(State::Ready);
        self.sync_engine(None);
        if let Some(f) = self.on_saved.borrow().as_ref() {
            f(&path, error.as_deref());
        }
        if let Some(err) = error {
            self.problem("The recording couldn't be saved", Some(&err), None);
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (p1, p2) = (path.clone(), path.clone());
        notice("Saved", Some(&name), Some(self.rect()),
               vec![action("Show in folder", "quiet", move || open(p1.parent().unwrap_or(Path::new("/")))),
                    action("Edit…", "primary", move || app().open_editor(Some(p2.clone())))],
               Some("check"), false, None);
    }

    pub fn discard(&self) {
        let st = self.state();
        if st != State::Recording && st != State::Paused {
            return;
        }
        notice("Discard this recording?", Some("What's been recorded so far is thrown away."), Some(self.rect()),
               vec![close_action("Keep recording", "quiet"), action("Discard", "danger", || {
                   let a = app();
                   let st = a.state();
                   if let Some(e) = a.engine().filter(|_| st == State::Recording || st == State::Paused) {
                       e.discard();
                       a.set_state(State::Ready);
                       a.sync_engine(None);
                   }
               })], None, false, None);
    }

    fn set_state(&self, state: State) {
        self.state.set(state);
        self.controls().set_state(state);
        self.frame().set_state(state);
    }

    // --- the toggles ---
    pub fn toggle(&self, name: &str) {
        let on = !settings::bool(name);
        settings::set(name, on.into());
        if let Some(e) = self.engine() {
            match name {
                "system_audio" => e.set_system_audio(on),
                "mic" => e.set_mic(on),
                "camera" => { e.set_camera(on); }
                "virtual_camera" => e.set_virtual_camera(on),
                _ => {}
            }
        }
        if name == "virtual_camera" {
            self.sync_engine(None);
            if on {
                notice("Virtual camera on", Some("Other apps can now pick “Explain on video” as a camera (browsers, \
                       OBS, and apps that use PipeWire cameras)."), Some(self.rect()), vec![], Some("live"), false,
                       Some(6));
            }
        }
        if name == "camera" && on && !self.preview_open() && self.state() == State::Ready {
            self.toggle_preview(); // (to see where it is)
        }
        self.controls().update();
        if let Some(p) = self.preview.borrow().as_ref() {
            p.update_hint();
        }
    }

    // --- the preview ---
    pub fn preview_open(&self) -> bool {
        self.preview.borrow().as_ref().is_some_and(|p| p.win.is_visible())
    }

    pub fn toggle_preview(&self) {
        if self.preview_open() {
            if let Some(p) = self.preview.borrow().as_ref() {
                p.win.hide();
            }
        } else {
            if self.preview.borrow().is_none() {
                *self.preview.borrow_mut() = Some(Preview::new());
            }
            let p = self.preview.borrow().clone().unwrap();
            if let Some(e) = self.engine() {
                p.set_size(e.out_w, e.out_h);
            }
            p.update_hint();
            p.win.show_all();
            self.dock_preview();
        }
        self.sync_engine(None);
        self.controls().update();
    }

    /// The preview sits on the controls: just above them (below, if they're at the top of the screen), centred,
    /// and goes where they go.
    pub fn dock_preview(&self) {
        let Some(p) = self.preview.borrow().clone() else { return };
        if !p.win.is_visible() {
            return;
        }
        let c = self.controls();
        let (cx, cy) = c.win.position();
        let (cw, ch) = c.win.size();
        let (pw, ph) = p.win.size();
        let (sx, sy, sw, sh) = self.screen_rect();
        const GAP: i32 = -14; // (both have 14 px of see-through margin around their cards: the cards 14 px apart)
        let x = (cx + (cw - pw) / 2).min(sx + sw - pw).max(sx);
        let y = if cy - ph - GAP >= sy { cy - ph - GAP } else { (cy + ch + GAP).min(sy + sh - ph) };
        if p.win.position() != (x, y) {
            p.win.move_(x, y);
        }
    }

    pub fn camera_shown(&self) -> bool {
        self.engine().is_some_and(|e| e.camera_on())
    }

    pub fn camera_rect(&self) -> Option<Rect> {
        self.engine().filter(|e| e.camera_on()).map(|e| e.camera_rect())
    }

    pub fn place_camera(&self) {
        if let Some(e) = self.engine() {
            e.place_camera();
        }
    }

    // --- other windows ---
    pub fn open_editor(&self, path: Option<PathBuf>) {
        let path = match path {
            Some(p) => p,
            None => match crate::editor::choose_file(None) {
                Some(p) => p,
                None => return,
            },
        };
        crate::editor::Editor::open(&path);
    }

    pub fn open_folder(&self) {
        open(&settings::folder());
    }

    pub fn open_settings(&self) {
        let existing = self.settings_win.borrow().clone();
        if let Some(w) = existing {
            w.present();
            return;
        }
        let w = crate::settings_window::window();
        w.connect_destroy(|_| {
            app().settings_win.borrow_mut().take();
        });
        w.present();
        *self.settings_win.borrow_mut() = Some(w);
    }

    /// A setting changed in the settings window: apply it.
    pub fn settings_changed(&self, name: &str) {
        let e = self.engine();
        match name {
            "camera_shape" | "camera_mirror" | "camera_device" => {
                if let Some(e) = e.filter(|e| e.camera_on()) {
                    e.set_camera(false);
                    e.set_camera(true);
                }
            }
            "mic_device" => {
                if let Some(e) = e.filter(|_| settings::bool("mic")) {
                    e.set_mic(false);
                    e.set_mic(true);
                }
            }
            "output" | "fps" | "background" => {
                if e.is_some() && self.state() == State::Ready {
                    self.restart_engine();
                }
            }
            "max_zoom" | "hidpi" => {
                if let Some(e) = e {
                    self.engine_area(&e, self.rect(), true);
                }
            }
            "restore_token" => {
                if self.state() == State::Ready {
                    let s = self.stream.borrow_mut().take();
                    if let Some(s) = s {
                        self.close_engine();
                        s.close();
                    }
                }
            }
            _ => {}
        }
        self.controls().update();
    }
}

fn inside(r: Rect, s: Rect) -> bool {
    s.0 <= r.0 && s.1 <= r.1 && r.0 + r.2 <= s.0 + s.2 && r.1 + r.3 <= s.1 + s.3
}

pub fn open(path: &Path) {
    let uri = gio::File::for_path(path).uri();
    if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE) {
        crate::warn!("couldn't open {uri}: {e}");
    }
}
