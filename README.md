# sView

<p align="center">
  <img src="src-tauri/icons/128x128@2x.png" width="128" alt="sView">
</p>

画像・動画・音楽を開ける、Tauri 製の軽量なクロスプラットフォームのビュアーです。ウィンドウフレームを持たず、デスクトップに「画像（や動画・アートワーク）だけが浮かんでいる」ような表示を目指しています。

> **自動更新機能はありません。** 個人利用が主のため導入しない方針で、新しいバージョンは
> [Releases](https://github.com/h2ayashiii/sView/releases) から手動でダウンロードしてください。
> おかしなところがあれば [Issue](https://github.com/h2ayashiii/sView/issues) で教えてください。

## ダウンロード

[リリースページ](https://github.com/h2ayashiii/sView/releases)から、お使いの OS のファイルをダウンロードしてください。

| OS | ファイル | 説明 |
| --- | --- | --- |
| Windows | `sView_<バージョン>_x64-setup.exe` | インストーラ。`C:\Program Files\sView` に全ユーザー向けでインストールします。起動時に管理者権限（UAC）の確認が出ます |
| Windows | `sView_<バージョン>_x64-portable.exe` | インストール不要。ダウンロードしてそのまま実行できます |
| macOS | `sView_<バージョン>_<アーキテクチャ>.dmg` | 開いて `sView.app` をアプリケーションフォルダへドラッグします |

macOS 版は GitHub Actions の `macos-latest` runner でビルドしているため **Apple Silicon (arm64) 向け**です。Intel Mac では動きません。

### バージョンの読み方

バージョンは `X.Y.Z` の 3 つの数字だけで、`-beta` のような接尾辞は付けません。
GitHub のタグ（`vX.Y.Z`）・Release・配布ファイル名・設定ウィンドウに表示されるバージョンは
すべて同じ値になります。`0.1.0` が最初の正式リリースです。

## 初回起動

配布物には **Apple Developer ID による署名・公証（notarization）を行っていません**（ad-hoc 署名のみ）。Apple Developer Program（有料）への登録なしにはこれを回避できないため、初回起動時に OS がブロックします。アプリが壊れている・マルウェアが含まれているわけではありません。

### macOS

「"sView" は壊れているため開けません」または「Apple は、"sView" に Mac に損害を与えたり、プライバシーを侵害する可能性のあるマルウェアが含まれていないことを検証できませんでした。」と表示される場合、未署名アプリに付与される quarantine 属性が原因です。

`sView.app` を `/Applications` に移動したうえで、ターミナルで以下を実行して quarantine 属性を解除してください。

```sh
xattr -rd com.apple.quarantine /Applications/sView.app
```

（`.app` を別の場所に置いている場合はそのパスを指定してください。システム設定 →「プライバシーとセキュリティ」→「このまま開く」からでも起動できる場合があります）

### Windows

SmartScreen の警告が出た場合は「詳細情報」→「実行」を選択してください。

## 特徴

- **画像・動画・音楽を 1 つのアプリで** — 画像はズーム・パン、動画（mp4 / m4v / webm / mov）と音楽（mp3 / m4a / aac / flac / wav / ogg / opus）は再生操作カードで再生できます。音楽はアートワークと曲名を表示します。コーデックは同梱せず、OS が再生できる形式を再生します
- **フレームレス表示** — タイトルバー・枠なし。ウィンドウ操作ボタン・前後の移動ボタン・ファイル名などは、マウスを動かしている間だけ表示され、3 秒動かさないと消えます。設定でウィンドウの形を表示中のファイルの縦横比に合わせることもできます
- **フォルダ / 圧縮フォルダ対応** — ファイル 1 つ・フォルダ・圧縮フォルダ（zip / cbz）のどれを開いても、中のファイルを自然順（`img2.png` → `img10.png`）で前後に移動できます。一覧に並ぶのは 1 種類（画像・動画・音楽のどれか）だけです。圧縮フォルダは展開せず、表示する画像だけを取り出します
- **本モード** — 画像を 2 ページずつ並べて見開きで表示できます。左開き・右開き（日本の漫画など）を切り替えられます
- **フォルダの変更に追従** — 開いているフォルダにファイルを追加・削除すると、開き直さずに一覧へ反映されます
- **ゴミ箱へ移動** — 表示中のファイルを OS のゴミ箱へ移動できます（完全削除はしません）
- **右クリックメニューと設定** — 右クリックでメニュー、`⌘ ,`（Windows は `Ctrl` + `,`）で設定ウィンドウが開きます
- **対応形式** — 画像: avif / bmp / gif / ico / jfif / jpe / jpeg / jpg / png / svg / tif / tiff / webp、動画: mp4 / m4v / webm / mov、音楽: mp3 / m4a / aac / flac / wav / ogg / opus、圧縮フォルダ: zip / cbz（中の画像のみ）
- **対応 OS** — Windows 10/11・macOS 10.15 以降（Apple Silicon）

## 使い方

- ファイル / フォルダ / zip・cbz をウィンドウにドラッグ＆ドロップするか、`O`（ファイル）/ `D`（フォルダ）キーで開きます。画像・動画・音楽は OS の「このアプリで開く」からも開けます
- 画像: `←` `→` またはホイールで前後の画像、下部の操作カードのスライダーで好きな位置へ、`+` `-` または右ボタンを押したままホイールでズーム、`0` でウィンドウに合わせる、`B` で本モード
- 動画・音楽: `Space` で再生 / 一時停止、`←` `→` で 5 秒戻る / 進む、`↑` `↓` またはホイールで音量。前後のファイルへは再生操作カードの ⏮ ⏭ ボタンで移ります
- `F` でフルスクリーン、`Del` でゴミ箱へ移動、`Esc` で閉じます

操作の一覧は [docs/specs/controls.md](docs/specs/controls.md)、各機能の詳しい仕様は下の表のドキュメントを参照してください。

## ドキュメント

| ファイル | 内容 |
| --- | --- |
| [docs/specs/controls.md](docs/specs/controls.md) | 開き方・キーボード・マウス・右クリックメニュー |
| [docs/specs/book.md](docs/specs/book.md) | 本モード（2 ページ表示）・左開き / 右開き・ページ送り |
| [docs/specs/files.md](docs/specs/files.md) | フォルダ・圧縮フォルダの探索範囲と読み込み、関連付け、フォルダの変更への追従、ファイルの削除 |
| [docs/specs/video.md](docs/specs/video.md) | 動画の再生・再生操作・対応形式 |
| [docs/specs/audio.md](docs/specs/audio.md) | 音楽の再生・アートワークと対応形式 |
| [docs/specs/window-size.md](docs/specs/window-size.md) | ウィンドウサイズの挙動（自由に変更 / メディアに合わせる）と大きさ・位置の保存 |
| [docs/specs/settings.md](docs/specs/settings.md) | 設定項目・ログ・キャッシュの置き場所 |
| [docs/architecture.md](docs/architecture.md) | 全体構成・設計方針・プラットフォーム別の注意点 |
| [docs/development.md](docs/development.md) | 開発環境・ビルド・CI・リリース手順 |
| [CHANGELOG.md](CHANGELOG.md) | 変更履歴 |
| [CLAUDE.md](CLAUDE.md) | AI エージェント向けの指示 |

## 開発

```sh
git clone https://github.com/h2ayashiii/sView.git
cd sView
npm install
npm run dev
```

前提条件・ビルド・リリース手順は [docs/development.md](docs/development.md) を参照してください。

## ライセンス

[Commons Clause License Condition v1.0](https://commonsclause.com/) を付した MIT ライセンスで公開しています。全文は [LICENSE](LICENSE) を参照してください。

| できること | できないこと |
| --- | --- |
| 個人・法人を問わず、業務でも自由に使う | 販売する（有償バンドル、SaaS 化、有償サポートを含む） |
| ソースを改変する | 対価を得る形で提供する |
| 改変版を**無償で**再配布・公開する | |

MIT が与える権利から「Sell する権利」だけを取り除いたものです。使うこと自体は商用・非商用を問わず自由ですが、sView やその派生物の機能を実質的な価値の源泉として対価を得ることはできません。

OSI 承認のオープンソースライセンスではないため、本プロジェクトは「オープンソース」ではなく**ソースコード公開（source-available）**と表現しています。

配布バイナリに含まれる第三者ソフトウェアのライセンス表示は [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES) にあります（作り直し方は [docs/development.md](docs/development.md#タグを打ってリリースする)）。

`LICENSE` と `THIRD-PARTY-NOTICES` はインストールされるアプリにも同梱されます（Windows はインストール先の `resources`、macOS は `sView.app/Contents/Resources/`）。
