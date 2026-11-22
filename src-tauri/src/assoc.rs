use std::path::Path;

use tauri::AppHandle;

use crate::media::{media_kind, MediaKind, AUDIO_EXTS, IMAGE_EXTS, VIDEO_EXTS};
#[cfg(target_os = "macos")]
use crate::platform::macos::mac_assoc;
#[cfg(target_os = "windows")]
use crate::platform::windows::win_assoc;

// ---- 拡張子の関連付け ----
// 設定ウィンドウから、どの拡張子を sView で開くかを選んで OS に登録する。
// OS ごとに「アプリが勝手に既定を奪えるか」が違うので、それぞれの流儀に従う:
// - Windows 8 以降は、アプリが既定のアプリを直接書き換えることを OS が認めていない
//   （UserChoice はハッシュで保護されている）。そこで sView を「この拡張子を開ける
//   アプリ」として登録し、最後の選択は Windows の「既定のアプリ」画面で本人にしてもらう
// - macOS は LaunchServices の API で既定のアプリを直接設定できる

/// 関連付けの対象にできる拡張子（画像・動画・音楽すべて。この OS だけの動画拡張子も含む）。
/// 書庫（zip / cbz）はここでは扱わない
pub(crate) fn associable_exts() -> Vec<&'static str> {
    [IMAGE_EXTS, VIDEO_EXTS, AUDIO_EXTS].concat()
}

#[derive(serde::Serialize)]
pub(crate) struct AssociationItem {
    pub(crate) ext: String,
    /// 種類（設定ウィンドウで種類ごとに分けて並べる）
    pub(crate) kind: MediaKind,
    /// いま sView がこの拡張子の既定のアプリになっているか（調べられないときは false）
    pub(crate) associated: bool,
}

#[derive(serde::Serialize)]
pub(crate) struct AssociationStatus {
    /// "windows" / "macos"
    pub(crate) platform: String,
    pub(crate) items: Vec<AssociationItem>,
}

/// 渡された拡張子を検証し、小文字にそろえる（関連付けできないものはエラー）
pub(crate) fn normalize_association_exts(exts: &[String]) -> Result<Vec<String>, String> {
    let allowed = associable_exts();
    let mut out: Vec<String> = Vec::new();
    for ext in exts {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        if !allowed.contains(&ext.as_str()) {
            return Err(format!("関連付けできない拡張子です: {ext}"));
        }
        if !out.contains(&ext) {
            out.push(ext);
        }
    }
    Ok(out)
}

/// 各拡張子の関連付けの状態を返す
#[tauri::command]
pub(crate) fn file_association_status(app: AppHandle) -> AssociationStatus {
    let items = associable_exts()
        .into_iter()
        .map(|ext| AssociationItem {
            ext: ext.to_string(),
            kind: media_kind(Path::new(&format!("x.{ext}"))).unwrap_or(MediaKind::Image),
            associated: is_associated(&app, ext),
        })
        .collect();
    AssociationStatus {
        platform: std::env::consts::OS.to_string(),
        items,
    }
}

/// 選んだ拡張子を sView に関連付ける。戻り値は設定ウィンドウに出す案内文
#[tauri::command]
pub(crate) fn apply_file_associations(app: AppHandle, exts: Vec<String>) -> Result<String, String> {
    let exts = normalize_association_exts(&exts)?;
    let message = apply_associations(&app, &exts)?;
    log::info!("拡張子の関連付けを変更しました: {exts:?}");
    Ok(message)
}

#[cfg(target_os = "windows")]
pub(crate) fn is_associated(_app: &AppHandle, ext: &str) -> bool {
    win_assoc::is_associated(ext)
}

#[cfg(target_os = "macos")]
pub(crate) fn is_associated(app: &AppHandle, ext: &str) -> bool {
    mac_assoc::is_associated(ext, &app.config().identifier)
}

// 対象外の OS（Linux など）は CI でテストをビルドするためだけの分岐
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn is_associated(_app: &AppHandle, _ext: &str) -> bool {
    false
}

#[cfg(target_os = "windows")]
pub(crate) fn apply_associations(_app: &AppHandle, exts: &[String]) -> Result<String, String> {
    win_assoc::register(exts, &associable_exts())?;
    if exts.is_empty() {
        return Ok("sView の関連付けの登録を外しました".into());
    }
    win_assoc::open_default_apps_settings()?;
    Ok(
        "sView を登録しました。開いた Windows の設定で、拡張子ごとに既定のアプリを sView にしてください"
            .into(),
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn apply_associations(app: &AppHandle, exts: &[String]) -> Result<String, String> {
    if exts.is_empty() {
        return Err("関連付ける拡張子を選んでください".into());
    }
    let bundle_id = app.config().identifier.clone();
    let failed: Vec<String> = exts
        .iter()
        .filter_map(|ext| mac_assoc::set_default(ext, &bundle_id).err())
        .collect();
    if failed.is_empty() {
        Ok(format!(
            "{} 種類の拡張子を sView で開くようにしました",
            exts.len()
        ))
    } else {
        Err(failed.join(" / "))
    }
}

// 対象外の OS（Linux など）は CI でテストをビルドするためだけの分岐
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn apply_associations(_app: &AppHandle, _exts: &[String]) -> Result<String, String> {
    Err("この OS には対応していません".into())
}
