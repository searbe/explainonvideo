//! The tray icon: KDE's StatusNotifierItem, with its menu (com.canonical.dbusmenu), served over D-Bus with gio on
//! the main thread. Clicking it shows or hides the controls; its menu starts and stops recording, shows or hides
//! the controls, opens the editor or the settings, and quits. While recording, its icon is a record dot.
use crate::app::{app, State};
use glib::{ToVariant, Variant};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";

const XML: &str = r#"<node>
  <interface name="org.kde.StatusNotifierItem">
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="Menu" type="o" access="read"/>
    <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
    <method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
    <signal name="NewIcon"/>
    <signal name="NewTitle"/>
    <signal name="NewToolTip"/>
    <signal name="NewStatus"><arg type="s"/></signal>
  </interface>
  <interface name="com.canonical.dbusmenu">
    <property name="Version" type="u" access="read"/>
    <property name="TextDirection" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconThemePath" type="as" access="read"/>
    <method name="GetLayout">
      <arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/>
      <arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/>
    </method>
    <method name="GetGroupProperties">
      <arg type="ai" direction="in"/><arg type="as" direction="in"/><arg type="a(ia{sv})" direction="out"/>
    </method>
    <method name="GetProperty">
      <arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/>
    </method>
    <method name="Event">
      <arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/>
      <arg type="u" direction="in"/>
    </method>
    <method name="EventGroup"><arg type="a(isvu)" direction="in"/><arg type="ai" direction="out"/></method>
    <method name="AboutToShow"><arg type="i" direction="in"/><arg type="b" direction="out"/></method>
    <method name="AboutToShowGroup">
      <arg type="ai" direction="in"/><arg type="ai" direction="out"/><arg type="ai" direction="out"/>
    </method>
    <signal name="ItemsPropertiesUpdated"><arg type="a(ia{sv})"/><arg type="a(ias)"/></signal>
    <signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
    <signal name="ItemActivationRequested"><arg type="i"/><arg type="u"/></signal>
  </interface>
</node>"#;

static REVISION: AtomicU32 = AtomicU32::new(1);

thread_local! {
    static CONN: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
}

/// Put the icon in the tray (now, and again whenever the tray comes back: Plasma restarting).
pub fn start() {
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, |res| {
        let conn = match res {
            Ok(c) => c,
            Err(e) => return crate::warn!("tray: no session bus: {e}"),
        };
        if let Err(e) = serve(&conn) {
            return crate::warn!("tray: {e}");
        }
        let _ = gio::bus_watch_name_on_connection(&conn, "org.kde.StatusNotifierWatcher",
                                                  gio::BusNameWatcherFlags::NONE, |c, _, _| register(&c), |_, _| {});
        CONN.with(|c| *c.borrow_mut() = Some(conn));
    });
}

/// The app's state changed: show it in the menu and the icon.
pub fn refresh() {
    let rev = REVISION.fetch_add(1, Ordering::SeqCst) + 1;
    CONN.with(|c| {
        if let Some(conn) = c.borrow().as_ref() {
            let _ = conn.emit_signal(None, MENU_PATH, MENU, "LayoutUpdated", Some(&(rev, 0i32).to_variant()));
            let _ = conn.emit_signal(None, ITEM_PATH, ITEM, "NewIcon", None);
            let _ = conn.emit_signal(None, ITEM_PATH, ITEM, "NewToolTip", None);
        }
    });
}

fn serve(conn: &gio::DBusConnection) -> Result<(), glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(XML)?;
    let (item, menu) = (node.lookup_interface(ITEM).unwrap(), node.lookup_interface(MENU).unwrap());
    conn.register_object(ITEM_PATH, &item, item_call, item_property, |_, _, _, _, _, _| false)?;
    conn.register_object(MENU_PATH, &menu, menu_call, menu_property, |_, _, _, _, _, _| false)?;
    Ok(())
}

fn register(conn: &gio::DBusConnection) {
    let Some(name) = conn.unique_name() else { return };
    conn.call(Some("org.kde.StatusNotifierWatcher"), "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher",
              "RegisterStatusNotifierItem", Some(&(name.as_str(),).to_variant()), None, gio::DBusCallFlags::NONE,
              -1, gio::Cancellable::NONE, |r| match r {
                  Ok(_) => crate::info!("tray: icon shown"),
                  Err(e) => crate::warn!("tray: the tray wouldn't take the icon: {e}"),
              });
}

fn recording() -> bool {
    matches!(app().state(), State::Recording | State::Paused)
}

// --- the icon ---
fn item_call(_: gio::DBusConnection, _: &str, _: &str, _: &str, method: &str, _: Variant,
             inv: gio::DBusMethodInvocation) {
    inv.return_value(None);
    if method == "Activate" {
        app().toggle_controls();
    }
}

fn item_property(_: gio::DBusConnection, _: &str, _: &str, _: &str, name: &str) -> Variant {
    let title = if recording() { "Explain on video: recording" } else { "Explain on video" };
    match name {
        "Category" => "ApplicationStatus".to_variant(),
        "Id" => "explainonvideo".to_variant(),
        "Title" => title.to_variant(),
        "Status" => "Active".to_variant(),
        "IconName" => (if recording() { "media-record" } else { "explainonvideo" }).to_variant(),
        "IconThemePath" => "".to_variant(),
        "ToolTip" => ("explainonvideo", Vec::<(i32, i32, Vec<u8>)>::new(), title, "").to_variant(),
        "ItemIsMenu" => false.to_variant(),
        _ => glib::variant::ObjectPath::try_from(MENU_PATH).unwrap().to_variant(), // ("Menu")
    }
}

// --- the menu ---
/// (id, label, enabled), or a separator (no label).
fn items() -> Vec<(i32, Option<&'static str>, bool)> {
    let a = app();
    let (state, rec) = (a.state(), recording());
    vec![
        (1, Some(if rec { "Stop recording" } else { "Start recording" }), state == State::Ready || rec),
        (2, Some(if a.controls_shown() { "Hide the controls" } else { "Show the controls" }), true),
        (3, None, true),
        (4, Some("Edit a recording…"), true),
        (5, Some("Settings…"), true),
        (6, None, true),
        (7, Some("Quit"), state == State::Ready),
    ]
}

fn props(label: Option<&str>, enabled: bool) -> HashMap<String, Variant> {
    let mut p = HashMap::new();
    match label {
        Some(l) => {
            p.insert("label".to_string(), l.to_variant());
            p.insert("enabled".to_string(), enabled.to_variant());
        }
        None => {
            p.insert("type".to_string(), "separator".to_variant());
        }
    }
    p
}

fn clicked(id: i32) {
    let a = app();
    match id {
        1 if recording() => a.stop(),
        1 => a.start(),
        2 => a.toggle_controls(),
        4 => a.open_editor(None),
        5 => a.open_settings(),
        7 => a.quit(),
        _ => {}
    }
}

fn menu_call(_: gio::DBusConnection, _: &str, _: &str, _: &str, method: &str, params: Variant,
             inv: gio::DBusMethodInvocation) {
    match method {
        "GetLayout" => {
            let (parent, _, _) = params.get::<(i32, i32, Vec<String>)>().unwrap_or((0, -1, vec![]));
            let children: Vec<Variant> = if parent == 0 {
                items().into_iter().map(|(id, l, e)| (id, props(l, e), Vec::<Variant>::new()).to_variant()).collect()
            } else {
                vec![]
            };
            let mut root = HashMap::new();
            root.insert("children-display".to_string(), "submenu".to_variant());
            let layout = (parent, root, children);
            inv.return_value(Some(&(REVISION.load(Ordering::SeqCst), layout).to_variant()));
        }
        "GetGroupProperties" => {
            let (ids, _) = params.get::<(Vec<i32>, Vec<String>)>().unwrap_or_default();
            let found: Vec<(i32, HashMap<String, Variant>)> = items().into_iter()
                .filter(|(id, _, _)| ids.is_empty() || ids.contains(id))
                .map(|(id, l, e)| (id, props(l, e))).collect();
            inv.return_value(Some(&(found,).to_variant()));
        }
        "GetProperty" => {
            let (id, name) = params.get::<(i32, String)>().unwrap_or_default();
            match items().into_iter().find(|(i, _, _)| *i == id).and_then(|(_, l, e)| props(l, e).remove(&name)) {
                Some(v) => inv.return_value(Some(&(v,).to_variant())),
                None => inv.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "no such property"),
            }
        }
        "Event" => {
            let (id, event, _, _) = params.get::<(i32, String, Variant, u32)>().unwrap_or((0, String::new(),
                                                                                        ().to_variant(), 0));
            inv.return_value(None);
            if event == "clicked" {
                glib::idle_add_local_once(move || clicked(id));
            }
        }
        "EventGroup" => {
            let (events,) = params.get::<(Vec<(i32, String, Variant, u32)>,)>().unwrap_or_default();
            inv.return_value(Some(&(Vec::<i32>::new(),).to_variant()));
            for (id, event, _, _) in events {
                if event == "clicked" {
                    glib::idle_add_local_once(move || clicked(id));
                }
            }
        }
        "AboutToShow" => inv.return_value(Some(&(false,).to_variant())),
        "AboutToShowGroup" => inv.return_value(Some(&(Vec::<i32>::new(), Vec::<i32>::new()).to_variant())),
        _ => inv.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
    }
}

fn menu_property(_: gio::DBusConnection, _: &str, _: &str, _: &str, name: &str) -> Variant {
    match name {
        "Version" => 3u32.to_variant(),
        "TextDirection" => "ltr".to_variant(),
        "Status" => "normal".to_variant(),
        _ => Vec::<String>::new().to_variant(), // ("IconThemePath")
    }
}
