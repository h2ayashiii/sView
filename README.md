# sView

Tauri 製の軽量クロスプラットフォーム画像ビュアーです。ウィンドウフレームを持たず、デスクトップに「画像だけが浮かんでいる」ような表示を目指しています。

## 特徴

- **フレームレス表示** — タイトルバー・枠なし。ウィンドウ操作ボタン（最小化 / 最大化 / 閉じる）やファイル名はウィンドウにマウスを乗せたときだけオーバーレイ表示されます
- **同一フォルダ内ナビゲーション** — 画像を1枚開くと、同じフォルダ内の画像を自然順（`img2.png` → `img10.png`）で前後に移動できます
- **軽量** — フロントエンドはフレームワーク・バンドラなしの素の HTML/CSS/JS。バックエンドは Rust。UI は OS 標準の WebView（WebView2 / WKWebView）を使うため、バイナリは数MB程度です
- **対応形式** — png / jpg / jpeg / jfif / gif / webp / bmp / ico / tif / tiff / avif / svg
- **対応OS** — Windows 10/11・macOS 10.15 以降（Linux でもビルド可能）

## 操作方法

### 画像を開く

- 画像ファイルをウィンドウにドラッグ＆ドロップ
- 起動直後の画面をクリック、または `O` キーでファイルダイアログを開く
- コマンドライン引数に画像パスを渡して起動（`sView path/to/image.png`）
- OS の「このアプリで開く」（ファイル関連付け）

### キーボード

| キー | 動作 |
| --- | --- |
| `→` `↓` `PageDown` `Space` | 次の画像 |
| `←` `↑` `PageUp` `Backspace` | 前の画像 |
| `Home` / `End` | 最初 / 最後の画像 |
| `+` / `-` | ズームイン / アウト |
| `0` | ウィンドウにフィット |
| `1` | 等倍（100%）表示 |
| `O` | ファイルを開く |
| `R` | フォルダを再スキャン |
| `F` | フルスクリーン切り替え |
| `Esc` | 終了 |

### マウス

| 操作 | 動作 |
| --- | --- |
| 「進む」ボタン（サイドボタン） | 次の画像 |
| 「戻る」ボタン（サイドボタン） | 前の画像 |
| ホイール | カーソル位置を中心にズーム |
| ドラッグ（フィット表示時） | ウィンドウの移動 |
| ドラッグ（ズーム中） | 画像のパン |
| ホバー時の左右の `❮` `❯` ボタン | 前 / 次の画像 |
| ダブルクリック | 最大化 / 復元 |

## 開発環境のセットアップ

必要なもの:

- [Rust](https://rustup.rs/)（stable）
- [Node.js](https://nodejs.org/)（18 以降）
- OS 別の前提条件（[Tauri 公式ドキュメント](https://tauri.app/start/prerequisites/)）
  - **Windows**: Microsoft C++ Build Tools、WebView2 ランタイム（Windows 11 は同梱済み）
  - **macOS**: Xcode Command Line Tools（`xcode-select --install`）

```sh
git clone https://github.com/h2ayashiii/sView.git
cd sView
npm install
```

## 実行・ビルド

```sh
# 開発モードで起動（ホットリロード付き）
npm run dev

# リリースビルド（インストーラも生成）
npm run build

# デバッグビルド（ビルドが速い・devtools 有効）
npm run build:debug
```

同梱のビルドスクリプトからも実行できます:

```sh
# macOS / Linux
./scripts/build.sh

# Windows (PowerShell)
.\scripts\build.ps1
```

ビルド成果物の場所:

| OS | 実行ファイル | インストーラ |
| --- | --- | --- |
| Windows | `src-tauri/target/release/sview.exe` | `src-tauri/target/release/bundle/msi/`・`bundle/nsis/` |
| macOS | `src-tauri/target/release/bundle/macos/sView.app` | `src-tauri/target/release/bundle/dmg/` |

GitHub Actions（`.github/workflows/build.yml`）で Windows / macOS のバイナリを自動ビルドしています。手元にビルド環境がない場合は Actions の成果物（Artifacts）を利用してください。

## プロジェクト構成

```
sView/
├── src/                  # フロントエンド（素の HTML/CSS/JS、ビルド不要）
│   ├── index.html
│   ├── main.js           # 画像ナビゲーション・ズーム・入力処理
│   └── style.css         # ホバー時オーバーレイUIなど
├── src-tauri/
│   ├── src/lib.rs        # フォルダスキャン・自然順ソート・起動ファイル処理
│   ├── tauri.conf.json   # ウィンドウ設定（フレームレス等）・バンドル設定
│   └── capabilities/     # フロントエンドに許可する API の定義
└── scripts/              # ビルドスクリプト
```

## プラットフォーム別の注意点

### Windows — カラープロファイル

Windows は色管理をアプリケーションレベルで行うため、ビュアー側で ICC プロファイルを扱う必要があります。sView は画像を**再エンコードせず元ファイルのまま** WebView2（Chromium）に渡して表示します。Chromium は埋め込み ICC プロファイルを解釈し、モニタのカラープロファイルへ変換して描画するため、広色域モニタでも色が過飽和にならず正しく表示されます（プロファイルなしの画像は sRGB として扱われます）。

### macOS — マウスボタン

macOS はマウスの「進む/戻る」ボタンの扱いが Windows と異なり、OS 標準の機能ではありません。sView はサイドボタンを DOM の `button 3 / 4` イベントとして直接ハンドリングするため、多くのマウス（Logicool 等）でそのまま動作します。ベンダー製ユーティリティ（Logi Options+ など）がボタンを別機能に割り当てている場合は、そちらの設定が優先される点に注意してください。その場合はキーボードの `←` `→` を利用できます。

また、フレームレス・透過ウィンドウの実現に `macOSPrivateApi` を使用しています。Mac App Store 配布を行う場合はこの設定を外す必要があります。

### 未署名バイナリの実行

- **macOS**: 署名なしのため、初回起動時に「開発元を検証できません」と表示されます。`.app` を右クリック →「開く」、または `xattr -cr sView.app` で回避できます
- **Windows**: SmartScreen の警告が出た場合は「詳細情報」→「実行」を選択してください

## ライセンス

MIT
