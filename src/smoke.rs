//! A manual end-to-end check of the engine (needs a desktop session): explainonvideo --engine-smoke OUTDIR.
//! Records a test pattern for a few seconds, changing the area, the camera and pausing on the way.
use crate::engine::{Engine, Screen};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

pub fn run(out: &str) {
    std::env::set_var("EOV_TEST_CAMERA", "1");
    let main = glib::MainLoop::new(None, false);
    let e = Engine::new(Screen { rect: (0, 0, 3440, 1440), stream: None }, 1280, 720, 30).expect("engine");
    let frames = Rc::new(Cell::new(0));
    let f = frames.clone();
    *e.on_frame.borrow_mut() = Some(Box::new(move |_| f.set(f.get() + 1)));
    *e.on_error.borrow_mut() = Some(Box::new(|m, f, w| println!("ERROR {m} {f} {w:?}")));
    e.set_area((100, 100, 1280, 720), None, false);
    e.start();
    let t0 = Instant::now();
    let out = std::path::PathBuf::from(out);
    let m = main.clone();
    let steps: Vec<(f64, Box<dyn Fn(&Engine)>)> = vec![
        (0.5, Box::new(move |e| {
            e.prepare(&out);
            let m = m.clone();
            e.record(move |p, err| {
                println!("SAVED {} {err:?}", p.display());
                m.quit();
            });
        })),
        (1.5, Box::new(|e| e.set_area((200, 200, 640, 360), None, true))),
        (1.8, Box::new(|e| { e.set_camera(true); })),
        (2.2, Box::new(|e| e.set_area((0, 0, 3440, 1440), Some(vec![(0, 0, 1000, 1000)]), true))),
        (2.6, Box::new(|e| e.pause())),
        (3.2, Box::new(|e| e.resume())),
        (3.4, Box::new(|e| { crate::settings::set("camera_x", 0.2.into()); e.place_camera(); })),
        (4.4, Box::new(|e| { e.set_camera(false); })),
        (5.2, Box::new(|e| e.stop())),
    ];
    let next = Cell::new(0usize);
    let e2 = e.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
        let now = t0.elapsed().as_secs_f64();
        while next.get() < steps.len() && steps[next.get()].0 <= now {
            println!("{now:.2} step {}", next.get());
            (steps[next.get()].1)(&e2);
            next.set(next.get() + 1);
        }
        glib::ControlFlow::Continue
    });
    let m = main.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(20), move || m.quit());
    main.run();
    println!("preview frames {}", frames.get());
    e.close();
}

/// A window that goes full screen and back every 1.5 seconds, drawing a circle in a square (for --window-drive: in
/// the video they must stay round and square): explainonvideo --resize-test.
pub fn resize_test() {
    use gtk::prelude::*;
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_title("Resize test");
    win.set_default_size(800, 500);
    win.move_(400, 300);
    let area = gtk::DrawingArea::new();
    area.connect_draw(|w, cr| {
        let (aw, ah) = (w.allocated_width() as f64, w.allocated_height() as f64);
        cr.set_source_rgb(0.1, 0.1, 0.2);
        let _ = cr.paint();
        let side = aw.min(ah) * 0.8;
        let (x, y) = ((aw - side) / 2.0, (ah - side) / 2.0);
        cr.set_source_rgb(0.9, 0.8, 0.2);
        cr.rectangle(x, y, side, side);
        let _ = cr.fill();
        cr.set_source_rgb(0.2, 0.5, 0.9);
        cr.arc(aw / 2.0, ah / 2.0, side / 2.0, 0.0, 2.0 * std::f64::consts::PI);
        let _ = cr.fill();
        glib::Propagation::Stop
    });
    win.add(&area);
    win.show_all();
    win.connect_destroy(|_| gtk::main_quit());
    let full = std::rc::Rc::new(std::cell::Cell::new(false));
    glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
        if full.replace(!full.get()) { win.unfullscreen() } else { win.fullscreen() }
        glib::ControlFlow::Continue
    });
    glib::timeout_add_local_once(std::time::Duration::from_secs(20), gtk::main_quit);
    gtk::main();
}
