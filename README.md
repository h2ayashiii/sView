# sView

Tauri 製の軽量クロスプラットフォーム画像ビュアーです。ウィンドウフレームを持たず、デスクトップに「画像だけが浮かんでいる」ような表示を目指しています。

## 特徴

- **フレームレス表示** — タイトルバー・枠なし。ウィンドウ操作ボタン（最小化 / 最大化 / 閉じる）やファイル名はウィンドウにマウスを乗せたときだけオーバーレイ表示されます
- **フォルダ / 圧縮フォルダ対応** — 画像1枚・フォルダ・圧縮フォルダ（zip / cbz）のいずれを開いても、その中の画像を自然順（`img2.png` → `img10.png`）で前後に移動できます
- **圧縮フォルダは必要な1枚だけ展開** — zip の索引（セントラルディレクトリ）だけを読み、表示する画像のみをその都度取り出します。書庫全体をメモリやディスクに展開しないため、数GBの書庫でも起動が一瞬です
- **軽量** — フロントエンドはフレームワーク・バンドラなしの素の HTML/CSS/JS。バックエンドは Rust。UI は OS 標準の WebView（WebView2 / WKWebView）を使うため、バイナリは数MB程度です
- **対応形式** — 画像: png / jpg / jpeg / jfif / gif / webp / bmp / ico / tif / tiff / avif / svg、書庫: zip / cbz
- **対応OS** — Windows 10/11・macOS 10.15 以降（Linux でもビルド可能）

## 操作方法

### 画像を開く

画像ファイル・フォルダ・圧縮フォルダ（zip / cbz）のいずれも開けます。

- ファイル / フォルダ / zip・cbz をウィンドウにドラッグ＆ドロップ
- 起動直後の画面をクリック、または `O` キーでファイルダイアログ、`D` キーでフォルダダイアログを開く
- コマンドライン引数にパスを渡して起動（`sView path/to/image.png`、`sView path/to/folder`、`sView path/to/book.cbz`）
- OS の「このアプリで開く」（ファイル関連付け。画像と `.cbz` を関連付けます）

### キーボード

| キー | 動作 |
| --- | --- |
| `→` `↓` `PageDown` `Space` | 次の画像 |
| `←` `↑` `PageUp` `Backspace` | 前の画像 |
| `Home` / `End` | 最初 / 最後の画像 |
| `+` / `-` | ズームイン / アウト |
| `0` | ウィンドウにフィット |
| `1` | 等倍（100%）表示 |
| `O` | ファイル（画像 / 圧縮フォルダ）を開く |
| `D` | フォルダを開く |
| `R` | 再スキャン |
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

| OS | 実行ファイル（インストール不要のポータブル版） | インストーラ |
| --- | --- | --- |
| Windows | `src-tauri/target/release/sview.exe` | `src-tauri/target/release/bundle/nsis/`（NSIS インストーラ。実行するとアプリ本体一式を Program Files にインストール） |
| macOS | `src-tauri/target/release/bundle/macos/sView.app` | `src-tauri/target/release/bundle/dmg/` |

GitHub Actions（`.github/workflows/build.yml`）で Windows / macOS のバイナリを自動ビルドしています。手元にビルド環境がない場合は Actions の成果物（Artifacts）を利用してください。Windows の Artifacts にはインストーラ（`*-setup.exe`）に加えて、インストール不要でそのまま実行できるポータブル版の `sview.exe` 単体も含まれます。

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

### Linux（Ubuntu）から Windows 向けバイナリをクロスビルドする

macOS 実機や CI（`macos-latest`）を使わずに開発機（Ubuntu）だけで完結させたい場合、Windows 向けバイナリは [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin) を使うことで Linux 上からクロスビルドできます（Ubuntu 上で動作確認済み）。

前提パッケージのインストール:

```sh
# リンクに使う clang / lld と、NSIS インストーラを生成する makensis
sudo apt-get install -y clang lld llvm nsis

rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
```

ビルド（Tauri CLI に `--runner cargo-xwin` を指定するのが重要です。省略すると通常の `cc`(GCC) が呼ばれてリソースコンパイルに失敗します）:

```sh
npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis
```

初回実行時に Windows SDK / MSVC CRT ヘッダーが自動ダウンロードされます（Microsoft の再配布可能条件下で提供されているもので、`cargo-xwin` が取得・管理します）。生成物:

- 実行ファイル: `src-tauri/target/x86_64-pc-windows-msvc/release/sview.exe`
- インストーラ: `src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/sView_<version>_x64-setup.exe`

制約事項:

- MSI（WiX）バンドルは生成しません（クロスビルドでの不安定さもあり、配布物は NSIS インストーラ1本に統一しています。`tauri.conf.json` の `bundle.targets` で `nsis` のみ指定）
- コード署名（Authenticode）は行われません。これは GitHub Actions の `windows-latest` ビルドでも同様です
- WebView2 ランタイムの検証など、一部の実行時挙動は実機の Windows での確認を推奨します

**macOS 向けバイナリは Linux からクロスビルドできません。** Apple の開発者利用許諾により macOS SDK を Linux 上で正規に取得・利用することができず、また `.app`/`.dmg` の生成や署名・公証には `codesign` などの macOS ネイティブツールが必要なため、Tauri でも公式にサポートされていません。macOS 向けビルドは引き続き macOS 実機か、CI の `macos-latest` runner（`.github/workflows/build.yml`）を利用してください。

### 未署名バイナリの実行

配布物は Apple Developer ID による署名・公証（notarization）を行っていません（ad-hoc 署名のみ）。Apple Developer Program（有料）への登録なしにはこれを回避できないため、ダウンロードした `.dmg` / `.app` は macOS の Gatekeeper と quarantine 属性の対象になり、初回起動時にブロックされます。

- **macOS**: 「"sView" は壊れているため開けません」または「Apple は、"sView" に Mac に損害を与えたり、プライバシーを侵害する可能性のあるマルウェアが含まれていないことを検証できませんでした。」と表示される場合、アプリが壊れている・マルウェアが含まれているわけではなく、未署名アプリに付与される quarantine 属性が原因です。`sView.app` を `/Applications` に移動したうえで、ターミナルで以下を実行して quarantine 属性を解除してください

  ```sh
  xattr -cr /Applications/sView.app
  ```

  （`.dmg` からドラッグ＆ドロップでインストールする運用を想定しています。`.app` を別の場所に置いている場合はそのパスを指定してください。システム設定 →「プライバシーとセキュリティ」→「このまま開く」からでも起動できる場合があります）

- **Windows**: SmartScreen の警告が出た場合は「詳細情報」→「実行」を選択してください
