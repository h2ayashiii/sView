use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::ipc::Response;
use tauri::State;
use zip::ZipArchive;

/// 対応する画像拡張子（小文字で比較）
const IMAGE_EXTS: &[&str] = &[
    "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg", "png", "svg", "tif", "tiff", "webp",
];

/// 対応する書庫（圧縮フォルダ）拡張子
const ARCHIVE_EXTS: &[&str] = &["cbz", "zip"];

/// 展開後サイズの上限（zip bomb 対策 / 1枚あたり）
const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;

/// 起動時に渡されたファイル（CLI 引数 / macOS の Opened イベント）を
/// フロントエンドが取りに来るまで保持する
struct StartupFile(Mutex<Option<String>>);

/// 直近に開いた書庫のハンドルを 1 つだけ保持する。
/// ZipArchive は生成時に末尾のセントラルディレクトリだけを読むため、
/// 書庫全体をメモリに展開することはない（実データは要求された1件のみ展開）。
struct OpenArchive {
    path: PathBuf,
    /// (更新日時, サイズ) — 書庫が差し替えられていたら開き直すための目印
    stamp: (Option<std::time::SystemTime>, u64),
    zip: ZipArchive<BufReader<File>>,
}
struct ArchiveCache(Mutex<Option<OpenArchive>>);

#[derive(serde::Serialize)]
struct ImageList {
    /// フォルダの場合は画像のフルパス、書庫の場合は書庫内のエントリ名
    images: Vec<String>,
    index: usize,
    /// 書庫を開いている場合はその書庫のフルパス
    archive: Option<String>,
    /// 表示名から取り除く共通フォルダ（書庫のみ。例: "book/"）
    prefix: String,
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_image(path: &Path) -> bool {
    has_ext(path, IMAGE_EXTS)
}

fn is_archive(path: &Path) -> bool {
    has_ext(path, ARCHIVE_EXTS)
}
/// 数値部分を数値として比較する自然順ソート（大文字小文字は無視）
/// 例: img2.png < img10.png
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let (mut i, mut j) = (0, 0);
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(&ca), Some(&cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let (mut x, mut y) = (i, j);
                    while a.get(x).is_some_and(|c| c.is_ascii_digit()) {
                        x += 1;
                    }
                    while b.get(y).is_some_and(|c| c.is_ascii_digit()) {
                        y += 1;
                    }
                    let na: &[char] = &a[i..x];
                    let nb: &[char] = &b[j..y];
                    // 先頭ゼロを除いた桁数 → 辞書順、で数値比較と同等
                    let ta = na.iter().position(|&c| c != '0').unwrap_or(na.len());
                    let tb = nb.iter().position(|&c| c != '0').unwrap_or(nb.len());
                    let (da, db) = (&na[ta..], &nb[tb..]);
                    let ord = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    i = x;
                    j = y;
                } else {
                    match ca.cmp(&cb) {
                        Ordering::Equal => {
                            i += 1;
                            j += 1;
                        }
                        ord => return ord,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort_orders_numbers_numerically() {
        let mut v = vec!["img10.png", "img2.png", "img1.png", "IMG3.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["img1.png", "img2.png", "IMG3.png", "img10.png"]);
    }

    #[test]
    fn natural_sort_handles_leading_zeros_and_plain_names() {
        let mut v = vec!["b.png", "a002.png", "a1.png", "a.png"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["a.png", "a1.png", "a002.png", "b.png"]);
    }

    #[test]
    fn image_extension_detection() {
        assert!(is_image(Path::new("/x/photo.JPG")));
        assert!(is_image(Path::new("/x/photo.webp")));
        assert!(!is_image(Path::new("/x/notes.txt")));
        assert!(!is_image(Path::new("/x/noext")));
    }

    #[test]
    fn archive_extension_detection() {
        assert!(is_archive(Path::new("/x/book.CBZ")));
        assert!(is_archive(Path::new("/x/pics.zip")));
        assert!(!is_archive(Path::new("/x/pics.rar")));
        assert!(!is_archive(Path::new("/x/photo.png")));
    }

    #[test]
    fn common_prefix_strips_only_a_shared_folder() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 余計に一層挟まれている → その一層を吸収
        assert_eq!(
            common_dir_prefix(&v(&["book/001.png", "book/002.png"])),
            "book/"
        );
        assert_eq!(common_dir_prefix(&v(&["a/b/1.png", "a/b/2.png"])), "a/b/");
        // 章分けされている → 共通部分までしか削らない
        assert_eq!(common_dir_prefix(&v(&["a/b/1.png", "a/c/2.png"])), "a/");
        assert_eq!(common_dir_prefix(&v(&["ch1/1.png", "ch2/1.png"])), "");
        // 名前が前方一致するだけの別フォルダを誤って削らない
        assert_eq!(common_dir_prefix(&v(&["ch1/1.png", "ch10/1.png"])), "");
        // 書庫直下に画像が並んでいる → 削らない
        assert_eq!(common_dir_prefix(&v(&["001.png", "002.png"])), "");
        assert_eq!(common_dir_prefix(&[]), "");
    }

    /// 画像2枚とテキスト1枚を含む zip を作り、一覧と単体取り出しを確認する
    #[test]
    fn archive_listing_and_single_entry_read() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("sview-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&path).unwrap());
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in [
                ("b/img10.png", "ten"),
                ("b/img2.png", "two"),
                ("readme.txt", "nope"),
            ] {
                w.start_file(name, opts).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
            // 圧縮済みエントリも展開できることを確認するため 1 件だけ deflate で入れる
            let deflated: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("b/img3.png", deflated).unwrap();
            w.write_all(&b"three".repeat(500)).unwrap();
            w.finish().unwrap();
        }

        let cache = ArchiveCache(Mutex::new(None));
        let list = list_archive_images(&path, &cache).unwrap();
        assert_eq!(list.images, vec!["b/img2.png", "b/img3.png", "b/img10.png"]);
        assert_eq!(
            list.archive.as_deref(),
            Some(path.to_string_lossy().as_ref())
        );
        // 画像はすべて b/ の下なので、その一層は表示名から取り除かれる
        assert_eq!(list.prefix, "b/");

        let bytes = read_archive_entry(&path, "b/img2.png", &cache).unwrap();
        assert_eq!(bytes, b"two");
        let bytes = read_archive_entry(&path, "b/img3.png", &cache).unwrap();
        assert_eq!(bytes, b"three".repeat(500));
        // 存在しないエントリ
        assert!(read_archive_entry(&path, "b/none.png", &cache).is_err());
        // 画像以外は取り出せない
        assert!(read_archive_entry(&path, "readme.txt", &cache).is_err());

        fs::remove_dir_all(&dir).ok();
    }
}

/// 書庫を開いてキャッシュしたうえで `f` を適用する。
/// 同じ書庫を続けて読む場合はファイルを開き直さない。
fn with_archive<T>(
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
fn common_dir_prefix(names: &[String]) -> String {
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

/// 書庫内の画像エントリ名を自然順で返す
fn list_archive_images(path: &Path, cache: &ArchiveCache) -> Result<ImageList, String> {
    let mut names = with_archive(path, cache, |zip| {
        Ok(zip
            .file_names()
            .filter(|n| !n.ends_with('/') && is_image(Path::new(n)))
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
        prefix,
    })
}

/// 書庫内の 1 エントリだけを展開して返す
fn read_archive_entry(path: &Path, entry: &str, cache: &ArchiveCache) -> Result<Vec<u8>, String> {
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

/// フォルダ内の画像一覧（自然順ソート）と、`current` の位置を返す
fn list_dir_images(dir: &Path, current: Option<&std::ffi::OsStr>) -> Result<ImageList, String> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("フォルダを読めません: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .collect();

    entries.sort_by(|x, y| {
        natural_cmp(
            &x.file_name().unwrap_or_default().to_string_lossy(),
            &y.file_name().unwrap_or_default().to_string_lossy(),
        )
    });

    // 同一フォルダ内なのでファイル名で自身を特定する
    let index = current
        .and_then(|name| entries.iter().position(|p| p.file_name() == Some(name)))
        .unwrap_or(0);

    let images = entries
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Ok(ImageList {
        images,
        index,
        archive: None,
        prefix: String::new(),
    })
}

/// 画像 / フォルダ / 書庫（zip・cbz）のいずれかを受け取り、
/// 表示対象の一覧と開始位置を返す
#[tauri::command]
fn list_images(path: String, cache: State<ArchiveCache>) -> Result<ImageList, String> {
    let target = PathBuf::from(&path);
    if target.is_dir() {
        return list_dir_images(&target, None);
    }
    if !target.is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    if is_archive(&target) {
        return list_archive_images(&target, &cache);
    }
    if !is_image(&target) {
        return Err(format!("対応していないファイル形式です: {path}"));
    }
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "親フォルダを取得できません".to_string())?;
    list_dir_images(dir, target.file_name())
}

/// 書庫内の画像 1 枚をバイト列のまま返す（IPC の raw payload で転送）
#[tauri::command]
fn read_archive_image(
    archive: String,
    entry: String,
    cache: State<ArchiveCache>,
) -> Result<Response, String> {
    read_archive_entry(Path::new(&archive), &entry, &cache).map(Response::new)
}

/// 起動引数（関連付け起動など）で渡されたファイルを一度だけ返す
#[tauri::command]
fn get_startup_file(state: State<StartupFile>) -> Option<String> {
    state.0.lock().unwrap().take()
}

fn startup_file_from_args() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-') && Path::new(a).exists())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(StartupFile(Mutex::new(startup_file_from_args())))
        .manage(ArchiveCache(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            list_images,
            read_archive_image,
            get_startup_file
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app_handle, _event| {
        // macOS: Finder / Dock からファイルを開いたときに届く
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = &_event {
            use tauri::{Emitter, Manager};
            if let Some(path) = urls
                .iter()
                .filter_map(|u| u.to_file_path().ok())
                .find(|p| p.exists())
            {
                let path = path.to_string_lossy().into_owned();
                // フロントエンドが未起動の場合に備えて state にも入れておく
                if let Some(state) = _app_handle.try_state::<StartupFile>() {
                    *state.0.lock().unwrap() = Some(path.clone());
                }
                let _ = _app_handle.emit("open-file", path);
            }
        }
    });
}
