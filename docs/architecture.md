# 全体構成

## 設計方針

- **軽量** — フロントエンドはフレームワーク・バンドラなしの素の HTML/CSS/JS。バックエンドは Rust。UI は OS 標準の WebView（WebView2 / WKWebView）を使うため、バイナリは数MB程度です
- 動画・音楽はコーデックを同梱せず、OS の WebView が再生できる形式だけを扱います（[動画](specs/video.md)・[音楽](specs/audio.md)）
- 通信は一切しません（自動更新もありません）

## プロジェクト構成

```
sView/
├── src/                     # フロントエンド（素の HTML/CSS/JS、ビルド不要。<script src> で読み込む）
│   ├── index.html           # 本体ウィンドウ
│   ├── main.js              # 画像・動画・音楽の表示、前後移動、ズーム、再生操作、本モード、入力処理、右クリックメニュー
│   ├── style.css            # オーバーレイ UI（自動表示・自動非表示）・再生操作カード・コンテキストメニュー
│   ├── settings.html        # 設定ウィンドウ
│   ├── settings.js          # 設定ウィンドウの UI 生成・保存
│   ├── settings.css         # 設定ウィンドウとショートカット一覧の見た目（本体と同じ配色）
│   ├── settings-defs.js     # 設定項目の定義（本体と設定ウィンドウで共有）
│   ├── shortcuts.html       # ショートカット一覧ウィンドウ（設定ウィンドウから開く）
│   ├── shortcuts.js         # ショートカット一覧の内容（main.js のキー操作の写し）
│   ├── theme.js             # 設定ウィンドウとショートカット一覧の配色を設定から作る
│   └── audio-artwork.svg    # アートワークの無い音楽ファイルに出す既定の絵
├── src-tauri/
│   ├── src/main.rs          # sview_lib::run() を呼ぶだけ
│   ├── src/lib.rs           # バックエンドの処理すべて（下記）とテスト
│   ├── tauri.conf.json      # ウィンドウ設定（フレームレス等）・CSP・バンドル設定・関連付け
│   ├── capabilities/        # ウィンドウごとにフロントエンドへ許可する API の定義
│   ├── installer-hooks.nsh  # Windows インストーラ（NSIS）のフック。旧ユーザー単位インストールの後始末
│   └── icons/               # アプリのアイコン
├── docs/                    # 仕様（specs/）・全体構成・開発手順
├── scripts/                 # ビルドスクリプト、リリース時にタグのバージョンを反映する set-version.mjs、
│                            # THIRD-PARTY-NOTICES を作り直す gen-third-party-notices.mjs
├── .github/
│   ├── workflows/ci.yml     # PR / main への push で cargo test
│   ├── workflows/build.yml  # タグ・手動実行で配布ビルド（タグはリリースも）
│   ├── release-notes/       # タグ名と同じ .md を置くとリリースノートに使われる
│   └── ISSUE_TEMPLATE/      # 不具合報告のテンプレート
├── CHANGELOG.md             # 変更履歴
├── CLAUDE.md                # AI エージェント向けの指示
├── LICENSE                  # Commons Clause + MIT
└── THIRD-PARTY-NOTICES      # 配布バイナリに含まれる第三者ソフトウェアのライセンス表示
```

`lib.rs` が受け持つのは次の処理です。

- ファイル一覧の作成（フォルダ・圧縮フォルダの探索、種類の判定、自然順ソート）と、圧縮フォルダからの画像の取り出し
- フォルダの変更の監視（`notify`）、ゴミ箱への移動（`trash`）
- 音楽のタグとアートワークの読み取り（`lofty`）
- 設定（`settings.json`）とウィンドウの大きさ・位置（`window.json`）の保存
- 「メディアに合わせる」の縦横比の維持と、ウィンドウの大きさ・位置の制御
- 起動時に渡されたファイルの受け取り、OS への関連付け、ログ

## ウィンドウ

| ウィンドウ | ページ | 内容 |
| --- | --- | --- |
| `main` | `index.html` | 本体。macOS で Finder / Dock から追加で開いたファイルは `viewer-N` という同じ作りのウィンドウで開く |
| `settings` | `settings.html` | 設定ウィンドウ。閉じても隠すだけ |
| `shortcuts` | `shortcuts.html` | ショートカット一覧。閉じても隠すだけ |

どれも `tauri.conf.json` に定義し、起動時に Rust 側（`create_configured_windows`）で作ります。許可する API はウィンドウごとに `capabilities/` の別ファイル（`default.json` / `settings.json` / `shortcuts.json`）で決めています。

## プラットフォーム別の注意点

### Windows — カラープロファイル

Windows は色管理をアプリケーションレベルで行うため、ビュアー側で ICC プロファイルを扱う必要があります。sView は画像を**再エンコードせず元ファイルのまま** WebView2（Chromium）に渡して表示します。Chromium は埋め込み ICC プロファイルを解釈し、モニタのカラープロファイルへ変換して描画するため、広色域モニタでも色が過飽和にならず正しく表示されます（プロファイルなしの画像は sRGB として扱われます）。

### macOS — マウスボタン

macOS はマウスの「進む/戻る」ボタンの扱いが Windows と異なり、OS 標準の機能ではありません。sView はサイドボタンを DOM の `button 3 / 4` イベントとして直接ハンドリングするため、多くのマウス（Logicool 等）でそのまま動作します。ベンダー製ユーティリティ（Logi Options+ など）がボタンを別機能に割り当てている場合は、そちらの設定が優先される点に注意してください。その場合は、画像ではキーボードの `←` `→`、動画・音楽では再生操作カードの ⏮ ⏭ ボタンで前後のファイルへ移れます。

また、フレームレス・透過ウィンドウの実現に `macOSPrivateApi` を使用しています。Mac App Store 配布を行う場合はこの設定を外す必要があります。
