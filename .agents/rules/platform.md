---
trigger: glob
globs: "src-tauri/src/lib.rs, src-tauri/src/platform/*.rs, src-tauri/src/logging.rs, src-tauri/src/window.rs, src/main.js, src-tauri/tauri.conf.json, src-tauri/installer-hooks.nsh, src-tauri/capabilities/*.json"
---

# Platform specifics (macOS / Windows), windows, bundling

- The windows in `tauri.conf.json` (`main` / `settings` / `shortcuts` / `info`) are `"create": false`; `create_configured_windows()` builds
  them from the same config in `setup()`. That detour exists only so `data_directory()` can be set:
  on Windows an unspecified WebView2 user-data folder is created next to the exe
  (`C:\Program Files\sView\sview.exe.WebView2`) and the app fails to start. It is pointed at
  `%LOCALAPPDATA%\sview` (`webview_data_dir()`). macOS (WKWebView) cannot set it — the OS derives
  `~/Library/WebKit/<bundle id>` and `~/Library/Caches/<bundle id>` from the identifier — so it
  returns `None` there. All window builders go through `webview_options()`, which sets that folder and,
  on Windows, `additional_browser_args(WEBVIEW2_BROWSER_ARGS)`: wry's defaults
  `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection` and
  `--autoplay-policy=no-user-gesture-required` (passing args replaces them, so both are
  copied) plus `--enable-blink-features=AudioVideoTracks` for audio-track switching. WebView2 instances on
  the same data folder must share identical args, so never give one window different args.
- The `info` window (file information) is hidden on close like `settings` / `shortcuts`. The file it shows
  lives in Rust (`InfoTargetState` in `info/mod.rs`): `open_info_window` sets it and emits `info-target` to
  the window (it also re-emits for the same file so reopening re-reads), `sync_info_window` (called by
  `main.js` from `show()` / `clearView()` / focus) only replaces it while the window is visible **and** the
  calling viewer is focused, so with several macOS viewers the front one wins. `info.js` also pulls it once
  with `get_info_target`, because the event can fire before the page has loaded. Info gathering reuses the
  MP4 box helpers in `mkv/mp4.rs` (made `pub(crate)` for this) and reads headers only.
- `tauri-plugin-log` is registered from `setup()` via `app.handle().plugin(...)`, not on the
  `Builder`, because the output folder needs an `AppHandle` (`log_dir()` → `config_dir()/logs`).
  A panic hook installed at the top of `run()` logs the panic and a backtrace before aborting,
  and a failed `Builder::build()` shows an OS message box instead of panicking.
- macOS: WKWebView only tracks the mouse while its window is key, so an inactive window gets no
  `mousemove` and leaving the window fires no reliable `mouseleave`. `mac_pointer` (in `lib.rs`)
  adds an always-active `NSTrackingArea` to the main window's content view and emits
  `pointer-inside` (bool) and, only while the window is not key, a throttled `pointer-moved`;
  `main.js` feeds both into `showChrome` / `hideChrome`. The window controls are drawn as
  traffic lights in CSS (`#app.mac`), greyed out via `#app.inactive`; the settings window's close
  button does the same (`#win.mac` / `#win.inactive` in `settings.css`).
- macOS: the main window has `acceptFirstMouse: true` (`tauri.conf.json`). Without it AppKit swallows the
  first click on an inactive window (it only activates it), so `data-tauri-drag-region` could not start a
  move until the window had been clicked once.
  Window moves on macOS go through `start_window_drag` (`mac_drag`), not Tauri's `startDragging()`: tao's
  `drag_window` builds a fake mouse-down with **screen** coordinates as the window location whenever
  `NSApp.currentEvent` isn't a mouse event (the case right after activation), so the window's top-left
  jumped to the cursor. `main.js` catches drag-region presses in a capture listener (`isDragRegion`, a copy
  of Tauri's `drag.js` check) and leaves double clicks to Tauri; the video press-and-move path calls
  `startWindowDrag()` too.
- macOS: viewer windows (`setup_viewer_window`) call `window.set_shadow(false)` before `show()`.
  With `transparent: true` + `decorations: false` + `hasShadow = true`, macOS WindowServer paints
  a full-strength dark outline and drop shadow around the window's alpha silhouette (even at low
  `backgroundOpacity`, and with rough corner-radius edges). Opaque auxiliary windows (`settings`,
  `shortcuts`, `info`) keep the default OS shadow.
- macOS: a file opened from Finder/Dock arrives via `RunEvent::Opened`, not argv. `open_in_viewer` opens it
  in a viewer window that has shown nothing yet (`KindState.shown` is `None` and not `claimed` — e.g. `main`
  right after launch), otherwise in a **new** viewer window (`create_viewer_window`: label `viewer-N`, built
  from `main`'s config, placed at the opened file's kind geometry and moved `CASCADE_OFFSET` off any viewer
  sitting at the same spot). Every per-window Rust state
  (`AspectLock`, `WindowKind`, `PendingPosition`, `FolderWatcher`, `StartupFile`, `StartupListing`) is a
  `PerWindow<T>` map keyed by window label and dropped in `forget_window` on close; closing the last viewer
  (`is_viewer`) exits. Window-targeted events (`folder-changed`, `pointer-*`, `open-file`) go out with
  `emit_to(label)` and `main.js` must receive them with `appWindow.listen`, since the global `listen` also
  catches events aimed at other windows. The same goes for Tauri's own `tauri://drag-enter` / `drag-leave` /
  `drag-drop`: with the global `listen`, a file dropped on one window opened in every window. `capabilities/default.json` covers `viewer-*`. `ArchiveCache` stays
  app-wide (it re-opens on a path change). Windows has no in-process multi-window (each file launch is its
  own process).
  `macOSPrivateApi` is enabled and builds are ad-hoc signed only.
- The Windows installer is `installMode: "perMachine"`: NSIS gets `RequestExecutionLevel admin`, so it
  asks for UAC on launch and defaults to `C:\Program Files\sView` (HKLM, all-users shortcuts and
  file associations). It used to be per-user (`%LOCALAPPDATA%`), so `installer-hooks.nsh` silently
  uninstalls a leftover per-user install in `NSIS_HOOK_PREINSTALL` — otherwise the HKCU file
  associations would keep winning over the new HKLM ones. NSIS files must be UTF-8 **with BOM**.
- Installer artwork lives in `src-tauri/installer/` and is wired up in `tauri.conf.json`:
  `bundle.windows.nsis.sidebarImage` (164×314, Welcome / Finish pages) /
  `headerImage` (150×57, other pages) — NSIS needs 24-bit **BMP**, not PNG/JPEG. `installerIcon` and
  `uninstallerIcon` both point at the app's `icons/icon.ico`: a badged installer icon would collide with
  the UAC shield Windows overlays on the bottom-right of the elevated (`perMachine`) setup exe, and without
  `uninstallerIcon` `uninstall.exe` gets NSIS's stock icon. The uninstaller has only the Confirm /
  Uninstalling pages, so it shows no sidebar, and its header falls back to `headerImage`. And
  `bundle.macOS.dmg.background` (660×400 = `windowSize`). `appPosition` / `applicationFolderPosition`
  are icon centres (icon size 128) placed on either side of the arrow drawn in the background, so
  move them together with the artwork (the arrow is centred in the panel; icons sit at x 196 / 464).
  All of this is applied by a Finder AppleScript inside Tauri's `bundle_dmg`, which Tauri skips
  (`--skip-jenkins`) when `CI=true` — the dmg then opens with no background and default icon layout.
  `build.yml` sets `TAURI_BUNDLER_DMG_IGNORE_CI=true` on the Build step to run it anyway. Icon size is
  fixed at 128 by `bundle_dmg`; Tauri has no setting for it. `dmg-background@2x.png` is kept as the Retina source but is not
  used: Tauri passes a single background file to `bundle_dmg`, and a multi-resolution TIFF has not been tried.
- `bundle.resources` uses the map form (`"../LICENSE": "LICENSE"`). The list form would place a
  parent-directory path under `_up_/` in the bundle, because `resource_relpath()` rewrites `..`.
- Prerelease suffixes are no longer used, but the tooling still handles them (`0.1.0-beta.1`
  shipped with one): NSIS derives a numeric
  `VIProductVersion` (`0.1.0.0`) from it and compares real semver for upgrade detection, and
  `tauri-winres` drops the prerelease from the exe's VERSIONINFO. What is *not* allowed is
  numeric-only build metadata (`+abc`). On macOS the raw string lands in
  `CFBundleShortVersionString`, which Apple documents as three period-separated integers —
  out of spec, effect on directly distributed apps unverified.
