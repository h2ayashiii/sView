# 全体構成

## 設計方針

- **軽量** — フロントエンドはフレームワーク・バンドラなしの素の HTML/CSS/JS。バックエンドは Rust。UI は OS 標準の WebView（WebView2 / WKWebView）を使うため、バイナリは数MB程度です
- 動画・音楽はコーデックを同梱せず、OS の WebView が再生できる形式だけを扱います（[動画](specs/video.md)・[音楽](specs/audio.md)）
- 通信は一切しません（自動更新もありません）

## プロジェクト構成

```
sView/
├── src/                     # フロントエンド（素の HTML/CSS/JS、ビルド不要。ネイティブ ES Modules で読み込む）
│   ├── index.html           # 本体ウィンドウ（<script type="module" src="main.js">）
│   ├── main.js              # 本体ウィンドウのエントリーポイント（表示制御、ナビゲーション、再生、ウィンドウ制御、入力処理）
│   ├── viewer/              # 本体ウィンドウの責務別 ES モジュール
│   │   ├── media-types.js   # 対応拡張子・MIME・パス/時間/色のユーティリティ
│   │   ├── archive-cache.js # 書庫内画像の遅延ロード・LRU blob キャッシュ
│   │   ├── audio-tracks.js  # 音声トラックの一覧・選択・ラベル生成
│   │   ├── subtitles.js     # MKV 字幕の読み込み・VTTCue 表示・メニュー項目生成
│   │   ├── tooltips.js      # スライダーの塗り分け・吹き出し位置計算
│   │   ├── wheel.js         # ホイール操作の軸固定・感度しきい値・音量調整
│   │   ├── delete-confirm.js # 削除確認ダイアログ
│   │   └── context-menu.js  # 右クリックメニュー・サブメニューの表示と操作
│   ├── mkv-player.js        # macOS で MKV を MSE で再生する（Rust が詰め直した fragmented MP4 を流す）
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
│   ├── src/
│   │   ├── main.rs          # sview_lib::run() を呼ぶだけ
│   │   ├── lib.rs           # Tauri アプリ起動（run()）・コマンド登録・ウィンドウ生成/終了ライフサイクル
│   │   ├── media.rs         # 対応拡張子・MediaKind / WindowKindKey・自然順ソート
│   │   ├── state.rs         # ウィンドウごとの管理状態（PerWindow・AspectLock・FolderWatcher 等）
│   │   ├── archive.rs       # zip / cbz 書庫のキャッシュ・一覧・画像取り出し
│   │   ├── listing.rs       # フォルダ/書庫の一覧作成・ゴミ箱への移動（trash）
│   │   ├── audio.rs         # 音楽のタグとアートワークの読み取り（lofty）
│   │   ├── watcher.rs       # フォルダの変更監視（notify）
│   │   ├── window.rs        # ウィンドウの大きさ・位置の保存（window.json）・縦横比維持・ドラッグ制御
│   │   ├── settings.rs      # 設定（settings.json）の読み書き・設定/ショートカットウィンドウの表示
│   │   ├── logging.rs       # ログ初期化・パニックフック・ファイルマネージャーで表示
│   │   ├── assoc.rs         # OS への拡張子関連付けの共通インターフェースとコマンド
│   │   ├── mkv/             # MKV 字幕抽出（subtitles.rs）・macOS 向けリマックス（remux.rs）・fMP4 書き出し（fmp4.rs）
│   │   ├── platform/        # OS 固有実装（windows.rs: レジストリ関連付け・WM_SIZING / macos.rs: LaunchServices・NSTrackingArea 等）
│   │   └── tests.rs         # ユニットテスト
│   ├── tauri.conf.json      # ウィンドウ設定（フレームレス等）・CSP・バンドル設定・関連付け
│   ├── testdata/            # テストで読むファイル（字幕入りの小さな MKV、詰め直しを確かめる MKV）
│   ├── capabilities/        # ウィンドウごとにフロントエンドへ許可する API の定義
│   ├── installer-hooks.nsh  # Windows インストーラ（NSIS）のフック。旧ユーザー単位インストールの後始末
│   ├── installer/           # インストーラの見た目（Windows のインストーラの画像、macOS の dmg の背景）
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
├── .claude/rules/           # AI エージェント向けの実装メモ（話題別。該当ファイルを触るときだけ読み込まれる）
├── LICENSE                  # Commons Clause + MIT
└── THIRD-PARTY-NOTICES      # 配布バイナリに含まれる第三者ソフトウェアのライセンス表示
```

バックエンド（`src-tauri/src/`）が受け持つのは次の処理です。

- ファイル一覧の作成（`listing.rs` / `media.rs`）と、圧縮フォルダからの画像の取り出し（`archive.rs`）
- フォルダの変更の監視（`watcher.rs` / `notify`）、ゴミ箱への移動（`listing.rs` / `trash`）
- 音楽のタグとアートワークの読み取り（`audio.rs` / `lofty`）
- MKV のテキスト字幕（SRT / ASS）の取り出し（`mkv/subtitles.rs`）と、macOS での MKV 再生のための fragmented MP4 リマックス（`mkv/remux.rs` / `mkv/fmp4.rs`）
- 設定（`settings.rs` / `settings.json`）とウィンドウの大きさ・位置（`window.rs` / `window.json`）の保存・縦横比維持
- 起動時に渡されたファイルの受け取り（`lib.rs`）、OS への関連付け（`assoc.rs` / `platform/`）、ログ（`logging.rs`）

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
