import { formatInfoTime, infoToText } from "./info-format.js";
import { normalizeSettings, SETTINGS_DEFAULTS } from "./settings-defs.js";
import { applyTheme } from "./theme.js";

// ファイルの情報ウィンドウ。ビューアが表示中のファイルの情報（Rust の media_info）を出す。
// 出すファイルは Rust 側が覚えていて、開いたとき・ビューアの表示が変わったときに
// "info-target" で届く（読み込み前に開かれた分は get_info_target で受け取る）

const tauri = window.__TAURI__ ?? {};
/** @type {import("./types.d.ts").TauriInvoke} */
const invoke =
  tauri.core?.invoke ?? (() => Promise.reject(new Error("Tauri API を利用できません")));
const listen = tauri.event?.listen ?? (() => Promise.resolve());
const infoWindow = tauri.window?.getCurrentWindow?.() ?? null;

const body = document.getElementById("s-body");
const copyButton = /** @type {HTMLButtonElement} */ (document.getElementById("info-copy"));

// 閉じても破棄はされず（Rust 側で隠すだけ）、次に開くときは同じウィンドウを使う
function closeWindow() {
  infoWindow?.close().catch(() => {});
}

document.getElementById("s-close").addEventListener("click", closeWindow);
window.addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  // ビューアと同じ I でも閉じる（開け閉めを同じキーでできるように）
  if (e.key === "Escape" || e.key === "i" || e.key === "I") closeWindow();
});
window.addEventListener("contextmenu", (e) => e.preventDefault());

// macOS だけ閉じるボタンを信号機ボタンにする（設定ウィンドウと同じ）
const winEl = document.getElementById("win");
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
winEl.classList.toggle("mac", IS_MAC);
function syncActive() {
  winEl.classList.toggle("inactive", !document.hasFocus());
}
window.addEventListener("focus", syncActive);
window.addEventListener("blur", syncActive);
syncActive();

/** @type {import("./types.d.ts").MediaInfo | null} */
let shown = null;

function message(text, className = "info-message") {
  shown = null;
  copyButton.hidden = true;
  body.replaceChildren();
  const el = document.createElement("div");
  el.className = className;
  el.textContent = text;
  body.appendChild(el);
}

/** @param {import("./types.d.ts").MediaInfo} info */
function render(info) {
  shown = info;
  copyButton.hidden = false;
  body.replaceChildren();
  const head = document.createElement("div");
  head.className = "info-name";
  head.textContent = info.name;
  head.title = info.name;
  body.appendChild(head);
  if (info.warning) {
    const warn = document.createElement("div");
    warn.className = "info-warning";
    warn.textContent = `一部の情報を読めませんでした: ${info.warning}`;
    body.appendChild(warn);
  }
  for (const section of info.sections) {
    if (!section.rows.length) continue;
    const title = document.createElement("div");
    title.className = "section-title";
    title.textContent = section.title;
    body.appendChild(title);
    for (const row of section.rows) {
      const line = document.createElement("div");
      line.className = "info-row";
      const label = document.createElement("span");
      label.className = "info-label";
      label.textContent = row.label;
      const value = document.createElement("span");
      value.className = "info-value";
      value.textContent = row.time != null ? formatInfoTime(row.time) : row.value;
      line.append(label, value);
      body.appendChild(line);
    }
  }
}

// 要求の世代。読み込み中に別のファイルへ替わったら、古い結果は捨てる
let token = 0;
/** @type {string | null} */
let currentKey = null;

/** @param {import("./types.d.ts").InfoTarget | null} target */
async function load(target) {
  const t = ++token;
  currentKey = target ? JSON.stringify([target.path, target.entry ?? null]) : null;
  if (!target?.path) {
    message("ファイルを表示していません");
    return;
  }
  // すぐ読めることが多いので、少し待ってから「読み込み中」を出す（ちらつかせない）
  const pending = setTimeout(() => {
    if (t === token) message("読み込み中…");
  }, 200);
  try {
    const info = await invoke("media_info", { path: target.path, entry: target.entry ?? null });
    if (t === token) render(info);
  } catch (e) {
    if (t === token) message(`情報を読めません: ${e}`, "info-message error");
  } finally {
    clearTimeout(pending);
  }
}

// 開き直したとき（同じファイルでも）は Rust 側が改めて送ってくるので、そのたびに読み直す
infoWindow
  ?.listen("info-target", (/** @type {{ payload: any }} */ event) => load(event.payload ?? null))
  .catch(() => {});

invoke("get_info_target")
  .then((target) => {
    // 先にイベントで受け取っていれば読み直さない
    const key = target ? JSON.stringify([target.path, target.entry ?? null]) : null;
    if (token === 0 || key !== currentKey) load(target);
  })
  .catch(() => message("ファイルを表示していません"));

// すべての情報をテキストでコピーする
copyButton.addEventListener("click", async () => {
  if (!shown) return;
  const text = infoToText(shown);
  let ok = false;
  try {
    await navigator.clipboard.writeText(text);
    ok = true;
  } catch {
    // クリップボード API が使えないときは、隠したテキスト欄を選んでコピーする
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    ok = document.execCommand("copy");
    area.remove();
  }
  copyButton.textContent = ok ? "コピーしました" : "コピーできません";
  setTimeout(() => {
    copyButton.textContent = "コピー";
  }, 1500);
});

// 配色は設定ウィンドウと同じく設定の背景色から作り、変わったら追従する。
// 背景は設定ウィンドウと同じく「背景の不透明度」によらず常に不透明（100%）にする
function applyInfoTheme(settings) {
  applyTheme({ ...settings, backgroundOpacity: 100 });
}

listen("settings-changed", (event) => applyInfoTheme(normalizeSettings(event.payload)));

applyInfoTheme(SETTINGS_DEFAULTS);
invoke("load_settings")
  .then((saved) => applyInfoTheme(normalizeSettings(saved)))
  .catch(() => {});
