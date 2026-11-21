---
paths:
  - "src/settings*.js"
  - "src/main.js"
  - "src-tauri/src/lib.rs"
---

# Settings and file associations

- Items in `SETTINGS_SECTIONS` without a `default` (i.e. `type: "action"` rows) are excluded from
  `SETTINGS_DEFAULTS` by a `.filter((i) => "default" in i)`. Removing it writes `undefined`
  into `settings.json`.
- `save_settings` **merges**: it writes only the top-level keys it is given over the file on disk
  (`merge_settings`), because on Windows every file launch is its own process holding the settings it
  read at startup, and writing the whole object back reverted keys another process had changed. So
  callers pass only what changed — `saveSettings(...keys)` in `main.js`, `persist({ key: value })` in
  `settings.js` ("Reset to defaults" sends every non-`HIDDEN_DEFAULTS` key). Other running processes still
  see a change only after a restart. `settings.json` / `window.json` are replaced via a per-process temp
  file (`temp_path`).
- `main.js` can write `settings.json` too (the "今後確認しない" checkbox → `saveSettings()`), so
  `settings.js` listens for `settings-changed` and re-renders — but only when the settings window
  is unfocused, otherwise its own emit echoes back and fights a slider being dragged.
- File associations are one `type: "associations"` row per kind tab (`associationSection(kind)`).
  On Windows `register()` rebuilds the whole set, so a row's apply also sends the other kinds'
  currently associated extensions; on macOS it sends only its own kind.
- File associations from the settings window (`file_association_status` / `apply_file_associations`,
  `type: "associations"` row in `settings-defs.js`) follow each OS's rules. Windows 8+ forbids apps
  from setting the default (UserChoice is hash-protected), so `win_assoc` only registers sView in
  HKCU (one ProgID per kind — `sView.Image` / `sView.Video` / `sView.Audio`, `PROG_IDS` —, `Capabilities`, `RegisteredApplications`,
  `OpenWithProgids`) and opens `ms-settings:defaultapps?registeredAppUser=sView` for the user to
  confirm. macOS sets it directly via `LSSetDefaultRoleHandlerForContentType` (raw FFI in
  `mac_assoc`); there is no API to unset. The selectable list is
  `associable_exts()` = `IMAGE_EXTS` + `VIDEO_EXTS` + `AUDIO_EXTS` (archives are deliberately
  excluded); the settings window groups them by `kind`.
