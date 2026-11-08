//! Windows that float over everything: transparent, undecorated, on top, out of the task bar and (through KWin,
//! see kwin_watch.js: their role starts eov-) never in a recording. And the moment's HUDs built on them: the
//! countdown and notices.
use crate::design::{self as d, Hud};
use crate::layout::Rect;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

/// A transparent floating window; draw on it, or add widgets (their background stays see-through).
pub fn window(role: &str, focus: bool) -> gtk::Window {
    let win = gtk::Window::builder().title("Explain on video").decorated(false).resizable(false)
        .app_paintable(true).skip_taskbar_hint(true).skip_pager_hint(true)
        .type_hint(gdk::WindowTypeHint::Utility).accept_focus(focus).build();
    win.set_role(role);
    if let Some(visual) = WidgetExt::screen(&win).and_then(|s| s.rgba_visual()) {
        win.set_visual(Some(&visual));
    }
    d::grayscale(&win);
    win.set_keep_above(true);
    win.stick();
    d::classes(win.clone(), &["hud-window", "eov"]);
    win.connect_draw(|_, cr| {
        d::clear(cr);
        glib::Propagation::Proceed
    });
    win
}

/// Ask KWin to blur behind card (kept up to date as it changes size).
pub fn blur_card(win: &gtk::Window, card: &impl IsA<gtk::Widget>) {
    let c = card.clone().upcast::<gtk::Widget>();
    win.connect_map_event(move |w, _| {
        d::blur_behind(w, &c);
        glib::Propagation::Proceed
    });
    let c = card.clone().upcast::<gtk::Widget>();
    win.connect_size_allocate(move |w, _| d::blur_behind(w, &c));
}

/// Let clicks through everywhere but rects (window coordinates).
pub fn clickable_only(win: &gtk::Window, rects: &[(f64, f64, f64, f64)]) {
    let Some(gdk) = win.window() else { return };
    let region = cairo::Region::create();
    for (x, y, w, h) in rects {
        let _ = region.union_rectangle(&cairo::RectangleInt::new(*x as i32, *y as i32, (*w as i32).max(1),
                                                                 (*h as i32).max(1)));
    }
    gdk.input_shape_combine_region(&region, 0, 0);
}

/// Move win to the middle of rect.
pub fn centre_on(win: &gtk::Window, rect: Rect) {
    let (w, h) = win.size();
    win.move_((rect.0 as f64 + (rect.2 - w) as f64 / 2.0) as i32, (rect.1 as f64 + (rect.3 - h) as f64 / 2.0) as i32);
}

/// A HUD card in a floating window that stays out of recordings.
pub fn hud(title: &str, width: i32, focus: bool) -> Hud {
    let h = Hud::new(title, width);
    h.win.set_role("eov-float");
    h.win.set_skip_pager_hint(true);
    h.win.stick();
    h.win.set_accept_focus(focus);
    d::classes(h.win.clone(), &["eov"]);
    h
}

pub type Action = (String, Option<Rc<dyn Fn()>>, String);

pub fn action(label: &str, kind: &str, cb: impl Fn() + 'static) -> Action {
    (label.to_string(), Some(Rc::new(cb)), kind.to_string())
}

pub fn close_action(label: &str, kind: &str) -> Action {
    (label.to_string(), None, kind.to_string())
}

/// A short HUD: a title, a line of explanation and buttons (label, callback, kind); closes when a button is
/// pressed (or after timeout seconds).
pub fn notice(title: &str, body: Option<&str>, near: Option<Rect>, buttons: Vec<Action>, icon: Option<&str>,
              warning: bool, timeout: Option<u32>) -> Hud {
    let h = hud(title, 380, true);
    h.heading(title, icon.or(if warning { Some("warning") } else { None }), warning);
    if let Some(b) = body {
        h.card.pack_start(&d::wrapped(b, "small", 50), false, false, 0);
    }
    let buttons = if buttons.is_empty() { vec![close_action("Close", "secondary")] } else { buttons };
    let made: Vec<gtk::Button> = buttons.into_iter().map(|(label, cb, kind)| {
        let h2 = h.clone();
        d::button(&label, &kind, None, None, move || {
            h2.close();
            if let Some(cb) = &cb {
                cb();
            }
        }).button
    }).collect();
    h.buttons(&made);
    h.win.show_all();
    match near {
        Some(r) => centre_on(&h.win, r),
        None => h.win.set_position(gtk::WindowPosition::Center),
    }
    h.win.present();
    if let Some(t) = timeout {
        let h2 = h.clone();
        glib::timeout_add_seconds_local_once(t, move || {
            if h2.is_open() {
                h2.close();
            }
        });
    }
    h
}

/// 3, 2, 1 in the middle of the area, then done(); cancelled (Esc or a click) calls cancel() instead.
pub struct Countdown {
    pub win: gtk::Window,
    left: Cell<i32>,
    started: std::time::Instant,
    timer: RefCell<Option<glib::SourceId>>,
    done: RefCell<Option<Box<dyn FnOnce(bool)>>>,
}

const SIZE: i32 = 168;

impl Countdown {
    pub fn new(seconds: i32, rect: Rect, finished: impl FnOnce(bool) + 'static) -> Rc<Countdown> {
        let win = window("eov-float", true);
        win.set_size_request(SIZE, SIZE + 40);
        win.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::KEY_PRESS_MASK);
        let c = Rc::new(Countdown { win: win.clone(), left: Cell::new(seconds), started: std::time::Instant::now(),
                                    timer: RefCell::new(None), done: RefCell::new(Some(Box::new(finished))) });
        let me = c.clone();
        win.connect_draw(move |_, cr| {
            me.paint(cr);
            glib::Propagation::Proceed
        });
        let me = c.clone();
        win.connect_button_press_event(move |_, _| {
            me.stop(true);
            glib::Propagation::Stop
        });
        let me = c.clone();
        win.connect_key_press_event(move |_, ev| {
            use gdk::keys::constants as k;
            let key = ev.keyval();
            if key == k::Escape {
                me.stop(true);
            } else if key == k::Return || key == k::KP_Enter || key == k::space {
                me.stop(false);
            }
            glib::Propagation::Stop
        });
        win.show_all();
        centre_on(&win, rect);
        win.present();
        let me = c.clone();
        *c.timer.borrow_mut() = Some(glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            me.left.set(me.left.get() - 1);
            if me.left.get() <= 0 {
                me.timer.borrow_mut().take();
                me.stop(false);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        }));
        let me = c.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            if me.done.borrow().is_some() {
                me.win.queue_draw();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        c
    }

    pub fn stop(&self, cancelled: bool) {
        if let Some(t) = self.timer.borrow_mut().take() {
            t.remove();
        }
        let done = self.done.borrow_mut().take();
        d::destroy(&self.win);
        if let Some(f) = done {
            f(cancelled);
        }
    }

    fn paint(&self, cr: &cairo::Context) {
        let s = SIZE as f64;
        let c = s / 2.0;
        cr.arc(c, c, c - 12.0, 0.0, 2.0 * PI);
        d::set_source(cr, "hud", Some(0.92));
        let _ = cr.fill_preserve();
        d::set_source(cr, "line-strong", None);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        let frac = self.started.elapsed().as_secs_f64() % 1.0; // the second running out, in the record colour
        d::set_source(cr, "record", Some(1.0));
        cr.set_line_width(4.0);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.arc(c, c, c - 22.0, -PI / 2.0, -PI / 2.0 + 2.0 * PI * (1.0 - frac));
        let _ = cr.stroke();
        let (lay, w, h) = d::layout(cr, &self.left.get().max(1).to_string(), "headline");
        let _ = cr.save();
        cr.translate(c, c);
        cr.scale(3.0, 3.0);
        d::set_source(cr, "text", Some(1.0));
        cr.move_to(-w as f64 / 2.0, -h as f64 / 2.0);
        d::show_layout(cr, &lay);
        let _ = cr.restore();
        let (lay, w, _) = d::layout(cr, "Click or Esc to cancel", "caption");
        cr.move_to(c - w as f64 / 2.0, s + 8.0);
        d::set_source(cr, "text-2", Some(1.0));
        d::show_layout(cr, &lay);
    }
}
