use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, State,
    WebviewWindow,
};

use crate::media::WindowKindKey;
#[cfg(target_os = "macos")]
use crate::platform::macos::{mac_drag, mac_sizing};
use crate::settings::{config_dir, fits_window_to_image, temp_path};
use crate::state::{
    AspectLock, AspectState, PendingPosition, RestoredPosition, WindowKind, WindowSlot,
};
use crate::{CASCADE_OFFSET, MAIN_LABEL, is_viewer};

/// 最小ウィンドウサイズ（tauri.conf.json の minWidth / minHeight と合わせる）
pub(crate) const MIN_WINDOW_SIZE: (f64, f64) = (200.0, 150.0);

/// ウィンドウの大きさと位置を覚えておくファイル（設定本体とは分けて、
/// 設定ウィンドウの「既定に戻す」で消えないようにする）
pub(crate) fn window_state_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("window.json"))
}

/// 各項目は Option。古い window.json（大きさだけ）もそのまま読めるようにし、
/// 保存できなかった項目は既定の挙動（中央・既定サイズ）に任せる。
/// 最上位の大きさ・位置は「自由に変更」の枠（種類によらず 1 つ）、
/// kinds は「メディアに合わせる」の種類ごとの枠
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct WindowState {
    pub(crate) width: Option<f64>,
    pub(crate) height: Option<f64>,
    pub(crate) x: Option<f64>,
    pub(crate) y: Option<f64>,
    /// x / y を記録したときのウィンドウの倍率（ディスプレイの拡大率）。
    /// Windows で位置を物理ピクセルへ戻すのに使う（無い古いファイルは今の倍率で読む）
    pub(crate) scale: Option<f64>,
    /// 「メディアに合わせる」の種類ごとの枠だけが持つ、縦横比を合わせるときに保つ広さ
    /// （論理ピクセルの面積。AspectState::area）。ウィンドウの大きさは画面に収めるために
    /// 広さより小さくなっていることがあるので、大きさから求め直さず、これで戻す
    /// （求め直すと、縦長の画像で画面に収めたあと種類を行き来するたびに縮んでいく）。
    /// 無い古いファイルは大きさから求める
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) area: Option<f64>,
    /// 「メディアに合わせる」の、種類（"image" / "video" / "audio" / "book"）ごとの大きさと位置。
    /// 表示する種類が変わるたびに、その種類で最後に使っていた大きさと位置へ戻す
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) kinds: BTreeMap<String, WindowState>,
}

impl WindowState {
    /// 大きさを持っているか（枠として使えるか）
    pub(crate) fn has_size(&self) -> bool {
        matches!((self.width, self.height), (Some(w), Some(h)) if w >= 1.0 && h >= 1.0)
    }

    /// 大きさ・位置（と広さ）だけを取り出す（種類ごとの分は含めない）
    pub(crate) fn geometry_only(&self) -> WindowState {
        WindowState {
            kinds: BTreeMap::new(),
            ..self.clone()
        }
    }

    /// 縦横比を合わせるときに保つ広さ。覚えていなければ大きさから求める
    pub(crate) fn fit_area(&self) -> f64 {
        match (self.area, self.width, self.height) {
            (Some(area), _, _) if area > 0.0 => area,
            (_, Some(width), Some(height)) => width * height,
            _ => 0.0,
        }
    }

    /// 枠 slot に geometry を書き込む。広さは種類ごとの枠だけが持つ
    pub(crate) fn store(&mut self, slot: WindowSlot, geometry: &WindowState) {
        match slot {
            WindowSlot::Free => copy_geometry(self, geometry),
            WindowSlot::Kind(kind) => {
                self.kinds
                    .insert(kind_key(kind).to_string(), geometry.geometry_only());
            }
        }
    }
}

/// 種類を window.json のキーにする
pub(crate) fn kind_key(kind: WindowKindKey) -> &'static str {
    match kind {
        WindowKindKey::Image => "image",
        WindowKindKey::Video => "video",
        WindowKindKey::Audio => "audio",
        WindowKindKey::Book => "book",
    }
}

/// 種類を表示するときに置くウィンドウの姿
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum KindTarget {
    /// その種類で最後に使っていた大きさと位置
    Record(WindowState),
    /// 今の姿をそのまま、その種類のものとして使う。
    /// 種類ごとの分をまだ 1 つも覚えていない（この機能より前の window.json の）ときの、
    /// 最初の種類だけ。前回閉じたときの姿を引き継ぐ
    Adopt,
    /// 既定の大きさ（tauri.conf.json の main）。その種類をまだ表示したことがないとき。
    /// 別の種類の大きさ・形は引き継がない
    Default,
}

/// 「メディアに合わせる」で種類 kind を表示するときに置く姿を決める。
/// first はウィンドウの今の姿がどの種類の枠のものでもないとき
/// （起動直後・「自由に変更」から切り替えたとき）
pub(crate) fn kind_target(saved: &WindowState, kind: WindowKindKey, first: bool) -> KindTarget {
    match saved.kinds.get(kind_key(kind)).filter(|r| r.has_size()) {
        Some(record) => KindTarget::Record(record.geometry_only()),
        None if first && saved.kinds.is_empty() => KindTarget::Adopt,
        None => KindTarget::Default,
    }
}

/// window.json を書く。大きさや位置が決まるたびに書くので、書きかけのファイルを
/// 読まれない（ほかのウィンドウ・別に起動した sView から）よう、一時ファイル経由で置き換える
pub(crate) fn write_window_state(app: &AppHandle, state: &WindowState) {
    let Ok(path) = window_state_file(app) else {
        return;
    };
    let (Some(dir), Ok(text)) = (path.parent(), serde_json::to_string_pretty(state)) else {
        return;
    };
    let _ = fs::create_dir_all(dir);
    let tmp = temp_path(&path);
    if fs::write(&tmp, text).is_ok() && fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// 今のウィンドウの位置と大きさ（論理ピクセル）。
/// 最小化中・最大化中・全画面中は普段の姿と違うので None
pub(crate) fn current_geometry(window: &tauri::Window) -> Option<WindowState> {
    let scale = window.scale_factor().ok()?;
    // 全画面を先に見る（macOS で全画面へ切り替える間に is_maximized を呼ぶと、
    // AppKit がその中から大きさの変更を問い合わせてくる。keep_aspect_on_resize と同じ）
    if window.is_fullscreen().unwrap_or(false)
        || window.is_minimized().unwrap_or(false)
        || window.is_maximized().unwrap_or(false)
    {
        return None;
    }
    let mut state = WindowState::default();
    if let Ok(pos) = window.outer_position() {
        let pos = pos.to_logical::<f64>(scale);
        state.x = Some(pos.x);
        state.y = Some(pos.y);
        state.scale = Some(scale);
    }
    if let Ok(size) = window.inner_size() {
        let size = size.to_logical::<f64>(scale);
        if size.width >= 1.0 && size.height >= 1.0 {
            state.width = Some(size.width);
            state.height = Some(size.height);
        }
    }
    Some(state)
}

/// geometry の大きさ・位置を state に書き写す（kinds はそのまま）。
/// 位置は倍率と組で意味を持つので、x / y / scale はまとめて書き写す
pub(crate) fn copy_geometry(state: &mut WindowState, geometry: &WindowState) {
    if geometry.x.is_some() && geometry.y.is_some() {
        state.x = geometry.x;
        state.y = geometry.y;
        state.scale = geometry.scale;
    }
    state.width = geometry.width.or(state.width);
    state.height = geometry.height.or(state.height);
}

pub(crate) fn read_window_state(app: &AppHandle) -> Option<WindowState> {
    let path = window_state_file(app).ok()?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// ウィンドウの今の位置と大きさを、その姿が属している枠（applied）に保存する。
/// 大きさ・位置が決まるたびに（フロントエンドの save_window_geometry）と、閉じるときに呼ぶ。
/// 枠を置き直す前（最大化中などに種類・設定が変わったとき）も、姿はまだ前の枠のものなので、
/// そちらに書く
pub(crate) fn save_window_state(window: &tauri::Window) {
    let app = window.app_handle();
    // 最小化中・最大化中・全画面中は位置も大きさも普段の姿と違うので触らない
    // （その状態のまま閉じたときは、そうする前の姿を覚えたままにする）
    let Some(geometry) = current_geometry(window) else {
        return;
    };
    // 何も表示していない（どの枠にも属していない）ウィンドウは記録しない
    let Some(slot) = app
        .state::<WindowKind>()
        .with(window.label(), |k| k.applied)
        .flatten()
    else {
        return;
    };
    let geometry = with_fit_area(window, slot, geometry);
    let mut state = read_window_state(app).unwrap_or_default();
    state.store(slot, &geometry);
    write_window_state(app, &state);
}

/// 種類ごとの枠に書くときは、縦横比を合わせるときに保っている広さも添える
pub(crate) fn with_fit_area(
    window: &tauri::Window,
    slot: WindowSlot,
    mut geometry: WindowState,
) -> WindowState {
    if matches!(slot, WindowSlot::Kind(_)) {
        geometry.area = window
            .app_handle()
            .state::<AspectLock>()
            .with(window.label(), |aspect| aspect.area)
            .filter(|area| *area > 0.0);
    }
    geometry
}

/// 大きさ・位置が決まったところで、すぐに window.json へ書く（閉じるまで待たない）。
/// フロントエンドが移動・サイズ変更の通知が落ち着き、マウスのボタンが離されたところで呼ぶ
#[tauri::command]
pub(crate) fn save_window_geometry(window: WebviewWindow) {
    save_window_state(&window.as_ref().window());
}

/// 画面上の長方形。ディスプレイの範囲や作業領域を表す。
/// 物理ピクセルか論理ピクセルかは使う側で揃える
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Area {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl Area {
    pub(crate) fn from_physical(position: PhysicalPosition<i32>, size: PhysicalSize<u32>) -> Self {
        Self {
            x: f64::from(position.x),
            y: f64::from(position.y),
            width: f64::from(size.width),
            height: f64::from(size.height),
        }
    }

    pub(crate) fn to_logical(self, scale: f64) -> Self {
        Self {
            x: self.x / scale,
            y: self.y / scale,
            width: self.width / scale,
            height: self.height / scale,
        }
    }

    pub(crate) fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// ディスプレイ全体の範囲（物理ピクセル）
pub(crate) fn monitor_bounds(monitor: &tauri::Monitor) -> Area {
    Area::from_physical(*monitor.position(), *monitor.size())
}

/// ディスプレイの作業領域（物理ピクセル）。
/// タスクバー・メニューバー・Dock を除いた、ウィンドウを置ける範囲
pub(crate) fn monitor_work_area(monitor: &tauri::Monitor) -> Area {
    let work_area = monitor.work_area();
    Area::from_physical(work_area.position, work_area.size)
}

/// 左上 (x, y)・大きさ (width, height) のウィンドウが area に収まるよう位置をずらす。
/// area より大きいときは左上を area の左上に合わせる（右下がはみ出す）
pub(crate) fn clamp_to_area(x: f64, y: f64, width: f64, height: f64, area: &Area) -> (f64, f64) {
    let x = x.min(area.x + area.width - width).max(area.x);
    let y = y.min(area.y + area.height - height).max(area.y);
    (x, y)
}

/// 画面上の位置（ウィンドウの左上・ディスプレイの範囲）を数える座標が物理ピクセルか。
/// OS によって、ディスプレイをまたいで通しで数えられる単位が違う。
/// Windows は仮想画面の物理ピクセルが通しの座標で、論理ピクセルはディスプレイの倍率
/// ごとに目盛りが変わる（倍率の違うディスプレイを並べると、論理ピクセルの位置では
/// どのディスプレイの上か決まらず、ウィンドウの今の倍率で戻すと別のディスプレイへ行く）。
/// macOS はポイント（論理ピクセル）が通しの座標で、物理ピクセルの方がディスプレイごとに変わる
pub(crate) const PHYSICAL_DESKTOP: bool = cfg!(target_os = "windows");

/// 論理ピクセルを画面上の位置の座標へ直す倍率（scale はその場所のディスプレイの倍率）
pub(crate) fn desktop_ratio(scale: f64, physical: bool) -> f64 {
    if physical { scale } else { 1.0 }
}

/// 保存した左上を画面上の位置の座標で返す。
/// window.json の x / y は論理ピクセルなので、記録したときの倍率で直す
/// （倍率の無い古いファイルは今の倍率 current_scale で読む）
pub(crate) fn saved_position(
    state: &WindowState,
    current_scale: f64,
    physical: bool,
) -> Option<(f64, f64)> {
    let ratio = desktop_ratio(state.scale.unwrap_or(current_scale), physical);
    Some((state.x? * ratio, state.y? * ratio))
}

/// 画面上の位置の座標を、倍率 scale のウィンドウの物理ピクセルへ直す
pub(crate) fn desktop_to_physical(
    (x, y): (f64, f64),
    scale: f64,
    physical: bool,
) -> PhysicalPosition<i32> {
    LogicalPosition::new(x, y).to_physical(scale / desktop_ratio(scale, physical))
}

/// ディスプレイの範囲（物理ピクセル）を画面上の位置の座標へ直す
pub(crate) fn desktop_area(area: Area, scale: f64) -> Area {
    area.to_logical(scale / desktop_ratio(scale, PHYSICAL_DESKTOP))
}

/// ウィンドウの左上を画面上の位置の座標で置く
pub(crate) fn set_desktop_position(window: &tauri::Window, (x, y): (f64, f64)) {
    let _ = if PHYSICAL_DESKTOP {
        window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32))
    } else {
        window.set_position(LogicalPosition::new(x, y))
    };
}

/// 保存した位置（画面上の位置の座標）が載っているディスプレイの作業領域（同じ座標）と、
/// そのディスプレイの倍率。
/// 前回使っていた外部ディスプレイが外れている場合は None（画面外へ開くのを防ぐ）
pub(crate) fn work_area_at(window: &tauri::Window, x: f64, y: f64) -> Option<(Area, f64)> {
    let monitors = window.available_monitors().ok()?;
    monitors
        .iter()
        .map(|m| (desktop_area(monitor_bounds(m), m.scale_factor()), m))
        .find(|(bounds, _)| {
            // 端にぴったり寄せた場合を落とさないよう少しだけ余裕を見る
            const SLACK: f64 = 8.0;
            let slack_bounds = Area {
                x: bounds.x - SLACK,
                y: bounds.y - SLACK,
                width: bounds.width + SLACK,
                height: bounds.height + SLACK,
            };
            slack_bounds.contains(x, y)
        })
        .map(|(_, m)| {
            (
                desktop_area(monitor_work_area(m), m.scale_factor()),
                m.scale_factor(),
            )
        })
}

/// 外枠と中身の大きさの差（物理ピクセル）。
/// Windows の枠なしウィンドウは影のぶん外枠が中身より大きい（macOS では 0）
pub(crate) fn frame_size(outer: PhysicalSize<u32>, inner: PhysicalSize<u32>) -> (u32, u32) {
    (
        outer.width.saturating_sub(inner.width),
        outer.height.saturating_sub(inner.height),
    )
}

/// 中身の大きさを inner から new_inner に変えたあとの、外枠の左上（物理ピクセル）。
/// anchor があればそこに左上を合わせ（起動直後の 1 枚目）、なければ変更前の中心を保つ。
/// どちらも、作業領域からはみ出す分は中へ寄せる。
///
/// 中心の計算は外枠の大きさで行う。Windows の枠なしウィンドウは外枠（outer）が
/// 中身（inner）より影のぶん大きく、新しい大きさに同じ差を足さずに中身の大きさで
/// 中心を求めると、画像を切り替えるたびに差の半分ずつ右下へずれていく
pub(crate) fn fitted_position(
    pos: PhysicalPosition<i32>,
    outer: PhysicalSize<u32>,
    inner: PhysicalSize<u32>,
    new_inner: PhysicalSize<u32>,
    anchor: Option<PhysicalPosition<i32>>,
    work_area: Option<&Area>,
) -> PhysicalPosition<i32> {
    let frame = frame_size(outer, inner);
    let new_outer = PhysicalSize::new(new_inner.width + frame.0, new_inner.height + frame.1);

    let (x, y) = match anchor {
        Some(anchor) => (anchor.x, anchor.y),
        // 整数のまま計算する（論理ピクセルの丸めで少しずつずれないように）
        None => (
            pos.x + (outer.width as i32 - new_outer.width as i32) / 2,
            pos.y + (outer.height as i32 - new_outer.height as i32) / 2,
        ),
    };

    match work_area {
        Some(area) => {
            let (x, y) = clamp_to_area(
                f64::from(x),
                f64::from(y),
                f64::from(new_outer.width),
                f64::from(new_outer.height),
                area,
            );
            PhysicalPosition::new(x as i32, y as i32)
        }
        None => PhysicalPosition::new(x, y),
    }
}

/// 同じ左上に置けるよう、保存した位置を PendingPosition に残しておく
pub(crate) fn restore_window_state(window: &tauri::Window, kind: Option<WindowKindKey>) {
    let app = window.app_handle();
    let label = window.label();
    let fit = fits_window_to_image(app);
    let kinds = app.state::<WindowKind>();
    kinds.with(label, |k| k.fit = fit);
    let Some(kind) = kind else {
        return;
    };
    let saved = read_window_state(app).unwrap_or_default();
    if !fit {
        kinds.with(label, |k| k.applied = Some(WindowSlot::Free));
        place_and_remember(window, &saved.geometry_only());
        return;
    }
    kinds.with(label, |k| k.applied = Some(WindowSlot::Kind(kind)));
    match kind_target(&saved, kind, true) {
        KindTarget::Record(record) => {
            set_fit_area(window, record.fit_area());
            place_and_remember(window, &record);
        }
        KindTarget::Adopt => place_and_remember(window, &saved.geometry_only()),
        // 初めて表示する種類は tauri.conf.json の既定の大きさのまま、画面の中央に開く
        KindTarget::Default => {}
    }
}

/// 縦横比を合わせるときに保つ広さを area にする（縦横比の固定は外す。
/// フロントエンドが表示するものに合わせてかけ直す）
pub(crate) fn set_fit_area(window: &tauri::Window, area: f64) {
    window
        .app_handle()
        .state::<AspectLock>()
        .with(window.label(), |aspect| {
            *aspect = AspectState {
                area,
                ..AspectState::default()
            }
        });
}

/// 保存した姿へ置き、「画像に合わせる」なら最初の縦横比合わせをその左上に合わせる
pub(crate) fn place_and_remember(window: &tauri::Window, state: &WindowState) {
    let placed = place_window(window, state);
    remember_placement(window, placed);
}

/// 「画像に合わせる」なら、次の fit_window_to_image が置いた左上に合わせるよう残す。
/// 置けなかったとき（位置を覚えていない・どの画面にも載らない）は、前に残したものも消す
/// （古い左上へ合わせて、別の場所へ飛ばないように）
pub(crate) fn remember_placement(window: &tauri::Window, placed: Option<RestoredPosition>) {
    let app = window.app_handle();
    let placed = placed.filter(|_| fits_window_to_image(app));
    app.state::<PendingPosition>()
        .with(window.label(), |pending| *pending = placed);
}

/// 保存した大きさ・位置にウィンドウを置く。
/// 大きさは置く先のディスプレイの作業領域の SCREEN_RATIO に収まるよう、縦横比を保って縮める。
/// 位置はウィンドウ全体が作業領域に収まるよう寄せる（ディスプレイの構成が
/// 変わっていたり、前回より大きく開いたりしたときに画面外へ出さない）。
/// 保存した位置がどの画面にも載らない（前回のディスプレイが外れている）・位置を覚えていない
/// ときは、今のディスプレイの作業領域の中央に置く。
/// 位置を戻したときは、保存した左上と実際に置いた左上を返す。
///
/// 大きさは論理ピクセルで戻す。倍率の違うディスプレイへ移ると、Windows でも macOS でも
/// 論理ピクセルの大きさを保ったまま描き直されるので、見た目の大きさは閉じたときと同じになる
pub(crate) fn place_window(
    window: &tauri::Window,
    state: &WindowState,
) -> Option<RestoredPosition> {
    let scale = window.scale_factor().ok()?;
    let size = (state.width, state.height);
    let size = match size {
        (Some(width), Some(height)) if width >= 1.0 && height >= 1.0 => Some((width, height)),
        _ => None,
    };
    let saved = saved_position(state, scale, PHYSICAL_DESKTOP);
    if size.is_none() && saved.is_none() {
        return None;
    }
    // 保存した位置が載っているディスプレイ。無ければ今のディスプレイ（そこの中央に置く）
    let restored = saved.and_then(|(x, y)| work_area_at(window, x, y));
    let screen = restored.or_else(|| current_desktop_work_area(window));

    // 中身の大きさ（論理ピクセル）。保存した大きさがあれば、画面に収めてそれにする
    let mut inner = window
        .inner_size()
        .map(|s| s.to_logical::<f64>(scale))
        .unwrap_or_else(|_| LogicalSize::new(0.0, 0.0));
    let mut resized = false;
    if let Some(size) = size {
        let limit = screen
            .map(|(area, target_scale)| desktop_limit(&area, target_scale))
            .unwrap_or(NO_LIMIT);
        let (width, height) = within_limit(size, limit);
        let _ = window.set_size(LogicalSize::new(width, height));
        inner = LogicalSize::new(width, height);
        resized = true;
    }

    let (work_area, target_scale) = screen?;
    // 外枠の大きさ = 中身 + 枠（Windows の枠なしウィンドウの影のぶん）。
    // 置く先のディスプレイの倍率で、画面上の位置の座標へ直す
    let frame = match (window.outer_size(), window.inner_size()) {
        (Ok(outer), Ok(current)) => frame_size(outer, current),
        _ => (0, 0),
    };
    let ratio = desktop_ratio(target_scale, PHYSICAL_DESKTOP);
    let outer_width = (inner.width + f64::from(frame.0) / scale) * ratio;
    let outer_height = (inner.height + f64::from(frame.1) / scale) * ratio;
    let (x, y) = match (saved, restored) {
        (Some((saved_x, saved_y)), Some(_)) => {
            clamp_to_area(saved_x, saved_y, outer_width, outer_height, &work_area)
        }
        _ => centered_in_area(outer_width, outer_height, &work_area),
    };
    set_desktop_position(window, (x, y));
    // Windows で倍率の違うディスプレイへ移したときは、移った先の倍率で大きさを付け直す
    // （移る前に付けた大きさは前のディスプレイの倍率で物理ピクセルにしてあり、
    // 設定によっては OS が描き直しても論理ピクセルの大きさが保たれない）。
    // 大きさを変えても左上は動かない。macOS は大きさを変えると左下が残って
    // 左上がずれるので、位置より後には付けない（倍率が違っても論理ピクセルのまま描かれる）
    if PHYSICAL_DESKTOP
        && resized
        && let Ok(now) = window.scale_factor()
        && (now - scale).abs() > f64::EPSILON
    {
        let _ = window.set_size(inner);
    }
    // 中央に置いたときは、最初の縦横比合わせも中心を保てばよいので覚えない
    restored?;
    Some(RestoredPosition {
        saved: saved?,
        placed: (x, y),
    })
}

/// 今のディスプレイの作業領域（画面上の位置の座標）と、そのディスプレイの倍率
pub(crate) fn current_desktop_work_area(window: &tauri::Window) -> Option<(Area, f64)> {
    let monitor = window.current_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    Some((desktop_area(monitor_work_area(&monitor), scale), scale))
}

/// 作業領域（画面上の位置の座標）の中央に、大きさ (width, height)（同じ座標）のウィンドウを
/// 置くときの左上。作業領域より大きいときは左上を作業領域の左上に合わせる
pub(crate) fn centered_in_area(width: f64, height: f64, area: &Area) -> (f64, f64) {
    clamp_to_area(
        area.x + (area.width - width) / 2.0,
        area.y + (area.height - height) / 2.0,
        width,
        height,
        area,
    )
}

/// ディスプレイの作業領域（画面上の位置の座標。倍率 scale のディスプレイ）に収める
/// ウィンドウの中身の大きさの上限（論理ピクセル）
pub(crate) fn desktop_limit(area: &Area, scale: f64) -> (f64, f64) {
    let ratio = desktop_ratio(scale, PHYSICAL_DESKTOP);
    (
        area.width / ratio * SCREEN_RATIO,
        area.height / ratio * SCREEN_RATIO,
    )
}

/// 大きさ size を、縦横比を保ったまま limit に収める（収まっていればそのまま）。
/// 最小サイズは下回らない
pub(crate) fn within_limit((width, height): (f64, f64), limit: (f64, f64)) -> (f64, f64) {
    let shrink = 1.0_f64.min(limit.0 / width).min(limit.1 / height);
    (
        (width * shrink).max(MIN_WINDOW_SIZE.0),
        (height * shrink).max(MIN_WINDOW_SIZE.1),
    )
}

/// 表示するファイルの種類（画像の本モードも 1 つの種類として扱う）と、ウィンドウのサイズの
/// 設定が「メディアに合わせる」か（fit）を受け取り、ウィンドウを使うべき枠の姿にそろえる
/// （apply_window_kind）。kind が null のとき（何も表示していない）は設定だけを受け取る。
/// 置き直したら true（フロントエンドは「メディアに合わせる」なら縦横比を合わせ直す）
#[tauri::command]
pub(crate) fn set_window_kind(
    window: WebviewWindow,
    state: State<WindowKind>,
    kind: Option<WindowKindKey>,
    fit: bool,
) -> bool {
    state.with(window.label(), |k| {
        k.fit = fit;
        if kind.is_some() {
            k.shown = kind;
        }
    });
    apply_window_kind(&window.as_ref().window())
}

/// ウィンドウの姿を、使うべき枠（KindState::target）のものにそろえる。置き直したら true。
/// 枠が変わるとき（「メディアに合わせる」で種類が変わったとき・設定を切り替えたとき）は、
/// 今の姿をそれまでの枠に書いてから、新しい枠で最後に使っていた大きさと位置へ置き直す。
///
/// 最大化中・全画面中・最小化中は待つ（大きさを変えるとその状態が解けてしまううえ、
/// 普段の姿が取れないので前の枠の分も書けない）。その間は縦横比の固定も外しておき、
/// 元に戻ったとき OS が戻す前の枠の大きさを、表示中のものの縦横比へ引き戻さないようにする。
/// 戻ったあとフロントエンドがもう一度呼ぶ（サイズ変更が落ち着いたとき・フォーカスを得たとき）か、
/// fit_window_to_image が呼ばれたところで置き直す
pub(crate) fn apply_window_kind(window: &tauri::Window) -> bool {
    let app = window.app_handle();
    let label = window.label();
    let kinds = app.state::<WindowKind>();
    let Some((target, applied)) = kinds.with(label, |k| (k.target(), k.applied)) else {
        return false;
    };
    let Some(target) = target else {
        return false;
    };
    if applied == Some(target) {
        return false;
    }
    // ウィンドウの状態は、ロックを取っていないここで調べる（current_geometry の中で
    // macOS の AppKit がサイズ変更の問い合わせ（mac_sizing）を呼ぶことがある）
    let Some(geometry) = current_geometry(window) else {
        // 縦横比の固定だけ外す。広さはまだ前の枠のものなので、置き直すときにそちらへ書く
        app.state::<AspectLock>()
            .with(label, |aspect| aspect.unlock());
        return false;
    };
    let mut saved = read_window_state(app).unwrap_or_default();
    // 置く姿は、今の姿を書き込む前の window.json から決める
    let placement = match target {
        // 「自由に変更」の枠が無ければ（初めて使う）、今の姿をそのまま使う
        WindowSlot::Free => saved.has_size().then(|| saved.geometry_only()),
        WindowSlot::Kind(kind) => {
            let first = !matches!(applied, Some(WindowSlot::Kind(_)));
            match kind_target(&saved, kind, first) {
                KindTarget::Record(record) => Some(record),
                KindTarget::Adopt => None,
                KindTarget::Default => default_geometry(window),
            }
        }
    };
    // 今の姿はそれまでの枠のもの。そちらに書いておく
    // （何も表示していなかったウィンドウの姿は、どの枠のものでもないので書かない）
    if let Some(previous) = applied {
        saved.store(previous, &with_fit_area(window, previous, geometry));
        write_window_state(app, &saved);
    }
    kinds.with(label, |k| k.applied = Some(target));
    match placement {
        Some(placement) => {
            place_kind_geometry(window, &placement);
            true
        }
        None => {
            // 今の姿をそのまま使う。前の枠の広さは持ち越さず、今の大きさから取り直す
            set_fit_area(window, 0.0);
            false
        }
    }
}

/// tauri.conf.json の main の大きさ（論理ピクセル）。まだ表示したことのない種類はこの大きさで開く
pub(crate) fn default_window_size(app: &AppHandle) -> (f64, f64) {
    app.config()
        .app
        .windows
        .iter()
        .find(|c| c.label == MAIN_LABEL)
        .map(|c| (c.width, c.height))
        .unwrap_or((960.0, 640.0))
}

/// まだ表示したことのない種類の姿。既定の大きさで、今のウィンドウの中心に置く
/// （画面からはみ出す分は作業領域の中へ寄せる）
pub(crate) fn default_geometry(window: &tauri::Window) -> Option<WindowState> {
    let scale = window.scale_factor().ok()?;
    let work_area = current_work_area(window);
    let (width, height) = within_limit(
        default_window_size(window.app_handle()),
        screen_limit(work_area, scale),
    );
    let pos = window.outer_position().ok()?;
    let outer = window.outer_size().ok()?;
    let inner = window.inner_size().ok()?;
    let new_inner: PhysicalSize<u32> = LogicalSize::new(width, height).to_physical(scale);
    let at = fitted_position(pos, outer, inner, new_inner, None, work_area.as_ref());
    // window.json と同じく、論理ピクセルと記録したときの倍率で持つ
    let at = at.to_logical::<f64>(scale);
    Some(WindowState {
        width: Some(width),
        height: Some(height),
        x: Some(at.x),
        y: Some(at.y),
        scale: Some(scale),
        ..WindowState::default()
    })
}

/// 種類の姿へ置き直す。
/// 縦横比の固定を外し、その枠の広さを次の縦横比合わせの基準にする
/// （外さないと、set_size の通知を手で変えたものと取り違えて前の縦横比へ引き戻す。
/// 固定はこのあとフロントエンドが表示するものに合わせてかけ直す）。
/// 置いたあと最初の縦横比合わせは、置いた左上に合わせる。
/// ロックは set_size の前に手放す（その場で Resized が呼ばれると止まる）
pub(crate) fn place_kind_geometry(window: &tauri::Window, target: &WindowState) {
    set_fit_area(window, target.fit_area());
    let target = cascaded(window, target);
    place_and_remember(window, &target);
}

/// ほかのビューア（macOS で開いた 2 枚目以降のウィンドウ）とぴったり重なる位置なら、
/// 重ならないところまで少しずつ右下へずらす
pub(crate) fn cascaded(window: &tauri::Window, target: &WindowState) -> WindowState {
    let others: Vec<(f64, f64)> = window
        .app_handle()
        .webview_windows()
        .into_values()
        .filter(|w| is_viewer(w.label()) && w.label() != window.label())
        .filter_map(|w| current_geometry(&w.as_ref().window()))
        .filter_map(|g| Some((g.x?, g.y?)))
        .collect();
    let mut result = target.clone();
    let (Some(mut x), Some(mut y)) = (target.x, target.y) else {
        return result;
    };
    for _ in 0..others.len() {
        let overlapping = others
            .iter()
            .any(|(ox, oy)| (ox - x).abs() < 2.0 && (oy - y).abs() < 2.0);
        if !overlapping {
            break;
        }
        x += CASCADE_OFFSET;
        y += CASCADE_OFFSET;
    }
    result.x = Some(x);
    result.y = Some(y);
    result
}

/// ウィンドウの大きさの上限（作業領域に対する割合）。
/// 記録した大きさで置くときと、「メディアに合わせる」で縦横比を合わせるたびに、この中へ収める
/// （手で端を引っ張ったときは抑えない）。画面ぎりぎりではなく少し余裕を残す
pub(crate) const SCREEN_RATIO: f64 = 0.95;

/// 大きさの制限なし（sized_to_aspect の limit に渡す）
pub(crate) const NO_LIMIT: (f64, f64) = (f64::INFINITY, f64::INFINITY);

/// ウィンドウの中身の大きさの上限（論理ピクセル）。
/// 画面（タスクバーなどを除いた作業領域）からはみ出させないためのもので、
/// 作業領域が分からないときは制限しない
pub(crate) fn screen_limit(work_area: Option<Area>, scale: f64) -> (f64, f64) {
    work_area
        .map(|a| a.to_logical(scale))
        .map(|a| (a.width * SCREEN_RATIO, a.height * SCREEN_RATIO))
        .unwrap_or(NO_LIMIT)
}

/// 今のディスプレイの作業領域（物理ピクセル）。取得できないときは None
pub(crate) fn current_work_area(window: &tauri::Window) -> Option<Area> {
    window
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| monitor_work_area(&m))
}

/// 縦横比 aspect（横 / 縦）と広さ area（論理ピクセルの面積）から、
/// ウィンドウの中身の大きさを決める。
/// extra（論理ピクセルの横・縦）は縦横比に含めない固定の余白で、縦横比は
/// 大きさからこの分を除いた残りに対して保つ（画像・動画では 0）。
/// limit に収まらない場合は縦横比を保ったまま縮め、小さすぎる場合は保ったまま広げる。
/// 極端な縦横比では両立しないことがあるが、そのときは最小サイズを優先する
pub(crate) fn sized_to_aspect(
    aspect: f64,
    extra: (f64, f64),
    area: f64,
    limit: (f64, f64),
) -> (f64, f64) {
    let aspect = if aspect > 0.0 { aspect } else { 1.0 };
    let (ex, ey) = (extra.0.max(0.0), extra.1.max(0.0));
    let area = area.max(MIN_WINDOW_SIZE.0 * MIN_WINDOW_SIZE.1);

    // 余白を除いた部分の横を cw として (cw + ex) * (cw / aspect + ey) = area を解く
    // （余白が 0 なら cw = √(area × aspect)）
    let a = 1.0 / aspect;
    let b = ey + ex / aspect;
    let c = ex * ey - area;
    let mut cw = ((b * b - 4.0 * a * c).max(0.0).sqrt() - b) / (2.0 * a);

    // 画面に収める（縮める）
    cw = cw.min(limit.0 - ex).min((limit.1 - ey) * aspect);
    // 最小サイズを下回らない（広げる）
    cw = cw
        .max(MIN_WINDOW_SIZE.0 - ex)
        .max((MIN_WINDOW_SIZE.1 - ey) * aspect)
        .max(1.0);
    (cw + ex, cw / aspect + ey)
}

/// 「画像に合わせる」で、ウィンドウの縦横比を表示中の画像に固定する。
/// ratio が null のときは解除する（「自由に変更」や、画像を開いていない・読み込み中のとき）。
/// 解除しても基準の広さは残す。広さを覚えていないときだけ、今のウィンドウの大きさを
/// 基準の広さとして覚える
///
/// 表示中の種類の大きさ・位置へまだ置き直していない間（最大化中などに種類が変わったとき）は
/// 固定しない。ウィンドウの普段の姿はまだ前の種類のもので、元に戻したときに OS が戻す
/// その大きさを、表示中のものの縦横比へ引き戻してしまうため
#[tauri::command]
pub(crate) fn set_aspect_lock(
    window: WebviewWindow,
    lock: State<AspectLock>,
    kinds: State<WindowKind>,
    ratio: Option<f64>,
    extra_width: Option<f64>,
    extra_height: Option<f64>,
) {
    let pending = kinds.with(window.label(), |k| k.pending()).unwrap_or(false);
    let Ok(mut map) = lock.0.lock() else {
        return;
    };
    let state = map.entry(window.label().to_string()).or_default();
    let Some(ratio) = ratio.filter(|r| *r > 0.0 && !pending) else {
        state.unlock();
        return;
    };
    state.ratio = Some(ratio);
    state.extra = (extra_width.unwrap_or(0.0), extra_height.unwrap_or(0.0));
    if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
        // 引っ張られた辺を判断する基準。ここから動いた分を見る
        // （ドラッグが終わったあとも呼ばれ、基準を実際の大きさに戻す）
        state.last = size;
        state.reported = size;
        // 広さは覚えていればそのまま使う（画像ごとに取り直すと、画面に収める
        // 丸め込みのぶんだけ縦長の画像のたびに少しずつ縮んでしまう）
        if state.area <= 0.0 {
            let inner = size.to_logical::<f64>(scale);
            state.area = inner.width * inner.height;
        }
    }
}

/// 引っ張られた辺（変化の割合が大きい方）と縦横比から、目指す広さを出す。
/// もう一方の辺はこの広さと縦横比から決まるので、引っ張った辺はそのまま残る
/// extra は縦横比に含めない固定の余白（sized_to_aspect と同じ）
pub(crate) fn dragged_area(
    now: LogicalSize<f64>,
    before: LogicalSize<f64>,
    ratio: f64,
    extra: (f64, f64),
) -> f64 {
    let (ex, ey) = extra;
    let dw = (now.width - before.width).abs() / before.width.max(1.0);
    let dh = (now.height - before.height).abs() / before.height.max(1.0);
    if dw >= dh {
        now.width * ((now.width - ex).max(0.0) / ratio + ey)
    } else {
        now.height * ((now.height - ey).max(0.0) * ratio + ex)
    }
}

/// 端・角を引っ張っている最中に、OS が大きさを決める前の段階で縦横比へ合わせるときの広さ。
/// horizontal / vertical は、引っ張られているのが左右の辺 / 上下の辺か（角なら両方）。
/// 角のときは「横に合わせた広さ」と「縦に合わせた広さ」の大きい方をとる。
/// どちらもマウスの位置に対して連続に変わるので、斜めに動かしても大きさが飛ばない
/// （変化の大きい方を毎回選び直すと、選ぶ辺が入れ替わるたびに大きさが行き来してちらつく）
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
pub(crate) fn sizing_area(
    horizontal: bool,
    vertical: bool,
    now: LogicalSize<f64>,
    ratio: f64,
    extra: (f64, f64),
) -> f64 {
    let (ex, ey) = extra;
    let by_width = now.width * ((now.width - ex).max(0.0) / ratio + ey);
    let by_height = now.height * ((now.height - ey).max(0.0) * ratio + ex);
    match (horizontal, vertical) {
        (true, false) => by_width,
        (false, true) => by_height,
        _ => by_width.max(by_height),
    }
}

/// ウィンドウの移動を始める（ドラッグ領域を押したとき）。
/// macOS では Tauri の start_dragging だと、非アクティブのウィンドウを掴んだときに
/// ウィンドウの左上がカーソルへ飛ぶので、自前で移動を始める（mac_drag）
#[tauri::command]
pub(crate) fn start_window_drag(window: WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let target = window.clone();
        window
            .run_on_main_thread(move || {
                if let Ok(ptr) = target.ns_window() {
                    // SAFETY: メインスレッドで、生きている NSWindow を指す
                    mac_drag::start(unsafe { &*ptr.cast::<objc2_app_kit::NSWindow>() });
                }
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.start_dragging().map_err(|e| e.to_string())
    }
}

/// マウスのボタンが押されたままか（どのボタンでも）。
/// ウィンドウの端を引っ張っている間は OS がマウスを握っていて WebView に mouseup が届かないので、
/// フロントエンドはサイズ変更の通知が途切れたときにこれで「まだ引っ張っているか」を確かめ、
/// 手を離すまで表示中のコンテンツの大きさを据え置く
#[tauri::command]
pub(crate) fn mouse_button_down() -> bool {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetAsyncKeyState(key: i32) -> i16;
        }
        // VK_LBUTTON / VK_RBUTTON / VK_MBUTTON。左右を入れ替えた設定でも物理ボタンで見るので両方調べる
        [0x01, 0x02, 0x04]
            .iter()
            .any(|&key| (unsafe { GetAsyncKeyState(key) } as u16 & 0x8000) != 0)
    }
    #[cfg(target_os = "macos")]
    {
        let buttons: usize =
            unsafe { objc2::msg_send![objc2::class!(NSEvent), pressedMouseButtons] };
        buttons != 0
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        false
    }
}

/// 大きさが変わるたびに、ウィンドウを画像の縦横比へ引き戻す。
/// ドラッグの最中も届くので、どの辺・どの角を引っ張っても縦横比のまま変わる
/// （OS に縦横比を渡す仕組みが Tauri には無いため、届いたその場で直している）。
/// 引っ張られた辺（変化の割合が大きい方）をそのまま活かし、もう一方をそこから決め、
/// そのときの大きさを次の画像へ引き継ぐ広さとして覚える
pub(crate) fn keep_aspect_on_resize(window: &tauri::Window, size: PhysicalSize<u32>) {
    // 最小化すると 0 を知らせてくる OS があるので、大きさの無い通知は捨てる
    // （そのまま計算すると、戻したときに最小サイズのウィンドウになってしまう）
    if size.width == 0 || size.height == 0 {
        return;
    }
    // ウィンドウの状態は AspectLock を取る前に調べる。macOS では全画面へ切り替える間、
    // is_maximized が NSWindow の isZoomed まで呼ぶ（tao が一時的にタイトル付きにするため）。
    // その中から AppKit が windowWillResize:toSize:（mac_sizing）を呼びうるので、
    // ロックを持ったままだと同じスレッドで取り直すことになり、アプリが落ちる。
    // 最小化中・全画面中・最大化中は普段の姿ではないので、覚えている大きさも触らない。
    // 全画面を先に見て、切り替え中は is_maximized を呼ばずに済ませる
    if window.is_fullscreen().unwrap_or(false)
        || window.is_minimized().unwrap_or(false)
        || window.is_maximized().unwrap_or(false)
    {
        return;
    }
    // macOS で端・角を引っ張っている間は、OS が大きさを決める前に mac_sizing が
    // 縦横比へ合わせ済み。ここで直し直すと（set_size は後から非同期で効く）
    // OS の大きさとこちらの大きさが交互に描かれてちらつく
    #[cfg(target_os = "macos")]
    if mac_sizing::in_live_resize(window) {
        return;
    }
    let Ok(scale) = window.scale_factor() else {
        return;
    };

    let app = window.app_handle();
    let lock = app.state::<AspectLock>();
    let Ok(mut map) = lock.0.lock() else {
        return;
    };
    let state = map.entry(window.label().to_string()).or_default();
    let Some(ratio) = state.ratio else {
        return;
    };
    // 自分で直したぶんの跳ね返り。OS の丸めで 1 ピクセルずれて返ってくることもある
    // （macOS の倍率など）。それを手で変えたものとして広さを取り直すと、画面に収めるために
    // 縮めた大きさが広さになってしまうので、基準だけ合わせて何もしない
    if size.width.abs_diff(state.last.width) <= 1 && size.height.abs_diff(state.last.height) <= 1 {
        state.reported = size;
        return;
    }
    let now = size.to_logical::<f64>(scale);
    let before = state.reported.to_logical::<f64>(scale);
    state.reported = size;
    let extra = state.extra;
    let area = dragged_area(now, before, ratio, extra);
    // 手で変えている間は画面に収める判定をしない（引っ張った先で止められない）
    let (width, height) = sized_to_aspect(ratio, extra, area, NO_LIMIT);
    let fixed: PhysicalSize<u32> = LogicalSize::new(width, height).to_physical(scale);

    // 1 ピクセルのずれは OS 側の丸めなので、直しに行かずそのまま受け入れる
    // （直すたびに丸め直されて、いつまでも往復することがある）
    if fixed.width.abs_diff(size.width) <= 1 && fixed.height.abs_diff(size.height) <= 1 {
        state.last = size;
        state.area = now.width * now.height;
        return;
    }

    // set_size より先に覚える。OS によっては set_size がその場で次の
    // Resized を呼ぶので、跳ね返りだと分かるようにしてから呼ぶ。
    // ロックも手放しておく（持ったまま呼ぶと、その場で呼ばれたときに止まる）
    state.last = fixed;
    state.area = width * height;
    drop(map);

    let _ = window.set_size(fixed);
}

/// ウィンドウを画像の縦横比に合わせる（「画像に合わせる」のとき）。
/// 大きさは set_aspect_lock が覚えている広さを保ち、縦横比だけを画像に合わせるので、
/// 画像を切り替えても、手でサイズを変えたあとも、大きさの印象は変わらない。
/// 毎回、作業領域（タスクバーなどを除いた画面）の SCREEN_RATIO に収まるよう
/// 縦横比を保ったまま縮める（覚えている広さは縮めない。縦長の画像で縮めても、
/// 次の横長の画像は元の広さで開く）。
/// 大きさを変えても中心は動かさず、作業領域からはみ出す分は中へ寄せる。
/// 起動直後・種類の姿へ置き直した直後の 1 枚だけは、置いた左上に合わせる。
/// 最大化中・全画面中は大きさを変えない（変えると解けてしまう）
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) fn fit_window_to_image(
    window: WebviewWindow,
    pending: State<PendingPosition>,
    lock: State<AspectLock>,
    kinds: State<WindowKind>,
    width: f64,
    height: f64,
    extra_width: Option<f64>,
    extra_height: Option<f64>,
) -> Result<(), String> {
    if !(width > 0.0 && height > 0.0) {
        return Err("画像サイズを取得できません".to_string());
    }
    // 全画面を先に見る（keep_aspect_on_resize と同じ）。最小化中も大きさは変えない
    if window.is_fullscreen().unwrap_or(false)
        || window.is_minimized().unwrap_or(false)
        || window.is_maximized().unwrap_or(false)
    {
        return Ok(());
    }
    // 表示中の種類の姿へまだ置き直していなければ（最大化などから戻った直後）、先に置き直す。
    // 前の種類の大きさのまま縦横比を合わせると、その大きさが表示中の種類の分になってしまう
    apply_window_kind(&window.as_ref().window());
    if kinds.with(window.label(), |k| k.pending()).unwrap_or(false) {
        return Ok(());
    }
    let scale = window.scale_factor().map_err(|e| e.to_string())?;

    // 変更前の位置と大きさ（位置の計算は物理ピクセルの整数で行う）
    let before = (|| {
        let pos = window.outer_position().ok()?;
        let outer = window.outer_size().ok()?;
        let inner = window.inner_size().ok()?;
        Some((pos, outer, inner))
    })();

    // 保つべき広さ。まだ覚えていなければ今のウィンドウの広さ
    let label = window.label();
    let area = lock.with(label, |state| state.area).unwrap_or(0.0);
    let area = if area > 0.0 {
        area
    } else {
        before
            .map(|(_, _, inner)| inner.to_logical::<f64>(scale))
            .map(|inner| inner.width * inner.height)
            .unwrap_or(0.0)
    };

    let work_area = current_work_area(&window.as_ref().window());
    let limit = screen_limit(work_area, scale);
    let extra = (extra_width.unwrap_or(0.0), extra_height.unwrap_or(0.0));
    let (w, h) = sized_to_aspect(width / height, extra, area, limit);
    let new_inner: PhysicalSize<u32> = LogicalSize::new(w, h).to_physical(scale);

    // 起動して最初の 1 枚は前回の左上に合わせる。ただし起動時に置いた場所から
    // 動いていたら（ユーザーが動かしていたら）そのまま中心を保つ
    let anchor = pending
        .with(label, Option::take)
        .flatten()
        .and_then(|restored| {
            const TOLERANCE: i32 = 2;
            let (pos, _, _) = before?;
            let placed = desktop_to_physical(restored.placed, scale, PHYSICAL_DESKTOP);
            let unmoved =
                (pos.x - placed.x).abs() <= TOLERANCE && (pos.y - placed.y).abs() <= TOLERANCE;
            unmoved.then(|| desktop_to_physical(restored.saved, scale, PHYSICAL_DESKTOP))
        });

    // set_size の跳ね返りを手動のサイズ変更と取り違えないよう、先に大きさを覚える。
    // 広さは変えない（画面に収めるために縮めた大きさを広さにすると、以後ずっと小さくなる）。
    // 覚えていなかったときだけ、上で使った広さを覚える
    lock.with(label, |state| {
        // 縦横比の固定もここでかけておく（上で種類の姿へ置き直したときは外れているため。
        // フロントエンドの set_aspect_lock と同じ値）
        state.ratio = Some(width / height);
        state.extra = extra;
        state.last = new_inner;
        state.reported = new_inner;
        if state.area <= 0.0 {
            state.area = area;
        }
    });
    // 既に縦横比どおりなら何もしない
    if before.map(|(_, _, inner)| inner) == Some(new_inner) && anchor.is_none() {
        return Ok(());
    }

    window
        .set_size(new_inner)
        .map_err(|e| format!("ウィンドウサイズを変更できません: {e}"))?;

    if let Some((pos, outer, inner)) = before {
        let new_pos = fitted_position(pos, outer, inner, new_inner, anchor, work_area.as_ref());
        let _ = window.set_position(new_pos);
    }
    Ok(())
}
