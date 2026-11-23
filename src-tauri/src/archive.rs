use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::State;
use tauri::ipc::Response;
use zip::ZipArchive;

use crate::media::{ImageList, MAX_DEPTH, MAX_ENTRY_BYTES, MediaKind, is_image, natural_cmp};

/// 直近に開いた書庫のハンドルを 1 つだけ保持する。
/// ZipArchive は生成時に末尾のセントラルディレクトリだけを読むため、
/// 書庫全体をメモリに展開することはない（実データは要求された1件のみ展開）。
pub(crate) struct OpenArchive {
    path: PathBuf,
    /// (更新日時, サイズ) — 書庫が差し替えられていたら開き直すための目印
    stamp: (Option<std::time::SystemTime>, u64),
    zip: ZipArchive<BufReader<File>>,
}
pub(crate) struct ArchiveCache(pub(crate) Mutex<Option<OpenArchive>>);

/// 書庫を開いてキャッシュしたうえで `f` を適用する。
/// 同じ書庫を続けて読む場合はファイルを開き直さない。
pub(crate) fn with_archive<T>(
    path: &Path,
    cache: &ArchiveCache,
    f: impl FnOnce(&mut ZipArchive<BufReader<File>>) -> Result<T, String>,
) -> Result<T, String> {
    let meta = fs::metadata(path).map_err(|e| format!("書庫を開けません: {e}"))?;
    let stamp = (meta.modified().ok(), meta.len());

    let mut slot = cache.0.lock().unwrap();
    let fresh = slot
        .as_ref()
        .is_some_and(|a| a.path == path && a.stamp == stamp);
    if !fresh {
        let file = File::open(path).map_err(|e| format!("書庫を開けません: {e}"))?;
        // セントラルディレクトリ（末尾の索引）のみを読む。全体展開はしない
        let zip =
            ZipArchive::new(BufReader::new(file)).map_err(|e| format!("書庫を読めません: {e}"))?;
        *slot = Some(OpenArchive {
            path: path.to_path_buf(),
            stamp,
            zip,
        });
    }
    f(&mut slot.as_mut().expect("just inserted").zip)
}

/// 全エントリが共有する先頭フォルダを返す（末尾に "/" を含む）。
/// 圧縮ソフトが余計に一層挟んだ場合、その一層をここで吸収して
/// 表示名を「書庫直下に画像が並んでいる」ように見せる
pub(crate) fn common_dir_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut prefix = match first.rfind('/') {
        Some(i) => &first[..=i],
        None => return String::new(),
    };
    for name in &names[1..] {
        // 候補が合わなければ 1 階層ずつ短くする
        while !prefix.is_empty() && !name.starts_with(prefix) {
            let without_slash = &prefix[..prefix.len() - 1];
            prefix = match without_slash.rfind('/') {
                Some(i) => &prefix[..=i],
                None => "",
            };
        }
        if prefix.is_empty() {
            break;
        }
    }
    prefix.to_owned()
}

/// 書庫内のエントリが何層目にあるか（書庫の直下が 1）
pub(crate) fn entry_depth(name: &str) -> usize {
    name.split('/').filter(|c| !c.is_empty()).count()
}

/// 書庫内の画像エントリ名を自然順で返す（MAX_DEPTH 層目まで）。
/// 書庫の中は画像しか読まないので、種類は常に画像になる
pub(crate) fn list_archive_images(path: &Path, cache: &ArchiveCache) -> Result<ImageList, String> {
    let mut names = with_archive(path, cache, |zip| {
        Ok(zip
            .file_names()
            .filter(|n| !n.ends_with('/') && entry_depth(n) <= MAX_DEPTH && is_image(Path::new(n)))
            .map(str::to_owned)
            .collect::<Vec<String>>())
    })?;
    if names.is_empty() {
        return Err(format!(
            "書庫に画像が見つかりません: {}",
            path.to_string_lossy()
        ));
    }
    names.sort_by(|x, y| natural_cmp(x, y));
    let prefix = common_dir_prefix(&names);
    Ok(ImageList {
        images: names,
        index: 0,
        archive: Some(path.to_string_lossy().into_owned()),
        dir: None,
        prefix,
        kind: Some(MediaKind::Image),
        depth: MAX_DEPTH,
    })
}

/// 書庫内の 1 エントリだけを展開して返す
pub(crate) fn read_archive_entry(
    path: &Path,
    entry: &str,
    cache: &ArchiveCache,
) -> Result<Vec<u8>, String> {
    if !is_image(Path::new(entry)) {
        return Err(format!("対応していないファイル形式です: {entry}"));
    }
    with_archive(path, cache, |zip| {
        let mut file = zip
            .by_name(entry)
            .map_err(|e| format!("書庫内のファイルを開けません ({entry}): {e}"))?;
        let size = file.size();
        if size > MAX_ENTRY_BYTES {
            return Err(format!("ファイルが大きすぎます ({entry})"));
        }
        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf)
            .map_err(|e| format!("書庫内のファイルを読めません ({entry}): {e}"))?;
        Ok(buf)
    })
}

/// 書庫内の画像 1 枚をバイト列のまま返す（IPC の raw payload で転送）
/// 重い処理（フォルダ・書庫・タグの読み込み）なので async にして、メインスレッド
/// （ウィンドウの描画・移動・入力）を止めないようにする
#[tauri::command(async)]
pub(crate) fn read_archive_image(
    archive: String,
    entry: String,
    cache: State<ArchiveCache>,
) -> Result<Response, String> {
    read_archive_entry(Path::new(&archive), &entry, &cache).map(Response::new)
}
