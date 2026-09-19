// 設定項目の定義。設定ウィンドウ（UI生成）と本体（既定値の解決）で共有する。
// ここに 1 項目足すだけで設定ウィンドウに項目が増える。
const SETTINGS_SECTIONS = [
  {
    id: "appearance",
    label: "外観",
    items: [
      {
        key: "backgroundColor",
        label: "背景色",
        hint: "画像の周りの余白の色です",
        type: "color",
        default: "#0e0e10",
      },
      {
        key: "backgroundOpacity",
        label: "背景の不透明度",
        hint: "小さくすると余白が透けて、背後のデスクトップが見えます",
        type: "range",
        min: 20,
        max: 100,
        step: 1,
        unit: "%",
        default: 97,
      },
      {
        key: "showFilename",
        label: "ファイル名を表示する",
        hint: "マウスを動かしたときに左下へ出るファイル名と、右下の枚数表示です",
        type: "toggle",
        default: true,
      },
      {
        key: "showNavButtons",
        label: "左右の移動ボタンを表示する",
        hint: "マウスを動かしたときに出る ❮ ❯ ボタンです",
        type: "toggle",
        default: true,
      },
    ],
  },
  {
    id: "window",
    label: "ウィンドウ",
    items: [
      {
        key: "windowSizeMode",
        label: "ウィンドウのサイズ",
        hint:
          "「自由に変更」は縦横どちらにも自由に広げられ、画像を切り替えても大きさは変わりません。" +
          "「画像に合わせる」は余白が出ないよう、表示中の画像の縦横比にウィンドウの形を合わせます" +
          "（端や角をドラッグしても縦横比のまま変わります）。" +
          "どちらでも、開いたときの大きさと位置は前回閉じたときのものです。",
        type: "select",
        options: [
          ["free", "自由に変更"],
          ["image", "画像に合わせる"],
        ],
        default: "free",
      },
      {
        key: "roundedCorners",
        label: "ウィンドウの角を丸くする",
        hint: "オフにすると四角いウィンドウになります。最大化している間は常に四角です",
        type: "toggle",
        default: true,
      },
      {
        key: "alwaysOnTop",
        label: "常に最前面に表示する",
        hint: "他のアプリの後ろに隠れなくなります",
        type: "toggle",
        default: false,
      },
    ],
  },
  {
    id: "view",
    label: "表示",
    items: [
      {
        key: "startupZoom",
        label: "画像を開いたときの表示倍率",
        hint:
          "画像を開いた直後の見え方です。「ウィンドウに合わせる」でも、" +
          "画像がウィンドウより小さい場合は元の大きさのまま表示します",
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
        hint: "ドット絵などは「ドットを保つ」が見やすくなります",
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
        hint: "ホイールを回したときに拡大縮小するか、ページをめくるかを選びます",
        type: "select",
        options: [
          ["zoom", "拡大縮小"],
          ["navigate", "前後の画像へ移動"],
        ],
        default: "zoom",
      },
      {
        key: "wheelSensitivity",
        label: "ホイールの感度",
        hint: "大きいほどホイール 1 回の変化が大きくなります",
        type: "range",
        min: 1,
        max: 10,
        step: 1,
        default: 5,
      },
      {
        key: "wrapAround",
        label: "端で最初 / 最後へ折り返す",
        hint: "オフのときは端で止まり、そこが端であることを短く表示します",
        type: "toggle",
        default: false,
      },
      {
        key: "sideButtons",
        label: "マウスのサイドボタンで移動する",
        hint: "「進む / 戻る」ボタンで次 / 前の画像へ移動します",
        type: "toggle",
        default: true,
      },
    ],
  },
  {
    id: "file",
    label: "ファイル",
    items: [
      {
        key: "confirmDelete",
        label: "削除の前に確認する",
        hint:
          "Del キーや右クリックメニューから削除するとき、確認のウィンドウを出します。" +
          "その中の「今後確認しない」をチェックすると、ここもオフになります",
        type: "toggle",
        default: true,
      },
      {
        key: "watchFolder",
        label: "フォルダの変更を自動で取り込む",
        hint:
          "表示中のフォルダに画像が増減したとき、開き直さずに一覧へ反映します。" +
          "OS の通知を使うので、変化が無い間は待っているだけで負荷はかかりません。" +
          "圧縮フォルダ (zip / cbz) を開いているときは動きません",
        type: "toggle",
        default: true,
      },
    ],
  },
  {
    id: "logs",
    label: "ログ",
    items: [
      {
        key: "openLogFolder",
        label: "ログフォルダを開く",
        hint: "動作の記録と、異常終了したときの内容が入っています。不具合の報告に添えてください",
        type: "action",
        action: "openLogFolder",
        buttonLabel: "開く",
      },
    ],
  },
];

// type: "action" の行はボタンだけで、保存する値を持たない（default がない）。
// フィルタを外すと undefined が settings.json に書き込まれてしまう
const SETTINGS_DEFAULTS = Object.fromEntries(
  SETTINGS_SECTIONS.flatMap((s) =>
    s.items.filter((i) => "default" in i).map((i) => [i.key, i.default])
  )
);

// 選択肢の名前を変えたときの読み替え表（古い settings.json をそのまま読めるようにする）。
// windowSizeMode: 0.1 系までは "fixed" / "flexible" だった
const LEGACY_VALUES = {
  windowSizeMode: { fixed: "free", flexible: "image" },
};

// 保存値に既定値を補い、欠けたキーのない設定オブジェクトを作る
function normalizeSettings(raw) {
  const settings = { ...SETTINGS_DEFAULTS, ...(raw && typeof raw === "object" ? raw : {}) };
  for (const [key, table] of Object.entries(LEGACY_VALUES)) {
    const value = settings[key];
    // 継承したプロパティ（"constructor" など）を拾わないよう自前の値だけ見る
    if (Object.prototype.hasOwnProperty.call(table, value)) settings[key] = table[value];
  }
  return settings;
}
