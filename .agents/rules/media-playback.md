---
trigger: glob
globs: "src/main.js, src/mkv-player.js, src/viewer/*.js, src/index.html, src/style.css, src-tauri/src/audio.rs, src-tauri/src/media.rs, src-tauri/src/mkv/*.rs, src-tauri/src/lib.rs"
---

# Video, audio and book mode

- Videos play in a `<video>` element with whatever decoder the OS WebView has (WebView2 / WKWebView). **No codec is bundled** — that keeps codec patent licensing out of the
  app — so `VIDEO_EXTS` is limited to containers the WebViews handle (`mp4` / `m4v` / `webm` /
  `mov` / `mkv` — WebView2 plays MKV natively, WKWebView doesn't, see the MKV remux note below);
  anything the WebView can't decode just shows an error. Videos are listed only from
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
  Audio tracks are switched only from the context menu (「音声」, read live from `video.audioTracks` when the
  menu opens; `selectAudioTrack` enables just the chosen one) — there is deliberately no control on the card.
  WebView2 exposes `audioTracks` only behind the experimental Blink feature `AudioVideoTracks`, switched on
  through `WEBVIEW2_BROWSER_ARGS` (see `platform.md`); where it is absent the item is greyed out.
  MKV / AV1 on macOS (`usesMkvPlayer` for `.mkv`, `video_needs_stream` for `.mp4`/`.m4v`/`.mov`/`.webm` when HW AV1 decode is absent): WKWebView can't demux MKV and M1/M2 Macs lack HW AV1 decode, so `MkvPlayer` (`src/mkv-player.js`) plays them through MSE. Rust (`MkvStream`, commands `video_needs_stream` / `mkv_open` / `mkv_init` / `mkv_seek` / `mkv_read` / `mkv_close`, one stream per window in `MkvStreams`, dropped in `forget_window`) handles:
  - Remuxing MKV (H.264 / HEVC, and AV1 on M3+ where `VTIsHardwareDecodeSupported('av01')` is true) to fragmented MP4 without re-encoding (`avcC` / `hvcC` / `av1C`; HEVC as `hvc1`, `hev1` fails in WebKit), audio AAC / Opus (one track per stream; switching audio re-opens at the current time via `switchMkvAudio`).
  - Transcoding AV1 (MP4 / M4V / MOV via `src-tauri/src/mkv/mp4.rs`, WebM / MKV via Matroska) on macOS machines without HW AV1 decode (or when `MkvPlayer` falls back via `canTranscode` + `transcode: true`): `rav1d` software decode (`n_threads` clamped to 2..4, `max_frame_delay = 2`, arm64 NEON/dotprod/i8mm asm without SVE2; macOS-only in shipped builds so Windows stays lean) directly into NV12 `CVPixelBuffer` (`src-tauri/src/mkv/vt.rs`), encoded in real time by VideoToolbox H.264 with `AllowFrameReordering = false` (no B-frames → PTS == DTS, no A/V drift), `AllowOpenGOP = false`, and `ForceKeyFrame = true` at every ~1 s fragment start (so long AV1 GOPs still start and seek quickly; seek skips encoding decoded frames before the seek target and aligns audio to the first output frame). MP4 `elst` priming delay / empty edit offsets are applied to keep A/V sync. When `transcoded: true`, `MkvPlayer` uses a shorter buffer window (`TRANSCODE_AHEAD_S = 6`, `TRANSCODE_BEHIND_S = 6`) to save CPU and battery.
  Bytes go back as `tauri::ipc::Response` (raw, never JSON arrays).
  `matroska-demuxer` only reads track info (supports both `matroska` and `webm`); Cluster blocks are walked by our own `MkvWalker` (lacing,
  BlockGroup keyframes via ReferenceBlock) and Cues by `read_mkv_layout` (the crate keeps Cues private and its
  `seek` stops at any frame, not a keyframe); without Cues, `scan_cluster_index` builds a cluster index on
  the first seek. For remuxed MKV, fragments are cut only at video keyframes (≥ `REMUX_FRAGMENT_SECONDS`), because DTS is
  derived per fragment: MKV has PTS only, so `video_decode_times` assigns the fragment's sorted PTS to the
  frames in decode order minus the reorder depth (`reorder`, probed from the first GOPs at open) so DTS ≤
  PTS; at the file start the negative DTS are squeezed into [0, first positive DTS). `trun` is version 1
  (signed cts) in case a later GOP reorders deeper than probed. Audio timestamps subtract CodecDelay (or MP4 `elst` media_time), frames
  before 0 are dropped, and `audio_next` keeps fragment boundaries gapless (an overlap makes WebKit drop a
  frame). The JS side runs one loop (`run`) so `mkv_read` / `mkv_seek` never overlap: it keeps
  `aheadSeconds()` buffered ahead, `remove()`s everything older than `behindSeconds()`, and on `seeking` outside
  the last buffered range clears the buffer and calls `mkv_seek` (previous keyframe). `nudgeGap` jumps small
  gaps at the start of a range (WebKit stalls when currentTime sits just before the first frame).
  `MediaSource.isTypeSupported` decides the audio (and triggers `transcode: true` fallback for AV1 if needed): unsupported tracks are `disabled` in the 「音声」 menu, and
  an unplayable default track falls back to another one or to no audio with a notice. CSP `media-src` needs
  `blob:` for the MediaSource URL.
  MKV text subtitles: Chromium never exposes in-band MKV subtitles (and the macOS remux drops them), so
  `video_subtitles` (Rust, async, MKV only) returns SRT / ASS tracks with their cues. Track info and
  ContentEncoding (zlib / header stripping; encrypted tracks are skipped) come from `matroska-demuxer`, but the
  blocks are walked by our own `scan_subtitle_blocks` from the first Cluster: the crate's `next_frame` reads every
  video / audio frame, i.e. the whole file, while the scan `seek_relative`s past other tracks' blocks. ASS is
  reduced to plain text (`clean_ass_text`: override tags dropped, `\p` drawings skipped, then HTML-escaped);
  SRT keeps `<i>`/`<b>`/`<u>`. `main.js` keeps **one** `addTextTrack` track (`subtitleTextTrack`) for good —
  tracks added that way can't be removed — and swaps its cues in `selectSubtitle` (set `hidden` first: `cues`
  is null while `disabled`); each cue gets `line = SUBTITLE_LINE` (-2) so it sits one line above the video's
  bottom edge instead of on it. Subtitles start **off** on every video (the track's default flag is ignored).
  Subtitles load per `show()` (`loadSubtitles`, dropped on a stale `showToken`) and are
  cleared in `unloadVideo`. The context menu shows 「音声」/「字幕」 (in that order) only for videos, as items with a `submenu`
  (`.ctx-submenu`, opened on hover by `openSubmenu`, placed inside `#ctxmenu` so the outside-click check still
  works); they are disabled when there is nothing to pick (no subtitles, < 2 audio tracks).
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
  the user-facing volume). The volume and mute state are stored per kind (`videoVolume` / `audioVolume`,
  `videoMuted` / `audioMuted`, picked by `volumeKey()` / `muteKey()`); all are `HIDDEN_DEFAULTS` in
  `settings-defs.js` — stored in `settings.json` but not shown in the settings window, and kept by
  "Reset to defaults". `playVideo()` never mutes or lowers volume on its own. `#audio-art` has fixed-px padding (left / right / top) that `mediaSize()`
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
