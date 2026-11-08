# Explain on video

A screen recorder for SteamOS and KDE Plasma, made for short videos that show how something works.

Most screen recorders capture a rectangle of your screen, so whatever drifts into that rectangle ends up in the
video. This one asks the compositor for **each window by itself**. Pick two windows and the video shows those two
windows, even with something else on top of them, and it follows them as they move.

It's one program of under 2 MB, written in Rust, using what SteamOS already ships: GTK 3, GStreamer, PipeWire, ffmpeg
and KWin.

## What it does

- **Records just the windows you choose.** Each comes from its own KWin stream, with its menus and dialogs. Windows
  that are far apart are placed close together, so the video isn't mostly empty space.
- **Changes while it records.** Add or remove windows, or drag the outline to resize the area, without stopping.
- **No sharing dialog.** It talks to KWin directly, so there's no "choose what to share" prompt.
- **Sound and camera.** The computer's sound, your microphone and a camera bubble, each switchable mid-recording. Drag
  the bubble around in the live preview.
- **Works as a camera.** Offers the picture to apps that take PipeWire cameras, such as Chrome, Firefox and OBS.
- **Trims what you made.** A built-in editor splits, deletes, reorders and crops, and never touches the original.
- **Saves MP4** (H.264 and AAC), encoded on the graphics card when it can.

## Install

```sh
git clone https://github.com/searbe/explainonvideo.git
cd explainonvideo
./setup.sh
```

Then start **Explain on video** from the Start menu. Starting it that way matters: KWin only offers its screen-cast
protocol to programs whose `.desktop` entry asks for it, and `setup.sh` writes that entry.

SteamOS has no C compiler, so `setup.sh` builds inside a podman container (about 1.5 GB to download the first time).
The program it produces links to the system's own libraries and is installed to `~/.local/bin`.

## Using it

The selection layer opens when the app starts.

| To record | Do this |
|---|---|
| An area | Drag a rectangle |
| One or more windows | Click them; click again to take one out |
| Only some windows inside an area | Draw the area, then click the windows |
| The whole screen | Choose **Screen** |

**Enter** or Record starts after a 3-second countdown, **Done** keeps the choice for later, and **Esc** cancels.

The controls float at the bottom of the screen and are never in the video. The preview sits on top of them and moves
with them.

While recording:

- **Windows…** beside the red outline adds or removes windows. Each click applies at once; Esc undoes it.
- **Drag the outline** to resize or move the area. If that detaches it from a window, **Follow** reattaches it.
- The video keeps its size, so a smaller area is enlarged to fill it (up to 2× by default) and a larger one is scaled
  down.

Recordings go to `~/Videos/Explain on video/`. After saving, **Edit…** opens the editor: **S** splits, **Del** deletes,
**C** crops, **Space** plays, and clips can be dragged to reorder or trim. Export writes a new file beside the
original.

## How it works

One GStreamer pipeline (`src/engine.rs`) composes the picture. With windows chosen, KWin's private
`zkde_screencast_unstable_v1` protocol (`src/kwin_cast.rs`) gives a PipeWire stream per window, and they're composed
in KWin's stacking order onto a canvas that replaces the screen. A small script loaded into KWin (`src/kwin_watch.js`)
reports where every window is and keeps the app's own windows out of recordings. Raw video and sound are piped to
ffmpeg (`src/export.rs`) in parts, one per stretch between pauses, and joined without re-encoding.

If KWin won't provide streams, it falls back to the desktop's screen-cast portal and to covering whatever isn't
chosen.

## Limits

- One screen at a time.
- Menus of a followed window are cut off at the window's edge, because the video doesn't grow for them.
- Apps that only read `/dev/video*` (the Zoom and Discord desktop apps) can't use the virtual camera. That needs
  v4l2loopback, which SteamOS doesn't include.
- Developed on a standard-DPI screen; on a scaled one the floating controls may sit slightly off.

## Development

`build/cargo.sh build --release` and `build/cargo.sh test` run cargo in the build container. End-to-end checks need a
desktop session:

| | |
|---|---|
| `explainonvideo --engine-smoke DIR` | Records a test pattern while changing the area and camera |
| `explainonvideo --ui-drive DIR` | Walks the whole app, screenshotting each step |
| `explainonvideo --window-drive TITLE DIR` | Records a real window from its own stream |
| `explainonvideo --try-preview DIR [TITLE]` | Real screen: preview, then adding a window mid-recording |
| `explainonvideo --resize-test` | A window that toggles full screen, to check nothing stretches |

Logs are in `~/.cache/explainonvideo/log.txt`.

## License

MIT. `src/protocols/screencast.xml` is KDE's, under LGPL-2.1-or-later.
