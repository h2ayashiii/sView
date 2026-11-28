// ショートカット定義（Single Source of Truth）。
// shortcuts.js（一覧表示）と main.js（キー操作の判定）の両方がこの定義を使う。
// win / mac が同じ操作なら keys だけを書く。macOS の Delete キーは Backspace として届く

export const SHORTCUT_SECTIONS = [
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
      { label: "ファイルの情報を開く / 閉じる", keys: "I" },
      { label: "全画面表示の切り替え", keys: "F" },
      { label: "ゴミ箱へ移動", win: "Delete", mac: "⌘ + Delete / fn + Delete" },
      { label: "設定を開く", win: "Ctrl + ,", mac: "⌘ + ," },
      { label: "メニューを閉じる / 終了", keys: "Esc" },
    ],
  },
  {
    label: "マウス",
    items: [
      { label: "次 / 前のファイル", keys: "サイドボタン 進む / 戻る" },
      {
        label: "次 / 前の画像（動画・音楽では 3 秒進む / 戻る）",
        keys: "ホイール 下 / 上（横のホイールは 右 / 左）",
      },
      { label: "ズーム（画像）", keys: "右ボタンを押したまま / Ctrl + ホイール" },
      { label: "音量（動画・音楽）", keys: "右ボタンを押したまま / Ctrl + ホイール" },
      { label: "再生 / 一時停止（動画・音楽）", keys: "クリック" },
      { label: "ウィンドウの移動（ズーム中は画像の移動）", keys: "ドラッグ" },
      { label: "メニューを開く", keys: "右クリック（長押しでは開かない）" },
      { label: "最大化 / 元のサイズ", keys: "上部をダブルクリック" },
    ],
  },
];

// 動画・音楽を表示している間だけのキー割り当て。
// 画像の前後移動キー（PageUp / PageDown / Backspace / Home / End）は誤操作防止のため
// ここで飲み込む（action: "noop"）
/** @type {Record<string, { action: string, ignoreRepeat?: boolean }>} */
const MEDIA_KEY_MAP = {
  " ": { action: "togglePlay", ignoreRepeat: true },
  k: { action: "togglePlay", ignoreRepeat: true },
  K: { action: "togglePlay", ignoreRepeat: true },
  m: { action: "toggleMute", ignoreRepeat: true },
  M: { action: "toggleMute", ignoreRepeat: true },
  j: { action: "seekBack" },
  J: { action: "seekBack" },
  ArrowLeft: { action: "seekBack" },
  l: { action: "seekForward" },
  L: { action: "seekForward" },
  ArrowRight: { action: "seekForward" },
  ArrowUp: { action: "volumeUp" },
  ArrowDown: { action: "volumeDown" },
  PageUp: { action: "noop" },
  PageDown: { action: "noop" },
  Backspace: { action: "noop" },
  Home: { action: "noop" },
  End: { action: "noop" },
};

// 画像・共通のキー割り当て（修飾キーなし）
/** @type {Record<string, { action: string, preventDefault?: boolean, ignoreRepeat?: boolean, bookOnly?: boolean }>} */
const VIEWER_KEY_MAP = {
  ArrowRight: { action: "stepNext", preventDefault: true },
  PageDown: { action: "stepNext", preventDefault: true },
  " ": { action: "stepNext", preventDefault: true },
  ArrowLeft: { action: "stepPrev", preventDefault: true },
  PageUp: { action: "stepPrev", preventDefault: true },
  Backspace: { action: "stepPrev", preventDefault: true },
  ArrowUp: { action: "stepBookPrev", preventDefault: true, bookOnly: true },
  ArrowDown: { action: "stepBookNext", preventDefault: true, bookOnly: true },
  Home: { action: "goFirst" },
  End: { action: "goLast" },
  b: { action: "toggleBookMode", ignoreRepeat: true },
  B: { action: "toggleBookMode", ignoreRepeat: true },
  "+": { action: "zoomIn" },
  "=": { action: "zoomIn" },
  "-": { action: "zoomOut" },
  0: { action: "fitMode" },
  1: { action: "zoomActual" },
  o: { action: "openDialog" },
  O: { action: "openDialog" },
  d: { action: "openFolderDialog" },
  D: { action: "openFolderDialog" },
  i: { action: "toggleInfo", ignoreRepeat: true },
  I: { action: "toggleInfo", ignoreRepeat: true },
  r: { action: "rescan" },
  R: { action: "rescan" },
  f: { action: "toggleFullscreen" },
  F: { action: "toggleFullscreen" },
  Delete: { action: "deleteCurrent", preventDefault: true },
  Escape: { action: "escape" },
};

/**
 * キーボードイベントから実行すべきアクションを解決する。
 * 該当しない場合は null を返す。
 *
 * @param {{ key: string, metaKey?: boolean, ctrlKey?: boolean, altKey?: boolean, repeat?: boolean }} e
 * @param {{ isMac: boolean, showingMedia: boolean, showingBook: boolean }} ctx
 * @returns {{ action: string, preventDefault: boolean } | null}
 */
export function resolveShortcutAction(e, { isMac, showingMedia, showingBook }) {
  // 設定を開くショートカット（macOS: Command + , / その他: Ctrl + ,）
  if (e.key === "," && (isMac ? e.metaKey : e.ctrlKey) && !e.altKey) {
    return { action: "openSettings", preventDefault: true };
  }
  // macOS で「ゴミ箱に入れる」は Command + Delete。この Delete は Backspace として
  // 届くので、修飾キーで弾く前にここで拾う（単体の Backspace は前の画像のまま）
  if (isMac && e.key === "Backspace" && e.metaKey && !e.ctrlKey && !e.altKey) {
    return { action: "deleteCurrent", preventDefault: true };
  }
  if (e.metaKey || e.ctrlKey || e.altKey) return null;

  if (showingMedia) {
    const entry = Object.prototype.hasOwnProperty.call(MEDIA_KEY_MAP, e.key)
      ? MEDIA_KEY_MAP[e.key]
      : null;
    if (entry) {
      if (entry.ignoreRepeat && e.repeat) return { action: "noop", preventDefault: true };
      return { action: entry.action, preventDefault: true };
    }
  }

  const entry = Object.prototype.hasOwnProperty.call(VIEWER_KEY_MAP, e.key)
    ? VIEWER_KEY_MAP[e.key]
    : null;
  if (!entry) return null;
  if (entry.bookOnly && !showingBook) return null;
  if (entry.ignoreRepeat && e.repeat) return null;
  return { action: entry.action, preventDefault: !!entry.preventDefault };
}
