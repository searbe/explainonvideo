//! The design system in code (Dictate's, shared so the two apps look like one family): the tokens, the CSS made
//! from them (dark or light, following the desktop's preference, live), a small icon set drawn in cairo, and the
//! components every surface is built from. Use these names, never raw colours or sizes.
use gio::prelude::*;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::f64::consts::PI;

const DARK: &[(&str, &str)] = &[
    ("bg", "#111214"), ("surface-1", "#17181b"), ("surface-2", "#1d1f23"), ("surface-3", "#25272c"),
    ("surface-4", "#2e3137"), ("raised", "#3a3d45"),
    ("line", "rgba(255,255,255,0.07)"), ("line-strong", "rgba(255,255,255,0.14)"),
    ("text", "#ececef"), ("text-2", "#a4a7b0"), ("text-3", "#6f727b"),
    ("accent", "#7c8cff"), ("accent-hover", "#96a3ff"), ("accent-weak", "rgba(124,140,255,0.16)"),
    ("on-accent", "#ffffff"), ("record", "#ff5a5f"), ("danger", "#ff7a7a"), ("danger-weak", "rgba(255,122,122,0.14)"),
    ("warning", "#ffb547"), ("warning-weak", "rgba(255,181,71,0.13)"),
    ("hud", "rgba(23,24,27,0.88)"), ("highlight", "rgba(255,255,255,0.06)"), ("shadow", "rgba(0,0,0,0.35)"),
];
const LIGHT: &[(&str, &str)] = &[
    ("bg", "#f6f6f7"), ("surface-1", "#efeff1"), ("surface-2", "#ffffff"), ("surface-3", "#e9e9ec"),
    ("surface-4", "#dfdfe4"), ("raised", "#ffffff"),
    ("line", "rgba(0,0,0,0.08)"), ("line-strong", "rgba(0,0,0,0.16)"),
    ("text", "#1b1c1f"), ("text-2", "#5d6068"), ("text-3", "#8b8e96"),
    ("accent", "#5b6cff"), ("accent-hover", "#4a5bf0"), ("accent-weak", "rgba(91,108,255,0.14)"),
    ("on-accent", "#ffffff"), ("record", "#e5484d"), ("danger", "#d93d42"), ("danger-weak", "rgba(217,61,66,0.10)"),
    ("warning", "#b86e00"), ("warning-weak", "rgba(184,110,0,0.14)"),
    ("hud", "rgba(250,250,251,0.90)"), ("highlight", "rgba(255,255,255,0.7)"), ("shadow", "rgba(0,0,0,0.18)"),
];
const FONT: &str = r#""Inter", "Noto Sans", "Cantarell", sans-serif"#;
const TYPE: &[(&str, i32, i32)] = &[("headline", 20, 600), ("title", 15, 600), ("body", 13, 400),
                                     ("body-strong", 13, 600), ("small", 12, 400), ("caption", 11, 400)];
pub const R_SM: f64 = 6.0;
pub const R_MD: f64 = 8.0;
pub const R_LG: f64 = 12.0;
pub const MORPH: f64 = 0.28; // seconds

thread_local! {
    static DARK_SCHEME: Cell<bool> = const { Cell::new(true) };
    static PROVIDER: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

fn scheme() -> &'static [(&'static str, &'static str)] {
    if DARK_SCHEME.with(|d| d.get()) { DARK } else { LIGHT }
}

pub fn tok(name: &str) -> &'static str {
    scheme().iter().find(|(n, _)| *n == name).map(|(_, v)| *v).unwrap_or("#ff00ff")
}

/// A token's colour (or any CSS colour) as RGBA.
pub fn rgba(colour: &str) -> gdk::RGBA {
    let css = if scheme().iter().any(|(n, _)| *n == colour) { tok(colour) } else { colour };
    gdk::RGBA::parse(css).unwrap_or(gdk::RGBA::new(1.0, 0.0, 1.0, 1.0))
}

pub fn set_source(cr: &cairo::Context, colour: &str, alpha: Option<f64>) {
    let c = rgba(colour);
    cr.set_source_rgba(c.red(), c.green(), c.blue(), alpha.unwrap_or(c.alpha()));
}

/// cubic-bezier(0.2, 0, 0, 1) for t in 0..1 (the morph curve), solved numerically.
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..20 {
        let u = (lo + hi) / 2.0;
        let x = 3.0 * (1.0 - u).powi(2) * u * 0.2 + u.powi(3);
        if x < t { lo = u } else { hi = u }
    }
    let u = (lo + hi) / 2.0;
    3.0 * (1.0 - u) * u * u + u.powi(3)
}

pub fn animations() -> bool {
    gtk::Settings::default().map(|s| s.is_gtk_enable_animations()).unwrap_or(true)
}

/// The stylesheet (style.css) for the scheme in use, its ${tokens} filled in.
pub fn css() -> String {
    let mut out = include_str!("style.css").to_string();
    let mut values: Vec<(String, String)> = scheme().iter().map(|(n, v)| (n.replace('-', "_"), v.to_string()))
        .collect();
    for (k, px, weight) in TYPE {
        values.push((format!("size_{}", k.replace('-', "_")), format!("font-size: {px}px; font-weight: {weight};")));
    }
    for (k, v) in [("font", FONT.to_string()), ("r_xs", "4".into()), ("r_sm", "6".into()), ("r_md", "8".into()),
                   ("r_lg", "12".into()), ("r_full", "999".into())] {
        values.push((k.to_string(), v));
    }
    values.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    for (k, v) in &values {
        out = out.replace(&format!("${{{k}}}"), v);
    }
    for (k, v) in &values {
        out = out.replace(&format!("${k}"), v);
    }
    out.replace("$$", "$")
}

/// Apply the design to the app's windows (and give them its icon); follow the desktop's light/dark preference.
pub fn install() {
    if let Some(dark) = desktop_scheme() {
        DARK_SCHEME.with(|d| d.set(dark));
    }
    apply();
    gtk::Window::set_default_icon_list(&[16, 24, 32, 48, 64, 128, 256].map(app_icon));
    if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        let id = bus.signal_subscribe(Some("org.freedesktop.portal.Desktop"), Some("org.freedesktop.portal.Settings"),
                                      Some("SettingChanged"), Some("/org/freedesktop/portal/desktop"), None,
                                      gio::DBusSignalFlags::NONE, |_, _, _, _, _, params| {
            let ns = params.child_value(0).get::<String>().unwrap_or_default();
            let key = params.child_value(1).get::<String>().unwrap_or_default();
            if ns == "org.freedesktop.appearance" && key == "color-scheme" {
                let v = unwrap_variant(params.child_value(2)).and_then(|v| v.get::<u32>()).unwrap_or(1);
                let dark = v != 2;
                if dark != DARK_SCHEME.with(|d| d.get()) {
                    DARK_SCHEME.with(|d| d.set(dark));
                    apply();
                }
            }
        });
        let _ = id; // (subscribed for the app's life)
    }
}

fn unwrap_variant(mut v: glib::Variant) -> Option<glib::Variant> {
    while v.is_type(glib::VariantTy::VARIANT) {
        v = v.as_variant()?;
    }
    Some(v)
}

fn apply() {
    let Some(screen) = gdk::Screen::default() else { return };
    PROVIDER.with(|p| {
        if let Some(old) = p.borrow_mut().take() {
            gtk::StyleContext::remove_provider_for_screen(&screen, &old);
        }
        let provider = gtk::CssProvider::new();
        if let Err(e) = provider.load_from_data(css().as_bytes()) {
            crate::warn!("style: {e}");
        }
        gtk::StyleContext::add_provider_for_screen(&screen, &provider, 600);
        *p.borrow_mut() = Some(provider);
    });
    if let Some(s) = gtk::Settings::default() {
        s.set_gtk_application_prefer_dark_theme(DARK_SCHEME.with(|d| d.get()));
    }
}

fn desktop_scheme() -> Option<bool> {
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let reply = bus.call_sync(Some("org.freedesktop.portal.Desktop"), "/org/freedesktop/portal/desktop",
                              "org.freedesktop.portal.Settings", "Read",
                              Some(&("org.freedesktop.appearance", "color-scheme").to_variant()), None,
                              gio::DBusCallFlags::NONE, 500, gio::Cancellable::NONE).ok()?;
    match unwrap_variant(reply.child_value(0))?.get::<u32>()? {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

// --- icons ---
/// Each icon on a 16×16 grid, as path commands: m x y (move), l x y (line), c x1 y1 x2 y2 x y (curve),
/// a cx cy r start end (arc; angles in turns), r x y w h radius (rounded rectangle), n (new sub-path), z (close).
fn icon_path(name: &str) -> String {
    let p = match name {
        "mic" => "n a 8 4.5 2.5 .5 0 l 10.5 7.5 a 8 7.5 2.5 0 .5 z n a 8 7.5 4.5 0 .5 m 8 12 l 8 14.5",
        "settings" => return format!("n a 8 8 2.25 0 1 {} n a 8 8 4.2 0 1", spokes(4.2, 6.0)),
        "copy" => "r 5.5 5.5 8 8 1.75 m 3.5 10.5 l 3.5 4.25 c 3.5 3.5 3.5 2.5 4.5 2.5 l 10.5 2.5",
        "play" => "m 5 3 l 13 8 l 5 13 z",
        "stop" => "r 4 4 8 8 1.5",
        "pause" => "m 5.5 3.5 l 5.5 12.5 m 10.5 3.5 l 10.5 12.5",
        "trash" => "m 2.5 4.5 l 13.5 4.5 m 6 4.5 l 6 2.5 l 10 2.5 l 10 4.5 m 4 4.5 l 4.75 13.5 l 11.25 13.5 l 12 4.5",
        "undo" => "m 5.5 3 l 2.5 6 l 5.5 9 m 2.5 6 l 9.5 6 c 12 6 13.5 7.5 13.5 9.75 c 13.5 12 12 13.5 9.5 13.5 l 6.5 13.5",
        "folder" => "m 2 4 l 6 4 l 7.5 5.5 l 14 5.5 l 14 12.5 l 2 12.5 z",
        "open" => "m 9 2.5 l 13.5 2.5 l 13.5 7 m 13.5 2.5 l 7.5 8.5 m 11.5 9.5 l 11.5 13.5 l 2.5 13.5 l 2.5 4.5 l 6.5 4.5",
        "plus" => "m 8 3 l 8 13 m 3 8 l 13 8",
        "check" => "m 3 8.5 l 6.5 12 l 13 4.5",
        "warning" => "m 8 2.5 l 14 13 l 2 13 z m 8 6.5 l 8 9.25 m 8 11.25 l 8 11.3",
        "close" => "m 4 4 l 12 12 m 12 4 l 4 12",
        "record" => "n a 8 8 4 0 1",
        "up" => "m 3.5 10.5 l 8 6 l 12.5 10.5",
        "down" => "m 3.5 5.5 l 8 10 l 12.5 5.5",
        "camera" => "r 1.5 4 9 8 1.75 m 10.5 7 l 14.5 4.75 l 14.5 11.25 l 10.5 9",
        "speaker" => "m 2 6 l 4.75 6 l 8.25 3 l 8.25 13 l 4.75 10 l 2 10 z m 10.75 5.75 c 12 7 12 9 10.75 10.25 \
                      m 12.75 3.75 c 15 6 15 10 12.75 12.25",
        "screen" => "r 1.5 2.5 13 9 1.5 m 5.5 14 l 10.5 14 m 8 11.5 l 8 14",
        "window" => "r 1.5 2.5 13 11 1.5 m 1.5 5.75 l 14.5 5.75",
        "area" => "m 2 5.5 l 2 2 l 5.5 2 m 10.5 2 l 14 2 l 14 5.5 m 14 10.5 l 14 14 l 10.5 14 m 5.5 14 l 2 14 l 2 10.5",
        "preview" => "r 1.5 2.5 13 11 1.5 r 8 8 4.5 3.5 .75",
        "scissors" => "n a 4.25 11.75 2.25 0 1 n a 4.25 4.25 2.25 0 1 m 6.1 5.6 l 14 12 m 6.1 10.4 l 14 4",
        "crop" => "m 4.5 1.5 l 4.5 11.5 l 14.5 11.5 m 1.5 4.5 l 11.5 4.5 l 11.5 14.5",
        "export" => "m 8 10 l 8 2.5 m 5 5.5 l 8 2.5 l 11 5.5 m 3 8.5 l 3 13.5 l 13 13.5 l 13 8.5",
        "redo" => "m 10.5 3 l 13.5 6 l 10.5 9 m 13.5 6 l 6.5 6 c 4 6 2.5 7.5 2.5 9.75 c 2.5 12 4 13.5 6.5 13.5 l 9.5 13.5",
        "more" => "n a 3.5 8 .9 0 1 n a 8 8 .9 0 1 n a 12.5 8 .9 0 1",
        "link" => "m 7.25 5 l 9 3.25 c 10.25 2 12.25 2 13.5 3.25 c 14.75 4.5 14.75 6.5 13.5 7.75 l 11.75 9.5 \
                   m 8.75 11 l 7 12.75 c 5.75 14 3.75 14 2.5 12.75 c 1.25 11.5 1.25 9.5 2.5 8.25 l 4.25 6.5 m 6 10 l 10 6",
        "grip" => "n a 6 4 .55 0 1 n a 10 4 .55 0 1 n a 6 8 .55 0 1 n a 10 8 .55 0 1 n a 6 12 .55 0 1 n a 10 12 .55 0 1",
        "film" => "r 2 2.5 12 11 1.5 m 5 2.5 l 5 13.5 m 11 2.5 l 11 13.5 m 2 8 l 5 8 m 11 8 l 14 8",
        "live" => "n a 8 8 1.5 0 1 m 5.2 5.2 c 3.7 6.7 3.7 9.3 5.2 10.8 m 10.8 5.2 c 12.3 6.7 12.3 9.3 10.8 10.8 \
                   m 3.1 3.1 c .4 5.8 .4 10.2 3.1 12.9 m 12.9 3.1 c 15.6 5.8 15.6 10.2 12.9 12.9",
        "left" => "m 10 3.5 l 5.5 8 l 10 12.5",
        "right" => "m 6 3.5 l 10.5 8 l 6 12.5",
        "split" => "m 8 1.5 l 8 14.5 m 5 4.5 l 2.5 4.5 l 2.5 11.5 l 5 11.5 m 11 4.5 l 13.5 4.5 l 13.5 11.5 l 11 11.5",
        _ => "",
    };
    p.to_string()
}

fn spokes(inner: f64, outer: f64) -> String {
    (0..8).map(|k| {
        let a = k as f64 * PI / 4.0;
        format!("m {} {} l {} {}", 8.0 + inner * a.cos(), 8.0 + inner * a.sin(), 8.0 + outer * a.cos(),
                8.0 + outer * a.sin())
    }).collect::<Vec<_>>().join(" ")
}

fn draw_path(cr: &cairo::Context, path: &str) {
    let words: Vec<&str> = path.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        let op = words[i];
        let n = match op { "m" | "l" => 2, "c" => 6, "a" | "r" => 5, _ => 0 };
        let a: Vec<f64> = words[i + 1..(i + 1 + n).min(words.len())].iter().map(|w| w.parse().unwrap_or(0.0))
            .collect();
        i += 1 + n;
        if a.len() < n {
            break;
        }
        match op {
            "m" => cr.move_to(a[0], a[1]),
            "l" => cr.line_to(a[0], a[1]),
            "c" => cr.curve_to(a[0], a[1], a[2], a[3], a[4], a[5]),
            "a" => cr.arc(a[0], a[1], a[2], a[3] * 2.0 * PI, a[4] * 2.0 * PI),
            "r" => round_rect(cr, a[0], a[1], a[2], a[3], a[4]),
            "n" => cr.new_sub_path(),
            _ => cr.close_path(),
        }
    }
}

pub fn round_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// Draw icon name at (x, y), size px, in colour. A name ending -off is struck through (turned off).
pub fn draw_icon(cr: &cairo::Context, name: &str, x: f64, y: f64, size: f64, colour: &gdk::RGBA) {
    let base = name.strip_suffix("-off").unwrap_or(name);
    let _ = cr.save();
    cr.translate(x, y);
    cr.scale(size / 16.0, size / 16.0);
    cr.set_source_rgba(colour.red(), colour.green(), colour.blue(), colour.alpha());
    cr.set_line_width(1.5);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.new_path();
    draw_path(cr, &icon_path(base));
    if ["play", "stop", "record", "more", "grip"].contains(&name) {
        let _ = cr.fill_preserve();
    }
    let _ = cr.stroke();
    if name.ends_with("-off") {
        cr.move_to(2.0, 2.0);
        cr.line_to(14.0, 14.0);
        let _ = cr.stroke();
    }
    let _ = cr.restore();
}

/// The app's icon at px by px: a charcoal rounded square lit along its top edge (Dictate's family), with a white
/// screen and, in its corner, a red recording dot.
pub fn draw_app_icon(cr: &cairo::Context, px: f64) {
    let pad = px * 0.06;
    let side = px - 2.0 * pad;
    let r = side * 0.24;
    let fill = cairo::LinearGradient::new(0.0, pad, 0.0, px - pad);
    fill.add_color_stop_rgb(0.0, 0x33 as f64 / 255.0, 0x36 as f64 / 255.0, 0x3d as f64 / 255.0);
    fill.add_color_stop_rgb(1.0, 0x16 as f64 / 255.0, 0x17 as f64 / 255.0, 0x1a as f64 / 255.0);
    round_rect(cr, pad, pad, side, side, r);
    let _ = cr.set_source(&fill);
    let _ = cr.fill();
    round_rect(cr, pad + 0.5, pad + 0.5, side - 1.0, side - 1.0, r - 0.5);
    let edge = cairo::LinearGradient::new(0.0, pad, 0.0, px - pad);
    edge.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, 0.30);
    edge.add_color_stop_rgba(0.5, 1.0, 1.0, 1.0, 0.06);
    edge.add_color_stop_rgba(1.0, 1.0, 1.0, 1.0, 0.10);
    let _ = cr.set_source(&edge);
    cr.set_line_width(1.0);
    let _ = cr.stroke();
    let unit = side * 0.64 / 16.0;
    let _ = cr.save();
    cr.translate((px - 16.0 * unit) / 2.0, (px - 16.0 * unit) / 2.0 + side * 0.02);
    cr.scale(unit, unit);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(if px <= 24.0 { 2.0 } else { 1.5 });
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    round_rect(cr, 1.0, 2.5, 14.0, 9.5, 1.75);
    let _ = cr.stroke();
    cr.move_to(5.5, 14.5);
    cr.line_to(10.5, 14.5);
    let _ = cr.stroke();
    cr.move_to(6.5, 5.0);
    cr.line_to(10.5, 7.25);
    cr.line_to(6.5, 9.5);
    cr.close_path();
    let _ = cr.fill();
    cr.arc(14.2, 2.4, if px <= 24.0 { 2.4 } else { 2.0 }, 0.0, 2.0 * PI);
    cr.set_source_rgb(1.0, 0x5a as f64 / 255.0, 0x5f as f64 / 255.0);
    let _ = cr.fill();
    let _ = cr.restore();
}

fn icon_surface(px: i32) -> cairo::ImageSurface {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, px, px).unwrap();
    draw_app_icon(&cairo::Context::new(&surface).unwrap(), px as f64);
    surface
}

/// Write the app icon to a PNG (for launchers).
pub fn save_app_icon(path: &std::path::Path, px: i32) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    icon_surface(px).write_to_png(&mut f).map_err(|e| std::io::Error::other(e.to_string()))
}

pub fn app_icon(px: i32) -> gdk_pixbuf::Pixbuf {
    gdk::pixbuf_get_from_surface(&icon_surface(px), 0, 0, px, px).unwrap()
}

/// An icon from the set, in its widget's CSS colour (so it follows hover and state).
#[derive(Clone)]
pub struct Icon {
    pub widget: gtk::DrawingArea,
    name: std::rc::Rc<RefCell<String>>,
}

impl Icon {
    pub fn new(name: &str, size: i32) -> Icon {
        let widget = gtk::DrawingArea::builder().width_request(size).height_request(size)
            .valign(gtk::Align::Center).halign(gtk::Align::Center).build();
        let name = std::rc::Rc::new(RefCell::new(name.to_string()));
        let n = name.clone();
        widget.connect_draw(move |w, cr| {
            let ctx = w.style_context();
            draw_icon(cr, &n.borrow(), 0.0, 0.0, size as f64, &ctx.color(ctx.state()));
            glib::Propagation::Proceed
        });
        Icon { widget, name }
    }

    pub fn set_icon(&self, name: &str) {
        *self.name.borrow_mut() = name.to_string();
        self.widget.queue_draw();
    }
}

// --- components ---
pub fn classes<W: IsA<gtk::Widget>>(w: W, names: &[&str]) -> W {
    let ctx = w.style_context();
    for c in names.iter().filter(|c| !c.is_empty()) {
        ctx.add_class(c);
    }
    w
}

pub fn style(w: &impl IsA<gtk::Widget>, name: &str, on: bool) {
    let ctx = w.style_context();
    if on { ctx.add_class(name) } else { ctx.remove_class(name) }
}

/// A label in a type style: headline, title, body, small or caption.
pub fn text(label: &str, kind: &str) -> gtk::Label {
    classes(gtk::Label::builder().label(label).xalign(0.0).build(), &[kind])
}

pub fn wrapped(label: &str, kind: &str, max_chars: i32) -> gtk::Label {
    let l = text(label, kind);
    l.set_line_wrap(true);
    l.set_line_wrap_mode(pango::WrapMode::WordChar);
    l.set_max_width_chars(max_chars);
    l
}

/// A button: kind primary, secondary, quiet, stop or danger; an icon before the label.
pub struct Button {
    pub button: gtk::Button,
    pub label: gtk::Label,
}

pub fn button(label: &str, kind: &str, icon: Option<&str>, tooltip: Option<&str>, cb: impl Fn() + 'static)
              -> Button {
    let b = classes(gtk::Button::new(), &["btn", if kind == "secondary" { "" } else { kind }]);
    b.set_tooltip_text(tooltip);
    let bx = gtk::Box::builder().spacing(6).halign(gtk::Align::Center).build();
    if let Some(icon) = icon {
        bx.pack_start(&Icon::new(icon, 16).widget, false, false, 0);
    }
    let text = gtk::Label::new(Some(label));
    bx.pack_start(&text, false, false, 0);
    b.add(&bx);
    b.connect_clicked(move |_| cb());
    Button { button: b, label: text }
}

/// A 28×28 quiet button with an icon; tooltip names the action.
pub fn icon_button(icon: &str, tooltip: &str, danger: bool, cb: impl Fn() + 'static) -> (gtk::Button, Icon) {
    let b = classes(gtk::Button::new(), &["btn", "icon", if danger { "danger" } else { "" }]);
    b.set_tooltip_text(Some(tooltip));
    let i = Icon::new(icon, 16);
    b.add(&i.widget);
    b.connect_clicked(move |_| cb());
    (b, i)
}

/// A segmented control: one button per option, chosen marked; on_choose(index) when another is clicked.
pub fn segmented(options: &[&str], chosen: usize, on_choose: impl Fn(usize) + 'static) -> (gtk::Box, Vec<gtk::Button>) {
    let bx = classes(gtk::Box::builder().spacing(2).homogeneous(true).build(), &["segmented"]);
    let buttons: Vec<gtk::Button> = options.iter().map(|o| classes(gtk::Button::with_label(o), &["segment"]))
        .collect();
    let on_choose = std::rc::Rc::new(on_choose);
    for (i, b) in buttons.iter().enumerate() {
        let all = buttons.clone();
        let f = on_choose.clone();
        b.connect_clicked(move |_| {
            for (j, o) in all.iter().enumerate() {
                style(o, "on", i == j);
            }
            f(i);
        });
        style(b, "on", i == chosen);
        bx.pack_start(b, true, true, 0);
    }
    (bx, buttons)
}

pub fn hairline(vertical: bool) -> gtk::Box {
    classes(gtk::Box::builder().hexpand(!vertical).vexpand(vertical).build(), &["hairline"])
}

pub fn group_header(label: &str) -> gtk::Label {
    classes(gtk::Label::builder().label(label).xalign(0.0).build(), &["group-header"])
}

/// Title and description at left, the control at right.
pub fn setting_row(title: &str, description: Option<&str>, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = classes(gtk::Box::builder().spacing(16).build(), &["setting"]);
    let words = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
    words.pack_start(&text(title, "body"), false, false, 0);
    if let Some(d) = description {
        words.pack_start(&wrapped(d, "small", 60), false, false, 0);
    }
    row.pack_start(&words, true, true, 0);
    control.set_valign(gtk::Align::Center);
    row.pack_end(control, false, false, 0);
    row
}

pub fn switch(on: bool, changed: impl Fn(bool) + 'static) -> gtk::Switch {
    let s = gtk::Switch::builder().active(on).build();
    s.connect_active_notify(move |w| changed(w.is_active()));
    s
}

// --- HUDs ---
/// Greyscale antialiasing for text on a translucent window (sub-pixel colour fringes show through it).
pub fn grayscale(w: &impl IsA<gtk::Widget>) {
    if let Ok(mut options) = cairo::FontOptions::new() {
        options.set_antialias(cairo::Antialias::Gray);
        w.set_font_options(Some(&options));
    }
}

/// A rounded rectangle as rectangles: the middle, then a row of pixels at a time for its corners.
pub fn rounded_region(x: i32, y: i32, w: i32, h: i32, r: i32) -> Vec<(i32, i32, i32, i32)> {
    let mut rects = vec![(x, y + r, w, h - 2 * r)];
    for i in 0..r {
        let d = (r - i) as f64 - 0.5;
        let inset = (r as f64 - ((r * r) as f64 - d * d).max(0.0).sqrt()).round() as i32;
        rects.push((x + inset, y + i, w - 2 * inset, 1));
        rects.push((x + inset, y + h - 1 - i, w - 2 * inset, 1));
    }
    rects
}

/// Ask KWin to blur what's behind card, a translucent window's card (X11: _KDE_NET_WM_BLUR_BEHIND_REGION;
/// ignored where there's no KWin). Only behind the card itself, rounded.
pub fn blur_behind(win: &impl IsA<gtk::Window>, card: &impl IsA<gtk::Widget>) {
    let Some(gdk_win) = win.as_ref().window() else { return };
    let a = card.allocation();
    let ctx = card.style_context();
    let m = ctx.margin(ctx.state());
    let rects = rounded_region(a.x() + m.left as i32, a.y() + m.top as i32,
                               a.width() - (m.left + m.right) as i32, a.height() - (m.top + m.bottom) as i32,
                               R_LG as i32);
    let values: Vec<std::os::raw::c_ulong> = rects.iter()
        .flat_map(|r| [r.0, r.1, r.2, r.3].map(|v| v.max(0) as std::os::raw::c_ulong)).collect();
    gdk::property_change(&gdk_win, &gdk::Atom::intern("_KDE_NET_WM_BLUR_BEHIND_REGION"), &gdk::Atom::intern("CARDINAL"),
                         32, gdk::PropMode::Replace, gdk::ChangeData::ULongs(&values));
}

/// A HUD card in a window of its own: translucent, rounded, floating above other windows. box_ is where its
/// content goes.
#[derive(Clone)]
pub struct Hud {
    pub win: gtk::Window,
    pub card: gtk::Box,
}

impl Hud {
    pub fn new(title: &str, width: i32) -> Hud {
        let win = classes(gtk::Window::builder().title(title).decorated(false).resizable(false).app_paintable(true)
            .skip_taskbar_hint(true).type_hint(gdk::WindowTypeHint::Dialog).build(), &["hud-window"]);
        if let Some(visual) = WidgetExt::screen(&win).and_then(|s| s.rgba_visual()) {
            win.set_visual(Some(&visual));
        }
        grayscale(&win);
        win.set_keep_above(true);
        let card = classes(gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .width_request(width).build(), &["hud"]);
        win.add(&card);
        win.connect_draw(|_, cr| {
            clear(cr);
            glib::Propagation::Proceed
        });
        let c = card.clone();
        win.connect_map_event(move |w, _| {
            blur_behind(w, &c);
            glib::Propagation::Proceed
        });
        win.connect_key_press_event(move |w, ev| {
            if ev.keyval() == gdk::keys::constants::Escape {
                w.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        win.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
        win.connect_button_press_event(|w, ev| {
            if ev.button() == 1 && ev.event_type() == gdk::EventType::ButtonPress {
                let (x, y) = ev.root();
                w.begin_move_drag(1, x as i32, y as i32, ev.time());
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        Hud { win, card }
    }

    pub fn heading(&self, title: &str, icon: Option<&str>, warning: bool) {
        let row = gtk::Box::builder().spacing(8).build();
        if let Some(icon) = icon {
            let i = Icon::new(icon, 16);
            classes(i.widget.clone(), &[if warning { "warning-text" } else { "accent-text" }]);
            row.pack_start(&i.widget, false, false, 0);
        }
        let t = wrapped(title, "title", 40);
        row.pack_start(&t, true, true, 0);
        self.card.pack_start(&row, false, false, 0);
    }

    /// Buttons at the bottom right: the safe choice first, the primary last.
    pub fn buttons(&self, buttons: &[gtk::Button]) {
        let row = gtk::Box::builder().spacing(8).margin_top(4).build();
        for b in buttons.iter().rev() {
            row.pack_end(b, false, false, 0);
        }
        self.card.pack_start(&row, false, false, 0);
    }

    pub fn close(&self) {
        destroy(&self.win);
    }

    pub fn is_open(&self) -> bool {
        self.win.is_mapped()
    }
}

/// Paint a window fully transparent (what's drawn after shows on its own).
pub fn clear(cr: &cairo::Context) {
    cr.set_operator(cairo::Operator::Source);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    let _ = cr.paint();
    cr.set_operator(cairo::Operator::Over);
}

/// A Pango layout of words in a type style, for drawing with cairo; (layout, width, height).
pub fn layout(cr: &cairo::Context, words: &str, kind: &str) -> (pango::Layout, i32, i32) {
    let lay = pangocairo::create_layout(cr);
    let (size, weight) = TYPE.iter().find(|(k, _, _)| *k == kind).map(|(_, s, w)| (*s, *w)).unwrap_or((11, 400));
    let mut desc = pango::FontDescription::new();
    desc.set_family("Inter,Noto Sans,Cantarell,sans-serif");
    desc.set_absolute_size(size as f64 * pango::SCALE as f64);
    desc.set_weight(if weight >= 600 { pango::Weight::Semibold } else { pango::Weight::Normal });
    lay.set_font_description(Some(&desc));
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrFontFeatures::new("tnum"));
    lay.set_attributes(Some(&attrs));
    lay.set_text(words);
    let (w, h) = lay.pixel_size();
    (lay, w, h)
}

pub fn show_layout(cr: &cairo::Context, lay: &pango::Layout) {
    pangocairo::show_layout(cr, lay);
}

pub fn page_box() -> gtk::Box {
    classes(gtk::Box::builder().orientation(gtk::Orientation::Vertical).build(), &["page"])
}

pub fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let s = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).hexpand(true)
        .build();
    s.add(child);
    s
}

/// Close a window for good. (GTK's destroy is unsafe in the bindings because other references to the widget would
/// then point at a destroyed one; ours only ever check is_visible or call destroy again, both harmless on it.)
pub fn destroy(win: &gtk::Window) {
    // SAFETY: see above; nothing touches the window's children after this.
    unsafe { win.destroy() };
}
