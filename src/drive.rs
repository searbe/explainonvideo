//! Walks the whole app through a recording with made-up input, taking a screenshot at each stage (needs a KDE
//! desktop session; records a test pattern, not the screen): explainonvideo --ui-drive OUTDIR.
//! It doesn't move the real pointer or press real keys: it calls the windows' handlers directly.
use crate::app::{app, App};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

fn shot(out: &str, n: &Cell<usize>, name: &str) {
    let path = format!("{out}/{:02}-{name}.png", n.get());
    n.set(n.get() + 1);
    let _ = std::process::Command::new("spectacle").args(["-b", "-n", "-f", "-o", &path]).status();
    println!("shot {path}");
}

pub fn run(out: &str) {
    std::env::set_var("EOV_TEST_SCREEN", "1");
    std::env::set_var("EOV_TEST_CAMERA", "1");
    std::env::set_var("EOV_INSTANCE", "drive");
    std::env::set_var("EOV_SHOW_IN_CAPTURE", "1");
    std::fs::create_dir_all(out).unwrap();
    let settings_file = format!("{out}/settings.json");
    std::fs::write(&settings_file, serde_json::json!({"folder": out, "countdown": 3, "mic": false,
        "system_audio": true, "camera": false}).to_string()).unwrap();
    std::env::set_var("EOV_SETTINGS", &settings_file);
    let a = App::new(true);
    let saved: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let s2 = saved.clone();
    *a.on_saved.borrow_mut() = Some(Box::new(move |p, e| {
        println!("saved {} {e:?}", p.display());
        *s2.borrow_mut() = Some(p.to_path_buf());
    }));
    let out = out.to_string();
    let n = Rc::new(Cell::new(0));
    let o = out.clone();
    let sh = move |name: &'static str| -> Box<dyn Fn()> {
        let (o, n) = (o.clone(), n.clone());
        Box::new(move || shot(&o, &n, name))
    };
    let s3 = saved.clone();
    let steps: Vec<(f64, Box<dyn Fn()>)> = vec![
        (2.0, sh("selector")),
        (0.2, Box::new(|| { // hover the topmost window
            let a = app();
            let sel = a.selector.borrow().clone().unwrap();
            let w = a.kwin.windows.borrow().last().cloned();
            if let Some(w) = w {
                sel.motion((w.rect.0 + w.rect.2 / 2 - sel.screen.0) as f64, (w.rect.1 + w.rect.3 / 2 - sel.screen.1) as f64);
            }
            println!("windows: {}", a.kwin.windows.borrow().len());
        })),
        (0.5, sh("hover")),
        (0.2, Box::new(|| { // drag out an area
            let sel = app().selector.borrow().clone().unwrap();
            sel.press(700.0, 300.0, 1);
            for k in 1..=10 {
                sel.motion(700.0 + 120.0 * k as f64, 300.0 + 70.0 * k as f64);
            }
            sel.release(1900.0, 1000.0, 1);
        })),
        (0.5, sh("area")),
        (0.2, Box::new(|| { // click a window inside the area
            let a = app();
            let sel = a.selector.borrow().clone().unwrap();
            let Some((ax, ay, aw, ah)) = *sel.area.borrow() else { return };
            let ws = a.kwin.windows.borrow().clone();
            for w in ws.iter().rev() {
                let (x, y, ww, hh) = w.rect;
                let (cx, cy) = (x.max(ax) + 30, y.max(ay) + 30);
                if ax < cx && cx < ax + aw && ay < cy && cy < ay + ah && x < cx && cx < x + ww && y < cy && cy < y + hh {
                    sel.press((cx - sel.screen.0) as f64, (cy - sel.screen.1) as f64, 1);
                    sel.release((cx - sel.screen.0) as f64, (cy - sel.screen.1) as f64, 1);
                    println!("chose {}", w.title);
                    return;
                }
            }
        })),
        (0.5, sh("area-window")),
        (0.2, Box::new(|| { let s = app().selector.borrow().clone().unwrap(); s.finish(false); })),
        (0.8, sh("ready")), // (an area around a window: the Follow button shows)
        (0.2, Box::new(|| { app().follow(); println!("follows: {:?}", app().selection.borrow().as_ref().map(|s| s.follows())); })),
        (0.8, sh("followed")),
        (0.2, Box::new(|| app().frame().drag("move", -150, -80))),
        (0.8, sh("detached")),
        (0.2, Box::new(|| { // no windows left to follow: Follow picks one
            let a = app();
            if let Some(s) = a.selection.borrow_mut().as_mut() { s.windows.clear(); }
            a.apply_selection(false);
            a.follow();
        })),
        (0.8, sh("pick-window")),
        (0.2, Box::new(|| {
            let a = app();
            let sel = a.selector.borrow().clone().unwrap();
            let w = a.kwin.windows.borrow().last().cloned();
            if let Some(w) = w {
                let (cx, cy) = ((w.rect.0 + w.rect.2 / 2 - sel.screen.0) as f64, (w.rect.1 + w.rect.3 / 2 - sel.screen.1) as f64);
                sel.press(cx, cy, 1);
                sel.release(cx, cy, 1);
            }
            sel.finish(false);
            println!("picked: follows {:?}", a.selection.borrow().as_ref().map(|s| s.follows()));
        })),
        (0.8, sh("picked")),
        (0.2, Box::new(|| app().toggle("camera"))),
        (2.0, sh("preview-camera")),
        (0.2, Box::new(|| { // move the camera in the preview
            let a = app();
            let (Some(p), Some((x, y, w, h))) = (a.preview.borrow().clone(), a.camera_rect()) else { return };
            let (sx, sy) = p.scale();
            let (cx, cy) = ((x as f64 + w as f64 / 2.0) * sx, (y as f64 + h as f64 / 2.0) * sy);
            p.press_at(cx, cy, 1);
            p.motion(cx - 200.0, cy - 60.0);
            p.release();
            println!("camera now {:?}", a.camera_rect());
        })),
        (0.6, sh("camera-moved")),
        (0.2, Box::new(|| app().start())),
        (1.2, sh("countdown")),
        (3.0, sh("recording")),
        (0.5, Box::new(|| app().frame().drag("se", -400, -200))),
        (1.0, sh("resized-while-recording")),
        (0.5, Box::new(|| app().pause_or_resume())),
        (0.6, sh("paused")),
        (0.5, Box::new(|| app().pause_or_resume())),
        (1.5, Box::new(|| app().stop())),
        (4.0, sh("saved")),
        (0.2, Box::new(move || {
            if let Some(p) = s3.borrow().clone() {
                crate::editor::Editor::open(&p);
            }
        })),
        (3.0, sh("editor")),
        (0.2, Box::new(|| crate::editor::drive_edits())),
        (1.5, sh("editor-edited")),
        (0.2, Box::new(|| crate::editor::drive_export())),
        (6.0, sh("exported")),
        (0.5, Box::new(gtk::main_quit)),
    ];
    let steps = Rc::new(steps);
    glib::idle_add_local_once(move || {
        let mut t = 0.0;
        for i in 0..steps.len() {
            t += steps[i].0;
            let steps = steps.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis((t * 1000.0) as u64), move || {
                (steps[i].1)();
            });
        }
    });
    glib::timeout_add_local_once(std::time::Duration::from_secs(150), gtk::main_quit);
    a.run();
    println!("done {:?}", saved.borrow());
}

/// Opens the preview on the real shared screen and takes a screenshot (explainonvideo --try-preview OUTDIR).
/// The real screen (from KWin or the portal): the preview (and moved with the controls), then 7 seconds recorded, a
/// second window clicked in half-way through; with title, of the topmost window whose title has it in it, chosen
/// before the preview opens.
pub fn try_preview(out: &str, title: Option<String>) {
    std::env::set_var("EOV_SHOW_IN_CAPTURE", "1");
    std::env::set_var("EOV_INSTANCE", "preview");
    std::fs::create_dir_all(out).unwrap();
    let settings_file = format!("{out}/settings.json");
    std::fs::write(&settings_file, serde_json::json!({"folder": out, "countdown": 0, "mic": false,
        "system_audio": true, "camera": false, "output": "1080"}).to_string()).unwrap();
    std::env::set_var("EOV_SETTINGS", &settings_file);
    let a = App::new(false);
    *a.on_saved.borrow_mut() = Some(Box::new(|p, e| println!("saved {} {e:?}", p.display())));
    let o = out.to_string();
    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        let a = app();
        if let Some(t) = title {
            let w = a.kwin.windows.borrow().iter().rev().find(|w| w.title.contains(&t)).cloned();
            if let Some(w) = w {
                println!("chose {}", w.title);
                *a.selection.borrow_mut() = Some(crate::selector::Selection { area: None, windows: vec![w.id],
                                                                              screen: false });
                a.apply_selection(false);
            }
        }
        a.toggle_preview();
    });
    let n = Rc::new(Cell::new(0));
    let (o2, n2) = (o.clone(), n.clone());
    glib::timeout_add_local_once(std::time::Duration::from_millis(4000), move || {
        shot(&o2, &n2, "preview");
        let c = app().controls(); // (the preview goes where the controls go)
        let (x, y) = c.win.position();
        c.win.move_(x - 600, y - 200);
    });
    let (o2, n2) = (o.clone(), n.clone());
    glib::timeout_add_local_once(std::time::Duration::from_millis(5000), move || {
        shot(&o2, &n2, "preview-moved");
        app().start();
    });
    // while recording, the Windows button: click a second window in, then Done
    let (o2, n2) = (o.clone(), n.clone());
    glib::timeout_add_local_once(std::time::Duration::from_millis(7000), move || {
        let a = app();
        a.choose();
        let chosen = a.selection.borrow().as_ref().map(|s| s.windows.clone()).unwrap_or_default();
        // (the big window furthest from the chosen one: the space between them is taken out of the video)
        let first = a.kwin.windows.borrow().iter().find(|w| chosen.contains(&w.id)).map(|w| w.rect);
        let centre = |r: crate::layout::Rect| (r.0 + r.2 / 2, r.1 + r.3 / 2);
        let far = |r: crate::layout::Rect| first.map_or(0, |f| {
            let ((ax, ay), (bx, by)) = (centre(f), centre(r));
            (ax - bx).abs() + (ay - by).abs()
        });
        let other = a.kwin.windows.borrow().iter()
            .filter(|w| !chosen.contains(&w.id) && w.rect.2 > 400 && w.rect.3 > 300)
            .max_by_key(|w| far(w.rect)).cloned();
        let sel = a.selector.borrow().clone();
        if let (Some(w), Some(sel)) = (other, sel) {
            println!("adding {} {:?}", w.title, w.rect);
            let (sx, sy, _, _) = sel.screen;
            let (wx, wy, ww, wh) = w.rect; // (a visible part of it: what's clicked is what's on top)
            let (px, py) = (1..10).flat_map(|i| (1..10).map(move |j| (wx + ww * i / 10, wy + wh * j / 10)))
                .find(|&(x, y)| a.kwin.at(x, y).is_some_and(|t| t.id == w.id))
                .unwrap_or((wx + ww / 2, wy + wh / 2));
            let (x, y) = ((px - sx) as f64, (py - sy) as f64);
            sel.press(x, y, 1);
            sel.release(x, y, 1);
        }
        let o3 = o2.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            let a = app(); // (followed windows far apart: the space between them is taken out)
            println!("video area {:?} (on the desktop {:?})", a.video_rect(), a.rect());
            shot(&o3, &n2, "picking");
            let s = app().selector.borrow().clone();
            if let Some(s) = s {
                s.finish(false);
            }
        });
    });
    glib::timeout_add_local_once(std::time::Duration::from_millis(12000), || app().stop());
    glib::timeout_add_local_once(std::time::Duration::from_millis(16000), gtk::main_quit);
    a.run();
}

/// Records real windows by their own streams (needs KWin to offer them to this binary): explainonvideo
/// --window-drive TITLE OUTDIR follows the topmost window whose title has TITLE in it for a few seconds, then
/// records it with a drawn area around it and a second window, and saves the MP4s in OUTDIR.
pub fn windows(title: &str, out: &str) {
    std::env::set_var("EOV_TEST_SCREEN", "1"); // (the screen itself isn't needed: only the windows' streams)
    std::env::set_var("EOV_INSTANCE", "windows");
    std::fs::create_dir_all(out).unwrap();
    let settings_file = format!("{out}/settings.json");
    std::fs::write(&settings_file, serde_json::json!({"folder": out, "countdown": 0, "mic": false,
        "system_audio": false, "camera": false, "output": "1080"}).to_string()).unwrap();
    std::env::set_var("EOV_SETTINGS", &settings_file);
    let a = App::new(false);
    *a.on_saved.borrow_mut() = Some(Box::new(|p, e| println!("saved {} {e:?}", p.display())));
    let title = title.to_string();
    let steps: Vec<(f64, Box<dyn Fn()>)> = vec![
        (1.5, Box::new(move || {
            let a = app();
            let ws = a.kwin.windows.borrow().clone();
            let Some(w) = ws.iter().rev().find(|w| w.title.contains(&title)) else {
                println!("no window {title:?}");
                return gtk::main_quit();
            };
            println!("following {} {:?}", w.title, w.rect);
            *a.selection.borrow_mut() = Some(crate::selector::Selection { area: None, windows: vec![w.id.clone()],
                                                                          screen: false });
            a.apply_selection(false);
            a.start();
        })),
        (4.0, Box::new(|| app().stop())),
        (3.0, Box::new(|| { // a drawn area across two windows: only those two, nothing over them
            let a = app();
            let ws = a.kwin.windows.borrow().clone();
            let chosen = a.selection.borrow().as_ref().map(|s| s.windows.clone()).unwrap_or_default();
            let Some(first) = ws.iter().find(|w| chosen.contains(&w.id)).cloned() else { return };
            let other = ws.iter().find(|w| !chosen.contains(&w.id) && w.rect.2 > 400 && w.rect.3 > 300).cloned();
            let mut ids = chosen.clone();
            let mut r = first.rect;
            if let Some(o) = other {
                println!("and {} {:?}", o.title, o.rect);
                ids.push(o.id.clone());
                r = crate::layout::bounds(&[r, o.rect]);
            }
            let area = (r.0 + 100, r.1 + 100, (r.2 - 200).max(300), (r.3 - 200).max(200));
            println!("area {area:?}");
            *a.selection.borrow_mut() = Some(crate::selector::Selection { area: Some(area), windows: ids,
                                                                          screen: false });
            a.apply_selection(false);
            a.start();
        })),
        (4.0, Box::new(|| app().stop())),
        (4.0, Box::new(gtk::main_quit)),
    ];
    let steps = Rc::new(steps);
    glib::idle_add_local_once(move || {
        let mut t = 0.0;
        for i in 0..steps.len() {
            t += steps[i].0;
            let steps = steps.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis((t * 1000.0) as u64), move || {
                (steps[i].1)();
            });
        }
    });
    a.run();
}
