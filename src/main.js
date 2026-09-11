// sView - minimal frameless image viewer
// withGlobalTauri: true のため window.__TAURI__ からAPIを利用（バンドラ不要）
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const dialog = window.__TAURI__.dialog;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const app = document.getElementById("app");
const stage = document.getElementById("stage");
const img = document.getElementById("image");
const placeholder = document.getElementById("placeholder");
const errorBox = document.getElementById("error");
const filenameEl = document.getElementById("filename");
const counterEl = document.getElementById("counter");
const navPrev = document.getElementById("nav-prev");
const navNext = document.getElementById("nav-next");
const ctxmenu = document.getElementById("ctxmenu");

// 現在の設定（settings-defs.js の既定値で開始し、読み込み後に上書きされる）
let settings = { ...SETTINGS_DEFAULTS };

// macOS だけ閉じるボタンを左上に置く（OS の慣習に合わせる）
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
app.classList.toggle("mac", IS_MAC);

const IMAGE_EXT_FILTER = [
  "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg",
  "png", "svg", "tif", "tiff", "webp",
];
const ARCHIVE_EXT_FILTER = ["zip", "cbz"];

// blob URL に必要な MIME（拡張子から判定）
const MIME = {
  avif: "image/avif", bmp: "image/bmp", gif: "image/gif", ico: "image/x-icon",
  jfif: "image/jpeg", jpe: "image/jpeg", jpeg: "image/jpeg", jpg: "image/jpeg",
  png: "image/png", svg: "image/svg+xml", tif: "image/tiff", tiff: "image/tiff",
  webp: "image/webp",
};

// フォルダの場合は画像のフルパス、書庫の場合は書庫内のエントリ名（自然順ソート済み）
let images = [];
let index = -1;
// 書庫を開いている場合はそのフルパス。フォルダの場合は null
let archivePath = null;
// 全エントリが共有する先頭フォルダ。表示名からはこの分を取り除く
let entryPrefix = "";
// 表示要求の世代。非同期読み込みの結果が古い場合は捨てる
let showToken = 0;

// ---- 書庫内画像の遅延ロード ----
// 書庫は一括展開せず、表示するエントリだけを Rust 側から取り出して
// blob URL 化する。前後の先読み分を含め数枚だけ保持する
const BLOB_CACHE_MAX = 5;
const blobCache = new Map(); // entry -> blob URL（挿入順 = LRU 順）

function extOf(name) {
  const m = /\.([^.\\/]+)$/.exec(name);
  return m ? m[1].toLowerCase() : "";
}

function clearBlobCache() {
  for (const url of blobCache.values()) URL.revokeObjectURL(url);
  blobCache.clear();
}

function trimBlobCache() {
  while (blobCache.size > BLOB_CACHE_MAX) {
    const [oldest, url] = blobCache.entries().next().value;
    if (oldest === images[index]) break; // 表示中のものは残す
    URL.revokeObjectURL(url);
    blobCache.delete(oldest);
  }
}

async function archiveBlobUrl(entry) {
  const cached = blobCache.get(entry);
  if (cached) {
    // LRU 更新
    blobCache.delete(entry);
    blobCache.set(entry, cached);
    return cached;
  }
  const bytes = await invoke("read_archive_image", { archive: archivePath, entry });
  const url = URL.createObjectURL(new Blob([bytes], { type: MIME[extOf(entry)] ?? "" }));
  blobCache.set(entry, url);
  trimBlobCache();
  return url;
}

// ---- zoom / pan state ----
// mode "fit": ウィンドウにフィット（このときステージはウィンドウドラッグ領域になる）
// mode "zoom": 拡大縮小・パン可能
let mode = "fit";
let scale = 1;
let tx = 0;
let ty = 0;

function setFitMode() {
  mode = "fit";
  img.className = "fit";
  img.style.transform = "";
  img.style.position = "";
  stage.setAttribute("data-tauri-drag-region", "");
  stage.classList.remove("panning");
}

function enterZoomMode() {
  if (mode === "zoom" || !img.src || !img.naturalWidth) return;
  const rect = img.getBoundingClientRect();
  scale = rect.width / img.naturalWidth;
  tx = rect.left;
  ty = rect.top;
  mode = "zoom";
  img.className = "";
  img.style.position = "absolute";
  img.style.left = "0";
  img.style.top = "0";
  stage.removeAttribute("data-tauri-drag-region");
  stage.classList.add("panning");
  applyTransform();
}

function applyTransform() {
  img.style.transform = `translate(${tx}px, ${ty}px) scale(${scale})`;
}

function zoomAt(cx, cy, factor) {
  enterZoomMode();
  if (mode !== "zoom") return;
  const next = Math.min(30, Math.max(0.03, scale * factor));
  const f = next / scale;
  tx = cx - (cx - tx) * f;
  ty = cy - (cy - ty) * f;
  scale = next;
  applyTransform();
}

function zoomTo(target) {
  enterZoomMode();
  if (mode !== "zoom") return;
  const cx = window.innerWidth / 2;
  const cy = window.innerHeight / 2;
  const f = target / scale;
  tx = cx - (cx - tx) * f;
  ty = cy - (cy - ty) * f;
  scale = target;
  applyTransform();
}

// ---- display ----
// エラーと案内（端に到達したなど）を同じ場所に出す。kind で色だけ変える
function showToast(message, kind) {
  errorBox.textContent = String(message);
  errorBox.classList.toggle("notice", kind === "notice");
  errorBox.hidden = false;
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => (errorBox.hidden = true), kind === "notice" ? 1500 : 4000);
}

function showError(message) {
  showToast(message, "error");
}

function baseName(path) {
  return path.split(/[\\/]/).pop();
}

function updateChrome() {
  if (index < 0 || !images.length) {
    app.classList.add("no-image");
    filenameEl.textContent = "";
    counterEl.textContent = "";
    appWindow.setTitle("sView").catch(() => {});
    return;
  }
  app.classList.remove("no-image");
  // 書庫内は同名ファイルが別フォルダに並びうるので、共通フォルダを除いた
  // 相対パスで表示する（単一フォルダの書庫なら結果的にファイル名だけになる）
  const name = archivePath
    ? images[index].slice(entryPrefix.length)
    : baseName(images[index]);
  filenameEl.textContent = archivePath ? `${baseName(archivePath)} / ${name}` : name;
  filenameEl.title = archivePath ? `${archivePath} :: ${images[index]}` : images[index];
  counterEl.textContent = `${index + 1} / ${images.length}`;
  navPrev.disabled = index === 0;
  navNext.disabled = index === images.length - 1;
  appWindow.setTitle(`${baseName(images[index])} - sView`).catch(() => {});
}

function preloadNeighbors() {
  if (!settings.preload || images.length < 2) return;
  for (const off of [1, -1]) {
    // 端で折り返さないので、範囲外は先読みしない
    const i = index + off;
    if (i < 0 || i >= images.length) continue;
    if (archivePath) {
      // 先読みも 1 件ずつ。失敗しても表示には影響させない
      archiveBlobUrl(images[i]).catch(() => {});
    } else {
      new Image().src = convertFileSrc(images[i]);
    }
  }
}

// 読み込みが終わってからでないと画像の実寸が分からないので、
// ウィンドウサイズ合わせと起動時倍率はここでまとめて行う
function onImageReady(run) {
  if (img.complete && img.naturalWidth) run();
  else img.addEventListener("load", run, { once: true });
}

// 「画像に合わせる」のとき、余白が出ないようウィンドウを画像の縦横比に合わせる
async function fitWindowToImage() {
  if (settings.windowSizeMode !== "flexible") return;
  if (!img.naturalWidth || !img.naturalHeight) return;
  try {
    await invoke("fit_window_to_image", {
      width: img.naturalWidth,
      height: img.naturalHeight,
    });
  } catch (e) {
    showError(e);
  }
}

// 設定「画像を開いたときの表示」が等倍なら 100% で表示する
function applyStartupZoom() {
  if (settings.startupZoom === "actual") zoomTo(1);
}

async function show() {
  if (index < 0 || index >= images.length) return;
  const token = ++showToken;
  setFitMode();
  placeholder.hidden = true;
  updateChrome();
  try {
    const src = archivePath
      ? await archiveBlobUrl(images[index])
      : convertFileSrc(images[index]);
    if (token !== showToken) return; // 既に別の画像へ移動している
    img.src = src;
    onImageReady(async () => {
      if (token !== showToken) return;
      await fitWindowToImage();
      if (token !== showToken) return;
      applyStartupZoom();
    });
  } catch (e) {
    if (token === showToken) showError(e);
    return;
  }
  preloadNeighbors();
}

img.addEventListener("error", () => {
  if (img.src) showError(`読み込みに失敗しました: ${baseName(images[index] ?? "")}`);
});

// ---- open / navigate ----
async function openPath(path, preferredIndex = -1) {
  try {
    const res = await invoke("list_images", { path });
    if (res.archive !== archivePath) clearBlobCache();
    archivePath = res.archive ?? null;
    entryPrefix = res.prefix ?? "";
    images = res.images;
    index = preferredIndex >= 0 && preferredIndex < images.length ? preferredIndex : res.index;
    await show();
  } catch (e) {
    showError(e);
  }
}

function step(delta) {
  if (images.length === 0) return;
  let next = index + delta;
  if (next < 0 || next >= images.length) {
    if (!settings.wrapAround) {
      // 折り返さない設定のときは、そこが端であることだけ知らせる
      showToast(next < 0 ? "最初の画像です" : "最後の画像です", "notice");
      return;
    }
    next = (next + images.length) % images.length;
  }
  index = next;
  show();
}

async function rescan() {
  if (index < 0 || !images.length) return;
  if (archivePath) {
    // 書庫の中身が差し替わっている可能性があるのでキャッシュを捨てて開き直す
    clearBlobCache();
    await openPath(archivePath, index);
  } else {
    await openPath(images[index]);
  }
}

async function openDialog() {
  try {
    const selected = await dialog.open({
      multiple: false,
      filters: [
        { name: "画像・圧縮フォルダ", extensions: [...IMAGE_EXT_FILTER, ...ARCHIVE_EXT_FILTER] },
        { name: "画像", extensions: IMAGE_EXT_FILTER },
        { name: "圧縮フォルダ (zip / cbz)", extensions: ARCHIVE_EXT_FILTER },
      ],
    });
    if (typeof selected === "string") await openPath(selected);
  } catch (e) {
    showError(e);
  }
}

async function openFolderDialog() {
  try {
    const selected = await dialog.open({ directory: true, multiple: false });
    if (typeof selected === "string") await openPath(selected);
  } catch (e) {
    showError(e);
  }
}

function toggleFullscreen() {
  appWindow.isFullscreen()
    .then((fs) => appWindow.setFullscreen(!fs))
    .catch(() => {});
}

// ---- input: keyboard ----
window.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  switch (e.key) {
    case "ArrowRight":
    case "ArrowDown":
    case "PageDown":
    case " ":
      e.preventDefault();
      step(1);
      break;
    case "ArrowLeft":
    case "ArrowUp":
    case "PageUp":
    case "Backspace":
      e.preventDefault();
      step(-1);
      break;
    case "Home":
      if (images.length) { index = 0; show(); }
      break;
    case "End":
      if (images.length) { index = images.length - 1; show(); }
      break;
    case "+":
    case "=":
      zoomAt(window.innerWidth / 2, window.innerHeight / 2, 1.25);
      break;
    case "-":
      zoomAt(window.innerWidth / 2, window.innerHeight / 2, 1 / 1.25);
      break;
    case "0":
      setFitMode();
      break;
    case "1":
      zoomTo(1);
      break;
    case "o":
    case "O":
      openDialog();
      break;
    case "d":
    case "D":
      openFolderDialog();
      break;
    case "r":
    case "R":
      rescan();
      break;
    case "f":
    case "F":
      toggleFullscreen();
      break;
    case ",":
      openSettings();
      break;
    case "Escape":
      // メニューが開いているときは、まずそれを閉じる
      if (!ctxmenu.hidden) hideContextMenu();
      else appWindow.close().catch(() => {});
      break;
  }
});

// ---- input: mouse back/forward buttons ----
// Windows: XBUTTON1/2 -> button 3/4。WebView2 の既定の履歴ナビゲーションは
// auxclick の preventDefault で抑止する。
// macOS: マウスにより挙動が異なるが、side button は同じく button 3/4 の
// mouseup として届く（届かないユーティリティ常駐マウスはキー操作で代替）。
window.addEventListener("mouseup", (e) => {
  if (!settings.sideButtons) return;
  if (e.button === 3) {
    e.preventDefault();
    step(-1);
  } else if (e.button === 4) {
    e.preventDefault();
    step(1);
  }
});
window.addEventListener("auxclick", (e) => {
  if (e.button === 3 || e.button === 4) e.preventDefault();
});

// ---- input: wheel zoom ----
window.addEventListener(
  "wheel",
  (e) => {
    if (!images.length) return;
    e.preventDefault();
    if (settings.wheelAction === "navigate") {
      wheelNavigate(e.deltaY);
      return;
    }
    const factor = Math.exp(-e.deltaY * 0.0004 * settings.wheelSensitivity);
    zoomAt(e.clientX, e.clientY, factor);
  },
  { passive: false }
);

// ---- input: pan (zoom mode only) ----
let panning = null;
stage.addEventListener("mousedown", (e) => {
  if (mode !== "zoom" || e.button !== 0) return;
  panning = { x: e.clientX, y: e.clientY };
});
window.addEventListener("mousemove", (e) => {
  if (!panning) return;
  tx += e.clientX - panning.x;
  ty += e.clientY - panning.y;
  panning = { x: e.clientX, y: e.clientY };
  applyTransform();
});
window.addEventListener("mouseup", () => (panning = null));

// ---- misc UI ----
placeholder.addEventListener("click", openDialog);
navPrev.addEventListener("click", () => step(-1));
navNext.addEventListener("click", () => step(1));
document.getElementById("btn-close").addEventListener("click", () => appWindow.close());
// Tauri のドラッグ領域はダブルクリックで最大化するが、最大化は使わないので止める
window.addEventListener(
  "mousedown",
  (e) => {
    if (e.detail >= 2 && e.target.closest?.("[data-tauri-drag-region]")) e.stopPropagation();
  },
  true
);
window.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  openContextMenu(e.clientX, e.clientY);
});

// ---- drag & drop ----
listen("tauri://drag-enter", () => app.classList.add("dropping"));
listen("tauri://drag-leave", () => app.classList.remove("dropping"));
listen("tauri://drag-drop", (event) => {
  app.classList.remove("dropping");
  const paths = event.payload?.paths ?? [];
  if (paths.length) openPath(paths[0]);
});

// ---- startup ----
// macOS の Dock / Finder からの "Opened" イベント（起動後）
listen("open-file", (event) => openPath(event.payload));

// CLI 引数 / 関連付け起動（Windows・Linux）、または起動前に届いた macOS の Opened
invoke("get_startup_file")
  .then((path) => {
    if (path) openPath(path);
  })
  .catch(() => {});

// ---- settings ----
// 設定ウィンドウからの変更は "settings-changed" で届き、その場で反映する
function hexToRgba(hex, alpha) {
  const m = /^#?([0-9a-f]{6})$/i.exec(String(hex).trim());
  const n = parseInt(m ? m[1] : "0e0e10", 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

function applySettings() {
  app.style.background = hexToRgba(settings.backgroundColor, settings.backgroundOpacity / 100);
  app.style.borderRadius = settings.roundedCorners ? "8px" : "0";
  app.classList.toggle("hide-filename", !settings.showFilename);
  app.classList.toggle("hide-nav", !settings.showNavButtons);
  img.style.imageRendering = settings.imageRendering;
  appWindow.setAlwaysOnTop(!!settings.alwaysOnTop).catch(() => {});
}

async function openSettings() {
  hideContextMenu();
  try {
    await invoke("open_settings_window");
  } catch (e) {
    showError(e);
  }
}

// ---- wheel navigation ----
// ホイール 1 段の量はデバイス差が大きいので、しきい値までためてから 1 枚送る
let wheelAccum = 0;
function wheelNavigate(deltaY) {
  const threshold = 220 - settings.wheelSensitivity * 18;
  if (Math.sign(deltaY) !== Math.sign(wheelAccum)) wheelAccum = 0;
  wheelAccum += deltaY;
  while (Math.abs(wheelAccum) >= threshold) {
    step(Math.sign(wheelAccum));
    wheelAccum -= Math.sign(wheelAccum) * threshold;
  }
}

// ---- context menu ----
const REVEAL_LABEL = IS_MAC
  ? "Finder で表示"
  : /Win/.test(navigator.platform || navigator.userAgent)
    ? "エクスプローラーで表示"
    : "ファイルマネージャーで表示";

// 表示中のファイル。書庫内の画像の場合は書庫そのものを指す
function currentFilePath() {
  if (archivePath) return archivePath;
  return index >= 0 && index < images.length ? images[index] : null;
}

async function revealCurrent() {
  const path = currentFilePath();
  if (!path) return;
  try {
    await invoke("reveal_in_file_manager", { path });
  } catch (e) {
    showError(e);
  }
}

function hideContextMenu() {
  ctxmenu.hidden = true;
}

function buildContextMenu() {
  const hasFile = currentFilePath() !== null;
  return [
    { label: REVEAL_LABEL, disabled: !hasFile, action: revealCurrent },
    { separator: true },
    { label: "ファイルを開く…", accel: "O", action: openDialog },
    { label: "フォルダを開く…", accel: "D", action: openFolderDialog },
    { label: "再読み込み", accel: "R", disabled: !hasFile, action: rescan },
    { separator: true },
    { label: "ウィンドウに合わせる", accel: "0", disabled: !hasFile, action: setFitMode },
    { label: "等倍 (100%)", accel: "1", disabled: !hasFile, action: () => zoomTo(1) },
    { label: "全画面表示", accel: "F", action: toggleFullscreen },
    { separator: true },
    { label: "設定…", accel: ",", action: openSettings },
    { label: "終了", accel: "Esc", action: () => appWindow.close().catch(() => {}) },
  ];
}

function openContextMenu(x, y) {
  ctxmenu.textContent = "";
  for (const item of buildContextMenu()) {
    if (item.separator) {
      const hr = document.createElement("div");
      hr.className = "ctx-sep";
      ctxmenu.appendChild(hr);
      continue;
    }
    const btn = document.createElement("button");
    btn.className = "ctx-item";
    btn.type = "button";
    btn.setAttribute("role", "menuitem");
    btn.disabled = !!item.disabled;
    const label = document.createElement("span");
    label.textContent = item.label;
    const accel = document.createElement("span");
    accel.className = "ctx-accel";
    accel.textContent = item.accel ?? "";
    btn.append(label, accel);
    btn.addEventListener("click", () => {
      hideContextMenu();
      item.action();
    });
    ctxmenu.appendChild(btn);
  }

  // いったん表示してから実寸で画面内に収める
  ctxmenu.style.left = "0px";
  ctxmenu.style.top = "0px";
  ctxmenu.hidden = false;
  const rect = ctxmenu.getBoundingClientRect();
  const left = Math.max(4, Math.min(x, window.innerWidth - rect.width - 4));
  const top = Math.max(4, Math.min(y, window.innerHeight - rect.height - 4));
  ctxmenu.style.left = `${left}px`;
  ctxmenu.style.top = `${top}px`;
}

window.addEventListener("mousedown", (e) => {
  if (!ctxmenu.hidden && !ctxmenu.contains(e.target)) hideContextMenu();
}, true);
window.addEventListener("blur", hideContextMenu);
window.addEventListener("resize", hideContextMenu);

listen("settings-changed", (event) => {
  const previousMode = settings.windowSizeMode;
  settings = normalizeSettings(event.payload);
  applySettings();
  // 「画像に合わせる」に切り替えた直後は、表示中の画像に合わせておく
  if (settings.windowSizeMode !== previousMode) fitWindowToImage();
});

invoke("load_settings")
  .then((saved) => {
    settings = normalizeSettings(saved);
    applySettings();
  })
  .catch(() => applySettings());
