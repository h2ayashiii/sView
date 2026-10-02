# 開発・ビルド・リリース

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
# macOS
./scripts/build.sh

# Windows (PowerShell)
.\scripts\build.ps1
```

ビルド成果物の場所:

| OS | 実行ファイル（インストール不要のポータブル版） | インストーラ |
| --- | --- | --- |
| Windows | `src-tauri/target/release/sview.exe` | `src-tauri/target/release/bundle/nsis/`（NSIS インストーラ。管理者権限で `C:\Program Files\sView` にインストールします） |
| macOS | `src-tauri/target/release/bundle/macos/sView.app` | `src-tauri/target/release/bundle/dmg/` |

`bundle/macos/sView.app` と `bundle/dmg/` の `.dmg` の中身は同じものです。`tauri build` はまず `.app` を組み立て、dmg バンドラはその `.app` をそのままディスクイメージに入れるため、`.dmg` を開いて出てくる `sView.app` は `bundle/macos/sView.app` と同一です。

GitHub Actions（`.github/workflows/build.yml`）で Windows / macOS のバイナリをビルドできます。手元にビルド環境がない場合は、リリースの添付ファイルか、Actions の成果物（Artifacts）を利用してください。

**配布用のバンドルビルドが走るタイミングは 2 つだけです。** PR と main への push では、別のワークフロー `ci`（`.github/workflows/ci.yml`）の `cargo test` だけが走ります。Actions タブ左の一覧には `ci` と `build` の 2 つが並びます。

| きっかけ | 動き |
| --- | --- |
| PR を開く / main へ push | `ci`: `ubuntu-latest` で `cargo test` だけを走らせる（バンドルは作らない） |
| `v*` タグを push | Windows / macOS の両方をビルドし、GitHub Release を作って成果物を添付する |
| 手動実行（Run workflow） | 選んだ OS だけをビルドし、Artifacts に置く（リリースは作らない） |

同じ ref で実行が重なった場合は、古い方を自動でキャンセルします。

ワークフローで使う Action はタグではなくコミット SHA で固定しています（`uses: actions/checkout@<SHA> # v4.4.0` の形。コメントが対応するバージョン）。上げるときは SHA とコメントを両方書き換えてください。`GITHUB_TOKEN` は既定で読み取りのみで、書き込み権限は Release を作る `release` job にだけ付けています。

Artifacts は zip を展開すると成果物がそのまま出てきます（`bundle/nsis/…` のような階層は作りません）。

| Artifacts | 展開すると出てくるもの |
| --- | --- |
| `sView-<バージョン>-windows` | `sView_<バージョン>_x64-setup.exe`（インストーラ）と `sView_<バージョン>_x64-portable.exe`（インストール不要でそのまま実行できる本体） |
| `sView-<バージョン>-macos` | `sView_<バージョン>_<アーキテクチャ>.dmg` |

macOS で `.app` を単体でアップロードしていないのは、`.app` が（1ファイルではなく）ディレクトリだからです。Artifacts は必ず zip に固められて配布されるため、`.app` をそのまま入れると展開時に実行権限が落ちて起動できなくなります。`.dmg` は 1 ファイルなので zip を経由しても中身が変化せず、マウントすれば実行権限も ad-hoc 署名も保たれた `sView.app` がそのまま取り出せます。

## タグを打ってリリースする

`v` から始まるタグを push すると、ビルドと GitHub Release の作成までが自動で行われます。

```sh
git tag v0.2.0
git push origin v0.2.0
```

- **バージョンはタグから決まります。** ビルド前に `scripts/set-version.mjs` が `src-tauri/tauri.conf.json` / `src-tauri/Cargo.toml` / `src-tauri/Cargo.lock`（sview 自身の行）/ `package.json` の `version` をタグの値（`v` を除いたもの）に書き換えるため、設定ウィンドウに出るバージョンも成果物のファイル名もタグと一致します。リポジトリ側のバージョンを事前に上げておく必要はありません（手動ビルドでのバージョンは次節）。
- タグは `v1.2.3` の形式にしてください。それ以外はビルド前にエラーで止まります。`v1.2.3-beta.1` のようなプレリリースも仕組み上は通りますが、方針として使いません（[バージョンの読み方](../README.md#バージョンの読み方)）。
- Release には `.dmg` と `.exe` が**そのまま**添付されます。Release の添付ファイルは zip に固められないため、ダウンロードしたらすぐ実行できます。
- リリースノートは、`.github/release-notes/<タグ名>.md` があればその内容を使い、無ければ GitHub の自動生成（`--generate-notes`）になります。節目のリリースでは手書きのノートを置いてください。同じタグで再実行した場合は、既存の Release にファイルを上書きアップロードします。
- **依存を足したり外したりしたら、`node scripts/gen-third-party-notices.mjs` で `THIRD-PARTY-NOTICES` を作り直してからタグを打ってください。** このファイルと `LICENSE` は配布物に同梱されます（バンドルのリソースフォルダに入ります）。
- 成果物はランナーのアーキテクチャ向けです。GitHub の `macos-latest` は Apple Silicon（arm64）のため、`.dmg` も Apple Silicon 向けになります。Intel Mac 向けも配る場合は、`--target universal-apple-darwin` でのユニバーサルビルドを追加してください。

## 任意のブランチで手動ビルドする

タグを打たずに、**任意のブランチを選んで好きなタイミングで手動実行**できます（開発中のブランチを実機で確認したいときなど）。

- **画面から**: Actions タブ → 左の `build` → 右上の「Run workflow」→ ブランチと「ビルドする OS」を選んで実行
- **CLI から**: `gh workflow run build.yml --ref <ブランチ名> -f targets=windows`

- **バージョン**: 手動ビルドでは `git describe` から `<直近のタグ>-dev.<タグからのコミット数>.g<コミットハッシュ>`（例: `0.3.0-dev.5.g3df11f1`）を作り、設定ウィンドウの表示と成果物のファイル名に使います。semver ではビルドメタデータ（`+…`）を使えないため、プレリリース部分に入れています。

「ビルドする OS」は `both`（既定）/ `windows` / `macos` から選べます。片方だけ確認したいときに選ぶと、もう一方のランナーは起動しません。タグ実行では常に両方をビルドします。

「Run workflow」ボタンが出るのは、ワークフローファイルがデフォルトブランチ（main）にあるためです。実行時には**選択したブランチ側の `build.yml`** が使われるので、ワークフロー自体の変更もそのブランチで試せます。ただし画面の入力欄の内容は main 側の定義が使われることがあるため、main にマージする前に新しい入力を試すときは上記の CLI（API に直接渡す形）が確実です。
