//! Choosing what to record: a see-through layer over the whole screen with a small toolbar.
//!
//! - Area: drag out a rectangle (then drag inside it to move it, or its handles to resize it).
//! - Windows: click a window to record just it; click more to add them (click again to take one away). With no
//!   area drawn, the recording follows the windows wherever they go and whatever size they become; inside a drawn
//!   area, only the chosen windows show and the rest of the area is the background.
//! - Screen: the whole screen.
use crate::design as d;
use crate::floating;
use crate::kwin::KWin;
use crate::layout::{self, Rect};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

const HANDLE: f64 = 6.0; // the handles' radius
const GRAB: i32 = 12; // how near a handle or edge a press counts as on it
const DRAG: i32 = 4; // how far the pointer moves before a press is a drag, not a click

/// area: on the desktop, or None to follow the windows; windows: ids of the windows to record (empty: everything
/// in the area); screen: the whole screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub area: Option<Rect>,
    pub windows: Vec<String>,
    pub screen: bool,
}

impl Selection {
    pub fn follows(&self) -> bool {
        self.area.is_none() && !self.windows.is_empty()
    }

    /// The area recorded now: the drawn area, the screen, or around the chosen windows.
    pub fn rect(&self, kwin: &KWin, screen: Rect) -> Option<Rect> {
        if self.screen {
            return Some(screen);
        }
        if self.area.is_some() {
            return self.area;
        }
        let rects: Vec<Rect> = self.windows.iter().filter_map(|i| kwin.window(i)).map(|w| w.rect).collect();
        (!rects.is_empty()).then(|| layout::clamp_rect(layout::bounds(&rects), screen))
    }

    /// What to show of rect (layout::visible): None for everything.
    pub fn shown(&self, kwin: &KWin, rect: Option<Rect>) -> Option<Vec<Rect>> {
        let rect = rect?;
        if self.windows.is_empty() {
            return None;
        }
        layout::visible(rect, &self.windows, &kwin.stacking())
    }

    pub fn describe(&self, kwin: &KWin) -> String {
        if self.screen {
            return "The whole screen".into();
        }
        let names: Vec<String> = self.windows.iter().filter_map(|i| kwin.window(i))
            .map(|w| if w.title.is_empty() { w.app } else { w.title }).collect();
        let n = names.len();
        let s = if n > 1 { "s" } else { "" };
        match (self.area.is_some(), n) {
            (true, n) if n > 0 => format!("{n} window{s} in an area"),
            (false, 1) => names[0].clone(),
            (false, n) if n > 1 => format!("{n} windows"),
            _ => "An area".into(),
        }
    }
}

const MODES: [&str; 3] = ["Area", "Window", "Screen"];

fn hint(mode: &str) -> &'static str {
    match mode {
        "Window" => "Click the windows to record. The video follows them as they move.",
        "Screen" => "The whole screen is recorded.",
        _ => "Drag to choose an area. Click windows to record only them.",
    }
}

type Press = (i32, i32, Option<String>, Option<Rect>);

pub struct Selector {
    pub win: gtk::Window,
    kwin: Rc<KWin>,
    pub screen: Rect,
    canvas: gtk::DrawingArea,
    hint: gtk::Label,
    done_btn: gtk::Button,
    rec_btn: Option<gtk::Button>,
    mode_btns: Vec<gtk::Button>,
    mode: RefCell<String>,
    pub area: RefCell<Option<Rect>>,
    windows: RefCell<Vec<String>>,
    hover: RefCell<Option<String>>,
    press: RefCell<Option<Press>>,
    dragging: Cell<bool>,
    done: RefCell<Option<Box<dyn FnOnce(Option<Selection>, bool)>>>,
    live: RefCell<Option<Box<dyn Fn(Selection)>>>,
    last: RefCell<Option<Selection>>,
    listener: Cell<Option<usize>>,
}

impl Selector {
    /// screen: the screen to choose on; current: the Selection so far. finished(Some(selection), record) when
    /// done (record: start recording now), finished(None, false) when cancelled.
    pub fn new(kwin: Rc<KWin>, screen: Rect, current: Option<Selection>, can_record: bool,
               finished: impl FnOnce(Option<Selection>, bool) + 'static) -> Rc<Selector> {
        let win = floating::window("eov-overlay", true);
        win.set_type_hint(gdk::WindowTypeHint::Normal);
        let cur = current.unwrap_or_default();
        let mode = if cur.screen { "Screen" } else if cur.follows() { "Window" } else { "Area" };
        win.move_(screen.0, screen.1);
        win.set_default_size(screen.2, screen.3);
        let overlay = gtk::Overlay::new();
        let canvas = gtk::DrawingArea::builder().can_focus(true).build();
        canvas.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK
                          | gdk::EventMask::POINTER_MOTION_MASK | gdk::EventMask::LEAVE_NOTIFY_MASK);
        overlay.add(&canvas);
        // the toolbar
        let bar = d::classes(gtk::Box::builder().spacing(12).halign(gtk::Align::Center).valign(gtk::Align::Start)
            .build(), &["toolbar"]);
        let hint_l = d::text(hint(mode), "small");
        hint_l.set_ellipsize(pango::EllipsizeMode::End);
        hint_l.set_max_width_chars(58);
        hint_l.set_width_chars(46);
        let done_btn = d::button("Done", "secondary", None, Some("Choose this, record later"), || {}).button;
        let rec_btn = can_record.then(|| d::button("Record", "primary", Some("record"), None, || {}).button);
        let mode_cell: Rc<RefCell<Option<std::rc::Weak<Selector>>>> = Rc::new(RefCell::new(None));
        let mc = mode_cell.clone();
        let (modes, mode_btns) = d::segmented(&MODES, MODES.iter().position(|m| *m == mode).unwrap(), move |i| {
            if let Some(me) = mc.borrow().as_ref().and_then(|w| w.upgrade()) {
                me.set_mode(MODES[i]);
            }
        });
        let s = Rc::new(Selector {
            win: win.clone(), kwin: kwin.clone(), screen, canvas: canvas.clone(), hint: hint_l.clone(),
            done_btn: done_btn.clone(), rec_btn: rec_btn.clone(), mode_btns, mode: RefCell::new(mode.into()),
            area: RefCell::new(cur.area), windows: RefCell::new(cur.windows.clone()), hover: RefCell::new(None),
            press: RefCell::new(None), dragging: Cell::new(false), done: RefCell::new(Some(Box::new(finished))),
            live: RefCell::new(None), last: RefCell::new(None), listener: Cell::new(None),
        });
        *mode_cell.borrow_mut() = Some(Rc::downgrade(&s));
        bar.pack_start(&modes, false, false, 0);
        bar.pack_start(&hint_l, false, false, 0);
        let me = Rc::downgrade(&s);
        let cancel = d::button("Cancel", "quiet", None, None, move || {
            if let Some(me) = me.upgrade() {
                me.cancel();
            }
        });
        cancel.button.child().and_then(|c| c.downcast::<gtk::Box>().ok())
            .map(|b| b.pack_start(&d::classes(gtk::Label::new(Some("Esc")), &["keyhint"]), false, false, 0));
        bar.pack_start(&cancel.button, false, false, 0);
        let me = Rc::downgrade(&s);
        done_btn.connect_clicked(move |_| {
            if let Some(me) = me.upgrade() {
                me.finish(false);
            }
        });
        bar.pack_start(&done_btn, false, false, 0);
        if let Some(r) = &rec_btn {
            let me = Rc::downgrade(&s);
            r.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.finish(true);
                }
            });
            bar.pack_start(r, false, false, 0);
        }
        overlay.add_overlay(&bar);
        win.add(&overlay);

        let me = s.clone();
        canvas.connect_draw(move |_, cr| {
            me.draw(cr);
            glib::Propagation::Proceed
        });
        let me = s.clone();
        canvas.connect_button_press_event(move |_, ev| me.on_press(ev));
        let me = s.clone();
        canvas.connect_button_release_event(move |_, ev| me.on_release(ev));
        let me = s.clone();
        canvas.connect_motion_notify_event(move |_, ev| me.on_motion(ev));
        let me = s.clone();
        win.connect_key_press_event(move |_, ev| me.on_key(ev));
        let me = Rc::downgrade(&s);
        let listener = kwin.on_change(move || {
            if let Some(me) = me.upgrade() {
                if me.win.is_visible() {
                    me.windows.borrow_mut().retain(|w| me.kwin.window(w).is_some());
                    me.update();
                }
            }
        });
        s.listener.set(Some(listener));
        win.connect_realize(|w| {
            if let Some(g) = w.window() {
                g.set_cursor(gdk::Cursor::from_name(&w.display(), "crosshair").as_ref());
            }
        });
        win.show_all();
        win.fullscreen();
        win.present();
        let w = win.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || w.present());
        s.update();
        s
    }

    /// Start in Window mode with nothing chosen: click the window(s) for the video to follow.
    pub fn pick_window(&self) {
        self.windows.borrow_mut().clear();
        self.mode_btns[1].clicked();
    }

    /// While recording: changed(selection) as soon as what's chosen changes (a window clicked in or out), so the
    /// recording follows along; Done keeps it, Esc goes back.
    pub fn set_live(&self, changed: impl Fn(Selection) + 'static) {
        *self.last.borrow_mut() = Some(self.selection());
        *self.live.borrow_mut() = Some(Box::new(changed));
        self.update();
    }

    fn set_mode(&self, mode: &str) {
        *self.mode.borrow_mut() = mode.into();
        if mode == "Screen" {
            *self.area.borrow_mut() = None;
            self.windows.borrow_mut().clear();
        } else if mode == "Window" {
            *self.area.borrow_mut() = None;
        }
        self.update();
    }

    fn update(&self) {
        let mode = self.mode.borrow().clone();
        let has = self.area.borrow().is_some() || !self.windows.borrow().is_empty();
        let live = self.live.borrow().is_some();
        self.hint.set_text(&if live && mode != "Screen" {
            "Click windows to add them to the recording or take them out. Esc undoes it.".to_string()
        } else if mode != "Area" || !has { hint(&mode).to_string() } else { self.summary() });
        if live {
            let sel = self.selection();
            let valid = sel.screen || sel.rect(&self.kwin, self.screen).is_some();
            if valid && self.last.borrow().as_ref() != Some(&sel) {
                *self.last.borrow_mut() = Some(sel.clone());
                if let Some(f) = self.live.borrow().as_ref() {
                    f(sel);
                }
            }
        }
        let ok = mode == "Screen" || has;
        self.done_btn.set_sensitive(ok);
        if let Some(r) = &self.rec_btn {
            r.set_sensitive(ok);
        }
        self.canvas.queue_draw();
    }

    fn summary(&self) -> String {
        let rect = self.selection().rect(&self.kwin, self.screen);
        let size = rect.map(|r| format!("{} × {}", r.2, r.3)).unwrap_or_default();
        let n = self.windows.borrow().len();
        if n > 0 {
            format!("{size} · only {n} window{}. Enter to record.", if n > 1 { "s" } else { "" })
        } else {
            format!("{size} · everything in it. Click windows to record only them.")
        }
    }

    pub fn selection(&self) -> Selection {
        if *self.mode.borrow() == "Screen" {
            return Selection { screen: true, ..Default::default() };
        }
        Selection { area: if *self.mode.borrow() == "Area" { *self.area.borrow() } else { None },
                    windows: self.windows.borrow().clone(), screen: false }
    }

    pub fn finish(&self, record: bool) {
        let sel = self.selection();
        if !sel.screen && sel.rect(&self.kwin, self.screen).is_none() {
            return;
        }
        let done = self.done.borrow_mut().take();
        self.close();
        if let Some(f) = done {
            f(Some(sel), record && self.rec_btn.is_some());
        }
    }

    pub fn cancel(&self) {
        let done = self.done.borrow_mut().take();
        self.close();
        if let Some(f) = done {
            f(None, false);
        }
    }

    fn close(&self) {
        if let Some(id) = self.listener.take() {
            self.kwin.remove_listener(id);
        }
        d::destroy(&self.win);
    }

    fn on_key(&self, ev: &gdk::EventKey) -> glib::Propagation {
        use gdk::keys::constants as k;
        let key = ev.keyval();
        if key == k::Escape {
            self.cancel();
        } else if key == k::Return || key == k::KP_Enter {
            self.finish(true);
        } else if key == k::BackSpace || key == k::Delete {
            *self.area.borrow_mut() = None;
            self.windows.borrow_mut().clear();
            self.update();
        }
        glib::Propagation::Stop
    }

    fn desk(&self, ev_pos: (f64, f64)) -> (i32, i32) {
        (ev_pos.0 as i32 + self.screen.0, ev_pos.1 as i32 + self.screen.1)
    }

    /// What a press at (x, y) (desktop) would take hold of: a handle ("n", "se", …), "move", or nothing.
    fn what(&self, x: i32, y: i32) -> Option<String> {
        if *self.mode.borrow() != "Area" {
            return None;
        }
        let (ax, ay, aw, ah) = (*self.area.borrow())?;
        let inside_x = ax - GRAB <= x && x <= ax + aw + GRAB;
        let inside_y = ay - GRAB <= y && y <= ay + ah + GRAB;
        let mut v = "";
        if inside_x {
            if (y - ay).abs() <= GRAB { v = "n" } else if (y - (ay + ah)).abs() <= GRAB { v = "s" }
        }
        let mut h = "";
        if inside_y {
            if (x - ax).abs() <= GRAB { h = "w" } else if (x - (ax + aw)).abs() <= GRAB { h = "e" }
        }
        if !v.is_empty() || !h.is_empty() {
            return Some(format!("{v}{h}"));
        }
        (ax <= x && x <= ax + aw && ay <= y && y <= ay + ah).then(|| "move".to_string())
    }

    fn cursor_name(what: Option<&str>) -> &'static str {
        match what {
            Some("n") => "n-resize", Some("s") => "s-resize", Some("e") => "e-resize", Some("w") => "w-resize",
            Some("ne") => "ne-resize", Some("nw") => "nw-resize", Some("se") => "se-resize",
            Some("sw") => "sw-resize", Some("move") => "move", _ => "crosshair",
        }
    }

    fn on_press(&self, ev: &gdk::EventButton) -> glib::Propagation {
        let (x, y) = ev.position();
        self.press(x, y, ev.button())
    }

    fn on_motion(&self, ev: &gdk::EventMotion) -> glib::Propagation {
        let (x, y) = ev.position();
        self.motion(x, y)
    }

    fn on_release(&self, ev: &gdk::EventButton) -> glib::Propagation {
        let (x, y) = ev.position();
        self.release(x, y, ev.button())
    }

    /// A press at (x, y) on the layer (its own coordinates).
    pub fn press(&self, ex: f64, ey: f64, button: u32) -> glib::Propagation {
        if button == 3 {
            *self.area.borrow_mut() = None;
            self.windows.borrow_mut().clear();
            self.update();
            return glib::Propagation::Stop;
        }
        if button != 1 {
            return glib::Propagation::Proceed;
        }
        let (x, y) = self.desk((ex, ey));
        *self.press.borrow_mut() = Some((x, y, self.what(x, y), *self.area.borrow()));
        self.dragging.set(false);
        glib::Propagation::Stop
    }

    pub fn motion(&self, ex: f64, ey: f64) -> glib::Propagation {
        let (x, y) = self.desk((ex, ey));
        let press = self.press.borrow().clone();
        let Some((px, py, what, start)) = press else {
            let what = self.what(x, y);
            if let Some(g) = self.win.window() {
                g.set_cursor(gdk::Cursor::from_name(&self.win.display(), Self::cursor_name(what.as_deref())).as_ref());
            }
            let on_handle = what.as_deref().is_some_and(|w| w != "move");
            let hover = if on_handle || *self.mode.borrow() == "Screen" { None } else { self.kwin.at(x, y).map(|w| w.id) };
            if hover != *self.hover.borrow() {
                *self.hover.borrow_mut() = hover;
                self.canvas.queue_draw();
            }
            return glib::Propagation::Stop;
        };
        if !self.dragging.get() && (x - px).abs() + (y - py).abs() < DRAG {
            return glib::Propagation::Stop;
        }
        if *self.mode.borrow() != "Area" {
            return glib::Propagation::Stop;
        }
        self.dragging.set(true);
        *self.hover.borrow_mut() = None;
        let (sx, sy, sw, sh) = self.screen;
        let new = match (what.as_deref(), start) {
            (None, _) | (_, None) => (px.min(x), py.min(y), (x - px).abs(), (y - py).abs()),
            (Some("move"), Some((ax, ay, aw, ah))) => {
                ((ax + x - px).max(sx).min(sx + sw - aw), (ay + y - py).max(sy).min(sy + sh - ah), aw, ah)
            }
            (Some(w), Some((ax, ay, aw, ah))) => {
                let (mut x0, mut y0, mut x1, mut y1) = (ax, ay, ax + aw, ay + ah);
                if w.contains('w') { x0 = x.min(x1 - layout::MIN_SIDE) }
                if w.contains('e') { x1 = x.max(x0 + layout::MIN_SIDE) }
                if w.contains('n') { y0 = y.min(y1 - layout::MIN_SIDE) }
                if w.contains('s') { y1 = y.max(y0 + layout::MIN_SIDE) }
                (x0, y0, x1 - x0, y1 - y0)
            }
        };
        *self.area.borrow_mut() = Some(new);
        self.update();
        glib::Propagation::Stop
    }

    pub fn release(&self, ex: f64, ey: f64, button: u32) -> glib::Propagation {
        if button != 1 {
            return glib::Propagation::Proceed;
        }
        let Some((_, _, what, _)) = self.press.borrow_mut().take() else { return glib::Propagation::Proceed };
        let (x, y) = self.desk((ex, ey));
        if self.dragging.get() {
            let area = *self.area.borrow();
            if let Some(a) = area {
                let small = a.2 < layout::MIN_SIDE || a.3 < layout::MIN_SIDE;
                *self.area.borrow_mut() = if small && what.is_none() { None }
                                          else { Some(layout::clamp_rect(a, self.screen)) };
            }
            self.dragging.set(false);
            self.update();
            return glib::Propagation::Stop;
        }
        // a click: choose (or un-choose) the window under it
        if *self.mode.borrow() == "Screen" {
            self.finish(false);
            return glib::Propagation::Stop;
        }
        let Some(win) = self.kwin.at(x, y) else { return glib::Propagation::Stop };
        if *self.mode.borrow() == "Area" {
            let area = *self.area.borrow();
            if let Some((ax, ay, aw, ah)) = area {
                if !(ax <= x && x <= ax + aw && ay <= y && y <= ay + ah) {
                    *self.area.borrow_mut() = None; // a window outside the area: record it instead of the area
                    self.windows.borrow_mut().clear();
                }
            }
        }
        {
            let mut ws = self.windows.borrow_mut();
            if let Some(i) = ws.iter().position(|w| *w == win.id) { ws.remove(i); } else { ws.push(win.id); }
        }
        self.update();
        glib::Propagation::Stop
    }

    // --- drawing ---
    fn draw(&self, cr: &cairo::Context) {
        let (sx, sy, _, _) = self.screen;
        cr.translate(-sx as f64, -sy as f64);
        let sel = self.selection();
        let rect = sel.rect(&self.kwin, self.screen);
        let accent = d::rgba("accent");
        let rr = |r: Rect| cr.rectangle(r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64);
        // dim everything but what's recorded
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.45);
        cr.set_fill_rule(cairo::FillRule::EvenOdd);
        rr(self.screen);
        let mut clear: Vec<Rect> = vec![];
        if sel.screen {
            clear.push(self.screen);
        } else if let Some(r) = rect {
            match sel.shown(&self.kwin, rect) {
                None => clear.push(r),
                Some(shown) => clear.extend(shown.iter().map(|s| (r.0 + s.0, r.1 + s.1, s.2, s.3))),
            }
        }
        clear.iter().for_each(|r| rr(*r));
        let _ = cr.fill();
        if sel.screen {
            outline(cr, self.screen, &accent, 3.0, true);
            return;
        }
        if let (Some(r), true) = (rect, self.area.borrow().is_some() && !self.windows.borrow().is_empty()) {
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.2); // the area, around only some windows: the rest faintly
            rr(r);
            clear.iter().for_each(|r| rr(*r));
            let _ = cr.fill();
        }
        for id in self.windows.borrow().iter() {
            if let Some(w) = self.kwin.window(id) {
                outline(cr, w.rect, &accent, 2.0, true);
                badge(cr, w.rect);
            }
        }
        let hover = self.hover.borrow().clone();
        if let Some(h) = hover.filter(|h| !self.dragging.get() && !self.windows.borrow().contains(h)) {
            if let Some(w) = self.kwin.window(&h) {
                cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), 0.10);
                rr(w.rect);
                let _ = cr.fill();
                let mut faint = accent;
                faint.set_alpha(0.7);
                outline(cr, w.rect, &faint, 2.0, true);
                chip(cr, if w.title.is_empty() { &w.app } else { &w.title }, (w.rect.0 + 8) as f64,
                     (w.rect.1 + 8) as f64);
            }
        }
        if let Some(r) = rect {
            outline(cr, r, &accent, 2.0, false);
            if *self.mode.borrow() == "Area" && self.area.borrow().is_some() {
                let (x, y, w, h) = (r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64);
                for (hx, hy) in [(x, y), (x + w / 2.0, y), (x + w, y), (x + w, y + h / 2.0), (x + w, y + h),
                                 (x + w / 2.0, y + h), (x, y + h), (x, y + h / 2.0)] {
                    cr.arc(hx, hy, HANDLE, 0.0, 2.0 * PI);
                    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
                    let _ = cr.fill_preserve();
                    cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), 1.0);
                    cr.set_line_width(2.0);
                    let _ = cr.stroke();
                }
            }
            let above = r.1 - sy > 34;
            chip(cr, &format!("{} × {}", r.2, r.3), r.0 as f64, (if above { r.1 - 30 } else { r.1 + 8 }) as f64);
        }
    }
}

pub fn outline(cr: &cairo::Context, r: Rect, colour: &gdk::RGBA, width: f64, inset: bool) {
    let o = if inset { width / 2.0 } else { -width / 2.0 };
    cr.set_source_rgba(colour.red(), colour.green(), colour.blue(), colour.alpha());
    cr.set_line_width(width);
    cr.rectangle(r.0 as f64 + o, r.1 as f64 + o, r.2 as f64 - 2.0 * o, r.3 as f64 - 2.0 * o);
    let _ = cr.stroke();
}

/// A tick in the corner of a chosen window.
fn badge(cr: &cairo::Context, r: Rect) {
    let (cx, cy) = ((r.0 + r.2 - 18) as f64, (r.1 + 18) as f64);
    cr.arc(cx, cy, 11.0, 0.0, 2.0 * PI);
    d::set_source(cr, "accent", Some(1.0));
    let _ = cr.fill();
    d::draw_icon(cr, "check", cx - 7.0, cy - 7.0, 14.0, &d::rgba("on-accent"));
}

pub fn chip(cr: &cairo::Context, words: &str, x: f64, y: f64) {
    let words: String = words.chars().take(80).collect();
    let (lay, w, h) = d::layout(cr, &words, "caption");
    let (px, py) = (8.0, 4.0);
    d::set_source(cr, "hud", Some(0.95));
    d::round_rect(cr, x, y, w as f64 + 2.0 * px, h as f64 + 2.0 * py, d::R_SM);
    let _ = cr.fill();
    d::set_source(cr, "text", Some(1.0));
    cr.move_to(x + px, y + py);
    d::show_layout(cr, &lay);
}
