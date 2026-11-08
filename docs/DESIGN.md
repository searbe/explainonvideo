# Design

Explain on video uses the design system of Dictate (another of my apps) so the two look like
one family: the same tokens, type, spacing, radii, HUD cards and icons (`src/design.rs`, `src/style.css`, adapted
from Dictate's). Additions:

- **The controls capsule** (`.capsule`): a HUD-styled pill, 30 px controls, full radius. Record is the view's one
  primary (accent) button; Stop is filled `record` red ("the stop"); toggles are icon buttons, accent when on, struck
  through (`-off` icons) when off; the microphone's icon carries a small accent level bar while it listens.
- **The area outline**: accent while ready, `record` red while recording, `text-3` while paused, drawn just outside
  the area with corner marks and a tab (grip, size, what's recorded).
- **The selection layer**: the screen dimmed (black 45 %), what's recorded clear; chosen windows outlined in accent
  with a tick badge; the window under the pointer tinted accent 10 % with its name in a chip.
- **New icons**: camera, speaker, screen, window, area, preview, scissors, crop, export, redo, more, grip, film,
  live, left, right, split; any icon name with `-off` is drawn struck through.
- **The app icon**: Dictate's charcoal square with a white screen and play mark, and the red dot.
