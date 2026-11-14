---
paths:
  - "src/main.js"
  - "src/index.html"
  - "src/style.css"
  - "src-tauri/src/lib.rs"
---

# Video, audio and book mode

- Videos play in a `<video>` element with whatever decoder the OS WebView has (WebView2 / WKWebView). **No codec is bundled** — that keeps codec patent licensing out of the
  app — so `VIDEO_EXTS` is limited to containers most WebViews handle (`mp4` / `m4v` / `webm` /
  `mov`); anything the WebView can't decode just shows an error. Videos are listed only from
  folders / single files (`is_media`), never from archives (`read_archive_entry` loads a whole entry into memory).
  They load through the asset protocol, so the CSP needs `media-src` alongside `img-src`.
  `unloadVideo()` (pause + drop `src` + `load()`) runs before every `show()` and before
  `delete_image` so the WebView lets go of the file. Videos are always shown fitted (no zoom);
  `mediaSize()` / `mediaEl()` stand in for `img.naturalWidth` etc. in the window-fit and freeze
  logic. While a video is shown, `handleVideoKey` takes Space / K (play-pause), M (mute),
  ← / → and J / L (±5 s) and ↑ / ↓ (volume ±5 %, saved after a 500 ms pause), and swallows the
  image paging keys (PageUp / PageDown / Backspace / Home / End): media never changes file by key —
  only the `#vb-prev` / `#vb-next` buttons in `#videobar` and the mouse side buttons (`step(±1)` for every
  kind) do. The wheel seeks ±3 s (`WHEEL_SEEK_S`) and right button + wheel or Ctrl + wheel changes the
  volume (`wheelVolume`). Horizontal wheel input (Shift + wheel, tilt wheel, a trackpad's
  left / right) is treated like vertical (right = down = next): `wheelDelta` picks the larger axis on the
  first event of a gesture (`WHEEL_AXIS_GAP_MS`) and keeps it until the gesture ends, so a diagonal swipe
  cannot fire next and previous in one go. No other trackpad-specific handling. For images ← / → and the wheel page (`wheelStep`:
  wheel down = next; the first event after `WHEEL_GESTURE_GAP_MS` steps at once, then every
  threshold), the wheel zooms only while the right button is held (`rightPress`) or with Ctrl
  (both OSes; a Windows touchpad pinch also arrives as Ctrl + wheel), and ↑ / ↓ are deliberately unbound (reserved for a future feature)
  except in book mode. `contextmenu` arrives after mouseup on Windows but on mousedown on macOS, so a
  right press that used the wheel or lasted `MENU_LONG_PRESS_MS` opens no menu: on macOS the
  menu position is held in `rightPress.menuAt` and opened on mouseup, on Windows `suppressMenu`
  drops the following `contextmenu`. Images get their own card, `#imagebar` (`‹` / `›` + a slider
  over the list, `ib-*`), at the same spot as `#videobar` (both `.ctlbar`); it shows when the app
  is neither `.video` / `.audio` nor `.no-image`. Slider moves go through `seekImageTo`, which
  updates the chrome at once but throttles `show()` (`IMAGE_SEEK_INTERVAL_MS`) so an archive is not
  read per pixel; in book mode `imageSeekTarget` keeps the spread's parity. `setFitMode` leaves `#stage` without
  `data-tauri-drag-region` while a video is shown — with it, the OS move loop starts on mousedown and
  the release never reaches the WebView, so a click could not be told apart. Instead a press that moves
  `VIDEO_DRAG_THRESHOLD_PX` calls `appWindow.startDragging()`, and one released within
  `VIDEO_CLICK_MS` without moving toggles play-pause. The playback controls are the `#videobar` card (seek bar on top; time /
  ±5 s + play / mute + volume below) inside `#chrome`, so they show and hide with the rest of the chrome.
- Audio (`AUDIO_EXTS` in `lib.rs` / `AUDIO_EXT_FILTER` in `main.js`, kept in sync by the same test;
  never listed from archives) plays through the **same `<video>` element**,
  which stays hidden while `#audio` shows the artwork and the title panel. `showingMedia`
  (video or audio) gates everything playback-related — keys, the press-to-play logic, `#videobar`,
  no zoom — while `showingVideo` / `showingAudio` pick what is displayed. Tags and artwork come from
  `audio_info` / `audio_artwork` (`lofty`, tags only, no audio properties); artwork falls back to a
  `cover` / `folder` / … image in the same folder (`COVER_STEMS`), then to `DEFAULT_ARTWORK`
  (`src/audio-artwork.svg`). In "image" mode `mediaSize()` returns the artwork size plus
  `extraWidth` / `extraHeight` — the fixed-px `#audio-art` padding and `#audio-panel`
  (`--audio-panel-h`) — which `set_aspect_lock` / `fit_window_to_image` store as
  `AspectState.extra`; `sized_to_aspect` / `dragged_area` keep the ratio on the window **minus**
  that margin (images / videos pass 0). Don't fold the margin into the ratio at the current
  window width: the ratio then depends on the previous window shape and the size drifts. `audioEnd` / `videoEnd` (`mediaEnd()`:
  `"next"` / `"repeat"` / `"stop"`; `videoEnd` replaced the old `videoLoop` toggle, migrated in
  `normalizeSettings`) — `"next"` advances on `ended` to the next entry of the same kind
  (`nextMediaIndex`; never wraps — paging stops at both ends, there is no wrap-around setting) and forces playback via `continuePlayback`. Audio is played at `AUDIO_VOLUME_GAIN` (0.5) of the
  slider value (`getVolume` / `setVolume` wrap `video.volume`; never read or write it directly for
  the user-facing volume). The volume is stored per kind (`videoVolume` / `audioVolume`, picked by
  `volumeKey()`); both are `HIDDEN_DEFAULTS` in `settings-defs.js` — stored in `settings.json` but not
  shown in the settings window, and kept by "Reset to defaults". Mute is never saved: every launch
  starts unmuted. `#audio-art` has fixed-px padding (left / right / top) that `mediaSize()`
  reads via `getComputedStyle`. `layoutArtwork()` sizes `#artwork` to the drawn picture (instead
  of `object-fit` on a full box) so its `border-radius` (`--window-radius`, same as `#app`)
  rounds the picture itself.
- Book mode (`bookMode`, `B` / context menu; `docs/specs/book.md`) shows images two at a time in
  `#book` (`#book-left` / `#book-right`) instead of `#image`. `showingBook` = `bookMode && !showingMedia`;
  the spread is `images[index]` + `images[index + 1]` (`shownEntries()`), and `settings.bookBinding`
  (`HIDDEN_DEFAULTS`, `"left"` / `"right"`) only decides which `<img>` gets the earlier page — ← is always
  previous and → next. `step()` moves by 2 (`stepBook(±2)`), ↑ / ↓ by 1 (stops at `lastSpreadIndex()`).
  `layoutBook()` sizes `#book` in px (pages share the taller page's height, `bookSize()`); zoom / pan /
  freeze treat the whole spread as one picture via `zoomEl()` / `zoomNaturalSize()` / `mediaEl()`.
  For the window it is its own kind: `shownKind()` returns `"book"` and Rust stores it under
  `WindowKindKey::Book` (`kinds.book` in `window.json`), separate from `MediaKind`, which stays the
  list kind. `bookMode` is not saved and is cleared when a non-image list opens; deleting is disabled
  in book mode.
