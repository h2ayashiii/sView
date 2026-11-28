import { normalizeSettings, SETTINGS_DEFAULTS } from "./settings-defs.js";
import { SHORTCUT_SECTIONS } from "./shortcut-defs.js";
import { applyTheme } from "./theme.js";

// ショートカット一覧ウィンドウ（定義本体は shortcut-defs.js）。

const tauri = window.__TAURI__ ?? {};
/** @type {import("./types.d.ts").TauriInvoke} */
const invoke =
  tauri.core?.invoke ?? (() => Promise.reject(new Error("Tauri API を利用できません")));
const listen = tauri.event?.listen ?? (() => Promise.resolve());
const shortcutsWindow = tauri.window?.getCurrentWindow?.() ?? null;

// 閉じても破棄はされず（Rust 側で隠すだけ）、次に開くときは同じウィンドウを使う
function closeWindow() {
  shortcutsWindow?.close().catch(() => {});
}

document.getElementById("s-close").addEventListener("click", closeWindow);
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeWindow();
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

function keyCell(text, current) {
  const cell = document.createElement("span");
  cell.className = current ? "sc-key current" : "sc-key";
  cell.textContent = text;
  return cell;
}

function build() {
  const body = document.getElementById("s-body");
  const head = document.createElement("div");
  head.className = "sc-row sc-head";
  head.append(
    document.createElement("span"),
    keyCell("Windows", !IS_MAC),
    keyCell("macOS", IS_MAC)
  );
  body.appendChild(head);
  for (const section of SHORTCUT_SECTIONS) {
    const title = document.createElement("div");
    title.className = "section-title";
    title.textContent = section.label;
    body.appendChild(title);
    for (const item of section.items) {
      const row = document.createElement("div");
      row.className = "sc-row";
      const label = document.createElement("span");
      label.className = "sc-label";
      label.textContent = item.label;
      row.append(
        label,
        keyCell(item.win ?? item.keys, !IS_MAC),
        keyCell(item.mac ?? item.keys, IS_MAC)
      );
      body.appendChild(row);
    }
  }
}

// 配色は設定ウィンドウと同じく設定の背景色から作り、変わったら追従する。
// 背景は設定ウィンドウと同じく「背景の不透明度」によらず常に不透明（100%）にする
function applyShortcutsTheme(settings) {
  applyTheme({ ...settings, backgroundOpacity: 100 });
}

listen("settings-changed", (event) => applyShortcutsTheme(normalizeSettings(event.payload)));

build();
applyShortcutsTheme(SETTINGS_DEFAULTS);
invoke("load_settings")
  .then((saved) => applyShortcutsTheme(normalizeSettings(saved)))
  .catch(() => {});
