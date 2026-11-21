use std::path::{Path, PathBuf};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{Emitter, State, WebviewWindow};

use crate::media::{is_archive, is_media, MAX_DEPTH};
use crate::state::{FolderWatcher, Watching};

/// `path` が監視中のフォルダ（`roots` のどれか）から数えて何層目か（直下が 1）。
/// 数えられないとき（OS が別の表記のパスを返したなど）は直下とみなす
pub(crate) fn watched_depth(path: &Path, roots: &[PathBuf]) -> usize {
    roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .map(|rel| rel.components().count())
        .unwrap_or(1)
}

/// 一覧を作り直す必要がある変更かどうか。
/// `depth` 層目までの画像・動画・音楽の増減（作成・削除・名前の変更）だけを拾い、
/// 中身の書き換えは無視する。サブフォルダまで見ているときは、その中にあるフォルダ自体の
/// 増減も拾う（フォルダごと移してきたときは、中のファイルの通知が来ないことがあるため）。
/// 種類を判別できない通知（EventKind::Any）は、取りこぼすより拾う方に倒す
pub(crate) fn is_listing_change(event: &notify::Event, roots: &[PathBuf], depth: usize) -> bool {
    use notify::event::{EventKind, ModifyKind};
    let kind_matches = matches!(
        event.kind,
        EventKind::Any
            | EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_))
    );
    // 名前の変更では変更前と変更後の両方が入るので、どちらかが対象なら拾う
    kind_matches
        && event.paths.iter().any(|p| {
            let level = watched_depth(p, roots);
            if level > depth {
                return false;
            }
            if is_media(p) {
                return true;
            }
            // 消えたものはフォルダだったか確かめられないので、拡張子の無いものをフォルダとみなす
            level < depth && (p.is_dir() || (!p.exists() && p.extension().is_none()))
        })
}

/// 表示中のフォルダの監視を開始する（`path` が null なら監視をやめる）。
///
/// OS のネイティブ通知を使うので、変化が無い間は CPU もディスクも使わない
/// （ポーリングのように一定間隔で read_dir する方式とはここが違う）。
/// `depth` は一覧と同じ深さ。1 なら直下だけを見て、2 以上ならサブフォルダも見たうえで
/// `depth` 層目より深いところの通知は捨てる。
/// 通知は数が多くなりがちなので、実際の再スキャンはフロントエンド側で
/// 一定時間まとめてから 1 回だけ行う
#[tauri::command]
pub(crate) fn watch_folder(
    window: WebviewWindow,
    state: State<FolderWatcher>,
    path: Option<String>,
    depth: Option<usize>,
) -> Result<(), String> {
    let label = window.label().to_string();
    let mut watchers = state
        .0
        .lock()
        .map_err(|_| "監視の状態を取得できません".to_string())?;
    let current = watchers.entry(label.clone()).or_default();

    let Some(path) = path else {
        *current = None;
        return Ok(());
    };
    let dir = PathBuf::from(path);
    let depth = depth.unwrap_or(1).clamp(1, MAX_DEPTH);
    if current
        .as_ref()
        .is_some_and(|w| w.path == dir && w.depth == depth)
    {
        return Ok(()); // 同じフォルダを同じ深さで見ているなら張り直さない
    }
    // 先に古い監視を解除してから張り直す（二重に監視しない）
    *current = None;
    if !dir.is_dir() {
        return Err(format!("フォルダが見つかりません: {}", dir.display()));
    }

    // macOS の FSEvents は実体のパス（/private/var/... など）で知らせてくることがあるので、
    // 渡されたパスと実体のパスの両方を基準にして深さを数える
    let mut roots = vec![dir.clone()];
    if let Ok(real) = dir.canonicalize() {
        if real != dir {
            roots.push(real);
        }
    }
    let handle = window.app_handle().clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) if is_listing_change(&event, &roots, depth) => {
                // 知らせるのは、このフォルダを開いているウィンドウだけ
                let _ = handle.emit_to(label.as_str(), "folder-changed", ());
            }
            // 監視できなくなった場合（フォルダごと消えたなど）は記録だけして続ける。
            // 一覧は R キーで作り直せる
            Err(e) => log::warn!("フォルダの監視でエラーが発生しました: {e}"),
            Ok(_) => {}
        }
    })
    .map_err(|e| format!("フォルダを監視できません: {e}"))?;

    let mode = if depth > 1 {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher
        .watch(&dir, mode)
        .map_err(|e| format!("フォルダを監視できません: {e}"))?;
    log::info!(
        "フォルダの監視を開始しました: {} ({depth} 層目まで)",
        dir.display()
    );
    *current = Some(Watching {
        path: dir,
        depth,
        _watcher: watcher,
    });
    Ok(())
}
