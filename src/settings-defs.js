// 設定項目の定義。設定ウィンドウ（UI生成）と本体（既定値の解決）で共有する。
// ここに 1 項目足すだけで設定ウィンドウに項目が増える。
const SETTINGS_SECTIONS = [
  {
    id: "appearance",
    label: "外観",
    items: [
      { key: "backgroundColor", label: "背景色", type: "color", default: "#0e0e10" },
      {
        key: "backgroundOpacity",
        label: "背景の不透明度",
        hint: "小さくするとウィンドウが透けます",
        type: "range",
        min: 20,
        max: 100,
        step: 1,
        unit: "%",
        default: 97,
      },
      { key: "roundedCorners", label: "ウィンドウの角を丸くする", type: "toggle", default: true },
      { key: "showFilename", label: "上部にファイル名を表示する", type: "toggle", default: true },
      { key: "showNavButtons", label: "左右の移動ボタンを表示する", type: "toggle", default: true },
    ],
  },
  {
    id: "view",
    label: "表示",
    items: [
      {
        key: "startupZoom",
        label: "画像を開いたときの表示",
        type: "select",
        options: [
          ["fit", "ウィンドウに合わせる"],
          ["actual", "等倍 (100%)"],
        ],
        default: "fit",
      },
      {
        key: "imageRendering",
        label: "拡大したときの補間",
        type: "select",
        options: [
          ["auto", "なめらか"],
          ["pixelated", "ドットを保つ"],
        ],
        default: "auto",
      },
      {
        key: "preload",
        label: "前後の画像を先読みする",
        hint: "めくりが速くなりますが、メモリを少し多く使います",
        type: "toggle",
        default: true,
      },
    ],
  },
  {
    id: "input",
    label: "操作",
    items: [
      {
        key: "wheelAction",
        label: "ホイールの動作",
        type: "select",
        options: [
          ["zoom", "拡大縮小"],
          ["navigate", "前後の画像へ移動"],
        ],
        default: "zoom",
      },
      { key: "wheelSensitivity", label: "ホイールの感度", type: "range", min: 1, max: 10, step: 1, default: 5 },
      { key: "wrapAround", label: "端で最初 / 最後へ折り返す", type: "toggle", default: false },
      { key: "sideButtons", label: "マウスのサイドボタンで移動する", type: "toggle", default: true },
    ],
  },
  {
    id: "window",
    label: "ウィンドウ",
    items: [{ key: "alwaysOnTop", label: "常に最前面に表示する", type: "toggle", default: false }],
  },
];

const SETTINGS_DEFAULTS = Object.fromEntries(
  SETTINGS_SECTIONS.flatMap((s) => s.items.map((i) => [i.key, i.default]))
);

// 保存値に既定値を補い、欠けたキーのない設定オブジェクトを作る
function normalizeSettings(raw) {
  return { ...SETTINGS_DEFAULTS, ...(raw && typeof raw === "object" ? raw : {}) };
}
