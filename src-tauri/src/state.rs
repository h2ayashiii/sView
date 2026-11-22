use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use notify::RecommendedWatcher;
use tauri::{LogicalSize, PhysicalSize};

use crate::media::{ImageList, WindowKindKey};

/// ビューアのウィンドウ（ラベル）ごとに持つ状態。
/// macOS では開いたファイルごとにウィンドウを増やすので、ウィンドウに結びつく
/// 状態はすべてラベルで引く。ウィンドウを閉じたら forget_window で消す
pub(crate) struct PerWindow<T>(pub(crate) Mutex<HashMap<String, T>>);

impl<T: Default> PerWindow<T> {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    /// label の分を取り出して f に渡す（無ければ既定値で作る）。ロックできなければ None
    pub(crate) fn with<R>(&self, label: &str, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut map = self.0.lock().ok()?;
        Some(f(map.entry(label.to_string()).or_default()))
    }

    pub(crate) fn remove(&self, label: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(label);
        }
    }
}

/// 起動時に渡されたファイル（CLI 引数 / macOS の Opened イベント）を
/// フロントエンドが取りに来るまで、ウィンドウごとに保持する
pub(crate) type StartupFile = PerWindow<Option<String>>;

/// ウィンドウで最初に開くものの一覧（開いたパスと、list_images と同じ結果）。
/// ウィンドウを出す前に種類を知るために作り、フロントエンドが最初に list_images を呼んだとき
/// そのまま渡す（同じフォルダ・書庫を 2 回読まない）
pub(crate) type StartupListing = PerWindow<Option<(String, Result<ImageList, String>)>>;

/// 「画像に合わせる」で起動したとき、最初の画像を前回と同じ場所に開くための覚え書き。
/// restore_window_state が入れ、fit_window_to_image が一度使ったら空にする
pub(crate) struct RestoredPosition {
    /// 前回閉じたときのウィンドウの左上（画面上の位置の座標。PHYSICAL_DESKTOP を参照）
    pub(crate) saved: (f64, f64),
    /// 起動時に実際に置いた左上（saved と同じ座標）。ここから動いていたら
    /// ユーザーが動かしたものとみなし、saved には合わせない
    pub(crate) placed: (f64, f64),
}
pub(crate) type PendingPosition = PerWindow<Option<RestoredPosition>>;

/// ウィンドウの大きさ・位置を覚える枠。
/// 「自由に変更」では種類によらず 1 つ（Free）、「メディアに合わせる」では種類ごとに 1 つずつ
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowSlot {
    /// 「自由に変更」の枠（window.json の最上位）
    Free,
    /// 「メディアに合わせる」の種類ごとの枠（window.json の kinds）
    Kind(WindowKindKey),
}

/// 表示中の種類と、ウィンドウの姿がどの枠のものかの覚え書き。
/// フロントエンドが set_window_kind で表示する種類とサイズの設定を知らせる
#[derive(Default)]
pub(crate) struct KindState {
    /// 最後に表示した種類（何も開いていない間も前の種類を覚えたまま）。
    /// まだ何も表示していなければ None
    pub(crate) shown: Option<WindowKindKey>,
    /// ウィンドウのサイズの設定が「メディアに合わせる」か
    pub(crate) fit: bool,
    /// ウィンドウの今の大きさ・位置（普段の姿）がどの枠のものか。
    /// 最大化中・全画面中・最小化中に枠が変わったときは、普段の姿に戻って
    /// その枠の大きさ・位置へ置き直すまで、前の枠のまま（target と食い違う）
    pub(crate) applied: Option<WindowSlot>,
    /// 開くファイルを割り当て済みか。まだ何も表示していないウィンドウでも、
    /// 割り当て済みなら次のファイルは別のウィンドウで開く
    pub(crate) claimed: bool,
}

impl KindState {
    /// 今の設定と表示中の種類で、ウィンドウが使うべき枠。
    /// まだ何も表示していなければ（ファイルを指定せずに起動しただけなら）どちらの設定でも None。
    /// そのウィンドウは既定の大きさで画面の中央に出し、どの枠にも記録しない
    pub(crate) fn target(&self) -> Option<WindowSlot> {
        let shown = self.shown?;
        Some(if self.fit {
            WindowSlot::Kind(shown)
        } else {
            WindowSlot::Free
        })
    }

    /// 使うべき枠の大きさ・位置へまだ置き直していないか
    pub(crate) fn pending(&self) -> bool {
        matches!(self.target(), Some(target) if self.applied != Some(target))
    }
}
pub(crate) type WindowKind = PerWindow<KindState>;

/// 「画像に合わせる」で、ウィンドウを画像の縦横比から外させないための覚え書き。
/// ドラッグの最中も含め、大きさが変わるたびに参照する
#[derive(Default)]
pub(crate) struct AspectState {
    /// 固定する縦横比（横 / 縦）。None のときは固定しない
    pub(crate) ratio: Option<f64>,
    /// 縦横比に含めない固定の余白（論理ピクセルの横・縦）。音楽のアートワークの周りの
    /// 余白や曲名の帯のように、ウィンドウの大きさによらず一定の部分。
    /// 縦横比はウィンドウからこの分を除いた残りに対して保つ
    pub(crate) extra: (f64, f64),
    /// 自分で直した大きさ（物理ピクセル）。その跳ね返りを見分けるのに使う
    pub(crate) last: PhysicalSize<u32>,
    /// OS が最後に知らせてきた大きさ（物理ピクセル）。
    /// ドラッグの最中、OS は掴んだときの大きさを基準に動かし続け、こちらが
    /// 直した大きさは見ていない。どの辺が引っ張られたかは、実際の大きさではなく
    /// 「前に知らせてきた大きさ」と比べないと分からない
    pub(crate) reported: PhysicalSize<u32>,
    /// macOS で端・角を引っ張っている間の様子（mac_sizing が引っ張り始めに作る）
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) live: Option<LiveResize>,
    /// 画像を切り替えても保つ広さ（論理ピクセルの面積）。
    /// 手で大きさを変えたときだけ更新する
    pub(crate) area: f64,
}
pub(crate) type AspectLock = PerWindow<AspectState>;

impl AspectState {
    /// 縦横比の固定を外す。広さ（area）は残す。
    /// 広さを今のウィンドウから取り直すと、画面に収めるために縮めた大きさが
    /// そのまま広さになり、以後の画像がずっと小さくなってしまう
    pub(crate) fn unlock(&mut self) {
        *self = AspectState {
            area: self.area,
            ..AspectState::default()
        };
    }
}

/// macOS で端・角を引っ張っている間の様子
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy)]
pub(crate) struct LiveResize {
    /// 引っ張り始めたカーソルが近かった辺（左右の辺 / 上下の辺）。引っ張れる辺の候補
    pub(crate) edges: (bool, bool),
    /// 引っ張り始めたときの中身の大きさ（論理ピクセル）
    pub(crate) grab: LogicalSize<f64>,
    /// 実際に引っ張られた辺。候補の辺のうち grab から動いたものを足していき、
    /// 引っ張り終えるまで戻さない（角を斜めに引っ張っている途中で、片方の辺が掴んだときの
    /// 長さへたまたま戻っても角のまま扱う。扱いが入れ替わると大きさが飛んでちらつく）
    pub(crate) pulled: (bool, bool),
}

/// 表示中のフォルダの監視。フォルダを開いている間だけ生き、
/// 書庫を開いたときや閉じたときは None に戻す（= ネイティブの監視も解除される）
pub(crate) struct Watching {
    /// 監視中のフォルダ。同じフォルダを開き直したときに張り直さないための目印
    pub(crate) path: PathBuf,
    /// 見ている深さ（直下だけなら 1）。これも同じなら張り直さない
    pub(crate) depth: usize,
    /// drop するとネイティブの監視も解除されるので、持っているだけでよい
    pub(crate) _watcher: RecommendedWatcher,
}
pub(crate) type FolderWatcher = PerWindow<Option<Watching>>;
