use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::webview_options;

/// 設定と window.json を置くフォルダ名。
/// Tauri の app_config_dir() は identifier（bundle ID）をそのままフォルダ名にするが、
/// 逆ドメイン名がそのまま見えるのは分かりにくいので、ここは短い名前に固定する
pub(crate) const CONFIG_DIR_NAME: &str = "sview";

/// 設定ファイル類を置くフォルダ（OS の設定フォルダ / sview）
pub(crate) fn config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .config_dir()
        .map_err(|e| format!("設定フォルダを取得できません: {e}"))?;
    Ok(dir.join(CONFIG_DIR_NAME))
}

/// WebView がキャッシュや localStorage を置くフォルダ。
///
/// Windows: 指定しないと WebView2 は実行ファイルの隣（`C:\Program Files\sView`）に
/// `sview.exe.WebView2` を作ろうとして、書き込めずに起動そのものが失敗する。
/// 明示的に `%LOCALAPPDATA%\sview` を渡す。フォルダ名を bundle ID ではなく
/// `sview` にしているのは CONFIG_DIR_NAME と同じ理由。
///
/// macOS: WKWebView にはデータフォルダを指定する仕組みが無く、置き場所

/// 設定ファイルから文字列項目を 1 つ読む（Rust 側から設定を参照する用）
pub(crate) fn settings_value(app: &AppHandle, key: &str) -> Option<String> {
    let path = settings_file(app).ok()?;
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get(key)?.as_str().map(str::to_owned)
}

/// ウィンドウサイズの設定が「画像に合わせる」か。
/// "flexible" は 0.1 系までの古い値（settings.json を書き換えずに読めるようにする）
pub(crate) fn fits_window_to_image(app: &AppHandle) -> bool {
    matches!(
        settings_value(app, "windowSizeMode").as_deref(),
        Some("image") | Some("flexible")
    )
}

/// 起動時に、開くファイルの種類 kind（関連付け・コマンドラインから開いたとき）に合わせて
/// ウィンドウを置く（ウィンドウを見せる前に呼ぶ）。
/// 「自由に変更」では種類によらず 1 つの枠（最上位）、「メディアに合わせる」では kind の枠の
/// 大きさ・位置で開く。ファイルを指定せずに起動したとき（kind が None）は何もしない
/// （tauri.conf.json の既定の大きさのまま、画面の中央に出る）。
/// 「画像に合わせる」では最初の画像で縦横比を合わせ直すので、そのときも

/// 設定ファイルの置き場所（OS の設定フォルダ / sview / settings.json）
pub(crate) fn settings_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("settings.json"))
}

/// アプリのバージョンを返す（tauri.conf.json の version。
/// リリース時は CI がタグの値を書き込んでからビルドする）
#[tauri::command]
pub(crate) fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// 保存済みの設定を返す。未保存・壊れている場合は null（フロント側で既定値を使う）
#[tauri::command]
pub(crate) fn load_settings(app: AppHandle) -> Result<Option<serde_json::Value>, String> {
    let path = settings_file(&app)?;
    match fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("設定を読めません: {e}")),
    }
}

/// 設定を保存する。settings に入っている項目だけを、いまの settings.json に上書きする
/// （書き込み途中で壊れないよう一時ファイル経由で置き換える）。
/// Windows ではファイルを開くたびに別のプロセスになり、それぞれが起動時に読んだ設定を持っている。
/// 全体を書き戻すと、ほかのプロセスで変えた項目を古い値へ戻してしまうので、変えた項目だけを渡す
#[tauri::command]
pub(crate) fn save_settings(app: AppHandle, settings: serde_json::Value) -> Result<(), String> {
    let path = settings_file(&app)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("設定フォルダを作れません: {e}"))?;
    }
    let current = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let merged = merge_settings(current, settings);
    let text =
        serde_json::to_string_pretty(&merged).map_err(|e| format!("設定を書けません: {e}"))?;
    let tmp = temp_path(&path);
    fs::write(&tmp, text).map_err(|e| format!("設定を書けません: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("設定を保存できません: {e}")
    })
}

/// 保存済みの設定 current に、変えた項目 changes を上書きする（最上位の項目ごと）。
/// current が無い・壊れている（オブジェクトでない）ときは changes だけにする
pub(crate) fn merge_settings(
    current: Option<serde_json::Value>,
    changes: serde_json::Value,
) -> serde_json::Value {
    let serde_json::Value::Object(changes) = changes else {
        return changes;
    };
    let mut merged = match current {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    merged.extend(changes);
    serde_json::Value::Object(merged)
}

/// 置き換え用の一時ファイル。別のプロセスが同時に書いても取り合わないよう、プロセス番号を付ける
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    path.with_extension(format!("json.{}.tmp", std::process::id()))
}

/// 設定ウィンドウを作り直す（tauri.conf.json の定義が失われた場合の保険）。
/// 通常は起動時に非表示で作られたものを使い回すので、ここは通らない
pub(crate) fn build_settings_window(app: &AppHandle) -> Result<(), String> {
    let builder =
        WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
            .title("sView の設定")
            .inner_size(470.0, 560.0)
            .min_inner_size(380.0, 300.0)
            .resizable(true)
            .decorations(false)
            .transparent(true)
            .center();
    webview_options(app, builder)
        .build()
        .map(|_| ())
        .map_err(|e| format!("設定ウィンドウを開けません: {e}"))
}

/// 設定ウィンドウを開く。
/// ウィンドウ自体は tauri.conf.json で起動時に（非表示で）作っておき、
/// ここでは表示して前面に出すだけにする。実行時に作った枠なし・半透明の
/// ウィンドウは中身が読み込まれないことがあるため（Windows で空のまま開く）
#[tauri::command]
pub(crate) fn open_settings_window(app: AppHandle) -> Result<(), String> {
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

/// ショートカット一覧ウィンドウを開く（設定ウィンドウの「ショートカット一覧」から）。
/// 設定ウィンドウと同じく、起動時に非表示で作ったものを表示して前面に出すだけ
#[tauri::command]
pub(crate) fn open_shortcuts_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("shortcuts")
        .ok_or("ショートカット一覧のウィンドウがありません")?;
    window
        .show()
        .map_err(|e| format!("ショートカット一覧を表示できません: {e}"))?;
    window
        .set_focus()
        .map_err(|e| format!("ショートカット一覧を前面にできません: {e}"))
}
