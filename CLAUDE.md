# CLAUDE.md

Guidance for coding agents working in this repository.

## Overview

sView is a minimal, frameless, cross-platform image viewer built with **Tauri v2**.
It opens single images, folders, and zip/cbz archives; the overlay UI shows while the mouse
moves and hides again after 3 s of no movement (`showChrome` / `hideChrome` toggle
`#app.chrome-visible` in `src/main.js`).

- **Backend**: Rust 2021, crate `sview` / lib `sview_lib` (`tauri`, `tauri-plugin-dialog`, `serde`, `zip`).
- **Frontend**: plain HTML/CSS/JS. **No framework, no bundler, no TypeScript.**
  `withGlobalTauri: true`, so APIs come from `window.__TAURI__` and scripts load via `<script src>`.
- Targets: Windows 10/11, macOS 10.15+, Linux.

## Layout

| Path | Contents |
| --- | --- |
| `src/` | Frontend, served as-is (`frontendDist: "../src"`) |
| `src/main.js` | Main window: navigation, zoom, input, context menu |
| `src/settings-defs.js` | `SETTINGS_SECTIONS` (single source of truth) + `SETTINGS_DEFAULTS` |
| `src/settings.js` / `settings.html` | Settings window, UI generated from `SETTINGS_SECTIONS` |
| `src-tauri/src/lib.rs` | **All** backend logic + inline `#[cfg(test)] mod tests` |
| `src-tauri/src/main.rs` | Only calls `sview_lib::run()` |
| `src-tauri/tauri.conf.json` | Windows, CSP, bundle, file associations |
| `src-tauri/capabilities/` | `default.json` (main) and `settings.json` — per-window permissions |
| `scripts/` | `build.sh` / `build.ps1` — thin `npm install && npm run build` wrappers; `set-version.mjs` — writes a release tag's version into `tauri.conf.json` / `Cargo.toml` / `package.json` |
| `.github/workflows/build.yml` | Only workflow: `cargo test` + `npm run build` for Windows/macOS. Runs **only** on `v*` tags and manual dispatch — never on a push to `main`. Tag runs attach the `.dmg` / `.exe` to a GitHub Release |

## Commands

```sh
npm install
npm run dev          # tauri dev
npm run build        # tauri build
npm run build:debug  # tauri build --debug

cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml natural_sort_orders_numbers_numerically  # single test
```

- `cargo test` is the verification step for backend changes, but it still links the Tauri
  crate, so it needs the platform GUI dev packages (on Linux: `libgtk-3-dev`,
  `libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev`, pkg-config). Without them the build fails at
  `gdk-3.0.pc` not found — that is a missing dependency, not a code error.
- `dev` / `build` additionally need a running display and a WebView runtime, so they do not
  work in a headless container. There are no frontend tests; verify JS changes by reading.
- Releasing is `git tag vX.Y.Z && git push origin vX.Y.Z`. CI derives the version from the tag
  (`scripts/set-version.mjs`), so there is no need to bump the version in the repo beforehand.
- Linux → Windows cross-build requires `--runner cargo-xwin`; Linux → macOS is not possible.

## Conventions

- No linter or formatter config exists. Rust: default `cargo fmt`. JS/HTML/CSS: 2-space indent, semicolons.
- **Comments, doc comments (`///`) and user-facing strings are Japanese.** Identifiers and
  file names are English. Match this in new code.
- Rust: `snake_case` functions, `SCREAMING_SNAKE` constants, commands return `Result<T, String>`
  with Japanese error messages, state via newtype-wrapped `Mutex` (`StartupFile`, `ArchiveCache`).
- JS: `const`/`let`, `camelCase`, module-level `SCREAMING_SNAKE` constants, global script scope.
  **No ES `import` / `export`** — it breaks the global-script setup.
- Commits: Japanese, often `Area: 説明` (e.g. `CI: 手動実行でビルドする OS を選べるようにする`).
  Not Conventional Commits. Work lands on `main` via PRs from `feat-…` / `bugfix-…` branches.

## Making common changes

- **Add a setting** → add one entry to `SETTINGS_SECTIONS` in `src/settings-defs.js`.
  `SETTINGS_DEFAULTS` and the settings window UI are both derived from it.
- **Add a Rust command** → define it in `lib.rs`, register it in `tauri::generate_handler![...]`
  inside `run()`, and add any needed permission to the right `src-tauri/capabilities/*.json`.
- **Add a frontend file** → add a `<script>` / `<link>` tag to `index.html` and/or `settings.html`.
  Nothing picks it up automatically.

## Gotchas

- The CSP in `tauri.conf.json` is strict (`default-src 'self'`; `'unsafe-inline'` for styles only).
  Inline `<script>` and remote assets are blocked.
- Capabilities are per-window (`main` vs `settings`). An API missing from that window's
  capability file fails at runtime, not at build time.
- `MIN_WINDOW_SIZE` in `lib.rs` must stay in sync with `minWidth` / `minHeight` in `tauri.conf.json`.
- The supported image extension list is duplicated in three places — update all of them:
  `IMAGE_EXTS` (`lib.rs`), `IMAGE_EXT_FILTER` / `MIME` (`src/main.js`),
  `fileAssociations` (`tauri.conf.json`).
- Both `settings.json` and `window.json` live in `config_dir()/sview` (`CONFIG_DIR_NAME` in
  `lib.rs`), **not** in Tauri's `app_config_dir()` — the folder name is deliberately kept
  independent of the bundle identifier. "Reset to defaults" only touches `settings.json`.
  `window.json` holds size and position: the position is restored in both size modes (skipped
  when it lands on no connected monitor); the size only in "fixed".
- Archives: `MAX_ENTRY_BYTES` (512 MB) guards against zip bombs, and the open archive handle is
  cached with an `(mtime, size)` stamp — don't re-open it naively.
- macOS: a file opened from Finder/Dock arrives via `RunEvent::Opened`, not argv.
  `macOSPrivateApi` is enabled and builds are ad-hoc signed only.
- `src-tauri/gen/` and `src-tauri/target/` are generated and gitignored. `capabilities/*.json`
  reference a schema under `gen/` that does not exist until a build has run.
