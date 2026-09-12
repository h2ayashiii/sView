use std::cmp::Ordering;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::ipc::Response;
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use zip::ZipArchive;

/// 対応する画像拡張子（小文字で比較）
const IMAGE_EXTS: &[&str] = &[
    "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg", "png", "svg", "tif", "tiff", "webp",
];

/// 対応する書庫（圧縮フォルダ）拡張子
const ARCHIVE_EXTS: &[&str] = &["cbz", "zip"];

/// 最小ウィンドウサイズ（tauri.conf.json の minWidth / minHeight と合わせる）
const MIN_WINDOW_SIZE: (f64, f64) = (200.0, 150.0);

/// 設定と window.json を置くフォルダ名。
/// Tauri の app_config_dir() は identifier（bundle ID）をそのままフォルダ名にするが、
/// 逆ドメイン名がそのまま見えるのは分かりにくいので、ここは短い名前に固定する
const CONFIG_DIR_NAME: &str = "sview";

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

/// 設定ファイル類を置くフォルダ（OS の設定フォルダ / sview）
fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .config_dir()
        .map_err(|e| format!("設定フォルダを取得できません: {e}"))?;
    Ok(dir.join(CONFIG_DIR_NAME))
}

/// ウィンドウの大きさと位置を覚えておくファイル（設定本体とは分けて、
/// 設定ウィンドウの「既定に戻す」で消えないようにする）
fn window_state_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("window.json"))
}

/// 各項目は Option。古い window.json（大きさだけ）もそのまま読めるようにし、
/// 保存できなかった項目は既定の挙動（中央・既定サイズ）に任せる
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WindowState {
    width: Option<f64>,
    height: Option<f64>,
    x: Option<f64>,
    y: Option<f64>,
}

fn read_window_state(app: &AppHandle) -> Option<WindowState> {
    let path = window_state_file(app).ok()?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 前回の続きから開けるよう、閉じるときのウィンドウの位置と大きさを保存する。
/// 位置はどちらのサイズ設定でも覚えるが、「画像に合わせる」で開いている間の
/// 大きさは画像都合なので覚えない（前に覚えた大きさをそのまま残す）
fn save_window_state(window: &tauri::Window) {
    let app = window.app_handle();
    let Ok(path) = window_state_file(app) else {
        return;
    };
    let Ok(scale) = window.scale_factor() else {
        return;
    };
    // 最小化中は位置も大きさも実際の見た目と違うので触らない
    if window.is_minimized().unwrap_or(false) {
        return;
    }

    let mut state = read_window_state(app).unwrap_or_default();

    if let Ok(pos) = window.outer_position() {
        let pos = pos.to_logical::<f64>(scale);
        state.x = Some(pos.x);
        state.y = Some(pos.y);
    }

    if settings_value(app, "windowSizeMode").as_deref() != Some("flexible") {
        if let Ok(size) = window.inner_size() {
            let size = size.to_logical::<f64>(scale);
            if size.width >= 1.0 && size.height >= 1.0 {
                state.width = Some(size.width);
                state.height = Some(size.height);
            }
        }
    }

    if let (Some(dir), Ok(text)) = (path.parent(), serde_json::to_string_pretty(&state)) {
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(&path, text);
    }
}

/// 保存した位置が今つながっているディスプレイのどれかに載っているかを見る。
/// 前回使っていた外部ディスプレイが外れている場合に、画面外へ開くのを防ぐ
fn position_is_on_screen(window: &tauri::Window, x: f64, y: f64) -> bool {
    let Ok(monitors) = window.available_monitors() else {
        return false;
    };
    monitors.iter().any(|m| {
        let scale = m.scale_factor();
        let origin = m.position().to_logical::<f64>(scale);
        let size = m.size().to_logical::<f64>(scale);
        // 端にぴったり寄せた場合を落とさないよう少しだけ余裕を見る
        const SLACK: f64 = 8.0;
        x >= origin.x - SLACK
            && y >= origin.y - SLACK
            && x < origin.x + size.width
            && y < origin.y + size.height
    })
}

/// 設定ファイルから文字列項目を 1 つ読む（Rust 側から設定を参照する用）
fn settings_value(app: &AppHandle, key: &str) -> Option<String> {
    let path = settings_file(app).ok()?;
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get(key)?.as_str().map(str::to_owned)
}

/// 起動時に、前回閉じたときのウィンドウを復元する。
/// 位置は常に、大きさは「固定」のときだけ戻す
/// （「画像に合わせる」は画像を開くまで既定サイズ）
fn restore_window_state(window: &tauri::Window) {
    let app = window.app_handle();
    let Some(state) = read_window_state(app) else {
        return;
    };

    if settings_value(app, "windowSizeMode").as_deref() != Some("flexible") {
        if let (Some(width), Some(height)) = (state.width, state.height) {
            if width >= 1.0 && height >= 1.0 {
                let _ = window.set_size(LogicalSize::new(width, height));
            }
        }
    }

    if let (Some(x), Some(y)) = (state.x, state.y) {
        if position_is_on_screen(window, x, y) {
            let _ = window.set_position(LogicalPosition::new(x, y));
        }
    }
}

/// ウィンドウを画像の縦横比ぴったりに合わせる（「画像に合わせる」のとき）。
/// 画面に収まらない画像は、画面の高さの 90% に収まるよう縮める。
/// 画面からはみ出さないよう横も 90% を上限にし、ウィンドウの中心は動かさない。
#[tauri::command]
fn fit_window_to_image(window: WebviewWindow, width: f64, height: f64) -> Result<(), String> {
    const SCREEN_RATIO: f64 = 0.9;
    if !(width > 0.0 && height > 0.0) {
        return Err("画像サイズを取得できません".to_string());
    }
    let scale = window.scale_factor().map_err(|e| e.to_string())?;

    // 画面（作業領域）に対する上限。取得できない場合は縮小せずそのまま
    let limit = window
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let size = m.size().to_logical::<f64>(m.scale_factor());
            (size.width * SCREEN_RATIO, size.height * SCREEN_RATIO)
        })
        .unwrap_or((f64::INFINITY, f64::INFINITY));

    // 拡大はせず、画面に収まらないときだけ縮める
    let ratio = (limit.1 / height).min(limit.0 / width).min(1.0);
    let w = (width * ratio).max(MIN_WINDOW_SIZE.0);
    let h = (height * ratio).max(MIN_WINDOW_SIZE.1);

    // 変更前の中心を保ったまま大きさだけ変える
    let center = (|| {
        let pos = window.outer_position().ok()?.to_logical::<f64>(scale);
        let size = window.outer_size().ok()?.to_logical::<f64>(scale);
        Some((pos.x + size.width / 2.0, pos.y + size.height / 2.0))
    })();

    window
        .set_size(LogicalSize::new(w, h))
        .map_err(|e| format!("ウィンドウサイズを変更できません: {e}"))?;

    if let Some((cx, cy)) = center {
        let _ = window.set_position(LogicalPosition::new(cx - w / 2.0, cy - h / 2.0));
    }
    Ok(())
}

/// 設定ファイルの置き場所（OS の設定フォルダ / sview / settings.json）
fn settings_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("settings.json"))
}

/// 保存済みの設定を返す。未保存・壊れている場合は null（フロント側で既定値を使う）
#[tauri::command]
fn load_settings(app: AppHandle) -> Result<Option<serde_json::Value>, String> {
    let path = settings_file(&app)?;
    match fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("設定を読めません: {e}")),
    }
}

/// 設定を保存する（書き込み途中で壊れないよう一時ファイル経由で置き換える）
#[tauri::command]
fn save_settings(app: AppHandle, settings: serde_json::Value) -> Result<(), String> {
    let path = settings_file(&app)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("設定フォルダを作れません: {e}"))?;
    }
    let text =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("設定を書けません: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| format!("設定を書けません: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("設定を保存できません: {e}"))
}

/// 設定ウィンドウを作り直す（tauri.conf.json の定義が失われた場合の保険）。
/// 通常は起動時に非表示で作られたものを使い回すので、ここは通らない
fn build_settings_window(app: &AppHandle) -> Result<(), String> {
    WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("sView の設定")
        .inner_size(470.0, 560.0)
        .min_inner_size(380.0, 300.0)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        .center()
        .build()
        .map(|_| ())
        .map_err(|e| format!("設定ウィンドウを開けません: {e}"))
}

/// 設定ウィンドウを開く。
/// ウィンドウ自体は tauri.conf.json で起動時に（非表示で）作っておき、
/// ここでは表示して前面に出すだけにする。実行時に作った枠なし・半透明の
/// ウィンドウは中身が読み込まれないことがあるため（Windows で空のまま開く）
#[tauri::command]
fn open_settings_window(app: AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("settings") else {
        return build_settings_window(&app);
    };
    window
        .show()
        .map_err(|e| format!("設定ウィンドウを表示できません: {e}"))?;
    window
        .set_focus()
        .map_err(|e| format!("設定ウィンドウを前面にできません: {e}"))
}

/// OS のファイルマネージャーで対象を選択状態にして開く
#[tauri::command]
fn reveal_in_file_manager(path: String) -> Result<(), String> {
    use std::process::Command;
    let target = PathBuf::from(&path);
    if !target.exists() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }

    #[cfg(target_os = "windows")]
    let result = {
        // explorer は選択に成功しても非 0 を返すことがあるため、起動できたかだけを見る
        Command::new("explorer")
            .arg(format!("/select,{}", target.display()))
            .spawn()
            .map(|_| ())
    };

    #[cfg(target_os = "macos")]
    let result = Command::new("open")
        .arg("-R")
        .arg(&target)
        .spawn()
        .map(|_| ());

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = {
        // Linux には選択して開く共通の方法がないので、親フォルダを開く
        let dir = if target.is_dir() {
            target.clone()
        } else {
            target.parent().unwrap_or(&target).to_path_buf()
        };
        Command::new("xdg-open").arg(dir).spawn().map(|_| ())
    };

    result.map_err(|e| format!("ファイルマネージャーを開けません: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            let tauri::WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            match window.label() {
                // 設定ウィンドウは閉じずに隠す。破棄してしまうと
                // 開き直すたびに作り直しになり、閉じ忘れるとアプリが残り続ける
                "settings" => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // 本体を閉じたらアプリごと終了する（非表示の設定ウィンドウが
                // 残っていても終了できるようにする）。
                // 閉じる直前の大きさは次回「固定」で開くときのために覚えておく
                "main" => {
                    save_window_state(window);
                    window.app_handle().exit(0);
                }
                _ => {}
            }
        })
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                restore_window_state(&window.as_ref().window());
                // サイズを整えてから見せる（起動直後のちらつきを避ける）
                let _ = window.show();
            }
            Ok(())
        })
        .manage(StartupFile(Mutex::new(startup_file_from_args())))
        .manage(ArchiveCache(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            list_images,
            read_archive_image,
            get_startup_file,
            load_settings,
            save_settings,
            open_settings_window,
            reveal_in_file_manager,
            fit_window_to_image
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app_handle, _event| {
        // macOS: Finder / Dock からファイルを開いたときに届く
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = &_event {
            use tauri::Emitter;
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
