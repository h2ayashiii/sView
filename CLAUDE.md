# CLAUDE.md

sView is a minimal, frameless media viewer (images, videos, audio, zip/cbz archives) built with
**Tauri v2**. Targets: Windows 10/11 and macOS 10.15+.

- **Backend**: Rust 2021. **All** backend logic and its tests live in `src-tauri/src/lib.rs`.
- **Frontend**: plain HTML/CSS/JS in `src/`, served as-is. **No framework, no bundler, no
  TypeScript, no ES `import` / `export`** — scripts load via `<script src>` into the global scope
  and Tauri APIs come from `window.__TAURI__`.
- **Linux is not a target.** It only has to build, because CI and cloud containers run
  `cargo test` there. The `#[cfg(not(any(target_os = "windows", target_os = "macos")))]` branches
  are stubs; don't add Linux-only features or docs.
- File layout: `docs/architecture.md`. Build / CI / release: `docs/development.md`.

## Commands

```sh
npm install
npm run dev          # tauri dev
npm run build        # tauri build

cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml natural_sort_orders_numbers_numerically  # single test
```

- `cargo test` is the verification step for backend changes. It links Tauri, so on Linux it needs
  `libgtk-3-dev`, `libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev` and pkg-config; a `gdk-3.0.pc` not
  found error is a missing dependency, not a code error.
- `dev` / `build` need a display and a WebView runtime, so they do not work in a headless
  container. There are no frontend tests; verify JS changes by reading.
- After changing dependencies, run `node scripts/gen-third-party-notices.mjs`
  (`THIRD-PARTY-NOTICES` ships inside the bundle).
- Releasing is `git tag vX.Y.Z && git push origin vX.Y.Z`; CI takes the version from the tag, so
  don't bump versions in the repo. Tags are plain `vX.Y.Z`, no prerelease suffix.
- Linux → Windows cross-build requires `--runner cargo-xwin`; Linux → macOS is not possible.

## Conventions

- **Comments, doc comments (`///`) and user-facing strings are Japanese.** Identifiers and file
  names are English.
- Rust: default `cargo fmt`. Commands return `Result<T, String>` with Japanese error messages;
  state is a newtype-wrapped `Mutex` (`StartupFile`, `ArchiveCache`).
- JS/HTML/CSS: 2-space indent, semicolons, `const` / `let`. No linter or formatter config exists.
- Commits: Japanese, often `Area: 説明` (e.g. `CI: 手動実行でビルドする OS を選べるようにする`).
  Not Conventional Commits. Work lands on `main` via PRs from `feat-…` / `bugfix-…` branches.
- "削除" always means the OS trash (`trash` crate), never `fs::remove_file`.

## Keep in sync when behaviour changes

- `docs/specs/*.md` (one file per feature, Japanese) — update the matching spec.
- `CHANGELOG.md` — add user-visible changes under `未リリース`.
- `src/shortcuts.js` — `SHORTCUT_SECTIONS` is a hand-written copy of the key handling in
  `main.js`; update it when a key changes.
- `README.md` is an entry point only; detailed behaviour goes in `docs/`.
- `.claude/rules/*.md` — fix the note there when the code it describes changes.

## Making common changes

- **Add a setting** → add one entry to `SETTINGS_SECTIONS` in `src/settings-defs.js`.
  `SETTINGS_DEFAULTS` and the settings window UI are derived from it. A new section needs a `tab`
  (`general` / `image` / `video` / `audio`, listed in `SETTINGS_TABS`).
- **Add a settings-window button** → add an entry with `type: "action"` and `action: "<name>"`,
  then register the handler in `ACTIONS` in `src/settings.js`. Keep `settings-defs.js` pure data —
  `main.js` loads it too.
- **Add a Rust command** → define it in `lib.rs` and register it in
  `tauri::generate_handler![...]` inside `run()`. Custom commands need no permission entry; only
  core / plugin APIs called from JS do (`src-tauri/capabilities/*.json`).
- **Add a frontend file** → add a `<script>` / `<link>` tag to `index.html`, `settings.html`
  and/or `shortcuts.html`. Nothing picks it up automatically.

## Gotchas

- The CSP in `tauri.conf.json` is strict (`default-src 'self'`; `'unsafe-inline'` for styles
  only). Inline `<script>` and remote assets are blocked.
- Capabilities are per-window (`main` / `viewer-*`, `settings`, `shortcuts`). An API missing from
  that window's capability file fails at runtime, not at build time. The files reference a schema
  under the gitignored `src-tauri/gen/`, which does not exist until a build has run.
- `MIN_WINDOW_SIZE` in `lib.rs` must stay in sync with `minWidth` / `minHeight` in
  `tauri.conf.json`.
- Supported extensions are duplicated in three places — update all of them: `IMAGE_EXTS` /
  `VIDEO_EXTS` / `AUDIO_EXTS` (`lib.rs`), `IMAGE_EXT_FILTER` / `MIME` / `VIDEO_EXT_FILTER` /
  `AUDIO_EXT_FILTER` (`src/main.js`), `fileAssociations` (`tauri.conf.json`, one entry per kind).
  The `supported_extensions_stay_in_sync` test fails when they drift. Archives (`ARCHIVE_EXTS`)
  are deliberately **not** file-associated.
- No codec is bundled (codec patent licensing): videos and audio play with the OS WebView's
  decoders only, so don't add formats the WebViews can't handle.

## Detailed implementation notes

Design decisions and pitfalls per area are in `.claude/rules/`. They load automatically when a
matching file is read or edited; read the relevant one before planning a change in that area.

| File | Covers |
| --- | --- |
| `file-listing.md` | `list_images` and list kind / depth, archives, async commands, folder watching, deleting |
| `media-playback.md` | Video and audio playback, key / wheel / mouse handling, `#videobar` / `#imagebar`, book mode |
| `window-geometry.md` | `window.json`, size modes, per-kind slots, aspect lock, resize drag, maximize / fullscreen |
| `settings.md` | `settings.json` merge-saving, settings window sync, file associations |
| `platform.md` | macOS pointer / drag / multi-window, Windows installer and WebView2 data folder, logging, bundle resources, version strings |
