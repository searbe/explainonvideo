//! KDE Plasma's window manager, KWin, through a small script loaded into it (kwin_watch.js): where every window
//! is, kept up to date; and the app's own floating windows kept on top and out of recordings.
//!
//! Also makes the app a single instance: it owns the D-Bus name BUS, and a second launch asks the first to show
//! itself (Activate) instead.
use crate::layout::Rect;
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const BUS: &str = "local.explainonvideo";
const PLUGIN: &str = "explainonvideo-watch";

/// The D-Bus name and KWin script name of this copy: EOV_INSTANCE gives a separate copy (for the test drive, so it
/// runs beside the app in use).
fn instance(base: &str) -> String {
    match std::env::var("EOV_INSTANCE") {
        Ok(i) if !i.is_empty() => format!("{base}.{i}"),
        _ => base.to_string(),
    }
}
const SCRIPT: &str = include_str!("kwin_watch.js");
const XML: &str = r#"<node><interface name="local.explainonvideo">
<method name="Windows"><arg type="s" direction="in"/></method>
<method name="Activate"/>
</interface></node>"#;

#[derive(Clone, Debug)]
pub struct Window {
    pub id: String,
    pub title: String,
    pub app: String,
    pub rect: Rect,
    /// A menu, tooltip or other popup: recorded with its window, never chosen itself.
    pub popup: bool,
    /// The window it belongs to (a popup's or a dialog's), if KWin knows it.
    pub parent: String,
    pub pid: i64,
}

type Listener = Rc<dyn Fn()>;

/// windows: every visible window, bottom first; screens. Listeners in on_change are called whenever either
/// changes; on_activate when another launch asks this one to show itself.
#[derive(Default)]
pub struct KWin {
    pub windows: RefCell<Vec<Window>>,
    /// windows and the popups (menus, tooltips) among them, bottom first.
    pub all: RefCell<Vec<Window>>,
    pub screens: RefCell<Vec<Rect>>,
    on_change: RefCell<Vec<(usize, Listener)>>,
    next_listener: Cell<usize>,
    on_activate: RefCell<Vec<Listener>>,
    pub available: Cell<bool>,
    conn: RefCell<Option<gio::DBusConnection>>,
    script: RefCell<Option<std::path::PathBuf>>,
}

impl KWin {
    pub fn new() -> Rc<KWin> {
        Rc::new(KWin::default())
    }

    /// Call f whenever the windows or screens change; the id is for remove_listener.
    pub fn on_change(&self, f: impl Fn() + 'static) -> usize {
        let id = self.next_listener.get();
        self.next_listener.set(id + 1);
        self.on_change.borrow_mut().push((id, Rc::new(f)));
        id
    }

    pub fn remove_listener(&self, id: usize) {
        self.on_change.borrow_mut().retain(|(i, _)| *i != id);
    }

    pub fn on_activate(&self, f: impl Fn() + 'static) {
        self.on_activate.borrow_mut().push(Rc::new(f));
    }

    /// Own the app's name; on_started() once that's done (and the script is loaded, if KWin is there), or
    /// on_already_running() if another copy has it (which has been asked to show itself).
    pub fn start(self: &Rc<Self>, on_started: impl Fn() + 'static, on_already_running: impl Fn() + 'static) {
        let got = crate::util::MainOnly::new((self.clone(), on_started));
        let lost = crate::util::MainOnly::new((self.clone(), on_already_running));
        let id = gio::bus_own_name(
            gio::BusType::Session, &instance(BUS), gio::BusNameOwnerFlags::DO_NOT_QUEUE,
            |_, _| {},
            move |conn, _| {
                let (me, on_started) = got.get();
                me.register(&conn);
                *me.conn.borrow_mut() = Some(conn);
                me.load();
                on_started();
            },
            move |conn, _| {
                let (me2, on_already_running) = lost.get();
                if me2.conn.borrow().is_none() {
                    if let Some(conn) = conn {
                        let _ = conn.call_sync(Some(&instance(BUS)), "/", BUS, "Activate", None, None,
                                               gio::DBusCallFlags::NONE, 2000, gio::Cancellable::NONE);
                    }
                    on_already_running();
                }
            });
        let _ = id; // (owned for the app's life: never unowned)
    }

    fn register(self: &Rc<Self>, conn: &gio::DBusConnection) {
        let info = gio::DBusNodeInfo::for_xml(XML).expect("D-Bus interface");
        let iface = info.lookup_interface(BUS).expect("interface");
        let me = crate::util::MainOnly::new(Rc::downgrade(self));
        let reg = conn.register_object("/", &iface,
            move |_, _, _, _, method, params, invocation| {
                invocation.return_value(None);
                let Some(me) = me.get().upgrade() else { return };
                if method == "Activate" {
                    let fs: Vec<Listener> = me.on_activate.borrow().clone();
                    fs.iter().for_each(|f| f());
                } else if let Some((text,)) = params.get::<(String,)>() {
                    me.update(&text);
                }
            },
            |_, _, _, _, _| ().to_variant(),
            |_, _, _, _, _, _| false);
        if let Err(e) = reg {
            crate::warn!("couldn't offer the D-Bus object: {e}");
        }
    }

    fn update(&self, text: &str) {
        let Ok(data) = serde_json::from_str::<serde_json::Value>(text) else { return };
        let rect = |v: &serde_json::Value| -> Rect {
            let n = |i: usize| v.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0).round() as i32;
            (n(0), n(1), n(2), n(3))
        };
        let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let all: Vec<Window> = data["windows"].as_array().map(|ws| ws.iter().map(|w| Window {
            id: s(w, "id"), title: s(w, "title"), app: s(w, "app"), rect: rect(&w["rect"]),
            popup: w["popup"].as_bool().unwrap_or(false), parent: s(w, "parent"), pid: w["pid"].as_i64().unwrap_or(0),
        }).collect()).unwrap_or_default();
        *self.windows.borrow_mut() = all.iter().filter(|w| !w.popup).cloned().collect();
        *self.all.borrow_mut() = all;
        *self.screens.borrow_mut() = data["screens"].as_array()
            .map(|ss| ss.iter().map(|s| rect(&s["rect"])).collect()).unwrap_or_default();
        let fs: Vec<Listener> = self.on_change.borrow().iter().map(|(_, f)| f.clone()).collect();
        fs.iter().for_each(|f| f());
    }

    fn load(&self) {
        let visible = if std::env::var_os("EOV_SHOW_IN_CAPTURE").is_some() { "true" } else { "false" };
        let pid = std::process::id();
        let js = SCRIPT.replace("__PID__", &pid.to_string()).replace("__BUS__", &instance(BUS)).replace("__VISIBLE__", visible);
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/tmp".into());
        let path = dir.join(format!("explainonvideo-{pid}.js"));
        if std::fs::write(&path, js).is_err() {
            return;
        }
        let conn = self.conn.borrow().clone().unwrap();
        let call = |method: &str, args: Option<&glib::Variant>| {
            conn.call_sync(Some("org.kde.KWin"), "/Scripting", "org.kde.kwin.Scripting", method, args, None,
                           gio::DBusCallFlags::NONE, 3000, gio::Cancellable::NONE)
        };
        let loaded = call("isScriptLoaded", Some(&(instance(PLUGIN),).to_variant()))
            .ok().and_then(|v| v.get::<(bool,)>()).map(|v| v.0).unwrap_or(false);
        if loaded { // left from a copy that crashed
            let _ = call("unloadScript", Some(&(instance(PLUGIN),).to_variant()));
        }
        let path_s = path.to_string_lossy().to_string();
        match call("loadScript", Some(&(path_s.as_str(), instance(PLUGIN).as_str()).to_variant())).and_then(|_| call("start", None)) {
            Ok(_) => {
                self.available.set(true);
                *self.script.borrow_mut() = Some(path);
            }
            Err(e) => crate::warn!("KWin scripting isn't available ({e}): windows can't be chosen"),
        }
    }

    /// Take the script out of KWin (on quitting).
    pub fn unload(&self) {
        if let Some(conn) = self.conn.borrow().as_ref() {
            let _ = conn.call_sync(Some("org.kde.KWin"), "/Scripting", "org.kde.kwin.Scripting", "unloadScript",
                                   Some(&(instance(PLUGIN),).to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                                   gio::Cancellable::NONE);
        }
        if let Some(p) = self.script.borrow_mut().take() {
            let _ = std::fs::remove_file(p);
        }
    }

    pub fn window(&self, id: &str) -> Option<Window> {
        self.windows.borrow().iter().find(|w| w.id == id).cloned()
    }

    /// The topmost window at (x, y) on the desktop.
    pub fn at(&self, x: i32, y: i32) -> Option<Window> {
        self.windows.borrow().iter().rev()
            .find(|w| w.rect.0 <= x && x < w.rect.0 + w.rect.2 && w.rect.1 <= y && y < w.rect.1 + w.rect.3).cloned()
    }

    /// [(id, rect)] bottom first (popups too), for layout::visible.
    pub fn stacking(&self) -> Vec<(String, Rect)> {
        self.all.borrow().iter().map(|w| (w.id.clone(), w.rect)).collect()
    }

    /// The chosen windows and what belongs to them (their dialogs, menus and tooltips), [(id, rect)] bottom first.
    pub fn with_popups(&self, chosen: &[String]) -> Vec<(String, Rect)> {
        let all = self.all.borrow();
        let mut ids: std::collections::HashSet<&str> = chosen.iter().map(|s| s.as_str()).collect();
        let pids: std::collections::HashSet<i64> = all.iter().filter(|w| ids.contains(w.id.as_str()) && w.pid > 0)
            .map(|w| w.pid).collect();
        loop {
            let before = ids.len();
            for w in all.iter() {
                let belongs = if w.parent.is_empty() { w.popup && pids.contains(&w.pid) }
                              else { ids.contains(w.parent.as_str()) };
                if belongs {
                    ids.insert(w.id.as_str());
                }
            }
            if ids.len() == before {
                break;
            }
        }
        all.iter().filter(|w| ids.contains(w.id.as_str())).map(|w| (w.id.clone(), w.rect)).collect()
    }

    pub fn screen_rects(&self) -> Vec<Rect> {
        self.screens.borrow().clone()
    }
}
