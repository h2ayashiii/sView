# 全体構成

## 設計方針

- **軽量** — フロントエンドはフレームワーク・バンドラなしの素の HTML/CSS/JS。バックエンドは Rust。UI は OS 標準の WebView（WebView2 / WKWebView）を使うため、バイナリは数MB程度です
- 動画・音楽はコーデックを同梱せず、OS の WebView が再生できる形式だけを扱います（[動画](specs/video.md)・[音楽](specs/audio.md)）
- 通信は一切しません（自動更新もありません）

## プロジェクト構成

```
sView/
├── src/                  # フロントエンド（素の HTML/CSS/JS、ビルド不要）
│   ├── index.html
│   ├── main.js           # 画像ナビゲーション・ズーム・入力処理・右クリックメニュー
│   ├── style.css         # オーバーレイUI（自動表示・自動非表示）・コンテキストメニュー
│   ├── settings.html     # 設定ウィンドウ
│   ├── settings.js       # 設定ウィンドウのUI生成・保存
│   ├── settings.css      # 設定ウィンドウの見た目（本体と同じ配色）
│   ├── settings-defs.js  # 設定項目の定義（本体と設定ウィンドウで共有）
│   └── audio-artwork.svg # アートワークの無い音楽ファイルに出す既定の絵
├── src-tauri/
│   ├── src/lib.rs        # フォルダスキャン・自然順ソート・起動ファイル処理・設定とウィンドウ状態（大きさ・位置）の保存・音楽のタグとアートワークの読み取り
│   ├── tauri.conf.json   # ウィンドウ設定（フレームレス等）・バンドル設定・バージョン
│   └── capabilities/     # フロントエンドに許可する API の定義
├── scripts/              # ビルドスクリプト、リリース時にタグのバージョンを反映する set-version.mjs、
│                         # THIRD-PARTY-NOTICES を作り直す gen-third-party-notices.mjs
├── .github/
│   ├── workflows/build.yml   # PR / main では cargo test、タグでは配布ビルドとリリース
│   ├── release-notes/        # タグ名と同じ .md を置くとリリースノートに使われる
│   └── ISSUE_TEMPLATE/       # 不具合報告のテンプレート
├── LICENSE               # Commons Clause + MIT
└── THIRD-PARTY-NOTICES   # 配布バイナリに含まれる第三者ソフトウェアのライセンス表示
```

## プラットフォーム別の注意点

### Windows — カラープロファイル

Windows は色管理をアプリケーションレベルで行うため、ビュアー側で ICC プロファイルを扱う必要があります。sView は画像を**再エンコードせず元ファイルのまま** WebView2（Chromium）に渡して表示します。Chromium は埋め込み ICC プロファイルを解釈し、モニタのカラープロファイルへ変換して描画するため、広色域モニタでも色が過飽和にならず正しく表示されます（プロファイルなしの画像は sRGB として扱われます）。

### macOS — マウスボタン

macOS はマウスの「進む/戻る」ボタンの扱いが Windows と異なり、OS 標準の機能ではありません。sView はサイドボタンを DOM の `button 3 / 4` イベントとして直接ハンドリングするため、多くのマウス（Logicool 等）でそのまま動作します。ベンダー製ユーティリティ（Logi Options+ など）がボタンを別機能に割り当てている場合は、そちらの設定が優先される点に注意してください。その場合はキーボードの `←` `→` を利用できます。

また、フレームレス・透過ウィンドウの実現に `macOSPrivateApi` を使用しています。Mac App Store 配布を行う場合はこの設定を外す必要があります。
