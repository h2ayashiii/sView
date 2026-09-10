use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::State;

/// 対応する画像拡張子（小文字で比較）
const IMAGE_EXTS: &[&str] = &[
    "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg", "png", "svg", "tif", "tiff", "webp",
];

/// 起動時に渡されたファイル（CLI 引数 / macOS の Opened イベント）を
/// フロントエンドが取りに来るまで保持する
struct StartupFile(Mutex<Option<String>>);

#[derive(serde::Serialize)]
struct ImageList {
    images: Vec<String>,
    index: usize,
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
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
}

/// 指定ファイルと同じフォルダ内の画像一覧（自然順ソート）と、
/// そのファイルの位置を返す
#[tauri::command]
fn list_images(path: String) -> Result<ImageList, String> {
    let file = PathBuf::from(&path);
    if !file.is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    if !is_image(&file) {
        return Err(format!("対応していないファイル形式です: {path}"));
    }
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "親フォルダを取得できません".to_string())?;

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
    let target = file.file_name();
    let index = entries
        .iter()
        .position(|p| p.file_name() == target)
        .unwrap_or(0);

    let images = entries
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Ok(ImageList { images, index })
}

/// 起動引数（関連付け起動など）で渡されたファイルを一度だけ返す
#[tauri::command]
fn get_startup_file(state: State<StartupFile>) -> Option<String> {
    state.0.lock().unwrap().take()
}

fn startup_file_from_args() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-') && Path::new(a).is_file())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(StartupFile(Mutex::new(startup_file_from_args())))
        .invoke_handler(tauri::generate_handler![list_images, get_startup_file])
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
                .find(|p| p.is_file())
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
