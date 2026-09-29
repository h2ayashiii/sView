# sView

Tauri 製の軽量クロスプラットフォーム画像ビュアーです。ウィンドウフレームを持たず、デスクトップに「画像だけが浮かんでいる」ような表示を目指しています。

> **自動更新機能はありません。** 個人利用が主のため導入しない方針で、新しいバージョンは
> [Releases](https://github.com/h2ayashiii/sView/releases) から手動でダウンロードしてください。
> おかしなところがあれば [Issue](https://github.com/h2ayashiii/sView/issues) で教えてください。

<p align="center">
  <img src="https://github.com/user-attachments/assets/61120f57-3b6c-46e6-9730-cb083d420851" width="800">
</p>

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
すべて同じ値になります。

`0.1.0` が最初の正式リリースです。それより前に `0.1.0-beta.1` を出していますが、
SemVer では `0.1.0` より前のバージョンとして扱われるため、ベータから `0.1.0` 以降へは
正しくアップグレードとして認識されます（Windows のインストーラは既存のインストールを
上書き更新します）。

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

- **フレームレス表示** — タイトルバー・枠なし。ウィンドウ操作ボタン（macOS は左上に「閉じる / 最小化 / 最大化」、Windows は右上に「最小化 / 最大化 / 閉じる」）や左右の移動ボタン、左下のファイル名・右下の枚数表示は、マウスを動かしている間だけオーバーレイ表示され、3 秒動かさないと消えて画像だけの表示に戻ります。最小化・最大化の挙動は OS 標準どおりで、タイトルバーのダブルクリックでも最大化できます。大きさはウィンドウの端をドラッグして変えられ、設定で表示中の画像の縦横比に合わせることもできます
- **フォルダ / 圧縮フォルダ対応** — 画像1枚・フォルダ・圧縮フォルダ（zip / cbz）のいずれを開いても、その中の画像を自然順（`img2.png` → `img10.png`）で前後に移動できます
- **動画の再生** — mp4 / m4v / webm / mov を、画像と同じ一覧の中で再生できます
- **音楽の再生** — mp3 / m4a / aac / flac / wav / ogg / opus を、アートワークと曲名を表示して再生できます。曲が終わると同じフォルダの次の曲へ進みます
- **右クリックメニューと設定** — アプリ上の右クリックでコンテキストメニューが開き、エクスプローラー / Finder での表示や各種操作、設定ウィンドウを呼び出せます
- **ビュアーから削除** — `Del` キーや右クリックメニューで、表示中の画像を OS のゴミ箱へ移動できます（完全削除はしないので戻せます）。確認ウィンドウには「今後確認しない」があり、チェックすると次からは確認なしで削除します
- **フォルダの変更に追従** — フォルダを開いている間は、あとから画像を追加・削除しても開き直さずに一覧へ反映されます。OS のネイティブ通知（Windows: ReadDirectoryChangesW / macOS: FSEvents）を使うので、変化が無い間は待っているだけで CPU もディスクも使いません
- **圧縮フォルダは必要な1枚だけ展開** — zip の索引（セントラルディレクトリ）だけを読み、表示する画像のみをその都度取り出します。書庫全体をメモリやディスクに展開しないため、数GBの書庫でも起動が一瞬です
- **対応形式** — 画像: avif / bmp / gif / ico / jfif / jpe / jpeg / jpg / png / svg / tif / tiff / webp、動画: mp4 / m4v / webm / mov、音楽: mp3 / m4a / aac / flac / wav / ogg / opus、書庫: zip / cbz
- **対応OS** — Windows 10/11・macOS 10.15 以降（Apple Silicon）

## 使い方

- ファイル / フォルダ / zip・cbz をウィンドウにドラッグ＆ドロップするか、`O`（ファイル）/ `D`（フォルダ）キーで開きます
- `←` `→` で前後の画像、`+` `-` でズーム、`0` でフィット、`F` でフルスクリーン、`Esc` で終了します
- 右クリックでメニュー、`⌘ ,`（Windows は `Ctrl` + `,`）で設定ウィンドウが開きます

操作の一覧は [docs/specs/controls.md](docs/specs/controls.md) を参照してください。

## ドキュメント

| ファイル | 内容 |
| --- | --- |
| [docs/specs/controls.md](docs/specs/controls.md) | 開き方・キーボード・マウス・右クリックメニュー |
| [docs/specs/files.md](docs/specs/files.md) | フォルダ・圧縮フォルダの探索範囲と読み込み、フォルダの変更への追従、画像の削除 |
| [docs/specs/video.md](docs/specs/video.md) | 動画の再生と対応形式 |
| [docs/specs/audio.md](docs/specs/audio.md) | 音楽の再生・アートワークと対応形式 |
| [docs/specs/window-size.md](docs/specs/window-size.md) | ウィンドウサイズの挙動と保存 |
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

配布バイナリに含まれる第三者ソフトウェアのライセンス表示は [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES) にあります。この一覧は `node scripts/gen-third-party-notices.mjs` で `cargo metadata` の依存グラフから生成しています。

`LICENSE` と `THIRD-PARTY-NOTICES` はインストールされるアプリにも同梱されます（Windows はインストール先の `resources`、macOS は `sView.app/Contents/Resources/`）。
