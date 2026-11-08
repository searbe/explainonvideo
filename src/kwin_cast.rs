//! KWin's own screen-cast protocol (zkde_screencast_unstable_v1): a PipeWire stream of one window by itself,
//! drawn by the compositor, whatever covers it; and of a whole screen, without the portal's dialog. It's a private protocol: KWin offers it only to programs whose
//! .desktop file lists it (X-KDE-Wayland-Interfaces), which setup.sh writes. Without it, windows are recorded by
//! covering the rest of the screen instead.
//!
//! The app's windows are X11 (XWayland), so this is a separate Wayland connection, read on its own thread; the
//! streams' events come back to the main loop.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

#[allow(non_upper_case_globals, non_camel_case_types, dead_code, clippy::all)]
mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("src/protocols/screencast.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("src/protocols/screencast.xml");
}
use protocol::zkde_screencast_stream_unstable_v1::{self as stream_v1, ZkdeScreencastStreamUnstableV1};
use protocol::zkde_screencast_unstable_v1::ZkdeScreencastUnstableV1;

const POINTER_EMBEDDED: u32 = 2;

#[derive(Debug, Clone)]
pub enum Event {
    /// The stream's PipeWire node.
    Created(u32),
    Failed(String),
    Closed,
}

struct State {
    tx: glib::Sender<(u64, Event)>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents,
             _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZkdeScreencastUnstableV1, ()> for State {
    fn event(_: &mut Self, _: &ZkdeScreencastUnstableV1, _: protocol::zkde_screencast_unstable_v1::Event, _: &(),
             _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZkdeScreencastStreamUnstableV1, u64> for State {
    fn event(state: &mut Self, _: &ZkdeScreencastStreamUnstableV1, event: stream_v1::Event, id: &u64,
             _: &Connection, _: &QueueHandle<Self>) {
        let ev = match event {
            stream_v1::Event::Created { node } => Event::Created(node),
            stream_v1::Event::Failed { error } => Event::Failed(error),
            stream_v1::Event::Closed => Event::Closed,
            _ => return,
        };
        let _ = state.tx.send((*id, ev));
    }
}

type Listener = std::rc::Rc<dyn Fn(Event)>;

pub struct Cast {
    conn: Connection,
    qh: QueueHandle<State>,
    cast: ZkdeScreencastUnstableV1,
    listeners: RefCell<HashMap<u64, Listener>>,
}

/// One window's stream; closed when dropped.
pub struct Stream {
    proxy: ZkdeScreencastStreamUnstableV1,
    id: u64,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.proxy.close();
        // (try_with: at exit the connection may already be gone)
        if let Some(c) = CAST.try_with(|c| c.try_borrow().ok().and_then(|c| c.clone())).ok().flatten() {
            c.listeners.borrow_mut().remove(&self.id);
            let _ = c.conn.flush();
        }
    }
}

thread_local! {
    static CAST: RefCell<Option<std::rc::Rc<Cast>>> = const { RefCell::new(None) };
    static TRIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

static NEXT: AtomicU64 = AtomicU64::new(1);


impl Cast {
    /// The connection (made once), or None if KWin doesn't offer the protocol to this program.
    pub fn get() -> Option<std::rc::Rc<Cast>> {
        if !TRIED.with(|t| t.replace(true)) {
            match Cast::connect() {
                Ok(c) => CAST.with(|s| *s.borrow_mut() = Some(c)),
                Err(e) => crate::info!("window streams aren't available ({e}): windows are recorded by covering \
                                        the rest"),
            }
        }
        CAST.with(|c| c.borrow().clone())
    }

    fn connect() -> Result<std::rc::Rc<Cast>, String> {
        if std::env::var_os("EOV_NO_WINDOW_STREAMS").is_some() {
            return Err("turned off".into());
        }
        let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let cast: ZkdeScreencastUnstableV1 = globals.bind(&qh, 1..=5, ())
            .map_err(|_| "KWin doesn't offer zkde_screencast_unstable_v1 to this program".to_string())?;
        let (tx, rx) = crate::util::channel::<(u64, Event)>();
        let mut state = State { tx };
        std::thread::spawn(move || {
            while queue.blocking_dispatch(&mut state).is_ok() {}
        });
        let c = std::rc::Rc::new(Cast { conn, qh, cast, listeners: RefCell::new(HashMap::new()) });
        let weak = std::rc::Rc::downgrade(&c);
        rx.attach(None, move |(id, ev)| {
            if let Some(c) = weak.upgrade() {
                // (not called while borrowed: a listener may ask for another stream, or close its own)
                let f = c.listeners.borrow().get(&id).cloned();
                if let Some(f) = f {
                    f(ev);
                }
            }
            glib::ControlFlow::Continue
        });
        crate::info!("window streams: available");
        Ok(c)
    }

    /// Whether whole screens can be streamed (version 5: KWin chooses the resolution).
    pub fn can_stream_screens(&self) -> bool {
        use wayland_client::Proxy;
        self.cast.version() >= 5
    }

    /// A stream of rect on the desktop (a whole screen, say) at the screen's own resolution; on_event(Created(node))
    /// when it's ready.
    pub fn stream_region(&self, rect: crate::layout::Rect, on_event: impl Fn(Event) + 'static) -> Stream {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        self.listeners.borrow_mut().insert(id, std::rc::Rc::new(on_event));
        let (x, y, w, h) = rect;
        // (scale 0: KWin picks the highest of the screens' scales, from version 5)
        let proxy = self.cast.stream_region(x, y, w as u32, h as u32, 0.0, POINTER_EMBEDDED, &self.qh, id);
        let _ = self.conn.flush();
        Stream { proxy, id }
    }

    /// A stream of the window with KWin's id uuid; on_event(Created(node)) when it's ready.
    pub fn stream_window(&self, uuid: &str, on_event: impl Fn(Event) + 'static) -> Stream {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        self.listeners.borrow_mut().insert(id, std::rc::Rc::new(on_event));
        let proxy = self.cast.stream_window(uuid.to_string(), POINTER_EMBEDDED, &self.qh, id);
        let _ = self.conn.flush();
        Stream { proxy, id }
    }
}
