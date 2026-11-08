#!/bin/sh
# Builds Explain on video (in a podman container: SteamOS needs no compiler) and adds it to the Start menu.
# The program is one file, ~/.local/bin/explainonvideo; it uses GTK 3, GStreamer, PipeWire and ffmpeg, which
# SteamOS already has.
set -e
DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/target/release/explainonvideo"
if [ ! -x "$BIN" ] || [ -n "$(find "$DIR/src" "$DIR/Cargo.toml" -newer "$BIN" 2>/dev/null)" ]; then
  if command -v cargo >/dev/null && command -v cc >/dev/null && command -v pkg-config >/dev/null; then
    (cd "$DIR" && cargo build --release)
  else
    "$DIR/build/cargo.sh" build --release
  fi
fi
BINDIR="$HOME/.local/bin"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
mkdir -p "$BINDIR" "$ICONS" "$APPS"
install -m 755 "$BIN" "$BINDIR/explainonvideo"
"$BINDIR/explainonvideo" --save-icon "$ICONS/explainonvideo.png"
cat > "$APPS/explainonvideo.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Explain on video
GenericName=Screen recorder
Comment=Record an area of the screen, with sound and your camera, then trim it
Exec=$BINDIR/explainonvideo
Icon=explainonvideo
Terminal=false
Categories=AudioVideo;Video;Recorder;
StartupWMClass=explainonvideo
# (KWin lets the program stream single windows by themselves, so only the chosen windows are recorded)
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1
DESKTOP
cat > "$APPS/explainonvideo-editor.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Explain on video editor
Comment=Trim, cut, reorder and crop a recording
Exec=$BINDIR/explainonvideo --edit %f
Icon=explainonvideo
Terminal=false
MimeType=video/mp4;video/x-matroska;
Categories=AudioVideo;Video;
NoDisplay=true
X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1
DESKTOP
command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" 2>/dev/null || true
echo "Installed $BINDIR/explainonvideo and added it to the Start menu: Explain on video"
