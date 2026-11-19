//! The virtual camera: the video offered to other apps as a camera (a PipeWire camera: browsers, OBS, …).
//!
//! It's a small pipeline of its own, fed by the engine through an inter channel (engine::set_virtual_camera), so it
//! outlives the engine: when the engine restarts (another size, frame rate or background), the camera stays, holding
//! its last picture meanwhile (black after a few seconds without one), and a call using it carries on. Its size is
//! fixed when it starts; a picture of another shape later is fitted inside it, with borders, since apps don't take a
//! camera changing size well.
use gstreamer as gst;
use gst::prelude::*;

/// The inter channel between the engine and the camera.
pub const CHANNEL: &str = "explainonvideo-vcam";

pub struct VirtualCamera {
    pipe: gst::Pipeline,
    _watch: gst::bus::BusWatchGuard,
}

impl VirtualCamera {
    /// A w × h camera at fps. on_error: it stopped working (why).
    pub fn new(w: i32, h: i32, fps: i64, on_error: impl Fn(String) + 'static) -> Result<VirtualCamera, String> {
        // (YUY2: one plane. Browsers' camera code (WebRTC's) reads only a frame's first plane, so I420's three were
        // all dropped as too short. use-bufferpool=false: with its pool lent upstream, pipewiresink took every
        // converted frame and passed on none (GStreamer 1.28, PipeWire 1.6): it copies them instead)
        let desc = format!("intervideosrc channel={CHANNEL} timeout=5000000000 \
                            ! queue max-size-buffers=2 leaky=downstream ! videoscale add-borders=true ! videoconvert \
                            ! video/x-raw,format=YUY2,width={w},height={h},pixel-aspect-ratio=1/1,framerate={fps}/1 \
                            ! pipewiresink name=sink mode=provide sync=false use-bufferpool=false \
                            client-name=explainonvideo");
        let pipe = gst::parse_launch(&desc).map_err(|e| e.to_string())?.downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline".to_string())?;
        // (set here, not in the description: GStreamer 1.28's parser drops the escapes around the spaces in the
        // name, and the whole property then fails to parse)
        let props = gst::Structure::builder("props").field("media.class", "Video/Source")
            .field("node.name", "explainonvideo").field("node.description", "Explain on video")
            .field("media.role", "Camera").build();
        pipe.by_name("sink").unwrap().set_property("stream-properties", &props);
        let watch = pipe.bus().unwrap().add_watch_local(move |_, msg| {
            if let gst::MessageView::Error(err) = msg.view() {
                crate::warn!("virtual camera: {} ({:?})", err.error(), err.debug());
                on_error(err.error().to_string());
            }
            glib::ControlFlow::Continue
        }).map_err(|e| e.to_string())?;
        pipe.set_state(gst::State::Playing).map_err(|e| e.to_string())?;
        Ok(VirtualCamera { pipe, _watch: watch })
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        let _ = self.pipe.set_state(gst::State::Null);
    }
}
