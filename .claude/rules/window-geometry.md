---
paths:
  - "src-tauri/src/lib.rs"
  - "src/main.js"
---

# Window size, position and aspect lock

- Both `settings.json` and `window.json` live in `config_dir()/sview` (`CONFIG_DIR_NAME` in
  `lib.rs`), **not** in Tauri's `app_config_dir()` — the folder name is deliberately kept
  independent of the bundle identifier. "Reset to defaults" only touches `settings.json`.
  `window.json` holds size and position, and **both are restored in both size modes** — the
  window always reopens where and how it was left (the position is skipped when it lands on no
  connected monitor, otherwise clamped into that monitor's work area). In "image" mode the first
  `fit_window_to_image` after startup only corrects the aspect ratio, and anchors the window at
  the saved top-left (`PendingPosition`) instead of recentering.
  Logs go to a `logs/` subfolder of the same directory.
  `window.json` stores `x` / `y` / `width` / `height` in logical px plus `scale` (the window's
  scale factor when the position was taken). Positions are compared and set in each OS's
  desktop-wide coordinates (`PHYSICAL_DESKTOP`): **physical px on Windows** (logical px change
  scale per monitor there, so a mixed-DPI setup restored onto the wrong monitor), **points
  (logical px) on macOS** (physical px are per monitor there). `saved_position` / `desktop_area` /
  `desktop_to_physical` / `set_desktop_position` do the conversion; sizes stay logical (both OSes
  keep the logical size when a window moves to another scale). On macOS never `set_size` after
  `set_position`: `setContentSize` keeps the bottom-left, so the top-left moves.
- `windowSizeMode` is `"free"` or `"image"` (shown as "メディアに合わせる": it fits videos and audio too); `"fixed"` / `"flexible"` are the 0.1-era values and
  are still read — `LEGACY_VALUES` in `settings-defs.js` maps them for the frontend, and
  `fits_window_to_image()` accepts `"flexible"` on the Rust side (Rust reads `settings.json`
  raw, so it never sees the frontend's mapping).
- Window geometry is recorded per **slot** (`WindowSlot`): in `"free"` mode one slot for every kind
  (`Free` = the top level of `window.json`); in `"image"` mode one slot per kind (`Kind(..)` =
  `WindowState.kinds`, keyed `"image"` / `"video"` / `"audio"` / `"book"`, plus top-level `kind` = the
  last fit-mode kind, used when launched without a file). The two modes never share a record. There is no
  per-kind toggle (`windowPerKind` was removed and is in `OBSOLETE_KEYS`). `KindState` (`WindowKind`) has
  `shown`, `fit` (both from `set_window_kind(kind, fit)`) and `applied` (whose slot the window's *normal*
  geometry is); `target()` is the slot it should be in. `apply_window_kind` switches only while the window
  is normal: it stores the current geometry into `applied`, then places the target slot — `Free`: the
  top level (or keep the window if there is none); `Kind`: `kind_target` = its record, else the default size
  (`default_geometry`, centred on the current window) — never the previous kind's shape.
  `KindTarget::Adopt` (keep the window) is only when the window is not in a kind slot yet and `kinds` is
  still empty (pre-per-kind `window.json`). `place_kind_geometry` resets `AspectLock` to the placed area
  (so the `set_size` echo is not taken for a drag), re-arms `FirstFitDone` (the next fit clamps to the
  screen like at launch) and `PendingPosition`, and cascades off other viewers. While maximized /
  fullscreen / minimized the switch is deferred (`KindState::pending()`): `AspectLock` is cleared and
  `set_aspect_lock` refuses a ratio, so the OS restoring the old slot's rect is not "corrected"; the
  frontend retries in `onResizeSettled` and on focus (`catchUpWindowKind*`), and `fit_window_to_image`
  calls `apply_window_kind` first. Saving is **immediate**: `onResized` / `onMoved` arm
  `scheduleWindowSave` (`WINDOW_SAVE_DELAY_MS`, waits while `mouse_button_down`) →
  `save_window_geometry` → `save_window_state`, which writes the current geometry into `applied`
  (`WindowState::store`; skipped while maximized / fullscreen / minimized) via a temp file + rename; close
  still saves too. The frontend sends nothing before `settingsReady` (the defaults say "free", which
  would move a fit-mode window to the free slot). At launch `setup()` reads the mode and builds the
  startup path's listing once (`prepare_startup_listing` → `list_path`, the body of `list_images`) to get
  its kind, and `restore_window_state` places that slot **before** the window is shown;
  `create_viewer_window` (macOS) does the same for the file it opens. The listing (or its error) is
  parked in `StartupListing` and handed over by the frontend's first `list_images` for that path
  (`take_startup_listing`; any call empties it), so the folder / archive is read only once — don't add a
  second read to learn the kind.
- In `"image"` mode only the **aspect ratio** comes from the image; the size does not.
  `sized_to_aspect` turns an area (logical px²) plus the aspect into a size, so paging between
  portrait and landscape keeps the window equally big. That area lives in `AspectLock` on the
  Rust side and is rewritten **only** by a real resize, never recomputed from the live window —
  otherwise the one clamp that does exist would shrink the window for good.
- The "fit on screen" clamp (`screen_limit`, `SCREEN_RATIO` = 95% of the work area) runs **once
  per launch and once per kind switch**: `FirstFitDone` is consumed by the first `fit_window_to_image`
  (and re-armed by `place_kind_geometry`), so a restored window still lands on screen when the
  monitor setup changed, and nothing fights the user's own size afterwards. That first fit also writes its (possibly clamped) area into `AspectLock` —
  otherwise the second image would jump back to the unclamped area and the window would grow
  once (easy to hit on small logical screens such as MacBooks, with a portrait image). A drag (`keep_aspect_on_resize`) is never clamped, so a tall image can end up
  sized past the bottom of the screen — that is deliberate.
- The aspect lock is enforced in `WindowEvent::Resized` (`keep_aspect_on_resize`), which also
  arrives mid-drag, so the window can only be dragged along the image's ratio. Tauri exposes no
  native aspect hint, so on Windows `win_sizing` subclasses the main window and rewrites the
  proposed rect in `WM_SIZING` (it knows the edge / corner being pulled; corners take the larger of
  the width- and height-driven sizes, `sizing_area`, which is continuous in the mouse position) and
  writes `last` / `reported` / `area` into `AspectLock`, so the following `Resized` is an echo.
  Without it a corner drag flickered: each mouse move painted the OS size and then our correction,
  and the "larger relative change" axis choice in `dragged_area` flipped between events.
  On macOS `mac_sizing` does the same by adding `windowWillResize:toSize:` and
  `windowWillStartLiveResize:` to tao's window-delegate class at runtime (then re-setting the delegate so
  NSWindow re-checks what it responds to). The pulled edges are the edges the cursor was near when the
  live resize started (`LiveResize.edges`), counted only once that axis actually moves from the grab size
  (`pulled`, sticky for the drag), then `sizing_area`. AppKit also calls `windowWillResize:toSize:` while
  entering / leaving fullscreen, so `will_resize` passes the size through when the style mask has `FullScreen`,
  and it uses `try_lock` (like `win_sizing`): it can be called from inside a window-state query on the main
  thread. For the same reason `keep_aspect_on_resize` queries the window state (fullscreen first) **before**
  taking `AspectLock`. `keep_aspect_on_resize` returns early while the
  window is `inLiveResize`: tao's `set_size` is `setContentSize` dispatched **async**, so correcting from
  `Resized` mid-drag painted the OS size and our size alternately (flicker) and, at corners, left the
  ratio broken. Outside a live resize it is still a correct-it-as-it-arrives loop. Two details keep it from misbehaving: write
  `AspectLock.last` **before** calling `set_size` (on Windows the event can come back
  synchronously, and a size equal to `last` is how the echo is recognised), and **drop the
  mutex guard before** `set_size` — holding it across that call deadlocks on the re-entrant
  event. The frontend only sets the ratio (`set_aspect_lock`, from `syncAspectLock`) and leaves
  the geometry alone.
- While a drag is in flight the image keeps its pixel size (`#image.frozen` plus an inline
  width/height captured on the first resize event) and is re-fitted only once `onResized` goes
  quiet — re-laying out the image on every frame is what made a drag feel heavy. `setFitMode`
  and `enterZoomMode` both thaw it, the latter because a zoom factor derived from the frozen
  size would apply twice. This applies in both size modes. A pause mid-drag also makes `onResized`
  go quiet, so `onResizeSettled` asks Rust `mouse_button_down` (`GetAsyncKeyState` /
  `NSEvent.pressedMouseButtons`; the OS owns the mouse during a resize, so no `mouseup` reaches the
  WebView) and keeps waiting while a button is held — thawing at the pause is what made a diagonal
  drag flicker. A resize we caused ourselves (`selfResizedAt`, `SELF_RESIZE_MS`) is
  not frozen, so paging images does not leave the picture a step behind the window (maximize,
  fullscreen and `set_window_kind` stamp it too).
- Which edge is being dragged is decided against `AspectLock.reported` — the size the OS last
  announced — **never** against the size we applied. A drag keeps reporting from the rect the
  window had when it was grabbed, so it re-sends the other axis unchanged; measuring against our
  own correction instead makes the axis flip every other event and the window snap back to where
  the drag started (it looks like "it shrinks but won't grow"). `syncAspectLock` on the
  resize-settled timer puts the baseline back on the real size once a drag ends.
- Maximizing and fullscreen interact with the window-size logic: `fit_window_to_image` and
  `keep_aspect_on_resize` both return early in those states (and while minimized), since
  resizing would silently drop out of them, and `save_window_state` skips those windows too so
  `window.json` keeps the ordinary geometry.
- `fit_window_to_image` does its position math in **physical** pixels and on the **outer**
  size. On Windows a frameless window's outer rect is larger than its client rect by the
  invisible shadow border, so mixing outer and inner sizes drifts the window a few pixels
  toward the bottom-right on every image. The result is clamped into the monitor's work area.
