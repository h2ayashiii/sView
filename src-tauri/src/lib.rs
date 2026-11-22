mod archive;
mod assoc;
mod audio;
mod listing;
mod logging;
mod media;
mod mkv;
mod platform;
mod settings;
mod state;
mod watcher;
mod window;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, WebviewWindowBuilder};

use crate::archive::ArchiveCache;
use crate::listing::prepare_startup_listing;
use crate::logging::{init_logging, install_panic_hook, show_fatal_error};
use crate::mkv::MkvStreams;
#[cfg(target_os = "macos")]
use crate::platform::macos::{mac_pointer, mac_sizing};
#[cfg(target_os = "windows")]
use crate::platform::windows::win_sizing;
use crate::settings::fits_window_to_image;
#[cfg(target_os = "windows")]
use crate::settings::CONFIG_DIR_NAME;
use crate::state::{
    AspectLock, FolderWatcher, PendingPosition, PerWindow, StartupFile, StartupListing, WindowKind,
    WindowSlot,
};
use crate::window::{
    current_geometry, default_geometry, keep_aspect_on_resize, kind_target, place_kind_geometry,
    read_window_state, restore_window_state, save_window_state, KindTarget,
};

/// 起動引数（関連付け起動など）やこのウィンドウに割り当てたファイルを一度だけ返す
#[tauri::command]
pub(crate) fn get_startup_file(window: WebviewWindow, state: State<StartupFile>) -> Option<String> {
    state.with(window.label(), Option::take).flatten()
}

pub(crate) fn startup_file_from_args() -> Option<String> {
    std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-') && Path::new(a).exists())
}

/// （`~/Library/WebKit/<bundle ID>` など）は OS が bundle ID から決める。
/// None を返して既定の挙動に任せる
#[cfg(target_os = "windows")]
pub(crate) fn webview_data_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(app.path().local_data_dir().ok()?.join(CONFIG_DIR_NAME))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn webview_data_dir(_app: &AppHandle) -> Option<PathBuf> {
    None
}

/// WebView2 の起動引数（Windows のみ）。
/// 前半は wry が既定で渡している指定（引数を自分で渡すと既定のものは渡されなくなるので書き写す）。
/// `AudioVideoTracks` は実験的機能の `HTMLMediaElement.audioTracks` を有効にし、
/// MKV などに入っている複数の音声（副音声）を再生操作カードから切り替えられるようにする。
/// 同じデータフォルダを使う WebView2 は起動引数がそろっていないと作れないので、
/// すべてのウィンドウに同じものを渡す（webview_options）
#[cfg(target_os = "windows")]
pub(crate) const WEBVIEW2_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
     --enable-blink-features=AudioVideoTracks";

/// すべてのウィンドウに共通の WebView の指定（データフォルダと、Windows では起動引数）
pub(crate) fn webview_options<'a>(
    app: &AppHandle,
    builder: WebviewWindowBuilder<'a, tauri::Wry, AppHandle>,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    let mut builder = builder;
    if let Some(dir) = webview_data_dir(app) {
        builder = builder.data_directory(dir);
    }
    #[cfg(target_os = "windows")]
    {
        builder = builder.additional_browser_args(WEBVIEW2_BROWSER_ARGS);
    }
    builder
}

/// tauri.conf.json のウィンドウ定義から実際のウィンドウを作る。
/// 定義側を `create: false` にして自前で作っているのは、webview_options（データフォルダと
/// WebView2 の起動引数）を渡せるようにするため。ウィンドウの見た目や大きさは tauri.conf.json のまま
pub(crate) fn create_configured_windows(app: &AppHandle) -> Result<(), String> {
    for config in &app.config().app.windows {
        let builder = WebviewWindowBuilder::from_config(app, config)
            .map_err(|e| format!("ウィンドウ「{}」を作れません: {e}", config.label))?;
        webview_options(app, builder)
            .build()
            .map_err(|e| format!("ウィンドウ「{}」を作れません: {e}", config.label))?;
    }
    Ok(())
}

/// 起動時に作るビューアのウィンドウのラベル（tauri.conf.json の定義）
pub(crate) const MAIN_LABEL: &str = "main";

/// あとから開くビューアのウィンドウのラベルの頭（capabilities/default.json の windows と合わせる）
pub(crate) const VIEWER_LABEL_PREFIX: &str = "viewer-";

/// 新しいウィンドウを、元のウィンドウからずらして置く量（論理ピクセル）
pub(crate) const CASCADE_OFFSET: f64 = 24.0;

/// ビューア（画像などを表示するウィンドウ）か。設定・ショートカット一覧のウィンドウは違う
pub(crate) fn is_viewer(label: &str) -> bool {
    label == MAIN_LABEL || label.starts_with(VIEWER_LABEL_PREFIX)
}

/// ビューアのウィンドウに OS ごとの仕掛けを取り付けて表示する。メインスレッドで呼ぶ
pub(crate) fn setup_viewer_window(window: &WebviewWindow) {
    // 失敗しても、縦横比はあとから直す方法（keep_aspect_on_resize）で保たれる
    #[cfg(target_os = "windows")]
    if let Err(e) = win_sizing::install(window) {
        log::warn!("ウィンドウのサイズ変更を縦横比に合わせられません: {e}");
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = mac_sizing::install(window) {
        log::warn!("ウィンドウのサイズ変更を縦横比に合わせられません: {e}");
    }
    // 失敗しても表示の自動非表示が待ち時間頼みになるだけなので続ける
    #[cfg(target_os = "macos")]
    if let Err(e) = mac_pointer::install(window) {
        log::warn!("カーソルの追跡を開始できません: {e}");
    }
    // サイズを整えてから見せる（開いた直後のちらつきを避ける）
    let _ = window.show();
}

/// ビューアのウィンドウをもう 1 枚作る（tauri.conf.json の main と同じ定義で）。
/// 前面のビューアから少しずらした場所に、同じ大きさで置く。メインスレッドで呼ぶ
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn create_viewer_window(app: &AppHandle, path: &str) -> Result<WebviewWindow, String> {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    let mut config = app
        .config()
        .app
        .windows
        .iter()
        .find(|c| c.label == MAIN_LABEL)
        .cloned()
        .ok_or("ウィンドウの定義が見つかりません")?;
    let label = format!(
        "{VIEWER_LABEL_PREFIX}{}",
        NEXT.fetch_add(1, AtomicOrdering::Relaxed)
    );
    config.label = label.clone();
    let builder = WebviewWindowBuilder::from_config(app, &config)
        .map_err(|e| format!("ウィンドウを作れません: {e}"))?;
    let window = webview_options(app, builder)
        .build()
        .map_err(|e| format!("ウィンドウを作れません: {e}"))?;

    // 「自由に変更」なら種類によらない 1 つの枠、「メディアに合わせる」なら開くファイルの種類の
    // 枠の大きさと位置に置く（ほかのビューアと重なるなら少しずらす）。
    // 前面のビューアの姿を写すと、動画のウィンドウの形のまま画像を開くことになる
    let target = window.as_ref().window();
    let saved = read_window_state(app).unwrap_or_default();
    let fit = fits_window_to_image(app);
    let kinds = app.state::<WindowKind>();
    kinds.with(&label, |state| state.fit = fit);
    let set_applied = |slot: Option<WindowSlot>| {
        kinds.with(&label, |state| state.applied = slot);
    };
    // 前面のビューア（無ければどれか）の姿と、それがどの枠のものか
    let front = || {
        let viewers: Vec<_> = app
            .webview_windows()
            .into_values()
            .filter(|w| is_viewer(w.label()) && w.label() != label)
            .collect();
        let front = viewers
            .iter()
            .find(|w| w.is_focused().unwrap_or(false))
            .or_else(|| viewers.first())?;
        let slot = kinds.with(front.label(), |k| k.applied).flatten();
        Some((current_geometry(&front.as_ref().window())?, slot))
    };
    let source = if !fit {
        set_applied(Some(WindowSlot::Free));
        if saved.has_size() {
            Some(saved.geometry_only())
        } else {
            front().map(|(geometry, _)| geometry)
        }
    } else {
        match prepare_startup_listing(app, &label, path) {
            Some(k) => {
                set_applied(Some(WindowSlot::Kind(k)));
                match kind_target(&saved, k, true) {
                    KindTarget::Record(record) => Some(record),
                    KindTarget::Default => default_geometry(&target),
                    // 種類ごとの分をまだ 1 つも覚えていないときは、前面のビューアの姿から作る。
                    // ただし前面のビューアが別の種類の枠なら、その形は写さず既定の大きさにする
                    KindTarget::Adopt => match front() {
                        Some((geometry, slot)) if !matches!(slot, Some(WindowSlot::Kind(f)) if f != k) => {
                            Some(geometry)
                        }
                        _ => default_geometry(&target),
                    },
                }
            }
            // 種類が分からない（開けなかった）ときは、どの枠にも属さない既定の大きさのまま
            // （ほかのビューアと重なるならずらす）
            None => default_geometry(&target),
        }
    };
    if let Some(state) = source {
        place_kind_geometry(&target, &state);
    }
    setup_viewer_window(&window);
    Ok(window)
}

/// ファイルをビューアで開く。まだ何も表示していないビューア（起動直後など）があれば
/// そこで開き、無ければ新しいウィンドウを作って開く。メインスレッドで呼ぶ
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn open_in_viewer(app: &AppHandle, path: String) {
    let kinds = app.state::<WindowKind>();
    // 起動中で setup がまだ走っていない（ビューアが 1 つも無い）ときは、
    // これから setup で作る main に預ける。ここで新しいウィンドウを作ると、
    // あとから作られる空の main と 2 つ並んでしまう
    // （Finder から起動すると Opened は setup より先に届く）
    if !app.webview_windows().keys().any(|l| is_viewer(l)) {
        app.state::<StartupFile>()
            .with(MAIN_LABEL, |file| *file = Some(path));
        kinds.with(MAIN_LABEL, |k| k.claimed = true);
        return;
    }
    let mut free = app
        .webview_windows()
        .into_values()
        .filter(|w| is_viewer(w.label()))
        .filter(|w| {
            kinds
                .with(w.label(), |k| k.shown.is_none() && !k.claimed)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    // main を優先する（起動直後に Finder から開いたファイルは main で開く）
    free.sort_by_key(|w| w.label() != MAIN_LABEL);
    let window = match free.into_iter().next() {
        Some(window) => window,
        None => match create_viewer_window(app, &path) {
            Ok(window) => window,
            Err(e) => {
                log::error!("{e}");
                return;
            }
        },
    };
    let label = window.label().to_string();
    kinds.with(&label, |k| k.claimed = true);
    // フロントエンドが読み込み前なら get_startup_file で、読み込み済みならイベントで受け取る
    app.state::<StartupFile>()
        .with(&label, |file| *file = Some(path.clone()));
    let _ = app.emit_to(label.as_str(), "open-file", path);
    let _ = window.set_focus();
}

/// 閉じたウィンドウの分の状態を捨てる（フォルダの監視もここで止まる）
pub(crate) fn forget_window(window: &tauri::Window) {
    let app = window.app_handle();
    let label = window.label();
    app.state::<StartupFile>().remove(label);
    app.state::<StartupListing>().remove(label);
    app.state::<PendingPosition>().remove(label);
    app.state::<WindowKind>().remove(label);
    app.state::<AspectLock>().remove(label);
    app.state::<FolderWatcher>().remove(label);
    app.state::<MkvStreams>().0.remove(label);
    #[cfg(target_os = "macos")]
    mac_sizing::uninstall(window);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    install_panic_hook();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| match event {
            // 「画像に合わせる」のときだけ、ドラッグの最中も縦横比を保つ
            tauri::WindowEvent::Resized(size) if is_viewer(window.label()) => {
                keep_aspect_on_resize(window, *size);
            }
            tauri::WindowEvent::CloseRequested { api, .. } => match window.label() {
                // 設定ウィンドウは閉じずに隠す。破棄してしまうと
                // 開き直すたびに作り直しになり、閉じ忘れるとアプリが残り続ける
                "settings" | "shortcuts" => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // 最後のビューアを閉じたらアプリごと終了する（非表示の設定ウィンドウが
                // 残っていても終了できるようにする）。ほかのビューアが残っていれば、
                // そのウィンドウだけを閉じる。
                // 閉じる直前の大きさと場所は、次回ここから開くために覚えておく
                label if is_viewer(label) => {
                    save_window_state(window);
                    forget_window(window);
                    let app = window.app_handle();
                    let others = app
                        .webview_windows()
                        .keys()
                        .any(|l| is_viewer(l) && l != label);
                    if !others {
                        app.exit(0);
                    }
                }
                _ => {}
            },
            _ => {}
        })
        .setup(|app| {
            // ログの出力先が AppHandle 依存なので、ここで初めて登録できる。
            // 失敗してもアプリは起動させる（ログが無いだけで機能は使える）
            if let Err(e) = init_logging(app.handle()) {
                eprintln!("{e}");
            }

            log::info!(
                "sView {} を起動しました ({} / {})",
                app.package_info().version,
                std::env::consts::OS,
                std::env::consts::ARCH
            );

            // 起動引数のファイルは main で開く。ウィンドウを作る前に預けておく
            // （Windows ではウィンドウを作る間にフロントエンドが動き出し、
            // 預ける前に get_startup_file を呼んで空振りすることがある）
            if let Some(path) = startup_file_from_args() {
                app.state::<StartupFile>()
                    .with(MAIN_LABEL, |file| *file = Some(path));
                app.state::<WindowKind>()
                    .with(MAIN_LABEL, |k| k.claimed = true);
            }

            // tauri.conf.json のウィンドウは create: false にしてあるので、
            // ここで作る（WebView のデータフォルダを指定するため）。
            // 失敗したら起動を諦める（show_fatal_error で理由を出す）
            if let Err(e) = create_configured_windows(app.handle()) {
                log::error!("{e}");
                return Err(e.into());
            }

            if let Some(window) = app.get_webview_window(MAIN_LABEL) {
                // 開くファイル（起動引数 / macOS で Finder から開いたもの）が分かっていれば、
                // その種類の大きさと位置で開く
                let kind = app
                    .state::<StartupFile>()
                    .with(MAIN_LABEL, |file| file.clone())
                    .flatten()
                    .and_then(|path| prepare_startup_listing(app.handle(), MAIN_LABEL, &path));
                restore_window_state(&window.as_ref().window(), kind);
                setup_viewer_window(&window);
            }
            Ok(())
        })
        .manage(StartupFile::new())
        .manage(StartupListing::new())
        .manage(ArchiveCache(Mutex::new(None)))
        .manage(PendingPosition::new())
        .manage(WindowKind::new())
        .manage(AspectLock::new())
        .manage(FolderWatcher::new())
        .manage(MkvStreams(PerWindow::new()))
        .invoke_handler(tauri::generate_handler![
            window::set_window_kind,
            window::save_window_geometry,
            listing::list_images,
            archive::read_archive_image,
            get_startup_file,
            settings::app_version,
            settings::load_settings,
            settings::save_settings,
            settings::open_settings_window,
            settings::open_shortcuts_window,
            logging::reveal_in_file_manager,
            logging::open_log_folder,
            window::fit_window_to_image,
            window::set_aspect_lock,
            window::mouse_button_down,
            window::start_window_drag,
            listing::delete_image,
            audio::audio_info,
            audio::audio_artwork,
            mkv::subtitles::video_subtitles,
            mkv::mkv_open,
            mkv::mkv_init,
            mkv::mkv_seek,
            mkv::mkv_read,
            mkv::mkv_close,
            watcher::watch_folder,
            assoc::file_association_status,
            assoc::apply_file_associations
        ])
        .build(tauri::generate_context!());

    // 起動に失敗したら、せめて理由を見せてから終わる（無言で死なせない）
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            let message = format!("sView を起動できませんでした: {e}");
            log::error!("{message}");
            eprintln!("{message}");
            show_fatal_error(&message);
            std::process::exit(1);
        }
    };

    app.run(|_app_handle, _event| {
        // macOS: Finder / Dock からファイルを開いたときに届く。
        // 開いているウィンドウを置き換えず、ファイルごとに新しいウィンドウで開く
        // （起動直後でまだ何も表示していないウィンドウがあれば、そこで開く）
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = &_event {
            for path in urls
                .iter()
                .filter_map(|u| u.to_file_path().ok())
                .filter(|p| p.exists())
            {
                open_in_viewer(_app_handle, path.to_string_lossy().into_owned());
            }
        }
    });
}
