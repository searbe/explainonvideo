//! The preview: a small live picture of the video being made, attached to the controls (and kept out of it). The camera can be
//! dragged around in it, and made bigger or smaller by dragging its corner or scrolling over it, while recording.
use crate::app::app;
use crate::design::{self as d, Icon};
use crate::floating;
use crate::{layout, settings};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;
use std::rc::Rc;

const WIDTH: i32 = 400; // the picture's width in the preview

type Press = (f64, f64, &'static str, (f64, f64, f64, f64));

pub struct Preview {
    pub win: gtk::Window,
    frame: RefCell<Option<gdk_pixbuf::Pixbuf>>,
    press: RefCell<Option<Press>>,
    out: Cell<(i32, i32)>,
    title: gtk::Label,
    pub picture: gtk::DrawingArea,
    hint: gtk::Label,
}

impl Preview {
    pub fn new() -> Rc<Preview> {
        let win = floating::window("eov-float", false);
        let card = d::classes(gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build(),
                              &["preview-card"]);
        let head = gtk::EventBox::new();
        let row = gtk::Box::builder().spacing(6).build();
        row.pack_start(&d::classes(Icon::new("grip", 16).widget, &["faint"]), false, false, 2);
        let title = d::text("Preview", "caption");
        row.pack_start(&title, true, true, 0);
        row.pack_end(&d::icon_button("close", "Close the preview", false, || app().toggle_preview()).0, false, false, 0);
        head.add(&row);
        head.connect_button_press_event(move |_, ev| { // (it's attached to the controls: move them both)
            if ev.button() == 1 {
                let (x, y) = ev.root();
                app().controls().drag(x, y, ev.time());
            }
            glib::Propagation::Stop
        });
        card.pack_start(&head, false, false, 0);
        let picture = gtk::DrawingArea::new();
        picture.add_events(gdk::EventMask::BUTTON_PRESS_MASK | gdk::EventMask::BUTTON_RELEASE_MASK
                           | gdk::EventMask::POINTER_MOTION_MASK | gdk::EventMask::SCROLL_MASK
                           | gdk::EventMask::SMOOTH_SCROLL_MASK);
        card.pack_start(&picture, false, false, 0);
        let hint = d::text("", "caption");
        card.pack_start(&hint, false, false, 0);
        win.add(&card);
        floating::blur_card(&win, &card);
        win.connect_size_allocate(|_, _| { glib::idle_add_local_once(|| app().dock_preview()); });
        let p = Rc::new(Preview { win, frame: RefCell::new(None), press: RefCell::new(None), out: Cell::new((1920, 1080)),
                                  title, picture: picture.clone(), hint });
        let me = p.clone();
        picture.connect_draw(move |w, cr| {
            me.draw(w, cr);
            glib::Propagation::Proceed
        });
        let me = p.clone();
        picture.connect_button_press_event(move |_, ev| {
            let (x, y) = ev.position();
            me.press_at(x, y, ev.button());
            glib::Propagation::Stop
        });
        let me = p.clone();
        picture.connect_button_release_event(move |_, _| {
            me.press.borrow_mut().take();
            me.picture.queue_draw();
            glib::Propagation::Stop
        });
        let me = p.clone();
        picture.connect_motion_notify_event(move |_, ev| {
            let (x, y) = ev.position();
            me.motion(x, y);
            glib::Propagation::Stop
        });
        let me = p.clone();
        picture.connect_scroll_event(move |_, ev| me.scroll(ev));
        p.set_size(1920, 1080);
        p
    }

    pub fn set_size(&self, w: i32, h: i32) {
        self.out.set((w, h));
        self.picture.set_size_request(WIDTH, (WIDTH as f64 * h as f64 / w as f64).round().max(60.0) as i32);
        self.title.set_text(&format!("Preview · {w} × {h}"));
        self.win.resize(1, 1);
    }

    pub fn show_frame(&self, pixbuf: gdk_pixbuf::Pixbuf) {
        *self.frame.borrow_mut() = Some(pixbuf);
        self.picture.queue_draw();
    }

    pub fn update_hint(&self) {
        self.hint.set_text(if app().camera_shown() {
            "Drag the camera to move it; drag its corner or scroll to resize it."
        } else {
            "Turn the camera on to place it here."
        });
    }

    // --- the camera, in the preview's coordinates ---
    pub fn scale(&self) -> (f64, f64) {
        let a = self.picture.allocation();
        let (ow, oh) = self.out.get();
        (a.width() as f64 / ow as f64, a.height() as f64 / oh as f64)
    }

    fn cam(&self) -> Option<(f64, f64, f64, f64)> {
        let r = app().camera_rect()?;
        let (sx, sy) = self.scale();
        Some((r.0 as f64 * sx, r.1 as f64 * sy, r.2 as f64 * sx, r.3 as f64 * sy))
    }

    fn over(&self, ex: f64, ey: f64) -> Option<&'static str> {
        let (x, y, w, h) = self.cam()?;
        let near = |cx: f64, cy: f64| (ex - cx).hypot(ey - cy) <= 12.0;
        if near(x + w, y + h) || near(x, y + h) || near(x, y) || near(x + w, y) {
            return Some("size");
        }
        (x <= ex && ex <= x + w && y <= ey && ey <= y + h).then_some("move")
    }

    pub fn press_at(&self, x: f64, y: f64, button: u32) {
        if let (1, Some(what), Some(r)) = (button, self.over(x, y), app().camera_rect()) {
            *self.press.borrow_mut() = Some((x, y, what, (r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64)));
        }
    }

    pub fn release(&self) {
        self.press.borrow_mut().take();
    }

    pub fn motion(&self, ex: f64, ey: f64) {
        let press = *self.press.borrow();
        let Some((px, py, what, (x, y, w, h))) = press else {
            let cursor = match self.over(ex, ey) { Some("move") => "grab", Some("size") => "nwse-resize", _ => "default" };
            if let Some(g) = self.picture.window() {
                g.set_cursor(gdk::Cursor::from_name(&self.picture.display(), cursor).as_ref());
            }
            return;
        };
        let (sx, sy) = self.scale();
        let (dx, dy) = ((ex - px) / sx, (ey - py) / sy);
        let rect = if what == "move" {
            (x + dx, y + dy, w, h)
        } else { // grow from the corner furthest from the pointer's start, keeping the shape
            let (cx, cy) = (x + w / 2.0, y + h / 2.0);
            let sign_x = if px / sx >= cx { 1.0 } else { -1.0 };
            let sign_y = if py / sy >= cy { 1.0 } else { -1.0 };
            let grow = (sign_x * dx / w).max(sign_y * dy / h);
            let (nw, nh) = ((w * (1.0 + grow)).max(40.0), (h * (1.0 + grow)).max(40.0 * h / w));
            (cx - nw / 2.0, cy - nh / 2.0, nw, nh)
        };
        self.set(rect);
    }

    fn set(&self, rect: (f64, f64, f64, f64)) {
        let (ow, oh) = self.out.get();
        let (cx, cy, size) = layout::camera_centre(rect, ow, oh);
        let r4 = |v: f64| (v * 10000.0).round() / 10000.0;
        settings::set("camera_x", r4(cx.clamp(0.0, 1.0)).into());
        settings::set("camera_y", r4(cy.clamp(0.0, 1.0)).into());
        settings::set("camera_size", r4(size.clamp(0.08, 0.9)).into());
        app().place_camera();
        self.picture.queue_draw();
    }

    fn scroll(&self, ev: &gdk::EventScroll) -> glib::Propagation {
        let Some(r) = app().camera_rect() else { return glib::Propagation::Proceed };
        let step = match ev.direction() {
            gdk::ScrollDirection::Up => 1.08,
            gdk::ScrollDirection::Down => 1.0 / 1.08,
            _ => ev.scroll_deltas().filter(|(_, dy)| *dy != 0.0).map(|(_, dy)| 1.0 / (1.0 + dy * 0.06)).unwrap_or(1.0),
        };
        let (x, y, w, h) = (r.0 as f64, r.1 as f64, r.2 as f64, r.3 as f64);
        let (nw, nh) = (w * step, h * step);
        self.set((x + (w - nw) / 2.0, y + (h - nh) / 2.0, nw, nh));
        glib::Propagation::Stop
    }

    fn draw(&self, widget: &gtk::DrawingArea, cr: &cairo::Context) {
        let a = widget.allocation();
        let (aw, ah) = (a.width() as f64, a.height() as f64);
        d::round_rect(cr, 0.0, 0.0, aw, ah, d::R_SM);
        cr.clip();
        if let Some(frame) = self.frame.borrow().as_ref() {
            if let Some(scaled) = frame.scale_simple(a.width(), a.height(), gdk_pixbuf::InterpType::Bilinear) {
                cr.set_source_pixbuf(&scaled, 0.0, 0.0);
                let _ = cr.paint();
            }
        } else {
            d::set_source(cr, "surface-2", Some(1.0));
            let _ = cr.paint();
            let (lay, w, h) = d::layout(cr, "Starting…", "caption");
            d::set_source(cr, "text-3", Some(1.0));
            cr.move_to((aw - w as f64) / 2.0, (ah - h as f64) / 2.0);
            d::show_layout(cr, &lay);
        }
        if let Some((x, y, w, h)) = self.cam() {
            let accent = d::rgba("accent");
            cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), if self.press.borrow().is_some() { 0.9 } else { 0.6 });
            cr.set_line_width(1.5);
            if settings::string("camera_shape") == "circle" {
                cr.arc(x + w / 2.0, y + h / 2.0, w.min(h) / 2.0, 0.0, 2.0 * PI);
            } else {
                d::round_rect(cr, x, y, w, h, h * 0.08);
            }
            let _ = cr.stroke();
            for (hx, hy) in [(x + w, y + h), (x, y), (x + w, y), (x, y + h)] { // the corners to resize it by
                cr.arc(hx, hy, 4.0, 0.0, 2.0 * PI);
                cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(accent.red(), accent.green(), accent.blue(), 1.0);
                cr.set_line_width(1.5);
                let _ = cr.stroke();
            }
        }
    }
}
