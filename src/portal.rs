//! The desktop's screen-cast portal (org.freedesktop.portal.ScreenCast): asks once to share a screen, then gives
//! a PipeWire stream of it. The permission is remembered (a restore token in the settings), so later runs don't
//! ask.
use crate::settings;
use gio::prelude::*;
use glib::{Variant, VariantDict};
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::rc::Rc;

const BUS: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.ScreenCast";
const MONITOR: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;

/// A shared screen: node (PipeWire node id), position and size (logical px, on the desktop).
pub struct Stream {
    /// KWin's stream, when it came from KWin rather than the portal (it ends when this is dropped).
    pub kwin: Option<crate::kwin_cast::Stream>,
    pub session: String,
    pub node: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Stream {
    /// A new PipeWire connection (a file descriptor) that can see this stream. (pipewiresrc takes a copy of it, so
    /// it's the caller's to close: dropped.)
    pub fn fd(&self) -> Result<OwnedFd, String> {
        let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).map_err(|e| e.to_string())?;
        let path = glib::variant::ObjectPath::try_from(self.session.clone()).map_err(|e| e.to_string())?;
        let args = Variant::tuple_from_iter([path.to_variant(), VariantDict::new(None).end()]);
        let (reply, fds) = conn.call_with_unix_fd_list_sync(
            Some(BUS), PATH, IFACE, "OpenPipeWireRemote", Some(&args), Some(glib::VariantTy::new("(h)").unwrap()),
            gio::DBusCallFlags::NONE, -1, gio::UnixFDList::NONE, gio::Cancellable::NONE).map_err(|e| e.to_string())?;
        let child = reply.child_value(0);
        let index = child.get::<glib::variant::Handle>().map(|h| h.0)
            .ok_or_else(|| format!("unexpected reply {reply}"))?;
        let fd = fds.get(index).map_err(|e| format!("fd {index} of {}: {e}", fds.length()))?;
        // SAFETY: UnixFDList::get returns a new descriptor (a dup) that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub fn close(&self) {
        if self.kwin.is_some() {
            return;
        }
        if let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            let _ = conn.call_sync(Some(BUS), &self.session, "org.freedesktop.portal.Session", "Close", None, None,
                                   gio::DBusCallFlags::NONE, 1000, gio::Cancellable::NONE);
        }
    }
}

pub struct Portal {
    conn: gio::DBusConnection,
    sender: String,
}

type Done = Rc<dyn Fn(Result<HashMap<String, Variant>, String>)>;

impl Portal {
    pub fn new() -> Option<Rc<Portal>> {
        let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
        let sender = conn.unique_name()?.trim_start_matches(':').replace('.', "_");
        Some(Rc::new(Portal { conn, sender }))
    }

    pub fn available(&self) -> bool {
        self.conn.call_sync(Some(BUS), PATH, "org.freedesktop.DBus.Properties", "Get",
                            Some(&(IFACE, "version").to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                            gio::Cancellable::NONE)
            .ok().and_then(|v| v.child_value(0).as_variant()).and_then(|v| v.get::<u32>())
            .map(|v| v >= 4).unwrap_or(false)
    }

    fn token(prefix: &str) -> String {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos())
            .unwrap_or(0) ^ std::process::id();
        format!("{prefix}{n}")
    }

    /// Call a portal method that answers with a Response signal on a request object.
    fn request(&self, method: &str, mut args: Vec<Variant>, options: VariantDict, done: Done) {
        let token = Portal::token("eov");
        let handle = format!("{PATH}/request/{}/{token}", self.sender);
        options.insert_value("handle_token", &token.to_variant());
        args.push(options.end());
        let sub: Rc<RefCell<Option<gio::SignalSubscriptionId>>> = Rc::new(RefCell::new(None));
        let (sub2, conn2, done2) = (sub.clone(), self.conn.clone(), done.clone());
        let id = self.conn.signal_subscribe(Some(BUS), Some("org.freedesktop.portal.Request"), Some("Response"),
                                            Some(&handle), None, gio::DBusSignalFlags::NONE,
                                            move |_, _, _, _, _, params| {
            if let Some(id) = sub2.borrow_mut().take() {
                #[allow(deprecated)]
                conn2.signal_unsubscribe(id);
            }
            let code = params.child_value(0).get::<u32>().unwrap_or(2);
            let results: HashMap<String, Variant> = params.child_value(1).get().unwrap_or_default();
            done2(match code {
                0 => Ok(results),
                1 => Err("cancelled".into()),
                _ => Err("refused".into()),
            });
        });
        *sub.borrow_mut() = Some(id);
        let (sub3, conn3) = (sub.clone(), self.conn.clone());
        self.conn.call(Some(BUS), PATH, IFACE, method, Some(&Variant::tuple_from_iter(args)), None,
                       gio::DBusCallFlags::NONE, -1, gio::Cancellable::NONE, move |result| {
            if let Err(e) = result {
                if let Some(id) = sub3.borrow_mut().take() {
                    #[allow(deprecated)]
                    conn3.signal_unsubscribe(id);
                }
                done(Err(e.message().to_string()));
            }
        });
    }

    /// Share a screen: the desktop asks the first time (which screen), then remembers.
    pub fn open(self: &Rc<Self>, on_ready: impl Fn(Stream) + 'static, on_error: impl Fn(String) + 'static) {
        let on_ready: Rc<dyn Fn(Stream)> = Rc::new(on_ready);
        let on_error: Rc<dyn Fn(String)> = Rc::new(on_error);
        let me = self.clone();
        let opts = VariantDict::new(None);
        opts.insert_value("session_handle_token", &Portal::token("eovs").to_variant());
        let err = on_error.clone();
        self.request("CreateSession", vec![], opts, Rc::new(move |r| {
            let results = match r { Ok(r) => r, Err(e) => return err(e) };
            let Some(session) = results.get("session_handle").and_then(|v| v.get::<String>()) else {
                return err("no session".into());
            };
            let opts = VariantDict::new(None);
            opts.insert_value("types", &MONITOR.to_variant());
            opts.insert_value("multiple", &false.to_variant());
            opts.insert_value("cursor_mode", &CURSOR_EMBEDDED.to_variant());
            opts.insert_value("persist_mode", &2u32.to_variant());
            let token = settings::string("restore_token");
            if !token.is_empty() {
                opts.insert_value("restore_token", &token.to_variant());
            }
            let path = glib::variant::ObjectPath::try_from(session.clone()).unwrap();
            let (me2, err2, ready2) = (me.clone(), err.clone(), on_ready.clone());
            me.request("SelectSources", vec![path.to_variant()], opts, Rc::new(move |r| {
                if let Err(e) = r { return err2(e) }
                let path = glib::variant::ObjectPath::try_from(session.clone()).unwrap();
                let (err3, ready3, session3) = (err2.clone(), ready2.clone(), session.clone());
                me2.request("Start", vec![path.to_variant(), "".to_variant()], VariantDict::new(None),
                            Rc::new(move |r| {
                    let results = match r { Ok(r) => r, Err(e) => return err3(e) };
                    if let Some(t) = results.get("restore_token").and_then(|v| v.get::<String>()) {
                        settings::set("restore_token", t.into());
                    }
                    let streams = results.get("streams").map(|v| v.clone());
                    let first = streams.as_ref().filter(|s| s.n_children() > 0).map(|s| s.child_value(0));
                    let Some(first) = first else { return err3("no screen was shared".into()) };
                    let node = first.child_value(0).get::<u32>().unwrap_or(0);
                    let props: HashMap<String, Variant> = first.child_value(1).get().unwrap_or_default();
                    let pair = |k: &str| props.get(k).and_then(|v| v.get::<(i32, i32)>()).unwrap_or((0, 0));
                    let (x, y) = pair("position");
                    let (w, h) = pair("size");
                    crate::info!("shared screen: node {node}, {x},{y} {w}×{h}");
                    ready3(Stream { kwin: None, session: session3.clone(), node, x, y, w, h });
                }));
            }));
        }));
    }
}
