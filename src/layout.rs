//! The geometry of a recording, as plain functions (no GTK, no GStreamer): how big the video is, where the area
//! goes in it as the area changes size, which parts of the area to show when only some windows are recorded,
//! and where the camera sits.

/// x, y, w, h (desktop pixels, or the video's).
pub type Rect = (i32, i32, i32, i32);

pub const MAX_W: i32 = 3840; // the largest video made
pub const MAX_H: i32 = 2160;
pub const MIN_SIDE: i32 = 64; // the smallest area
/// The smallest video side: AMD's hardware H.264 encoder faults (and resets) on smaller pictures, such as a
/// 68-pixel-high one made by following a small window. A smaller area sits on the background in the middle.
pub const MIN_VIDEO: i32 = 128;

/// n rounded to an even whole number (video encoders want even sizes), at least 2.
pub fn even(n: f64) -> i32 {
    ((n / 2.0).round_ties_even() as i32 * 2).max(2)
}

pub fn even_floor(n: f64) -> i32 {
    ((n as i32) / 2 * 2).max(0)
}

/// The video's size for the area first recorded: the area's own size for "area" (so nothing is scaled at first),
/// else a 16:9 preset. At most 4K, keeping the shape.
pub fn output_size(choice: &str, area_w: i32, area_h: i32) -> (i32, i32) {
    match choice {
        "720" => (1280, 720),
        "1080" => (1920, 1080),
        "1440" => (2560, 1440),
        _ => {
            let (w, h) = (area_w.max(MIN_SIDE) as f64, area_h.max(MIN_SIDE) as f64);
            let shrink = (MAX_W as f64 / w).min(MAX_H as f64 / h).min(1.0);
            (even(w * shrink).max(MIN_VIDEO), even(h * shrink).max(MIN_VIDEO))
        }
    }
}

/// Where an area of area_w × area_h goes in an out_w × out_h video: scaled to fit, keeping its shape; enlarged
/// only up to max_zoom (so a small area isn't blown up without limit), and centred, with the background around it.
pub fn fit(area_w: i32, area_h: i32, out_w: i32, out_h: i32, max_zoom: f64) -> Rect {
    if area_w <= 0 || area_h <= 0 {
        return (0, 0, out_w, out_h);
    }
    let z = (out_w as f64 / area_w as f64).min(out_h as f64 / area_h as f64).min(max_zoom.max(0.01));
    let w = out_w.min((area_w as f64 * z).round_ties_even() as i32);
    let h = out_h.min((area_h as f64 * z).round_ties_even() as i32);
    ((out_w - w) / 2, (out_h - h) / 2, w, h)
}

/// The rectangle t (0–1) of the way from a to b.
pub fn lerp_rect(a: Rect, b: Rect, t: f64) -> Rect {
    let l = |p: i32, q: i32| (p as f64 + (q - p) as f64 * t).round_ties_even() as i32;
    (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2), l(a.3, b.3))
}

fn region_of(r: Rect) -> cairo::Region {
    cairo::Region::create_rectangle(&cairo::RectangleInt::new(r.0, r.1, r.2.max(0), r.3.max(0)))
}

/// The parts of area to record when only the chosen windows are: each chosen window, less whatever other windows
/// cover it. windows: every window's (id, rect), bottom first (the stacking order). Rectangles relative to the
/// area; None means all of it (no windows chosen).
pub fn visible(area: Rect, chosen: &[String], windows: &[(String, Rect)]) -> Option<Vec<Rect>> {
    if chosen.is_empty() {
        return None;
    }
    let shown = cairo::Region::create();
    for id in chosen {
        let Some(pos) = windows.iter().position(|(w, _)| w == id) else { continue };
        let region = region_of(windows[pos].1);
        for (other, rect) in &windows[pos + 1..] {
            if !chosen.contains(other) {
                let _ = region.subtract(&region_of(*rect));
            }
        }
        let _ = shown.union(&region);
    }
    let _ = shown.intersect(&region_of(area));
    shown.translate(-area.0, -area.1);
    Some((0..shown.num_rectangles()).map(|i| {
        let r = shown.rectangle(i);
        (r.x(), r.y(), r.width(), r.height())
    }).collect())
}

/// The smallest rectangle around rects.
pub fn bounds(rects: &[Rect]) -> Rect {
    let x0 = rects.iter().map(|r| r.0).min().unwrap_or(0);
    let y0 = rects.iter().map(|r| r.1).min().unwrap_or(0);
    let x1 = rects.iter().map(|r| r.0 + r.2).max().unwrap_or(0);
    let y1 = rects.iter().map(|r| r.1 + r.3).max().unwrap_or(0);
    (x0, y0, x1 - x0, y1 - y0)
}

/// rect moved and cut to lie within screen, at least MIN_SIDE a side.
pub fn clamp_rect(rect: Rect, screen: Rect) -> Rect {
    let (x, y, w, h) = rect;
    let (sx, sy, sw, sh) = screen;
    let w = w.max(MIN_SIDE).min(sw);
    let h = h.max(MIN_SIDE).min(sh);
    (x.max(sx).min(sx + sw - w), y.max(sy).min(sy + sh - h), w, h)
}

/// videocrop's (left, right, top, bottom) in the screen stream's pixels, for area on screen (both desktop
/// coordinates); scale: stream pixels per desktop pixel. Even numbers, so the picture stays aligned.
pub fn crop(area: Rect, screen: Rect, scale_x: f64, scale_y: f64) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = clamp_rect(area, screen);
    let (sx, sy, sw, sh) = screen;
    let full_w = (sw as f64 * scale_x).round_ties_even() as i32;
    let full_h = (sh as f64 * scale_y).round_ties_even() as i32;
    let left = even_floor((x - sx) as f64 * scale_x);
    let top = even_floor((y - sy) as f64 * scale_y);
    let width = even(w as f64 * scale_x).min(full_w - left);
    let height = even(h as f64 * scale_y).min(full_h - top);
    (left, full_w - left - width, top, full_h - top - height)
}

pub const CAMERA_MARGIN: f64 = 0.03; // of the video's height, kept clear around the camera

/// Where the camera goes in the video, centred on (cx, cy) (shares of the video's width and height), size × the
/// video's height tall, aspect wide per tall; kept inside the video with a small margin.
pub fn camera_rect(out_w: i32, out_h: i32, cx: f64, cy: f64, size: f64, aspect: f64) -> Rect {
    let (ow, oh) = (out_w as f64, out_h as f64);
    let h = (size * oh).min(oh).max(16.0);
    let w = ow.min(h * aspect);
    let h = w / aspect;
    let m = CAMERA_MARGIN * oh;
    let x = (cx * ow - w / 2.0).max(m).min(ow - w - m);
    let y = (cy * oh - h / 2.0).max(m).min(oh - h - m);
    (if x > 1.0 { even(x) } else { 0 }, if y > 1.0 { even(y) } else { 0 }, even(w), even(h))
}

/// (cx, cy, size) for a camera at rect in an out_w × out_h video: camera_rect's inverse.
pub fn camera_centre(rect: (f64, f64, f64, f64), out_w: i32, out_h: i32) -> (f64, f64, f64) {
    let (x, y, w, h) = rect;
    ((x + w / 2.0) / out_w as f64, (y + h / 2.0) / out_h as f64, h / out_h as f64)
}

/// The space between windows taken out: on each axis, a stretch that no window covers, wider than GAP, is narrowed
/// to GAP. Windows keep their sizes, their order and their overlaps; only the empty space between them goes. For
/// recording windows that are far apart, which otherwise means a mostly empty video.
pub struct Squeeze {
    x: Vec<(i32, i32)>, // (where a gap starts, how much of it goes), on each axis
    y: Vec<(i32, i32)>,
}

pub const SQUEEZE_GAP: i32 = 24;

impl Squeeze {
    pub fn new(rects: &[Rect]) -> Squeeze {
        fn gaps(mut spans: Vec<(i32, i32)>) -> Vec<(i32, i32)> {
            spans.sort();
            let mut cuts = vec![];
            let mut end: Option<i32> = None;
            for (a, b) in spans {
                match end {
                    Some(e) if a - e > SQUEEZE_GAP => cuts.push((e, a - e - SQUEEZE_GAP)),
                    _ => {}
                }
                end = Some(end.map_or(b, |e| e.max(b)));
            }
            cuts
        }
        Squeeze { x: gaps(rects.iter().map(|r| (r.0, r.0 + r.2)).collect()),
                  y: gaps(rects.iter().map(|r| (r.1, r.1 + r.3)).collect()) }
    }

    fn axis(cuts: &[(i32, i32)], v: i32) -> i32 {
        cuts.iter().fold(v, |out, &(start, cut)| out - (v - start - SQUEEZE_GAP).clamp(0, cut))
    }

    pub fn point(&self, x: i32, y: i32) -> (i32, i32) {
        (Self::axis(&self.x, x), Self::axis(&self.y, y))
    }

    /// Where rect goes (a window: the same size; an area around windows: without the gaps).
    pub fn rect(&self, r: Rect) -> Rect {
        let (x0, y0) = self.point(r.0, r.1);
        let (x1, y1) = self.point(r.0 + r.2, r.1 + r.3);
        (x0, y0, x1 - x0, y1 - y0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(rects: &[Rect]) -> i32 {
        rects.iter().map(|r| r.2 * r.3).sum()
    }

    #[test]
    fn output_size_area_is_even_and_capped() {
        assert_eq!(output_size("area", 1001, 601), (1000, 600));
        let (w, h) = output_size("area", 7680, 2160);
        assert!(w <= 3840 && h <= 2160 && w % 2 == 0);
        assert_eq!(output_size("1080", 10, 10), (1920, 1080));
        assert_eq!(output_size("area", 396, 68), (396, 128));
    }

    #[test]
    fn fit_scales_down_bigger_areas_and_limits_zoom() {
        assert_eq!(fit(1920, 1080, 1920, 1080, 2.0), (0, 0, 1920, 1080));
        assert_eq!(fit(3840, 1080, 1920, 1080, 2.0), (0, 270, 1920, 540));
        assert_eq!(fit(480, 270, 1920, 1080, 2.0), (480, 270, 960, 540));
        assert_eq!(fit(480, 270, 1920, 1080, 1.0), (720, 405, 480, 270));
        assert_eq!(fit(480, 270, 1920, 1080, 99.0), (0, 0, 1920, 1080));
    }

    #[test]
    fn visible_cuts_out_windows_on_top() {
        let windows: Vec<(String, Rect)> = vec![("a".into(), (0, 0, 100, 100)), ("b".into(), (50, 50, 100, 100)),
                                                ("c".into(), (0, 0, 20, 20))];
        let rects = visible((0, 0, 200, 200), &["a".into()], &windows).unwrap();
        assert_eq!(area(&rects), 100 * 100 - 50 * 50 - 20 * 20);
        assert_eq!(visible((0, 0, 200, 200), &[], &windows), None);
        let both = visible((0, 0, 200, 200), &["a".into(), "b".into()], &windows).unwrap();
        assert_eq!(area(&both), 100 * 100 + 100 * 100 - 50 * 50 - 20 * 20);
    }

    #[test]
    fn visible_is_relative_to_area() {
        let rects = visible((40, 40, 20, 20), &["a".into()], &[("a".into(), (0, 0, 100, 100))]).unwrap();
        assert_eq!(rects, vec![(0, 0, 20, 20)]);
    }

    #[test]
    fn crop_in_stream_pixels() {
        assert_eq!(crop((100, 50, 400, 300), (0, 0, 1000, 800), 1.0, 1.0), (100, 500, 50, 450));
        let (l, r, t, b) = crop((100, 50, 400, 300), (0, 0, 1000, 800), 2.0, 2.0);
        assert!((l, t) == (200, 100) && 2000 - l - r == 800 && 1600 - t - b == 600);
        let (l, r, t, b) = crop((900, 700, 400, 300), (0, 0, 1000, 800), 1.0, 1.0);
        assert!(l.min(r).min(t).min(b) >= 0);
    }

    #[test]
    fn camera_rect_stays_inside_and_round_trips() {
        let (x, y, w, h) = camera_rect(1920, 1080, 1.0, 1.0, 0.25, 1.0);
        assert!(x + w <= 1920 && y + h <= 1080 && w == h);
        let (cx, cy, size) = camera_centre((800.0, 400.0, 270.0, 270.0), 1920, 1080);
        assert_eq!(camera_rect(1920, 1080, cx, cy, size, 1.0), (800, 400, 270, 270));
    }

    #[test]
    fn squeeze_takes_out_the_space_between_windows() {
        let (a, b) = ((100, 100, 400, 300), (2000, 900, 500, 200)); // far apart both ways
        let s = Squeeze::new(&[a, b]);
        assert_eq!(s.rect(a), a);
        assert_eq!(s.rect(b), (500 + SQUEEZE_GAP, 400 + SQUEEZE_GAP, 500, 200));
        assert_eq!(s.rect(bounds(&[a, b])), (100, 100, 900 + SQUEEZE_GAP, 500 + SQUEEZE_GAP));
        // overlapping windows and a menu sticking out of one stay as they are
        let (c, menu) = ((450, 150, 300, 300), (700, 400, 200, 300));
        let s = Squeeze::new(&[a, c, menu]);
        assert_eq!((s.rect(a), s.rect(c), s.rect(menu)), (a, c, menu));
        // a small gap stays
        let d = (500 + 10, 100, 100, 100);
        assert_eq!(Squeeze::new(&[a, d]).rect(d), d);
    }
}
