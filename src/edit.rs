//! A recording's edits, as plain data: which stretches of it are kept, in which order (clips), and what part of
//! the picture (crop). Nothing here touches the file; export.rs turns an Edit into an ffmpeg command.
use serde_json::{json, Value};

pub const MIN_CLIP: f64 = 0.1; // seconds: nothing shorter is kept

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub start: f64, // in the recording, seconds
    pub end: f64,
}

impl Clip {
    pub fn new(start: f64, end: f64) -> Clip {
        Clip { start, end }
    }
    pub fn length(&self) -> f64 {
        self.end - self.start
    }
}

/// (x, y, w, h) as shares of the picture.
pub type Crop = (f64, f64, f64, f64);
type State = (Vec<Clip>, Option<Crop>);

#[derive(Clone, Debug)]
pub struct Edit {
    pub duration: f64, // the recording's
    pub clips: Vec<Clip>,
    pub crop: Option<Crop>, // None for all of the picture
    undone: Vec<State>,
    redone: Vec<State>,
}

impl Edit {
    pub fn new(duration: f64) -> Edit {
        Edit::with(duration, vec![], None)
    }

    pub fn with(duration: f64, clips: Vec<Clip>, crop: Option<Crop>) -> Edit {
        let clips = if clips.is_empty() { vec![Clip::new(0.0, duration)] } else { clips };
        Edit { duration, clips, crop, undone: vec![], redone: vec![] }
    }

    // --- reading ---
    /// How long the edited video is.
    pub fn length(&self) -> f64 {
        self.clips.iter().map(|c| c.length()).sum()
    }

    /// Where each clip starts in the edited video.
    pub fn starts(&self) -> Vec<f64> {
        let mut t = 0.0;
        self.clips.iter().map(|c| {
            let s = t;
            t += c.length();
            s
        }).collect()
    }

    /// (clip index, time in the recording) for time t in the edited video; the end if past it.
    pub fn locate(&self, t: f64) -> Option<(usize, f64)> {
        if self.clips.is_empty() {
            return None;
        }
        let t = t.max(0.0);
        for (i, (c, s)) in self.clips.iter().zip(self.starts()).enumerate() {
            if t < s + c.length() {
                return Some((i, c.start + (t - s)));
            }
        }
        Some((self.clips.len() - 1, self.clips.last().unwrap().end))
    }

    /// Time in the edited video of source_time within clip index.
    pub fn timeline_time(&self, index: usize, source_time: f64) -> f64 {
        let c = self.clips[index];
        self.starts()[index] + source_time.max(c.start).min(c.end) - c.start
    }

    #[cfg(test)]
    pub fn edited(&self) -> bool {
        self.crop.is_some() || self.clips.len() != 1 || self.clips[0].start > 0.01
            || self.clips[0].end < self.duration - 0.01
    }

    pub fn can_undo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redone.is_empty()
    }

    // --- changing (each can be undone) ---
    fn save(&mut self) {
        self.undone.push((self.clips.clone(), self.crop));
        self.redone.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some((clips, crop)) = self.undone.pop() else { return false };
        self.redone.push((std::mem::replace(&mut self.clips, clips), std::mem::replace(&mut self.crop, crop)));
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some((clips, crop)) = self.redone.pop() else { return false };
        self.undone.push((std::mem::replace(&mut self.clips, clips), std::mem::replace(&mut self.crop, crop)));
        true
    }

    /// Cut the clip under time t (edited video) in two there. The index of the second part, or None if t is too
    /// near a clip's edge.
    pub fn split(&mut self, t: f64) -> Option<usize> {
        let (i, at) = self.locate(t)?;
        let c = self.clips[i];
        if at - c.start < MIN_CLIP || c.end - at < MIN_CLIP {
            return None;
        }
        self.save();
        self.clips.splice(i..=i, [Clip::new(c.start, at), Clip::new(at, c.end)]);
        Some(i + 1)
    }

    /// Take clip i out (the last clip can't be).
    pub fn delete(&mut self, i: usize) -> bool {
        if self.clips.len() <= 1 || i >= self.clips.len() {
            return false;
        }
        self.save();
        self.clips.remove(i);
        true
    }

    /// Move clip i to position j.
    pub fn move_clip(&mut self, i: usize, j: usize) -> bool {
        if i == j || i >= self.clips.len() || j >= self.clips.len() {
            return false;
        }
        self.save();
        let c = self.clips.remove(i);
        self.clips.insert(j, c);
        true
    }

    /// Set clip i's start or end (recording time), within the recording and at least MIN_CLIP long.
    pub fn trim(&mut self, i: usize, start: Option<f64>, end: Option<f64>, save: bool) -> bool {
        let c = self.clips[i];
        let new_start = start.map_or(c.start, |s| s.max(0.0).min(c.end - MIN_CLIP));
        let new_end = end.map_or(c.end, |e| e.min(self.duration).max(new_start + MIN_CLIP));
        if (new_start, new_end) == (c.start, c.end) {
            return false;
        }
        if save {
            self.save();
        }
        self.clips[i] = Clip::new(new_start, new_end);
        true
    }

    pub fn clear_redo(&mut self) {
        self.redone.clear();
    }

    /// Mark an undo point before a change made a step at a time (dragging a clip's edge).
    pub fn begin_change(&mut self) {
        self.save();
    }

    /// Crop to rect (shares of the picture), or None for the whole picture.
    pub fn set_crop(&mut self, rect: Option<Crop>) -> bool {
        let rect = rect.and_then(|(x, y, w, h)| {
            let (x, y) = (x.max(0.0).min(0.95), y.max(0.0).min(0.95));
            let r = (x, y, w.min(1.0 - x), h.min(1.0 - y));
            if r.2 > 0.995 && r.3 > 0.995 { None } else { Some(r) }
        });
        if rect == self.crop {
            return false;
        }
        self.save();
        self.crop = rect;
        true
    }

    // --- keeping ---
    pub fn to_json(&self) -> String {
        json!({"duration": self.duration,
               "clips": self.clips.iter().map(|c| [c.start, c.end]).collect::<Vec<_>>(),
               "crop": self.crop.map(|(x, y, w, h)| vec![x, y, w, h])}).to_string()
    }

    pub fn from_json(text: &str, duration: f64) -> Edit {
        let parsed = (|| {
            let data: Value = serde_json::from_str(text).ok()?;
            let mut clips = vec![];
            for pair in data.get("clips")?.as_array()? {
                let (a, b) = (pair.get(0)?.as_f64()?, pair.get(1)?.as_f64()?);
                if 0.0 <= a && a < b && b <= duration + 0.5 {
                    clips.push(Clip::new(a, b));
                }
            }
            let crop = data.get("crop").and_then(|c| c.as_array()).and_then(|c| {
                Some((c.first()?.as_f64()?, c.get(1)?.as_f64()?, c.get(2)?.as_f64()?, c.get(3)?.as_f64()?))
            });
            (!clips.is_empty()).then(|| Edit::with(duration, clips, crop))
        })();
        parsed.unwrap_or_else(|| Edit::new(duration))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(e: &Edit) -> Vec<(f64, f64)> {
        e.clips.iter().map(|c| (c.start, c.end)).collect()
    }

    #[test]
    fn split_delete_move_and_undo() {
        let mut e = Edit::new(10.0);
        assert_eq!(e.split(4.0), Some(1));
        assert_eq!(pairs(&e), vec![(0.0, 4.0), (4.0, 10.0)]);
        assert_eq!(e.split(7.0), Some(2));
        assert!(e.delete(1) && e.length() == 7.0);
        assert_eq!(pairs(&e), vec![(0.0, 4.0), (7.0, 10.0)]);
        assert!(e.move_clip(1, 0) && e.clips[0].start == 7.0);
        assert_eq!(e.locate(1.0), Some((0, 8.0)));
        assert_eq!(e.locate(3.5), Some((1, 0.5)));
        assert!(e.undo() && e.clips[0].start == 0.0);
        assert!(e.undo() && e.clips.len() == 3);
        assert!(e.redo() && e.clips.len() == 2);
    }

    #[test]
    fn split_refuses_edges_and_last_clip_stays() {
        let mut e = Edit::new(10.0);
        assert_eq!(e.split(0.01), None);
        assert!(!e.delete(0));
    }

    #[test]
    fn trim_and_crop_limits() {
        let mut e = Edit::new(10.0);
        e.trim(0, Some(-5.0), Some(50.0), true);
        assert_eq!(pairs(&e), vec![(0.0, 10.0)]);
        e.trim(0, Some(9.99), None, true);
        assert!(e.clips[0].length() >= 0.1 - 1e-9);
        assert!(!e.set_crop(Some((0.0, 0.0, 1.0, 1.0))));
        assert!(e.set_crop(Some((0.1, 0.1, 0.5, 0.5))) && e.edited());
    }

    #[test]
    fn json_round_trip() {
        let mut e = Edit::new(10.0);
        e.split(5.0);
        e.set_crop(Some((0.1, 0.2, 0.3, 0.4)));
        let back = Edit::from_json(&e.to_json(), 10.0);
        assert_eq!(pairs(&back), vec![(0.0, 5.0), (5.0, 10.0)]);
        assert_eq!(back.crop, Some((0.1, 0.2, 0.3, 0.4)));
        assert_eq!(Edit::from_json("nonsense", 3.0).clips[0].end, 3.0);
    }
}
