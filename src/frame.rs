//! The outline around the area being recorded: indigo while ready, red while recording, grey while paused. Its
//! edges and corners can be dragged to resize the area, and its tab to move it, even while recording; everywhere
//! else clicks go through to what's underneath. Once the area no longer follows a window (it was dragged, or
//! drawn), a "Follow" button beside the tab attaches it to a window again; while recording, a "Windows…" button adds or takes out windows. It sits just outside the area (and is kept out of recordings).
use crate::app::State;
use crate::design as d;
use crate::floating;
use crate::layout::{self, Rect};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const M: i32 = 12; // the frame's band around the area: the outline, and room to grab it
const TAB_H: i32 = 26;

pub struct Frame {
    pub win: gtk::Window,
    pub screen: Cell<Rect>,
    pub area: Cell<Option<Rect>>,
    state: Cell<State>,
    label: RefCell<String>,
    origin: Cell<(i32, i32)>,
    tab_w: Cell<f64>,
    press: RefCell<Option<(f64, f64, Option<String>, Rect)>>,
    follow: RefCell<Option<String>>,
    follow_w: Cell<f64>,
    pick: Cell<bool>,
    pick_w: Cell<f64>,
    on_change: Box<dyn Fn(Rect)>,
    on_done: Box<dyn Fn(Rect)>,
    on_follow: Box<dyn Fn()>,
    on_pick: Box<dyn Fn()>,
}

impl Frame {
    /// screen: where the area may go; on_change(rect) while it's dragged, on_done(rect) after; on_follow() when its
    /// Follow button is clicked, on_pick() when its Windows button is (to add or take out windows while recording).
    pub fn new(screen: Rect, on_change: impl Fn(Rect) + 'static, on_done: impl Fn(Rect) + 'static,
               on_follow: impl Fn() + 'static, on_pick: impl Fn() + 'static) -> Rc<Frame> {
        let win = floating::window("eov-float", false);
        win.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK
                       | gdk::EventMask::POINTER_MOTION_MASK | gdk::EventMask::LEAVE_NOTIFY_MASK);
        let f = Rc::new(Frame {
            win: win.clone(), screen: Cell::new(screen), area: Cell::new(None), state: Cell::new(State::Ready),
            label: RefCell::new(String::new()), origin: Cell::new((0, 0)), tab_w: Cell::new(120.0),
            press: RefCell::new(None), follow: RefCell::new(None), follow_w: Cell::new(0.0),
            pick: Cell::new(false), pick_w: Cell::new(84.0),
            on_change: Box::new(on_change), on_done: Box::new(on_done), on_follow: Box::new(on_follow),
            on_pick: Box::new(on_pick),
        });
        let me = f.clone();
        win.connect_draw(move |_, cr| {
            me.draw(cr);
            glib::Propagation::Proceed
        });
        let me = f.clone();
        win.connect_button_press_event(move |_, ev| {
            let Some(area) = me.area.get() else { return glib::Propagation::Proceed };
            if ev.button() != 1 {
                return glib::Propagation::Proceed;
            }
            let (rx, ry) = ev.root();
            let (x, y) = ev.position();
            let what = me.what(x, y);
            if what.as_deref() == Some("follow") {
                (me.on_follow)();
                return glib::Propagation::Stop;
            }
            if what.as_deref() == Some("pick") {
                (me.on_pick)();
                return glib::Propagation::Stop;
            }
            *me.press.borrow_mut() = Some((rx, ry, what, area));
            glib::Propagation::Stop
        });
        let me = f.clone();
        win.connect_button_release_event(move |_, _| {
            if me.press.borrow_mut().take().is_some() {
                if let Some(a) = me.area.get() {
                    (me.on_done)(a);
                }
            }
            glib::Propagation::Stop
        });
        let me = f.clone();
        win.connect_motion_notify_event(move |_, ev| me.motion(ev));
        let me = f.clone();
        win.connect_size_allocate(move |_, _| me.shape());
        f
    }

    pub fn set_area(&self, rect: Rect, label: Option<&str>) {
        if let Some(l) = label {
            *self.label.borrow_mut() = l.to_string();
        }
        if Some(rect) == self.area.get() && self.win.is_visible() {
            self.win.queue_draw();
            return;
        }
        self.area.set(Some(rect));
        let (x, y, w, h) = rect;
        let top = if self.tab_above() { TAB_H } else { 0 };
        self.origin.set((x - M, y - M - top));
        self.win.set_size_request(w + 2 * M, h + 2 * M + top);
        self.win.resize(w + 2 * M, h + 2 * M + top);
        self.win.move_(x - M, y - M - top);
        if !self.win.is_visible() {
            self.win.show_all();
        }
        self.shape();
        self.win.queue_draw();
    }

    pub fn set_state(&self, state: State) {
        self.state.set(state);
        self.shape();
        self.win.queue_draw();
    }

    /// Whether windows can be added and taken out (the Windows button, while recording).
    pub fn set_pick(&self, can: bool) {
        if self.pick.replace(can) != can {
            self.shape();
            self.win.queue_draw();
        }
    }

    fn picking(&self) -> bool {
        self.pick.get() && matches!(self.state.get(), State::Recording | State::Paused)
    }

    /// The Follow button's words (None: no button), e.g. "Follow Firefox" or "Follow a window".
    pub fn set_follow(&self, words: Option<&str>) {
        let words = words.map(|w| w.chars().take(40).collect::<String>());
        if *self.follow.borrow() != words {
            *self.follow.borrow_mut() = words;
            self.shape();
            self.win.queue_draw();
        }
    }

    pub fn hide(&self) {
        self.win.hide();
    }

    fn tab_above(&self) -> bool {
        self.area.get().map(|a| a.1 - self.screen.get().1 >= M + TAB_H).unwrap_or(true)
    }

    fn local(&self) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = self.area.get().unwrap_or((0, 0, 0, 0));
        let (ox, oy) = self.origin.get();
        ((x - ox) as f64, (y - oy) as f64, w as f64, h as f64)
    }

    fn tab_rect(&self) -> (f64, f64, f64, f64) {
        let (x, y, _, _) = self.local();
        let (m, th) = (M as f64, TAB_H as f64);
        if self.tab_above() { (x - 2.0, y - m - th + 4.0, self.tab_w.get(), th - 2.0) }
        else { (x + 6.0, y + 6.0, self.tab_w.get(), th - 2.0) }
    }

    /// The Follow button, just after the tab (None when there isn't one).
    fn follow_rect(&self) -> Option<(f64, f64, f64, f64)> {
        self.follow.borrow().as_ref()?;
        let (tx, ty, tw, th) = self.tab_rect();
        Some((tx + tw + 6.0, ty, self.follow_w.get().max(th), th))
    }

    /// The Windows button, after the tab and the Follow button (None when there isn't one).
    fn pick_rect(&self) -> Option<(f64, f64, f64, f64)> {
        if !self.picking() {
            return None;
        }
        let (bx, by, bw, bh) = self.follow_rect().unwrap_or_else(|| self.tab_rect());
        Some((bx + bw + 6.0, by, self.pick_w.get(), bh))
    }

    fn shape(&self) {
        if self.area.get().is_none() {
            return;
        }
        let (x, y, w, h) = self.local();
        let m = M as f64;
        let mut parts = vec![(x - m, y - m, w + 2.0 * m, m), (x - m, y + h, w + 2.0 * m, m),
                             (x - m, y, m, h), (x + w, y, m, h), self.tab_rect()];
        parts.extend(self.follow_rect());
        parts.extend(self.pick_rect());
        floating::clickable_only(&self.win, &parts);
    }

    fn what(&self, ex: f64, ey: f64) -> Option<String> {
        self.area.get()?;
        let (x, y, w, h) = self.local();
        if let Some((fx, fy, fw, fh)) = self.follow_rect() {
            if fx <= ex && ex <= fx + fw && fy <= ey && ey <= fy + fh {
                return Some("follow".into());
            }
        }
        if let Some((px, py, pw, ph)) = self.pick_rect() {
            if px <= ex && ex <= px + pw && py <= ey && ey <= py + ph {
                return Some("pick".into());
            }
        }
        let (tx, ty, tw, th) = self.tab_rect();
        if tx <= ex && ex <= tx + tw && ty <= ey && ey <= ty + th {
            return Some("move".into());
        }
        let mut v = if ey < y { "n" } else if ey > y + h { "s" } else { "" };
        let mut hz = if ex < x { "w" } else if ex > x + w { "e" } else { "" };
        let corner = 24.0; // (near a corner: both ways)
        if v.is_empty() && (ey < y + corner || ey > y + h - corner) && !hz.is_empty() {
            v = if ey < y + corner { "n" } else { "s" };
        }
        if hz.is_empty() && (ex < x + corner || ex > x + w - corner) && !v.is_empty() {
            hz = if ex < x + corner { "w" } else { "e" };
        }
        let s = format!("{v}{hz}");
        (!s.is_empty()).then_some(s)
    }

    fn motion(&self, ev: &gdk::EventMotion) -> glib::Propagation {
        let press = self.press.borrow().clone();
        let Some((px, py, what, (ax, ay, aw, ah))) = press else {
            let (x, y) = ev.position();
            let name = match self.what(x, y).as_deref() {
                Some("n") => "n-resize", Some("s") => "s-resize", Some("e") => "e-resize", Some("w") => "w-resize",
                Some("ne") => "ne-resize", Some("nw") => "nw-resize", Some("se") => "se-resize",
                Some("sw") => "sw-resize", Some("move") => "move", Some("follow") | Some("pick") => "pointer",
                _ => "default",
            };
            if let Some(g) = self.win.window() {
                g.set_cursor(gdk::Cursor::from_name(&self.win.display(), name).as_ref());
            }
            return glib::Propagation::Proceed;
        };
        let (rx, ry) = ev.root();
        let (dx, dy) = ((rx - px) as i32, (ry - py) as i32);
        let rect = match what.as_deref() {
            Some("move") => layout::clamp_rect((ax + dx, ay + dy, aw, ah), self.screen.get()),
            Some(w) => {
                let (mut x0, mut y0, mut x1, mut y1) = (ax, ay, ax + aw, ay + ah);
                if w.contains('w') { x0 = (x0 + dx).min(x1 - layout::MIN_SIDE) }
                if w.contains('e') { x1 = (x1 + dx).max(x0 + layout::MIN_SIDE) }
                if w.contains('n') { y0 = (y0 + dy).min(y1 - layout::MIN_SIDE) }
                if w.contains('s') { y1 = (y1 + dy).max(y0 + layout::MIN_SIDE) }
                layout::clamp_rect((x0, y0, x1 - x0, y1 - y0), self.screen.get())
            }
            _ => return glib::Propagation::Stop,
        };
        if Some(rect) != self.area.get() {
            self.set_area(rect, None);
            (self.on_change)(rect);
        }
        glib::Propagation::Stop
    }

    /// For tests: drag from a handle by (dx, dy) on the desktop.
    pub fn drag(&self, what: &str, dx: i32, dy: i32) {
        let Some(a) = self.area.get() else { return };
        *self.press.borrow_mut() = Some((0.0, 0.0, Some(what.into()), a));
        let (ax, ay, aw, ah) = a;
        let rect = match what {
            "move" => layout::clamp_rect((ax + dx, ay + dy, aw, ah), self.screen.get()),
            _ => layout::clamp_rect((ax, ay, (aw + dx).max(layout::MIN_SIDE), (ah + dy).max(layout::MIN_SIDE)),
                                    self.screen.get()),
        };
        self.set_area(rect, None);
        (self.on_change)(rect);
        self.press.borrow_mut().take();
        (self.on_done)(rect);
    }

    fn draw(&self, cr: &cairo::Context) {
        let Some(area) = self.area.get() else { return };
        let colour = match self.state.get() {
            State::Recording => d::rgba("record"),
            State::Paused => d::rgba("text-3"),
            _ => d::rgba("accent"),
        };
        let (x, y, w, h) = self.local();
        cr.set_source_rgba(colour.red(), colour.green(), colour.blue(), 1.0);
        cr.set_line_width(2.0);
        cr.rectangle(x - 2.0, y - 2.0, w + 4.0, h + 4.0); // (just outside the area)
        let _ = cr.stroke();
        cr.set_line_width(4.0); // corner marks, to show it can be grabbed
        cr.set_line_cap(cairo::LineCap::Round);
        let n = 18f64.min(w / 4.0).min(h / 4.0);
        for (cx, cy, sx, sy) in [(x - 3.0, y - 3.0, 1.0, 1.0), (x + w + 3.0, y - 3.0, -1.0, 1.0),
                                 (x - 3.0, y + h + 3.0, 1.0, -1.0), (x + w + 3.0, y + h + 3.0, -1.0, -1.0)] {
            cr.move_to(cx, cy + sy * n);
            cr.line_to(cx, cy);
            cr.line_to(cx + sx * n, cy);
            let _ = cr.stroke();
        }
        let label = self.label.borrow();
        let mut words = format!("{} × {}", area.2, area.3);
        if !label.is_empty() {
            words += &format!(" · {label}");
        }
        let words: String = words.chars().take(60).collect();
        let (lay, tw, th) = d::layout(cr, &words, "caption");
        let old_w = self.tab_w.replace(tw as f64 + 36.0);
        let (tx, ty, _, tht) = self.tab_rect();
        d::set_source(cr, "hud", Some(0.94));
        d::round_rect(cr, tx, ty, self.tab_w.get(), tht, d::R_SM);
        let _ = cr.fill();
        d::draw_icon(cr, "grip", tx + 6.0, ty + (tht - 14.0) / 2.0, 14.0, &d::rgba("text-3"));
        d::set_source(cr, "text", Some(1.0));
        cr.move_to(tx + 24.0, ty + (tht - th as f64) / 2.0);
        d::show_layout(cr, &lay);
        let mut changed = (old_w - self.tab_w.get()).abs() > 0.5;
        let follow = self.follow.borrow().clone();
        if let Some(words) = follow {
            let (lay, fw, fh) = d::layout(cr, &words, "caption");
            changed |= (self.follow_w.replace(fw as f64 + 34.0) - self.follow_w.get()).abs() > 0.5;
            let (fx, fy, fw, fht) = self.follow_rect().unwrap();
            d::set_source(cr, "accent", Some(1.0));
            d::round_rect(cr, fx, fy, fw, fht, d::R_SM);
            let _ = cr.fill();
            d::draw_icon(cr, "link", fx + 7.0, fy + (fht - 14.0) / 2.0, 14.0, &d::rgba("on-accent"));
            d::set_source(cr, "on-accent", Some(1.0));
            cr.move_to(fx + 26.0, fy + (fht - fh as f64) / 2.0);
            d::show_layout(cr, &lay);
        }
        if let Some((px, py, _, pht)) = self.pick_rect() {
            let (lay, pw, ph) = d::layout(cr, "Windows…", "caption");
            changed |= (self.pick_w.replace(pw as f64 + 34.0) - self.pick_w.get()).abs() > 0.5;
            let pw = self.pick_w.get();
            d::set_source(cr, "hud", Some(0.94));
            d::round_rect(cr, px, py, pw, pht, d::R_SM);
            let _ = cr.fill();
            d::draw_icon(cr, "window", px + 7.0, py + (pht - 14.0) / 2.0, 14.0, &d::rgba("text"));
            d::set_source(cr, "text", Some(1.0));
            cr.move_to(px + 26.0, py + (pht - ph as f64) / 2.0);
            d::show_layout(cr, &lay);
        }
        if changed { // (the sizes are known now: let them be grabbed all along)
            self.shape();
        }
    }
}
