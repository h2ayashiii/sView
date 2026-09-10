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

const IMAGE_EXT_FILTER = [
  "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg",
  "png", "svg", "tif", "tiff", "webp",
];

let images = []; // 同一フォルダ内の画像のフルパス（自然順ソート済み）
let index = -1;

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
function showError(message) {
  errorBox.textContent = String(message);
  errorBox.hidden = false;
  clearTimeout(showError.timer);
  showError.timer = setTimeout(() => (errorBox.hidden = true), 4000);
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
  const name = baseName(images[index]);
  filenameEl.textContent = name;
  filenameEl.title = images[index];
  counterEl.textContent = `${index + 1} / ${images.length}`;
  appWindow.setTitle(`${name} - sView`).catch(() => {});
}

function preloadNeighbors() {
  if (images.length < 2) return;
  for (const off of [1, -1]) {
    const i = (index + off + images.length) % images.length;
    if (i !== index) new Image().src = convertFileSrc(images[i]);
  }
}

function show() {
  if (index < 0 || index >= images.length) return;
  setFitMode();
  placeholder.hidden = true;
  img.src = convertFileSrc(images[index]);
  updateChrome();
  preloadNeighbors();
}

img.addEventListener("error", () => {
  if (img.src) showError(`読み込みに失敗しました: ${baseName(images[index] ?? "")}`);
});

// ---- open / navigate ----
async function openPath(path) {
  try {
    const res = await invoke("list_images", { path });
    images = res.images;
    index = res.index;
    show();
  } catch (e) {
    showError(e);
  }
}

function step(delta) {
  if (images.length === 0) return;
  index = (index + delta + images.length) % images.length;
  show();
}

async function rescan() {
  if (index < 0 || !images.length) return;
  await openPath(images[index]);
}

async function openDialog() {
  try {
    const selected = await dialog.open({
      multiple: false,
      filters: [{ name: "画像", extensions: IMAGE_EXT_FILTER }],
    });
    if (typeof selected === "string") await openPath(selected);
  } catch (e) {
    showError(e);
  }
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
    case "r":
    case "R":
      rescan();
      break;
    case "f":
    case "F":
      appWindow.isFullscreen()
        .then((fs) => appWindow.setFullscreen(!fs))
        .catch(() => {});
      break;
    case "Escape":
      appWindow.close().catch(() => {});
      break;
  }
});

// ---- input: mouse back/forward buttons ----
// Windows: XBUTTON1/2 -> button 3/4。WebView2 の既定の履歴ナビゲーションは
// auxclick の preventDefault で抑止する。
// macOS: マウスにより挙動が異なるが、side button は同じく button 3/4 の
// mouseup として届く（届かないユーティリティ常駐マウスはキー操作で代替）。
window.addEventListener("mouseup", (e) => {
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
    const factor = Math.exp(-e.deltaY * 0.002);
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
document.getElementById("nav-prev").addEventListener("click", () => step(-1));
document.getElementById("nav-next").addEventListener("click", () => step(1));
document.getElementById("btn-min").addEventListener("click", () => appWindow.minimize());
document.getElementById("btn-max").addEventListener("click", () => appWindow.toggleMaximize());
document.getElementById("btn-close").addEventListener("click", () => appWindow.close());
window.addEventListener("contextmenu", (e) => e.preventDefault());

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
