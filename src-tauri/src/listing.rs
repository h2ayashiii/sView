use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::archive::{ArchiveCache, list_archive_images};
use crate::media::{
    ImageList, MAX_DEPTH, MediaKind, WindowKindKey, is_archive, is_media, media_kind, natural_cmp,
};
use crate::state::StartupListing;

/// `dir` の下の画像・動画・音楽を `depth` 層目まで集める（`dir` の直下が 1 層目）。
/// 読めないサブフォルダは飛ばす。
/// ファイルかフォルダかは read_dir が一緒に返す種類で見て、1 件ずつ OS に問い合わせない
/// （ファイルの多いフォルダやネットワークドライブで遅くなるため）
pub(crate) fn collect_media(
    dir: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        let Some(file_type) = entry_file_type(&entry, &path) else {
            continue;
        };
        if file_type.is_file() {
            if is_media(&path) {
                out.push(path);
            }
        } else if depth > 1 && file_type.is_dir() {
            if let Err(e) = collect_media(&path, depth - 1, out) {
                log::warn!("フォルダを読めません ({}): {e}", path.display());
            }
        }
    }
    Ok(())
}

/// read_dir の 1 件の種類。シンボリックリンク（Windows のジャンクションも含む）だけは
/// 指す先を問い合わせて、その種類を返す。取れなければ None
pub(crate) fn entry_file_type(entry: &fs::DirEntry, path: &Path) -> Option<fs::FileType> {
    let file_type = entry.file_type().ok()?;
    if file_type.is_symlink() {
        fs::metadata(path).ok().map(|m| m.file_type())
    } else {
        Some(file_type)
    }
}

/// 並べ替えに使う、`root` からの相対パス（区切りは書庫と同じ "/" にそろえる）
pub(crate) fn relative_key(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// フォルダの中を `depth` 層目まで読み、`kind` の種類だけを自然順で返す。
/// `kind` が None なら、並べたときに先頭に来るファイルの種類にする。
/// `current`（直下のファイル名）があれば、その位置を開始位置にする
pub(crate) fn list_dir_images(
    dir: &Path,
    depth: usize,
    kind: Option<MediaKind>,
    current: Option<&std::ffi::OsStr>,
) -> Result<ImageList, String> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_media(dir, depth.max(1), &mut entries)
        .map_err(|e| format!("フォルダを読めません: {e}"))?;

    // 書庫と同じく、開いたフォルダからの相対パスの自然順で並べる
    let mut keyed: Vec<(String, PathBuf)> = entries
        .into_iter()
        .map(|p| (relative_key(dir, &p), p))
        .collect();
    keyed.sort_by(|x, y| natural_cmp(&x.0, &y.0));

    let kind = kind.or_else(|| keyed.first().and_then(|(_, p)| media_kind(p)));
    let entries: Vec<PathBuf> = keyed
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| media_kind(p) == kind)
        .collect();

    // 直下のファイル名で自身を特定する
    let index = current
        .and_then(|name| {
            entries
                .iter()
                .position(|p| p.parent() == Some(dir) && p.file_name() == Some(name))
        })
        .unwrap_or(0);

    // サブフォルダまで読んだときは、開いたフォルダからの相対パスで表示する
    let prefix = if depth > 1 {
        let mut root = dir.to_string_lossy().into_owned();
        if !root.ends_with(std::path::MAIN_SEPARATOR) {
            root.push(std::path::MAIN_SEPARATOR);
        }
        root
    } else {
        String::new()
    };

    let images = entries
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Ok(ImageList {
        images,
        index,
        archive: None,
        dir: Some(dir.to_string_lossy().into_owned()),
        prefix,
        kind,
        depth: depth.max(1),
    })
}

/// 画像・動画・音楽 / フォルダ / 書庫（zip・cbz）のいずれかを受け取り、
/// 表示対象の一覧と開始位置を返す。
/// - 単体のファイル: 同じフォルダの直下にある、同じ種類のファイルだけ
/// - フォルダ・書庫: MAX_DEPTH 層目までのうち、並べたとき先頭に来るファイルと同じ種類だけ
///
/// `kind` / `depth` は読み直しのときに、最初に開いたときと同じ条件で並べるために渡す
/// 重い処理（フォルダ・書庫・タグの読み込み）なので async にして、メインスレッド
/// （ウィンドウの描画・移動・入力）を止めないようにする
#[tauri::command(async)]
pub(crate) fn list_images(
    window: WebviewWindow,
    path: String,
    kind: Option<MediaKind>,
    depth: Option<usize>,
    cache: State<ArchiveCache>,
    startup: State<StartupListing>,
) -> Result<ImageList, String> {
    // 最初に開くものは、ウィンドウを出す前に一覧を作ってある（prepare_startup_listing）。
    // 同じものを開くならそれを使い、読み直さない
    let prepared = startup
        .with(window.label(), |slot| {
            take_startup_listing(slot, &path, kind, depth)
        })
        .flatten();
    match prepared {
        Some(listing) => listing,
        None => list_path(&path, kind, depth, &cache),
    }
}

/// 預けてある一覧を、同じ path を最初の条件（kind / depth の指定なし）で開くときだけ返す。
/// 呼ばれたら一致しなくても空にする（一度きり。古い一覧を後から使わない）
pub(crate) fn take_startup_listing(
    slot: &mut Option<(String, Result<ImageList, String>)>,
    path: &str,
    kind: Option<MediaKind>,
    depth: Option<usize>,
) -> Option<Result<ImageList, String>> {
    let (prepared_path, listing) = slot.take()?;
    (prepared_path == path && kind.is_none() && depth.is_none()).then_some(listing)
}

/// path の一覧と開始位置を作る（list_images の中身）。
/// - 単体のファイル: 同じフォルダの直下にある、同じ種類のファイルだけ
/// - フォルダ・書庫: MAX_DEPTH 層目までのうち、並べたとき先頭に来るファイルと同じ種類だけ
pub(crate) fn list_path(
    path: &str,
    kind: Option<MediaKind>,
    depth: Option<usize>,
    cache: &ArchiveCache,
) -> Result<ImageList, String> {
    let target = PathBuf::from(path);
    if target.is_dir() {
        let depth = depth.unwrap_or(MAX_DEPTH).clamp(1, MAX_DEPTH);
        return list_dir_images(&target, depth, kind, None);
    }
    if !target.is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    if is_archive(&target) {
        return list_archive_images(&target, cache);
    }
    let Some(kind) = media_kind(&target) else {
        return Err(format!("対応していないファイル形式です: {path}"));
    };
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "親フォルダを取得できません".to_string())?;
    list_dir_images(dir, 1, Some(kind), target.file_name())
}

/// ウィンドウで最初に開くもの（起動時のファイル / macOS で新しく作るウィンドウのファイル）の
/// 一覧を、ウィンドウを出す前に作ってそのウィンドウに預け、並ぶ種類を返す。
/// 種類はウィンドウをその種類の大きさ・位置で出すのに使う。
/// フロントエンドの最初の list_images は預けた一覧（失敗ならそのエラー）をそのまま受け取るので、
/// ファイル・フォルダ・書庫を読むのはこの 1 回だけ（以前はフロントエンドが読んでいた分を前に移しただけ）
pub(crate) fn prepare_startup_listing(
    app: &AppHandle,
    label: &str,
    path: &str,
) -> Option<WindowKindKey> {
    let listing = list_path(path, None, None, &app.state::<ArchiveCache>());
    let kind = listing
        .as_ref()
        .ok()
        .and_then(|l| l.kind)
        .map(WindowKindKey::from);
    app.state::<StartupListing>()
        .with(label, |slot| *slot = Some((path.to_string(), listing)));
    kind
}

/// 表示中の画像を OS のゴミ箱（Windows: ごみ箱 / macOS: ゴミ箱）へ送る。
/// 完全削除はしないので、取り違えても OS 側から戻せる
#[tauri::command]
pub(crate) fn delete_image(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    if !is_media(&target) {
        return Err(format!("画像・動画・音楽ファイルではありません: {path}"));
    }
    trash::delete(&target).map_err(|e| format!("ゴミ箱へ移動できませんでした: {e}"))?;
    log::info!("ゴミ箱へ移動しました: {path}");
    Ok(())
}
