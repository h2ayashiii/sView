use std::path::{Path, PathBuf};

use tauri::AppHandle;

use crate::settings::config_dir;

/// ログの置き場所（設定フォルダ / sview / logs）。
/// settings.json と同じ場所にまとめて、バグ報告のときに 1 箇所を見れば済むようにする
pub(crate) fn log_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("logs"))
}

/// tauri-plugin-log を後から登録する。
/// 出力先の決定に AppHandle が必要なので、Builder ではなく setup() から呼ぶ。
/// 2MB ごとにファイルを切り替え、直近 3 本だけ残す（放置しても膨らまない）
pub(crate) fn init_logging(app: &AppHandle) -> Result<(), String> {
    use tauri_plugin_log::{Builder, RotationStrategy, Target, TargetKind, TimezoneStrategy};

    let plugin = Builder::new()
        .level(log::LevelFilter::Info)
        // ログの時刻は報告者の手元の時計と合っている方が突き合わせやすい
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .rotation_strategy(RotationStrategy::KeepSome(3))
        .max_file_size(2 * 1024 * 1024)
        .targets([
            Target::new(TargetKind::Stdout),
            Target::new(TargetKind::Folder {
                path: log_dir(app)?,
                file_name: Some("sview".into()),
            }),
        ])
        .build();

    app.plugin(plugin)
        .map_err(|e| format!("ログを初期化できません: {e}"))
}

/// パニックの内容をログに残してから、既定の挙動（stderr へ出力して abort）に渡す。
/// panic = "abort" でも hook 自体は abort の前に呼ばれる
pub(crate) fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!(
            "パニックが発生しました: {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        default_hook(info);
    }));
}

/// ダイアログへ渡せるように、引用符・バックスラッシュ・改行などを空白に置き換えて詰める。
/// OS のコマンドの引数として埋め込むため、クォートを壊す文字を残さない
pub(crate) fn sanitize_dialog_text(message: &str) -> String {
    message
        .chars()
        .map(|c| match c {
            '\'' | '"' | '\\' | '`' | '$' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// アプリを組み立てられなかったときだけ使う、OS 標準のメッセージダイアログ。
/// tauri-plugin-dialog はアプリが出来ていないと使えないので、ここは OS のコマンドを直接呼ぶ
pub(crate) fn show_fatal_error(message: &str) {
    let text = sanitize_dialog_text(message);

    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "Add-Type -AssemblyName PresentationFramework; \
             [System.Windows.MessageBox]::Show('{text}', 'sView') | Out-Null"
        ))
        .status();

    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"display alert "sView" message "{text}" as critical"#
        ))
        .status();

    // 対象外の OS（Linux など）は CI でテストをビルドするためだけの分岐
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = text;
}

/// OS のファイルマネージャーでフォルダを開く（中身を表示する。選択状態にはしない）
pub(crate) fn open_folder(dir: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let command = "explorer";
    #[cfg(target_os = "macos")]
    let command = "open";
    // 対象外の OS（Linux など）は CI でテストをビルドするためだけの分岐
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let command = "xdg-open";

    std::process::Command::new(command)
        .arg(dir)
        .spawn()
        // explorer は成功しても非 0 を返すことがあるため、起動できたかだけを見る
        .map(|_| ())
        .map_err(|e| format!("フォルダを開けません: {e}"))
}

/// ログフォルダを開く（まだ無い場合は作ってから開く）
#[tauri::command]
pub(crate) fn open_log_folder(app: AppHandle) -> Result<(), String> {
    let dir = log_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| format!("ログフォルダを作れません: {e}"))?;
    open_folder(&dir)
}

/// OS のファイルマネージャーで対象を選択状態にして開く
#[tauri::command]
pub(crate) fn reveal_in_file_manager(path: String) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
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

    // 対象外の OS（Linux など）は CI でテストをビルドするためだけの分岐
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result: std::io::Result<()> = Err(std::io::Error::other("この OS には対応していません"));

    result.map_err(|e| format!("ファイルマネージャーを開けません: {e}"))
}
