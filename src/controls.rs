//! The controls: one small capsule floating over everything, like a call's controls. Record (or stop, pause and
//! the time while recording), what's recorded (area, computer sound, microphone, camera), the preview, the virtual
//! camera, and a menu. It can be dragged anywhere by its grip and remembers where it was left; it is never in the
//! recording.
use crate::app::{app, State};
use crate::design::{self as d, Icon};
use crate::floating;
use crate::layout::Rect;
use crate::settings;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

struct Toggle {
    button: gtk::Button,
    icon: Option<Icon>,
    base: &'static str,
    tips: (&'static str, &'static str),
}

pub struct Controls {
    pub win: gtk::Window,
    state: Cell<State>,
    since: Cell<Option<Instant>>,
    paused_for: Cell<f64>,
    paused_at: Cell<Option<Instant>>,
    record: gtk::Button,
    stop: gtk::Button,
    time: gtk::Label,
    pause: (gtk::Button, Icon),
    discard: gtk::Button,
    more: gtk::Button,
    preview: gtk::Button,
    live: gtk::Button,
    toggles: Vec<(&'static str, Toggle)>,
    mic_name: Rc<RefCell<String>>,
    level: Rc<Cell<f64>>,
    mic_area: gtk::DrawingArea,
    ticking: RefCell<Option<glib::SourceId>>,
    dragged: Cell<bool>,
}

fn sep() -> gtk::Box {
    d::classes(gtk::Box::builder().width_request(1).height_request(20).valign(gtk::Align::Center).build(),
               &["hairline"])
}

impl Controls {
    pub fn new() -> Rc<Controls> {
        let win = floating::window("eov-float", false);
        let card = d::classes(gtk::Box::builder().spacing(2).valign(gtk::Align::Center).halign(gtk::Align::Center)
            .build(), &["capsule"]);
        win.add(&card);
        let grip = d::classes(gtk::EventBox::builder().tooltip_text("Drag to move").valign(gtk::Align::Center)
            .build(), &["grip"]);
        grip.add(&d::classes(Icon::new("grip", 16).widget, &["faint"]));
        card.pack_start(&grip, false, false, 0);
        let record = d::button("Record", "primary", Some("record"), Some("Start recording"), || app().start()).button;
        let stop = d::button("Stop", "stop", Some("stop"), Some("Stop and save"), || app().stop()).button;
        d::classes(record.clone(), &["capsule-main"]);
        d::classes(stop.clone(), &["capsule-main"]);
        let time = d::classes(gtk::Label::builder().label("0:00").width_chars(5).xalign(0.0).build(),
                              &["timer", "tabular"]);
        let pause = d::icon_button("pause", "Pause", false, || app().pause_or_resume());
        card.pack_start(&record, false, false, 0);
        card.pack_start(&stop, false, false, 0);
        card.pack_start(&time, false, false, 0);
        card.pack_start(&pause.0, false, false, 0);
        card.pack_start(&sep(), false, false, 4);
        let area = d::icon_button("area", "Choose what to record", false, || app().choose());
        card.pack_start(&area.0, false, false, 0);

        // the mic's icon shows its loudness as a small bar under it
        let mic_name = Rc::new(RefCell::new("mic".to_string()));
        let level = Rc::new(Cell::new(0.0));
        let mic_area = gtk::DrawingArea::builder().width_request(16).height_request(20)
            .valign(gtk::Align::Center).build();
        let (n, l) = (mic_name.clone(), level.clone());
        mic_area.connect_draw(move |w, cr| {
            let ctx = w.style_context();
            d::draw_icon(cr, &n.borrow(), 0.0, 0.0, 16.0, &ctx.color(ctx.state()));
            if *n.borrow() == "mic" && l.get() > 0.02 {
                d::set_source(cr, "accent", Some(1.0));
                d::round_rect(cr, 1.0, 18.0, (14.0f64 * l.get()).max(2.0), 2.0, 1.0);
                let _ = cr.fill();
            }
            glib::Propagation::Proceed
        });
        let mut toggles = vec![];
        for (name, icon, on_tip, off_tip) in [
            ("system_audio", "speaker", "Recording the computer's sound", "Not recording the computer's sound"),
            ("mic", "mic", "Recording the microphone", "Not recording the microphone"),
            ("camera", "camera", "Showing the camera", "Not showing the camera")] {
            let b = d::classes(gtk::Button::new(), &["btn", "icon"]);
            let i = if name == "mic" {
                b.add(&mic_area);
                None
            } else {
                let i = Icon::new(icon, 16);
                b.add(&i.widget);
                Some(i)
            };
            b.connect_clicked(move |_| app().toggle(name));
            card.pack_start(&b, false, false, 0);
            toggles.push((name, Toggle { button: b, icon: i, base: icon, tips: (on_tip, off_tip) }));
        }
        let preview = d::icon_button("preview", "Show the preview", false, || app().toggle_preview()).0;
        let live = d::icon_button("live", "Virtual camera", false, || app().toggle("virtual_camera")).0;
        card.pack_start(&preview, false, false, 0);
        card.pack_start(&live, false, false, 0);
        card.pack_start(&sep(), false, false, 4);
        let discard = d::icon_button("trash", "Discard this recording", true, || app().discard()).0;
        let more = d::icon_button("more", "More", false, || {}).0;
        card.pack_start(&discard, false, false, 0);
        card.pack_start(&more, false, false, 0);
        let c = Rc::new(Controls {
            win: win.clone(), state: Cell::new(State::Ready), since: Cell::new(None), paused_for: Cell::new(0.0),
            paused_at: Cell::new(None), record, stop, time, pause, discard, more: more.clone(), preview, live,
            toggles, mic_name, level, mic_area, ticking: RefCell::new(None), dragged: Cell::new(false),
        });
        let me = Rc::downgrade(&c);
        more.connect_clicked(move |_| {
            if let Some(me) = me.upgrade() {
                me.menu();
            }
        });
        let me = Rc::downgrade(&c);
        grip.connect_button_press_event(move |_, ev| {
            if let Some(me) = me.upgrade() {
                if ev.button() == 1 {
                    let (x, y) = ev.root();
                    me.drag(x, y, ev.time());
                }
            }
            glib::Propagation::Stop
        });
        let me = Rc::downgrade(&c);
        win.connect_configure_event(move |w, _| {
            app().dock_preview(); // (the preview goes with them)
            if let Some(me) = me.upgrade() {
                if w.is_mapped() && me.dragged.get() { // (only where it was put, not where it first appeared)
                    let w = w.clone();
                    glib::idle_add_local_once(move || {
                        let (x, y) = w.position();
                        settings::set("controls_pos", serde_json::json!([x, y]));
                    });
                }
            }
            false
        });
        floating::blur_card(&win, &card);
        win.show_all();
        c.update();
        c
    }

    /// Move them with the pointer (pressed at x, y on the desktop), from the grip or the preview's title.
    pub fn drag(&self, x: f64, y: f64, time: u32) {
        self.dragged.set(true);
        self.win.begin_move_drag(1, x as i32, y as i32, time);
    }

    fn menu(&self) {
        let menu = gtk::Menu::new();
        let busy = self.state.get() != State::Ready;
        let items: [(Option<&str>, Option<fn()>); 5] = [
            (Some("Edit a recording…"), Some(|| app().open_editor(None))),
            (Some("Open the recordings folder"), Some(|| app().open_folder())),
            (Some("Settings…"), Some(|| app().open_settings())),
            (None, None),
            (Some("Quit"), Some(|| app().quit())),
        ];
        for (label, cb) in items {
            let item: gtk::MenuItem = match label {
                None => gtk::SeparatorMenuItem::new().upcast(),
                Some(l) => gtk::MenuItem::with_label(l),
            };
            if let Some(cb) = cb {
                item.connect_activate(move |_| cb());
            }
            if busy && label == Some("Quit") {
                item.set_sensitive(false);
            }
            menu.append(&item);
        }
        menu.show_all();
        menu.set_attach_widget(Some(&self.more));
        menu.popup_at_widget(&self.more, gdk::Gravity::NorthEast, gdk::Gravity::SouthEast, None);
    }

    pub fn update(&self) {
        let state = self.state.get();
        let rec = state == State::Recording || state == State::Paused;
        self.record.set_visible(state == State::Ready || state == State::Countdown);
        self.record.set_sensitive(state == State::Ready);
        self.stop.set_visible(rec || state == State::Saving);
        self.stop.set_sensitive(rec);
        self.time.set_visible(rec || state == State::Saving);
        self.pause.0.set_visible(rec);
        self.discard.set_visible(rec);
        self.pause.1.set_icon(if state == State::Paused { "play" } else { "pause" });
        self.pause.0.set_tooltip_text(Some(if state == State::Paused { "Resume" } else { "Pause" }));
        d::style(&self.time, "paused", state == State::Paused);
        for (name, t) in &self.toggles {
            let on = settings::bool(name);
            let icon = if on { t.base.to_string() } else { format!("{}-off", t.base) };
            match &t.icon {
                Some(i) => i.set_icon(&icon),
                None => {
                    *self.mic_name.borrow_mut() = icon;
                    self.mic_area.queue_draw();
                }
            }
            t.button.set_tooltip_text(Some(if on { t.tips.0 } else { t.tips.1 }));
            d::style(&t.button, "on", on);
        }
        d::style(&self.preview, "on", app().preview_open());
        let live = settings::bool("virtual_camera");
        d::style(&self.live, "on", live);
        self.live.set_tooltip_text(Some(if live { "Virtual camera: on (other apps can use this video as a camera)" }
                                        else { "Virtual camera: off" }));
        if state == State::Saving {
            self.time.set_text("Saving…");
        }
        if rec && self.ticking.borrow().is_none() {
            let time = self.time.clone();
            let weak = Rc::downgrade(&app().controls());
            *self.ticking.borrow_mut() = Some(glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                let Some(me) = weak.upgrade() else { return glib::ControlFlow::Break };
                let s = me.state.get();
                if s != State::Recording && s != State::Paused {
                    me.ticking.borrow_mut().take();
                    return glib::ControlFlow::Break;
                }
                time.set_text(&clock(me.elapsed()));
                glib::ControlFlow::Continue
            }));
        }
        if rec {
            self.time.set_text(&clock(self.elapsed()));
        }
        self.win.resize(1, 1); // (as small as what it shows)
    }

    pub fn set_state(&self, state: State) {
        let now = Instant::now();
        let old = self.state.get();
        if state == State::Recording && old != State::Paused {
            self.since.set(Some(now));
            self.paused_for.set(0.0);
        }
        if state == State::Recording && old == State::Paused {
            if let Some(at) = self.paused_at.get() {
                self.paused_for.set(self.paused_for.get() + (now - at).as_secs_f64());
            }
        }
        if state == State::Paused {
            self.paused_at.set(Some(now));
        }
        self.state.set(state);
        self.update();
    }

    pub fn elapsed(&self) -> f64 {
        let Some(since) = self.since.get() else { return 0.0 };
        let end = if self.state.get() == State::Paused { self.paused_at.get().unwrap_or_else(Instant::now) }
                  else { Instant::now() };
        (end - since).as_secs_f64() - self.paused_for.get()
    }

    pub fn set_level(&self, level: f64) {
        if (level - self.level.get()).abs() > 0.02 {
            self.level.set(level);
            self.mic_area.queue_draw();
        }
    }

    /// Where it was left, if that's on screen; else the bottom middle of it.
    pub fn place(&self, screen: Rect) {
        self.win.show_all();
        self.update();
        let (w, h) = self.win.size();
        let (sx, sy, sw, sh) = screen;
        let pos = settings::get("controls_pos");
        let p = pos.as_array().and_then(|a| Some((a.first()?.as_i64()? as i32, a.get(1)?.as_i64()? as i32)));
        match p {
            Some((x, y)) if sx <= x && x <= sx + sw - 40 && sy <= y && y <= sy + sh - 20 => self.win.move_(x, y),
            _ => self.win.move_(sx + (sw - w) / 2, sy + sh - h - 72),
        }
    }
}
