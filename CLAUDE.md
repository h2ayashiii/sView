# CLAUDE.md

Guidance for coding agents working in this repository.

## Overview

sView is a minimal, frameless, cross-platform image viewer built with **Tauri v2**.
It opens single images, folders, and zip/cbz archives; the overlay UI shows while the mouse
moves and hides again after 3 s of no movement (`showChrome` / `hideChrome` toggle
`#app.chrome-visible` in `src/main.js`).

- **Backend**: Rust 2021, crate `sview` / lib `sview_lib` (`tauri`, `tauri-plugin-dialog`,
  `tauri-plugin-log`, `log`, `serde`, `zip`).
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
| `src-tauri/installer-hooks.nsh` | Windows インストーラ (NSIS) のフック。旧ユーザー単位インストールの後始末 |
| `src-tauri/capabilities/` | `default.json` (main) and `settings.json` — per-window permissions |
| `scripts/` | `build.sh` / `build.ps1` — thin `npm install && npm run build` wrappers; `set-version.mjs` — writes a release tag's version into `tauri.conf.json` / `Cargo.toml` / `package.json`; `gen-third-party-notices.mjs` — regenerates `THIRD-PARTY-NOTICES` from `cargo metadata` |
| `.github/workflows/build.yml` | Only workflow. `test` job: `cargo test` on `ubuntu-latest`, runs on PRs and pushes to `main` (needs the GTK/WebKit apt packages). `build` / `release` jobs: `cargo test` + `npm run build` for Windows/macOS, **only** on `v*` tags and manual dispatch; tag runs attach the `.dmg` / `.exe` to a GitHub Release |
| `.github/ISSUE_TEMPLATE/` | Bug-report form (`bug_report.yml`); blank issues are disabled |
| `.github/release-notes/` | `<tag>.md` here becomes that release's notes; otherwise `--generate-notes` is used |

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
  Prerelease tags (`v0.1.0-beta.1`) are supported; the leading `v` is stripped before it reaches
  any manifest. **After changing dependencies, run `node scripts/gen-third-party-notices.mjs`**
  — `THIRD-PARTY-NOTICES` and `LICENSE` ship inside the bundle via `bundle.resources`.
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
- **Add a settings-window button** (not a stored value) → add an entry with `type: "action"`
  and `action: "<name>"`, then register the handler under that name in `ACTIONS` in
  `src/settings.js`. Keep `settings-defs.js` pure data — `main.js` loads it too.
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
  `fileAssociations` (`tauri.conf.json`). The `supported_extensions_stay_in_sync` test reads
  `main.js` and `tauri.conf.json` and fails when they drift, so `cargo test` catches it.
  The one deliberate difference: `ARCHIVE_EXTS` includes `zip`, but only `cbz` is file-associated
  (taking `.zip` from archivers would be hostile). `.zip` still opens via drag & drop and `O`.
- Items in `SETTINGS_SECTIONS` without a `default` (i.e. `type: "action"` rows) are excluded from
  `SETTINGS_DEFAULTS` by a `.filter((i) => "default" in i)`. Removing it writes `undefined`
  into `settings.json`.
- Both `settings.json` and `window.json` live in `config_dir()/sview` (`CONFIG_DIR_NAME` in
  `lib.rs`), **not** in Tauri's `app_config_dir()` — the folder name is deliberately kept
  independent of the bundle identifier. "Reset to defaults" only touches `settings.json`.
  `window.json` holds size and position: the position is restored in both size modes (skipped
  when it lands on no connected monitor); the size only in "fixed". Logs go to a `logs/`
  subfolder of the same directory.
- `tauri-plugin-log` is registered from `setup()` via `app.handle().plugin(...)`, not on the
  `Builder`, because the output folder needs an `AppHandle` (`log_dir()` → `config_dir()/logs`).
  A panic hook installed at the top of `run()` logs the panic and a backtrace before aborting,
  and a failed `Builder::build()` shows an OS message box instead of panicking.
- Archives: `MAX_ENTRY_BYTES` (512 MB) guards against zip bombs, and the open archive handle is
  cached with an `(mtime, size)` stamp — don't re-open it naively.
- The Windows installer is `installMode: "perMachine"`: NSIS gets `RequestExecutionLevel admin`, so it
  asks for UAC on launch and defaults to `C:\Program Files\sView` (HKLM, all-users shortcuts and
  file associations). It used to be per-user (`%LOCALAPPDATA%`), so `installer-hooks.nsh` silently
  uninstalls a leftover per-user install in `NSIS_HOOK_PREINSTALL` — otherwise the HKCU file
  associations would keep winning over the new HKLM ones. NSIS files must be UTF-8 **with BOM**.
- The two windows in `tauri.conf.json` are `"create": false`; `create_configured_windows()` builds
  them from the same config in `setup()`. That detour exists only so `data_directory()` can be set:
  on Windows an unspecified WebView2 user-data folder is created next to the exe
  (`C:\Program Files\sView\sview.exe.WebView2`) and the app fails to start. It is pointed at
  `%LOCALAPPDATA%\sview` (`webview_data_dir()`). macOS (WKWebView) cannot set it — the OS derives
  `~/Library/WebKit/<bundle id>` and `~/Library/Caches/<bundle id>` from the identifier — and
  WebKitGTK already defaults to `~/.local/share/sview` + `~/.cache/sview`, so both return `None`.
- macOS: a file opened from Finder/Dock arrives via `RunEvent::Opened`, not argv.
  `macOSPrivateApi` is enabled and builds are ad-hoc signed only.
- `bundle.resources` uses the map form (`"../LICENSE": "LICENSE"`). The list form would place a
  parent-directory path under `_up_/` in the bundle, because `resource_relpath()` rewrites `..`.
- Versions may carry a semver prerelease (`0.1.0-beta.1`): NSIS derives a numeric
  `VIProductVersion` (`0.1.0.0`) from it and compares real semver for upgrade detection, and
  `tauri-winres` drops the prerelease from the exe's VERSIONINFO. What is *not* allowed is
  numeric-only build metadata (`+abc`). On macOS the raw string lands in
  `CFBundleShortVersionString`, which Apple documents as three period-separated integers —
  out of spec, effect on directly distributed apps unverified.
- `src-tauri/gen/` and `src-tauri/target/` are generated and gitignored. `capabilities/*.json`
  reference a schema under `gen/` that does not exist until a build has run.
