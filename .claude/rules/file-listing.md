---
paths:
  - "src-tauri/src/lib.rs"
  - "src/main.js"
---

# File listing, archives, folder watching, deleting

- A list holds **one kind** only (`MediaKind`: image / video / audio). `list_images` decides it:
  a single file → that file's kind, its folder's direct children only (`depth` 1); a folder → every
  media file down to `MAX_DEPTH` (3, the opened folder's direct children = level 1), sorted by the
  relative path, then filtered to the kind of the first entry; an archive → images only, also down to
  `MAX_DEPTH` (counted from the archive root, before `common_dir_prefix`). `ImageList` returns `kind`
  and `depth`; `main.js` keeps them (`listKind` / `listDepth`) and passes them back on every re-list
  (`R`, `refreshFolder`) and to `watch_folder`, so a re-list never switches the kind or turns a
  single-file list into a recursive one.
- Archives: `MAX_ENTRY_BYTES` (512 MB) guards against zip bombs, and the open archive handle is
  cached with an `(mtime, size)` stamp — don't re-open it naively.
- `list_images`, `read_archive_image`, `audio_info` and `audio_artwork` are `#[tauri::command(async)]`:
  a plain sync command runs on the main thread and blocks window moves / painting while it reads disk.
  Keep window-touching commands sync. `collect_media` takes file / dir from `DirEntry::file_type()`
  (`entry_file_type`, follows symlinks only) — don't go back to `path.is_file()` per entry.
  In `main.js`, `archiveBlobUrl` shares an in-flight read per entry (`blobLoading`) and drops results
  from before the last `clearBlobCache()` (`blobGeneration`).
- Folder watching (`watch_folder` / `FolderWatcher`) uses `notify`'s OS-native backends, so it
  costs nothing while idle — do **not** replace it with polling. Exactly one folder is watched at a
  time, to the list's `depth` (non-recursive for 1, recursive for more, with events deeper than
  `depth` dropped by `is_listing_change`), and only while a folder (not an archive) is open; passing
  `path: null` drops the watcher. `is_listing_change` filters out content-only writes so a save
  doesn't rebuild the list, and in recursive mode also reacts to sub-folders appearing / vanishing
  (a moved-in folder may not report its children). Rust only emits `folder-changed`; the debounce (`RESCAN_DELAY_MS`, 800 ms in
  `main.js`) and the actual re-listing live in the frontend — that delay also keeps a
  half-copied file from being listed, so don't shorten it without a reason.
- Deleting goes through `trash`, never `fs::remove_file` — "削除" in this app always means the
  OS trash. `main.js` splices the entry out itself instead of waiting for the watcher, because
  watching fails on some network drives. It looks the path up again after `delete_image` returns
  (`images.indexOf(path)`): a watcher re-list may have rebuilt `images` meanwhile.
