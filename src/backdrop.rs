//! How bright the desktop is behind an area: a small screenshot of it from KWin (org.kde.KWin.ScreenShot2, which
//! the app's .desktop file is allowed to use; see setup.sh). The app's own windows are kept out of screenshots, so
//! it's what's behind them. For the glass cards (the controls, the preview, notices, the toolbar), to keep them
//! readable.
use crate::layout::Rect;
use gio::prelude::*;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use glib::ToVariant;
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsRawFd;

/// Make card (in win) glass that stays readable: it has no fill of its own, so its ink and a faint tint follow how
/// bright the desktop is behind it, a few times a second while it shows: dark ink on a light backdrop (.on-light),
/// light ink on a dark one, with room between the two so it doesn't flicker. (The look: .glass in style.css.)
/// reaches: how much of the desktop's brightness reaches the card (1; less where something dims it first).
pub fn follow(win: &gtk::Window, card: &impl IsA<gtk::Widget>, reaches: f64) {
    let card = card.as_ref().clone();
    card.style_context().add_class("glass");
    let (win, card) = (win.downgrade(), card.downgrade());
    let (on_light, looking) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    glib::timeout_add_local(std::time::Duration::from_millis(350), move || {
        let (Some(w), Some(c)) = (win.upgrade(), card.upgrade()) else { return glib::ControlFlow::Break };
        if !w.is_visible() || looking.get() {
            return glib::ControlFlow::Continue;
        }
        let (x, y, cw, ch) = crate::design::card_rect(&c);
        let (wx, wy) = w.position();
        looking.set(true);
        let (on_light, looking, card) = (on_light.clone(), looking.clone(), c.downgrade());
        brightness((wx + x, wy + y, cw, ch), move |light| {
            looking.set(false);
            let (Some(light), Some(card)) = (light.map(|l| l * reaches), card.upgrade()) else { return };
            let now = if on_light.get() { light > 0.45 } else { light > 0.60 };
            if now != on_light.get() {
                on_light.set(now);
                let ctx = card.style_context();
                if now { ctx.add_class("on-light") } else { ctx.remove_class("on-light") }
            }
        });
        glib::ControlFlow::Continue
    });
}

/// Look behind rect (on the desktop) and call back on the main thread with how bright it is, 0–1 (None: no look,
/// e.g. KWin isn't there or won't allow it; a reason is logged once). Nothing here waits on the main thread: KWin's
/// answer and the picture (read from a pipe on a thread) each arrive when they arrive, and the look ends when both
/// have, or after 3 s.
pub fn brightness(rect: Rect, then: impl FnOnce(Option<f64>) + 'static) {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return then(None);
    }
    let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else { return then(None) };
    let Ok((mut read, write)) = std::io::pipe() else { return then(None) };
    let fds = gio::UnixFDList::new();
    let Ok(idx) = fds.append(write.as_raw_fd()) else { return then(None) };
    drop(write); // (KWin has its own copy now: when it's done writing, the pipe ends)
    let look = Rc::new(RefCell::new(Look { info: None, bytes: None, then: Some(Box::new(then)) }));
    let l = look.clone();
    crate::util::in_thread(move || {
        let mut bytes = vec![];
        let _ = read.read_to_end(&mut bytes);
        bytes
    }, move |bytes| {
        l.borrow_mut().bytes = Some(bytes);
        Look::finish(&l);
    });
    let l = look.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || Look::give_up(&l));
    let args = (x, y, w as u32, h as u32, HashMap::<String, glib::Variant>::new(), glib::variant::Handle(idx));
    conn.call_with_unix_fd_list(Some("org.kde.KWin"), "/org/kde/KWin/ScreenShot2", "org.kde.KWin.ScreenShot2",
                                "CaptureArea", Some(&args.to_variant()), None, gio::DBusCallFlags::NONE, 2000,
                                Some(&fds), gio::Cancellable::NONE, move |res| {
        let info = match res {
            Ok((v, out_fds)) => {
                // (the reply carries no fds, and gio 0.18 wraps that nothing as a list anyway: dropping it would
                // unref NULL, a GLib warning each time)
                std::mem::forget(out_fds);
                let info = v.child_value(0).get::<HashMap<String, glib::Variant>>().unwrap_or_default();
                let get = |k: &str| info.get(k).and_then(|v| v.get::<u32>());
                match (get("width"), get("height"), get("stride"), get("format")) {
                    (Some(w), Some(h), Some(stride), Some(format)) => Some((w, h, stride, format)),
                    _ => None,
                }
            }
            Err(e) => {
                warn_once(&e.to_string());
                None
            }
        };
        look.borrow_mut().info = Some(info);
        Look::finish(&look);
    });
}

/// A look behind, on its way: KWin's answer (the picture's width, height, stride, format; None: no picture), and
/// the picture.
struct Look {
    info: Option<Option<(u32, u32, u32, u32)>>,
    bytes: Option<Vec<u8>>,
    then: Option<Box<dyn FnOnce(Option<f64>)>>,
}

impl Look {
    fn finish(look: &Rc<RefCell<Look>>) {
        let answer = {
            let l = look.borrow();
            match (&l.info, &l.bytes) {
                (Some(None), _) => Some(None),
                (Some(Some((w, h, stride, format))), Some(bytes)) =>
                    Some(mean_brightness(bytes, *w as usize, *h as usize, *stride as usize, *format)),
                _ => None, // (the other half is still on its way)
            }
        };
        if let Some(answer) = answer {
            let then = look.borrow_mut().then.take();
            if let Some(f) = then {
                f(answer);
            }
        }
    }

    fn give_up(look: &Rc<RefCell<Look>>) {
        let then = look.borrow_mut().then.take();
        if let Some(f) = then {
            f(None);
        }
    }
}

/// The mean brightness (0–1) of a QImage's pixels: 32 bits each, as RGBA bytes (QImage's RGBA8888 formats, 17–19)
/// or as BGRA (its ARGB32 / RGB32 formats, 4–6, little-endian).
fn mean_brightness(bytes: &[u8], w: usize, h: usize, stride: usize, format: u32) -> Option<f64> {
    if w == 0 || h == 0 || bytes.len() < stride * (h - 1) + w * 4 {
        return None;
    }
    let rgba = (17..=19).contains(&format);
    let mut sum = 0.0;
    for row in 0..h {
        for px in bytes[row * stride..row * stride + w * 4].chunks_exact(4) {
            let (r, g, b) = if rgba { (px[0], px[1], px[2]) } else { (px[2], px[1], px[0]) };
            sum += 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
        }
    }
    Some(sum / (w * h) as f64 / 255.0)
}

fn warn_once(why: &str) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| crate::warn!("the controls can't see behind themselves: {why}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_brightness_reads_both_byte_orders_and_skips_row_padding() {
        let white = [255u8; 4 * 2 * 2];
        assert!((mean_brightness(&white, 2, 2, 8, 6).unwrap() - 1.0).abs() < 1e-9);
        // one pure-blue pixel: BGRA (format 6) vs RGBA (format 18), with 4 bytes of padding after each row
        let bgra = [255, 0, 0, 255, 9, 9, 9, 9];
        let rgba = [0, 0, 255, 255, 9, 9, 9, 9];
        assert!((mean_brightness(&bgra, 1, 1, 8, 6).unwrap() - 0.0722).abs() < 1e-9);
        assert!((mean_brightness(&rgba, 1, 1, 8, 18).unwrap() - 0.0722).abs() < 1e-9);
        assert_eq!(mean_brightness(&[0u8; 3], 1, 1, 4, 6), None);
    }
}
