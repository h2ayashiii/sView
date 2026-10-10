// ショートカット一覧ウィンドウ。
// キー操作は main.js の keydown / mouseup / wheel の処理にそのまま書かれていて、
// 対応表は持っていない。ここはその表示用の写しなので、main.js の操作を変えたら
// こちらも合わせて直すこと。
// win / mac が同じ操作なら keys だけを書く。macOS の Delete キーは Backspace として届く
const SHORTCUT_SECTIONS = [
  {
    label: "画像",
    items: [
      { label: "次の画像", keys: "→ / PageDown / Space" },
      { label: "前の画像", win: "← / PageUp / Backspace", mac: "← / PageUp / Delete" },
      { label: "最初 / 最後の画像", keys: "Home / End" },
      { label: "ズームイン / アウト", keys: "+ / -" },
      { label: "ウィンドウに合わせる", keys: "0" },
      { label: "等倍 (100%)", keys: "1" },
    ],
  },
  {
    label: "本モード（画像を 2 ページ並べる）",
    items: [
      { label: "本モードの切り替え", keys: "B" },
      { label: "次 / 前の見開き（2 ページずつ）", keys: "→ / ←" },
      { label: "1 ページずつずらす（次 / 前）", keys: "↓ / ↑" },
      { label: "最初 / 最後の見開き", keys: "Home / End" },
    ],
  },
  {
    label: "動画・音楽",
    items: [
      { label: "再生 / 一時停止", keys: "Space / K" },
      { label: "5 秒戻る / 進む", keys: "← / →、J / L" },
      { label: "音量を上げる / 下げる", keys: "↑ / ↓" },
      { label: "消音の切り替え", keys: "M" },
    ],
  },
  {
    label: "共通",
    items: [
      { label: "ファイルを開く", keys: "O" },
      { label: "フォルダを開く", keys: "D" },
      { label: "再読み込み", keys: "R" },
      { label: "全画面表示の切り替え", keys: "F" },
      { label: "ゴミ箱へ移動", win: "Delete", mac: "⌘ + Delete / fn + Delete" },
      { label: "設定を開く", win: "Ctrl + ,", mac: "⌘ + ," },
      { label: "メニューを閉じる / 終了", keys: "Esc" },
    ],
  },
  {
    label: "マウス",
    items: [
      { label: "次 / 前の画像（動画・音楽では 5 秒進む / 戻る）", keys: "サイドボタン 進む / 戻る" },
      { label: "ズーム（動画・音楽では音量）", keys: "ホイール" },
      { label: "メニューを開く", keys: "右クリック" },
      { label: "最大化 / 元のサイズ", keys: "上部をダブルクリック" },
    ],
  },
];

const tauri = window.__TAURI__ ?? {};
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

// 配色は設定ウィンドウと同じく設定の背景色から作り、変わったら追従する
listen("settings-changed", (event) => applyTheme(normalizeSettings(event.payload)));

build();
applyTheme(SETTINGS_DEFAULTS);
invoke("load_settings")
  .then((saved) => applyTheme(normalizeSettings(saved)))
  .catch(() => {});
