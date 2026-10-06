use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tauri::ipc::Response;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use zip::ZipArchive;

/// 対応する画像拡張子（小文字で比較）
const IMAGE_EXTS: &[&str] = &[
    "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg", "png", "svg", "tif", "tiff", "webp",
];

/// 対応する動画拡張子（小文字で比較）。
/// デコードは OS の WebView に任せる（コーデックは同梱しない）ので、
/// どの OS の WebView でもおおむね再生できるコンテナだけに絞っている
const VIDEO_EXTS: &[&str] = &["m4v", "mov", "mp4", "webm"];

/// 対応する音楽拡張子（小文字で比較）。
/// 動画と同じく再生は OS の WebView に任せる（コーデックは同梱しない）
const AUDIO_EXTS: &[&str] = &["aac", "flac", "m4a", "mp3", "ogg", "opus", "wav"];

/// 音楽ファイルと同じフォルダに置かれたジャケット画像とみなすファイル名（拡張子を除く・小文字）。
/// 埋め込みのアートワークが無いときだけ使う
const COVER_STEMS: &[&str] = &["cover", "folder", "front", "album", "albumart"];

/// アートワークとして読む画像の上限（埋め込み・同じフォルダの画像とも）
const MAX_ARTWORK_BYTES: u64 = 64 * 1024 * 1024;

/// 対応する書庫（圧縮フォルダ）拡張子
const ARCHIVE_EXTS: &[&str] = &["cbz", "zip"];

/// 最小ウィンドウサイズ（tauri.conf.json の minWidth / minHeight と合わせる）
const MIN_WINDOW_SIZE: (f64, f64) = (200.0, 150.0);

/// 設定と window.json を置くフォルダ名。
/// Tauri の app_config_dir() は identifier（bundle ID）をそのままフォルダ名にするが、
/// 逆ドメイン名がそのまま見えるのは分かりにくいので、ここは短い名前に固定する
const CONFIG_DIR_NAME: &str = "sview";

/// フォルダ・書庫を開いたときに読む深さ。開いたフォルダ・書庫の直下を 1 層目とし、
/// その下 2 層（= 3 層目）まで。ファイルが多すぎる場所を開いても一覧が膨らみすぎないようにする
const MAX_DEPTH: usize = 3;

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

/// 「画像に合わせる」で起動したとき、最初の画像を前回と同じ場所に開くための覚え書き。
/// restore_window_state が入れ、fit_window_to_image が一度使ったら空にする
struct RestoredPosition {
    /// 前回閉じたときのウィンドウの左上（論理ピクセル）
    saved: (f64, f64),
    /// 起動時に実際に置いた左上（論理ピクセル）。ここから動いていたら
    /// ユーザーが動かしたものとみなし、saved には合わせない
    placed: (f64, f64),
}
struct PendingPosition(Mutex<Option<RestoredPosition>>);

/// 表示中のファイルの種類と、設定「ファイルの種類ごとにウィンドウを保持する」。
/// フロントエンドが set_window_kind で知らせる
#[derive(Default)]
struct KindState {
    /// 最後に表示した種類（何も開いていない間も前の種類を覚えたまま）
    kind: Option<MediaKind>,
    per_kind: bool,
}
struct WindowKind(Mutex<KindState>);

/// 画面に収まる大きさへ抑えるのは、立ち上げてから最初にウィンドウを開くときだけ。
/// true の間がその 1 回で、済ませたら false にして、終了するまで戻さない
/// （動かしている間は、ユーザーが決めた大きさをそのまま尊重する）
struct StartupFit(Mutex<bool>);

/// 「画像に合わせる」で、ウィンドウを画像の縦横比から外させないための覚え書き。
/// ドラッグの最中も含め、大きさが変わるたびに参照する
#[derive(Default)]
struct AspectState {
    /// 固定する縦横比（横 / 縦）。None のときは固定しない
    ratio: Option<f64>,
    /// 縦横比に含めない固定の余白（論理ピクセルの横・縦）。音楽のアートワークの周りの
    /// 余白や曲名の帯のように、ウィンドウの大きさによらず一定の部分。
    /// 縦横比はウィンドウからこの分を除いた残りに対して保つ
    extra: (f64, f64),
    /// 自分で直した大きさ（物理ピクセル）。その跳ね返りを見分けるのに使う
    last: PhysicalSize<u32>,
    /// OS が最後に知らせてきた大きさ（物理ピクセル）。
    /// ドラッグの最中、OS は掴んだときの大きさを基準に動かし続け、こちらが
    /// 直した大きさは見ていない。どの辺が引っ張られたかは、実際の大きさではなく
    /// 「前に知らせてきた大きさ」と比べないと分からない
    reported: PhysicalSize<u32>,
    /// 画像を切り替えても保つ広さ（論理ピクセルの面積）。
    /// 手で大きさを変えたときだけ更新する
    area: f64,
}
struct AspectLock(Mutex<AspectState>);

/// 表示中のフォルダの監視。フォルダを開いている間だけ生き、
/// 書庫を開いたときや閉じたときは None に戻す（= ネイティブの監視も解除される）
struct Watching {
    /// 監視中のフォルダ。同じフォルダを開き直したときに張り直さないための目印
    path: PathBuf,
    /// 見ている深さ（直下だけなら 1）。これも同じなら張り直さない
    depth: usize,
    /// drop するとネイティブの監視も解除されるので、持っているだけでよい
    _watcher: RecommendedWatcher,
}
struct FolderWatcher(Mutex<Option<Watching>>);

/// ファイルの種類。一覧には 1 種類だけを並べる
/// （単体のファイルを開いたらその種類、フォルダ・書庫なら並び順で先頭のファイルの種類）
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum MediaKind {
    Image,
    Video,
    Audio,
}

#[derive(serde::Serialize)]
struct ImageList {
    /// フォルダの場合は画像のフルパス、書庫の場合は書庫内のエントリ名
    images: Vec<String>,
    index: usize,
    /// 書庫を開いている場合はその書庫のフルパス
    archive: Option<String>,
    /// 監視対象のフォルダ（フォルダを開いた場合のみ。書庫では None）
    dir: Option<String>,
    /// 表示名から取り除く共通フォルダ
    /// （書庫では全エントリの共通フォルダ。例: "book/"。フォルダをサブフォルダごと
    /// 読んだときは開いたフォルダのパス。単体のファイルから開いたときは空）
    prefix: String,
    /// 並べている種類（一覧が空のときは None）
    kind: Option<MediaKind>,
    /// 読んだ深さ（単体のファイルから開いたときは 1、フォルダ・書庫は MAX_DEPTH）。
    /// 読み直しと監視で同じ深さを使うために返す
    depth: usize,
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

fn is_video(path: &Path) -> bool {
    has_ext(path, VIDEO_EXTS)
}

fn is_audio(path: &Path) -> bool {
    has_ext(path, AUDIO_EXTS)
}

/// フォルダで一覧に並べるもの（画像・動画・音楽）。書庫の中の動画・音楽は対象外
fn is_media(path: &Path) -> bool {
    media_kind(path).is_some()
}

fn media_kind(path: &Path) -> Option<MediaKind> {
    if is_image(path) {
        Some(MediaKind::Image)
    } else if is_video(path) {
        Some(MediaKind::Video)
    } else if is_audio(path) {
        Some(MediaKind::Audio)
    } else {
        None
    }
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
    #[test]
    fn association_exts_are_validated_and_normalized() {
        assert_eq!(
            normalize_association_exts(&[".JPG".into(), "png".into(), "jpg".into()]).unwrap(),
            vec!["jpg", "png"]
        );
        // 動画・音楽も関連付けられる
        assert_eq!(
            normalize_association_exts(&["MP4".into(), "flac".into()]).unwrap(),
            vec!["mp4", "flac"]
        );
        // 書庫は関連付けの対象にしない
        assert!(normalize_association_exts(&["zip".into()]).is_err());
        assert!(normalize_association_exts(&["cbz".into()]).is_err());
        assert!(normalize_association_exts(&["exe".into()]).is_err());
    }

    use super::*;

    #[test]
    fn clamp_to_area_keeps_window_inside_the_area() {
        let area = Area {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1040.0,
        };
        // 収まっていればそのまま
        assert_eq!(
            clamp_to_area(100.0, 100.0, 800.0, 600.0, &area),
            (100.0, 100.0)
        );
        // 右下にはみ出す → 右端・下端に寄せる
        assert_eq!(
            clamp_to_area(1500.0, 800.0, 800.0, 600.0, &area),
            (1120.0, 440.0)
        );
        // 左上にはみ出す
        assert_eq!(clamp_to_area(-50.0, -20.0, 800.0, 600.0, &area), (0.0, 0.0));
        // 領域より大きいときは左上を優先する
        assert_eq!(
            clamp_to_area(100.0, 100.0, 2500.0, 600.0, &area),
            (0.0, 100.0)
        );
    }

    #[test]
    fn window_state_reads_old_files_and_keeps_kinds() {
        // 種類ごとの分が無い古い window.json もそのまま読める
        let old: WindowState =
            serde_json::from_str(r#"{"width":800,"height":600,"x":10,"y":20}"#).unwrap();
        assert_eq!(old.width, Some(800.0));
        assert!(old.kinds.is_empty());
        // 種類ごとの分が無ければ書き出しにも出さない
        assert!(!serde_json::to_string(&old).unwrap().contains("kinds"));

        let mut state = old.clone();
        let mut video = WindowState::default();
        copy_geometry(
            &mut video,
            &WindowState {
                width: Some(1280.0),
                height: Some(720.0),
                ..WindowState::default()
            },
        );
        state
            .kinds
            .insert(kind_key(MediaKind::Video).to_string(), video);
        let text = serde_json::to_string(&state).unwrap();
        let read: WindowState = serde_json::from_str(&text).unwrap();
        assert_eq!(read.kinds["video"].width, Some(1280.0));
        assert_eq!(read.kinds["video"].x, None);
        assert_eq!(read.x, Some(10.0));
    }

    #[test]
    fn clamp_to_area_uses_the_area_origin() {
        // 右側に並べた 2 台目のディスプレイ
        let area = Area {
            x: 1920.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
        };
        assert_eq!(
            clamp_to_area(3000.0, 500.0, 800.0, 600.0, &area),
            (2400.0, 120.0)
        );
        assert_eq!(
            clamp_to_area(1800.0, 10.0, 800.0, 600.0, &area),
            (1920.0, 10.0)
        );
    }

    #[test]
    fn fitted_position_keeps_the_center() {
        let pos = fitted_position(
            PhysicalPosition::new(300, 200),
            PhysicalSize::new(1000, 700),
            PhysicalSize::new(1000, 700),
            PhysicalSize::new(600, 300),
            None,
            None,
        );
        assert_eq!(pos, PhysicalPosition::new(500, 400));
    }

    #[test]
    fn fitted_position_does_not_drift_with_hidden_frame() {
        // Windows の枠なしウィンドウは影のぶん外枠が中身より大きい（ここでは 16 x 8）。
        // 大きさの違う 2 枚を何度行き来しても左上が元に戻ること（右下へずれていかない）
        let frame = (16, 8);
        let a = PhysicalSize::new(1000, 700);
        let b = PhysicalSize::new(801, 903);
        let mut pos = PhysicalPosition::new(300, 200);
        let mut inner = a;
        for _ in 0..10 {
            for next in [b, a] {
                let outer = PhysicalSize::new(inner.width + frame.0, inner.height + frame.1);
                pos = fitted_position(pos, outer, inner, next, None, None);
                inner = next;
            }
        }
        assert_eq!(pos, PhysicalPosition::new(300, 200));
    }

    #[test]
    fn fitted_position_stays_inside_the_work_area() {
        // 小さなウィンドウの中心を保ったまま大きな画像に合わせるとはみ出すので、
        // 作業領域の中へ寄せる
        let area = Area {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1040.0,
        };
        let pos = fitted_position(
            PhysicalPosition::new(1200, 700),
            PhysicalSize::new(400, 300),
            PhysicalSize::new(400, 300),
            PhysicalSize::new(1600, 900),
            None,
            Some(&area),
        );
        assert_eq!(pos, PhysicalPosition::new(320, 140));
    }

    #[test]
    fn fitted_position_uses_the_anchor_for_the_first_image() {
        let area = Area {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1040.0,
        };
        let default_size = PhysicalSize::new(960, 640);
        // 前回の左上にそのまま置く
        let pos = fitted_position(
            PhysicalPosition::new(0, 0),
            default_size,
            default_size,
            PhysicalSize::new(800, 600),
            Some(PhysicalPosition::new(500, 300)),
            Some(&area),
        );
        assert_eq!(pos, PhysicalPosition::new(500, 300));
        // 前回の左上だとはみ出す大きさなら寄せる
        let pos = fitted_position(
            PhysicalPosition::new(0, 0),
            default_size,
            default_size,
            PhysicalSize::new(1600, 900),
            Some(PhysicalPosition::new(500, 300)),
            Some(&area),
        );
        assert_eq!(pos, PhysicalPosition::new(320, 140));
    }

    #[test]
    fn sized_to_aspect_keeps_the_area_and_takes_the_ratio_from_the_image() {
        let limit = (1728.0, 972.0);
        // 500x500 相当の広さで 16:9 の画像に合わせる → 広さはそのまま、形だけ画像に合う
        let (w, h) = sized_to_aspect(16.0 / 9.0, (0.0, 0.0), 500.0 * 500.0, limit);
        assert!((w / h - 16.0 / 9.0).abs() < 1e-9);
        assert!((w * h - 250_000.0).abs() < 1e-6);
        // 縦長の画像でも広さは変わらない（切り替えても大きさの印象が揃う）
        let (w, h) = sized_to_aspect(9.0 / 16.0, (0.0, 0.0), 500.0 * 500.0, limit);
        assert!((w / h - 9.0 / 16.0).abs() < 1e-9);
        assert!((w * h - 250_000.0).abs() < 1e-6);
    }

    #[test]
    fn dragged_area_keeps_the_edge_that_was_pulled() {
        let limit = (1728.0, 972.0);
        let ratio = 16.0 / 9.0;
        let before = LogicalSize::new(800.0, 450.0);
        // 右端を引っ張った（横だけ変わった）→ 横はそのまま、縦が縦横比で決まる
        let now = LogicalSize::new(1000.0, 450.0);
        let (w, h) = sized_to_aspect(
            ratio,
            (0.0, 0.0),
            dragged_area(now, before, ratio, (0.0, 0.0)),
            limit,
        );
        assert!((w - 1000.0).abs() < 1e-9);
        assert!((h - 1000.0 / ratio).abs() < 1e-9);
        // 下端を引っ張った（縦だけ変わった）→ 縦はそのまま、横が縦横比で決まる
        let now = LogicalSize::new(800.0, 600.0);
        let (w, h) = sized_to_aspect(
            ratio,
            (0.0, 0.0),
            dragged_area(now, before, ratio, (0.0, 0.0)),
            limit,
        );
        assert!((h - 600.0).abs() < 1e-9);
        assert!((w - 600.0 * ratio).abs() < 1e-9);
    }

    #[test]
    fn sizing_area_follows_the_pulled_edge_and_is_continuous_at_corners() {
        let limit = (1728.0, 972.0);
        let ratio = 16.0 / 9.0;
        let size = |h, v, w, ht| {
            sized_to_aspect(
                ratio,
                (0.0, 0.0),
                sizing_area(h, v, LogicalSize::new(w, ht), ratio, (0.0, 0.0)),
                limit,
            )
        };
        // 左右の辺: 横はそのまま、縦が縦横比で決まる
        let (w, h) = size(true, false, 1000.0, 450.0);
        assert!((w - 1000.0).abs() < 1e-9 && (h - 1000.0 / ratio).abs() < 1e-9);
        // 上下の辺: 縦はそのまま、横が縦横比で決まる
        let (w, h) = size(false, true, 800.0, 600.0);
        assert!((h - 600.0).abs() < 1e-9 && (w - 600.0 * ratio).abs() < 1e-9);
        // 角: 大きい方に合わせる（縦横比から外れた位置でも、縦横比のまま覆う大きさ）
        let (w, h) = size(true, true, 1000.0, 600.0);
        assert!((w - 600.0 * ratio).abs() < 1e-9 && (h - 600.0).abs() < 1e-9);
        // 角: マウスが少し動いただけなら大きさも少ししか変わらない（どちらの辺が
        // 大きく動いたかで行き来しない）
        let (w1, _) = size(true, true, 1000.0, 562.0);
        let (w2, _) = size(true, true, 1001.0, 562.5);
        assert!((w2 - w1).abs() < 2.0);
    }

    #[test]
    fn sized_to_aspect_keeps_the_ratio_outside_the_fixed_margin() {
        let limit = (1728.0, 972.0);
        // 音楽: アートワークの周りの余白（横 56、縦 186）はウィンドウの大きさによらず一定。
        // 縦横比は余白を除いた部分で保ち、広さはウィンドウ全体で保つ
        let extra = (56.0, 186.0);
        for aspect in [1.0, 2.0 / 3.0, 16.0 / 9.0] {
            let (w, h) = sized_to_aspect(aspect, extra, 500.0 * 700.0, limit);
            assert!(((w - extra.0) / (h - extra.1) - aspect).abs() < 1e-9);
            assert!((w * h - 350_000.0).abs() < 1e-6);
        }
        // 横を引っ張ったら横はそのまま、縦は余白を除いた部分の縦横比で決まる
        let before = LogicalSize::new(500.0, 700.0);
        let now = LogicalSize::new(600.0, 700.0);
        let (w, h) = sized_to_aspect(1.0, extra, dragged_area(now, before, 1.0, extra), limit);
        assert!((w - 600.0).abs() < 1e-6);
        assert!((h - (600.0 - 56.0 + 186.0)).abs() < 1e-6);
        // 画面に収めるときも、余白を除いた部分の縦横比は崩さない
        let (w, h) = sized_to_aspect(1.0, extra, 4000.0 * 4000.0, limit);
        assert!(w <= limit.0 + 1e-9 && h <= limit.1 + 1e-9);
        assert!(((w - extra.0) / (h - extra.1) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sized_to_aspect_shrinks_to_fit_the_screen() {
        let limit = (1728.0, 972.0);
        // 画面に収まらない広さを求められたら、縦横比を保ったまま縮める
        let (w, h) = sized_to_aspect(1.0, (0.0, 0.0), 4000.0 * 4000.0, limit);
        assert!((w - 972.0).abs() < 1e-9);
        assert!((h - 972.0).abs() < 1e-9);
    }

    #[test]
    fn sized_to_aspect_grows_to_the_minimum_size() {
        let limit = (1728.0, 972.0);
        // 小さすぎる指定でも最小サイズは下回らない
        let (w, h) = sized_to_aspect(1.0, (0.0, 0.0), 1.0, limit);
        assert!(w >= MIN_WINDOW_SIZE.0 - 1e-9);
        assert!(h >= MIN_WINDOW_SIZE.1 - 1e-9);
        assert!((w / h - 1.0).abs() < 1e-9);
    }

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
    fn video_extension_detection() {
        assert!(is_video(Path::new("/x/clip.MP4")));
        assert!(is_video(Path::new("/x/clip.webm")));
        assert!(!is_video(Path::new("/x/clip.avi")));
        assert!(!is_video(Path::new("/x/photo.png")));
        assert!(is_media(Path::new("/x/clip.mov")));
        assert!(is_media(Path::new("/x/photo.png")));
        assert!(!is_media(Path::new("/x/notes.txt")));
    }

    #[test]
    fn audio_extension_detection() {
        assert!(is_audio(Path::new("/x/song.MP3")));
        assert!(is_audio(Path::new("/x/song.flac")));
        assert!(is_audio(Path::new("/x/song.m4a")));
        assert!(!is_audio(Path::new("/x/song.wma")));
        assert!(!is_audio(Path::new("/x/clip.mp4")));
        assert!(is_media(Path::new("/x/song.ogg")));
    }

    /// 埋め込みのアートワークが無いときは、同じフォルダのジャケット画像を
    /// COVER_STEMS の順（cover → folder → …）で選ぶ。大文字小文字は問わない
    #[test]
    fn folder_artwork_prefers_cover_names_in_order() {
        let dir = std::env::temp_dir().join(format!("sview-cover-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let song = dir.join("01 song.mp3");
        fs::write(&song, b"x").unwrap();
        assert_eq!(folder_artwork(&song), None);
        fs::write(dir.join("booklet.jpg"), b"booklet").unwrap();
        assert_eq!(folder_artwork(&song), None);
        fs::write(dir.join("Folder.JPG"), b"folder").unwrap();
        assert_eq!(folder_artwork(&song).as_deref(), Some(&b"folder"[..]));
        fs::write(dir.join("cover.png"), b"cover").unwrap();
        assert_eq!(folder_artwork(&song).as_deref(), Some(&b"cover"[..]));
        fs::remove_dir_all(&dir).ok();
    }

    /// タグに埋め込まれたアートワークと曲名を読める。表紙（CoverFront）を優先する
    #[test]
    fn reads_embedded_tags_and_artwork() {
        use lofty::config::WriteOptions;
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::prelude::*;
        use lofty::tag::{Tag, TagType};

        let dir = std::env::temp_dir().join(format!("sview-embed-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let song = dir.join("song.mp3");
        // MPEG-1 Layer III / 128 kbps / 44.1 kHz のフレーム（中身は無音でよい）を並べる
        let mut frame = vec![0xFF, 0xFB, 0x90, 0x00];
        frame.resize(417, 0);
        fs::write(&song, frame.repeat(4)).unwrap();
        // 同じフォルダのジャケット画像より、埋め込みの方を先に使う
        fs::write(dir.join("cover.jpg"), b"folder cover").unwrap();

        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("曲名".to_string());
        tag.set_artist("アーティスト".to_string());
        let picture = |kind, data: &[u8]| {
            Picture::unchecked(data.to_vec())
                .pic_type(kind)
                .mime_type(MimeType::Png)
                .build()
        };
        tag.push_picture(picture(PictureType::Other, b"other"));
        tag.push_picture(picture(PictureType::CoverFront, b"front"));
        tag.save_to_path(&song, WriteOptions::default()).unwrap();

        let path = song.to_string_lossy().into_owned();
        let info = audio_info(path).unwrap();
        assert_eq!(info.title.as_deref(), Some("曲名"));
        assert_eq!(info.artist.as_deref(), Some("アーティスト"));
        assert_eq!(info.album, None);
        assert_eq!(embedded_artwork(&song).as_deref(), Some(&b"front"[..]));
        fs::remove_dir_all(&dir).ok();
    }

    /// タグの無い（壊れた）音楽ファイルでもエラーにせず、空の情報を返す
    #[test]
    fn audio_info_tolerates_missing_tags() {
        let dir = std::env::temp_dir().join(format!("sview-tag-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let song = dir.join("broken.mp3");
        fs::write(&song, b"not really an mp3").unwrap();
        let info = audio_info(song.to_string_lossy().into_owned()).unwrap();
        assert_eq!((info.title, info.artist, info.album), (None, None, None));
        assert!(audio_info(dir.join("x.png").to_string_lossy().into_owned()).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    /// 一覧のファイル名だけを取り出す
    fn file_names(list: &ImageList) -> Vec<String> {
        list.images
            .iter()
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sview-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 単体のファイルから開くと、同じフォルダの直下にある同じ種類のファイルだけを並べる
    #[test]
    fn single_file_lists_only_its_own_kind() {
        let dir = test_dir("single");
        for name in [
            "a10.png",
            "a2.mp4",
            "a1.jpg",
            "a3.webm",
            "notes.txt",
            "a4.avi",
            "a5.mp3",
            "a6.wma",
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/a0.png"), b"x").unwrap();

        let list =
            list_dir_images(&dir, 1, Some(MediaKind::Video), Some("a3.webm".as_ref())).unwrap();
        assert_eq!(file_names(&list), vec!["a2.mp4", "a3.webm"]);
        assert_eq!(list.index, 1);
        assert_eq!(list.kind, Some(MediaKind::Video));
        assert_eq!(list.prefix, "");

        // サブフォルダの画像は拾わない
        let list =
            list_dir_images(&dir, 1, Some(MediaKind::Image), Some("a10.png".as_ref())).unwrap();
        assert_eq!(file_names(&list), vec!["a1.jpg", "a10.png"]);
        assert_eq!(list.index, 1);

        let list =
            list_dir_images(&dir, 1, Some(MediaKind::Audio), Some("a5.mp3".as_ref())).unwrap();
        assert_eq!(file_names(&list), vec!["a5.mp3"]);
        fs::remove_dir_all(&dir).ok();
    }

    /// フォルダを開くと 3 層目まで読み、並べたとき先頭に来るファイルの種類だけを残す
    #[test]
    fn folder_listing_keeps_the_first_kind_down_to_three_levels() {
        let dir = test_dir("folder");
        for name in [
            "b.mp4",
            "a/1.mp3",
            "a/x/2.png",
            "a/x/y/3.png",
            "c.png",
            "notes.txt",
        ] {
            let path = dir.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }

        // 相対パスの自然順で a/1.mp3 が先頭なので、音楽だけ
        let list = list_dir_images(&dir, MAX_DEPTH, None, None).unwrap();
        assert_eq!(file_names(&list), vec!["1.mp3"]);
        assert_eq!(list.kind, Some(MediaKind::Audio));
        assert_eq!(list.depth, MAX_DEPTH);
        assert!(list.prefix.ends_with(std::path::MAIN_SEPARATOR));

        // 読み直しでは最初に決めた種類を保つ。
        // a/x/2.png は 3 層目なので読み、a/x/y/3.png は 4 層目なので読まない
        let list = list_dir_images(&dir, MAX_DEPTH, Some(MediaKind::Image), None).unwrap();
        assert_eq!(file_names(&list), vec!["2.png", "c.png"]);

        // 空のフォルダは種類が決まらない
        let empty = test_dir("folder-empty");
        let list = list_dir_images(&empty, MAX_DEPTH, None, None).unwrap();
        assert!(list.images.is_empty());
        assert_eq!(list.kind, None);

        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn listing_change_only_reacts_to_images_appearing_or_disappearing() {
        use notify::event::{
            CreateKind, DataChange, EventKind, ModifyKind, RemoveKind, RenameMode,
        };
        let event = |kind, paths: &[&str]| notify::Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        };
        let roots = [PathBuf::from("/x")];
        let is_listing_change = |e: &notify::Event| is_listing_change(e, &roots, 1);

        // 画像が増えた・消えた・名前が変わった → 一覧を作り直す
        assert!(is_listing_change(&event(
            EventKind::Create(CreateKind::File),
            &["/x/new.png"]
        )));
        assert!(is_listing_change(&event(
            EventKind::Remove(RemoveKind::File),
            &["/x/old.jpg"]
        )));
        assert!(is_listing_change(&event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &["/x/a.png", "/x/b.png"]
        )));
        // 種類が分からない通知は取りこぼすより拾う
        assert!(is_listing_change(&event(EventKind::Any, &["/x/new.png"])));

        // 中身だけの書き換えでは枚数も並び順も変わらない
        assert!(!is_listing_change(&event(
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            &["/x/a.png"]
        )));
        // 動画も一覧に並ぶので対象
        assert!(is_listing_change(&event(
            EventKind::Create(CreateKind::File),
            &["/x/new.mp4"]
        )));
        // 画像・動画以外は無関係
        assert!(!is_listing_change(&event(
            EventKind::Create(CreateKind::File),
            &["/x/notes.txt"]
        )));
    }

    #[test]
    fn listing_change_respects_the_depth() {
        use notify::event::{CreateKind, EventKind, RemoveKind};
        let event = |kind, path: &str| notify::Event {
            kind,
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        };
        let roots = [PathBuf::from("/x")];
        let created = |path: &str, depth: usize| {
            is_listing_change(
                &event(EventKind::Create(CreateKind::File), path),
                &roots,
                depth,
            )
        };

        // 3 層目までは拾い、4 層目は捨てる
        assert!(created("/x/a/b/c.png", 3));
        assert!(!created("/x/a/b/c/d.png", 3));
        // 直下だけを見ているときはサブフォルダの中を拾わない
        assert!(!created("/x/a/c.png", 1));

        // 消えたサブフォルダ（拡張子なし）は、中に一覧のファイルがあったかもしれないので拾う
        let removed = |path: &str, depth: usize| {
            is_listing_change(
                &event(EventKind::Remove(RemoveKind::Any), path),
                &roots,
                depth,
            )
        };
        assert!(removed("/x/chapter", 3));
        assert!(removed("/x/a/chapter", 3));
        // 3 層目のフォルダの中身は読まないので無関係。直下だけのときも無関係
        assert!(!removed("/x/a/b/chapter", 3));
        assert!(!removed("/x/chapter", 1));
    }

    #[test]
    fn archive_extension_detection() {
        assert!(is_archive(Path::new("/x/book.CBZ")));
        assert!(is_archive(Path::new("/x/pics.zip")));
        assert!(!is_archive(Path::new("/x/pics.rar")));
        assert!(!is_archive(Path::new("/x/photo.png")));
    }

    /// 目印の直後から終端記号までを切り出し、その中の "…" を集める
    fn quoted_items(text: &str, marker: &str, end: char) -> Vec<String> {
        let rest = text
            .split_once(marker)
            .unwrap_or_else(|| panic!("目印が見つかりません: {marker}"))
            .1;
        let block = &rest[..rest.find(end).expect("リストの終端が見つかりません")];
        block
            .split('"')
            .skip(1)
            .step_by(2)
            .map(String::from)
            .collect()
    }

    /// 同じく切り出した範囲から、`key: value` の key だけを集める
    fn object_keys(text: &str, marker: &str, end: char) -> Vec<String> {
        let rest = text
            .split_once(marker)
            .unwrap_or_else(|| panic!("目印が見つかりません: {marker}"))
            .1;
        let block = &rest[..rest.find(end).expect("リストの終端が見つかりません")];
        block
            .split(',')
            .filter_map(|pair| pair.split_once(':'))
            .map(|(key, _)| key.trim().to_string())
            .collect()
    }

    fn sorted<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Vec<String> {
        let mut v: Vec<String> = items.into_iter().map(Into::into).collect();
        v.sort();
        v
    }

    #[test]
    fn dialog_text_loses_quotes_and_newlines() {
        // OS のコマンドに埋め込むので、クォートを壊す文字が残っていないこと
        let text = sanitize_dialog_text("a'b\"c\\d`e$f\ng\th");
        assert_eq!(text, "a b c d e f g h");
        assert!(!text.contains('\''));
        assert!(!text.contains('"'));
        assert!(!text.contains('\n'));
    }

    /// 対応拡張子の一覧は lib.rs / main.js / tauri.conf.json の 3 箇所にあり、
    /// 手で揃えるしかない。ずれたらここで落として気付けるようにする
    #[test]
    fn supported_extensions_stay_in_sync() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("リポジトリのルートが取れません");
        let main_js = fs::read_to_string(root.join("src/main.js")).expect("main.js を読めません");
        let conf: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(root.join("src-tauri/tauri.conf.json"))
                .expect("tauri.conf.json を読めません"),
        )
        .expect("tauri.conf.json が JSON として壊れています");

        // tauri.conf.json の fileAssociations から、指定した name の ext を取り出す
        let association = |name: &str| -> Vec<String> {
            conf["bundle"]["fileAssociations"]
                .as_array()
                .expect("fileAssociations がありません")
                .iter()
                .find(|a| a["name"] == name)
                .unwrap_or_else(|| panic!("fileAssociations に {name} がありません"))["ext"]
                .as_array()
                .expect("ext が配列ではありません")
                .iter()
                .map(|e| e.as_str().expect("ext が文字列ではありません").to_string())
                .collect()
        };

        let images = sorted(IMAGE_EXTS.to_vec());
        assert_eq!(
            sorted(quoted_items(&main_js, "const IMAGE_EXT_FILTER = [", ']')),
            images,
            "main.js の IMAGE_EXT_FILTER が IMAGE_EXTS とずれています"
        );
        assert_eq!(
            sorted(object_keys(&main_js, "const MIME = {", '}')),
            images,
            "main.js の MIME が IMAGE_EXTS とずれています"
        );
        assert_eq!(
            sorted(association("Image")),
            images,
            "tauri.conf.json の fileAssociations が IMAGE_EXTS とずれています"
        );

        let videos = sorted(VIDEO_EXTS.to_vec());
        assert_eq!(
            sorted(quoted_items(&main_js, "const VIDEO_EXT_FILTER = [", ']')),
            videos,
            "main.js の VIDEO_EXT_FILTER が VIDEO_EXTS とずれています"
        );
        assert_eq!(
            sorted(association("Video")),
            videos,
            "tauri.conf.json の fileAssociations が VIDEO_EXTS とずれています"
        );
        let audios = sorted(AUDIO_EXTS.to_vec());
        assert_eq!(
            sorted(quoted_items(&main_js, "const AUDIO_EXT_FILTER = [", ']')),
            audios,
            "main.js の AUDIO_EXT_FILTER が AUDIO_EXTS とずれています"
        );
        assert_eq!(
            sorted(association("Audio")),
            audios,
            "tauri.conf.json の fileAssociations が AUDIO_EXTS とずれています"
        );

        let archives = sorted(ARCHIVE_EXTS.to_vec());
        assert_eq!(
            sorted(quoted_items(&main_js, "const ARCHIVE_EXT_FILTER = [", ']')),
            archives,
            "main.js の ARCHIVE_EXT_FILTER が ARCHIVE_EXTS とずれています"
        );
        // 書庫は開けるが OS には関連付けない（ドラッグ＆ドロップと O キーで開く）。
        // 関連付けは画像・動画・音楽の 3 件だけ
        assert_eq!(
            conf["bundle"]["fileAssociations"]
                .as_array()
                .map(|a| a.len()),
            Some(3),
            "tauri.conf.json の fileAssociations は画像・動画・音楽の 3 件だけにします"
        );
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

    /// 画像3枚とテキスト・動画各1件を含む zip を作り、一覧と単体取り出しを確認する
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
                // 書庫の中の動画は一覧に出さない（丸ごとメモリに読む方式なので）
                ("b/clip.mp4", "video"),
                // 4 層目は読まない
                ("b/c/d/img1.png", "deep"),
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
        assert_eq!(list.kind, Some(MediaKind::Image));
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
        assert!(read_archive_entry(&path, "b/clip.mp4", &cache).is_err());

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

/// 書庫内のエントリが何層目にあるか（書庫の直下が 1）
fn entry_depth(name: &str) -> usize {
    name.split('/').filter(|c| !c.is_empty()).count()
}

/// 書庫内の画像エントリ名を自然順で返す（MAX_DEPTH 層目まで）。
/// 書庫の中は画像しか読まないので、種類は常に画像になる
fn list_archive_images(path: &Path, cache: &ArchiveCache) -> Result<ImageList, String> {
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

/// `dir` の下の画像・動画・音楽を `depth` 層目まで集める（`dir` の直下が 1 層目）。
/// 読めないサブフォルダは飛ばす
fn collect_media(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            if is_media(&path) {
                out.push(path);
            }
        } else if depth > 1 && path.is_dir() {
            if let Err(e) = collect_media(&path, depth - 1, out) {
                log::warn!("フォルダを読めません ({}): {e}", path.display());
            }
        }
    }
    Ok(())
}

/// 並べ替えに使う、`root` からの相対パス（区切りは書庫と同じ "/" にそろえる）
fn relative_key(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// フォルダの中を `depth` 層目まで読み、`kind` の種類だけを自然順で返す。
/// `kind` が None なら、並べたときに先頭に来るファイルの種類にする。
/// `current`（直下のファイル名）があれば、その位置を開始位置にする
fn list_dir_images(
    dir: &Path,
    depth: usize,
    kind: Option<MediaKind>,
    current: Option<&std::ffi::OsStr>,
) -> Result<ImageList, String> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_media(dir, depth.max(1), &mut entries)
        .map_err(|e| format!("フォルダを読めません: {e}"))?;

    // 書庫と同じく、開いたフォルダからの相対パスの自然順で並べる
    let mut keyed: Vec<(String, PathBuf)> = entries
        .into_iter()
        .map(|p| (relative_key(dir, &p), p))
        .collect();
    keyed.sort_by(|x, y| natural_cmp(&x.0, &y.0));

    let kind = kind.or_else(|| keyed.first().and_then(|(_, p)| media_kind(p)));
    let entries: Vec<PathBuf> = keyed
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| media_kind(p) == kind)
        .collect();

    // 直下のファイル名で自身を特定する
    let index = current
        .and_then(|name| {
            entries
                .iter()
                .position(|p| p.parent() == Some(dir) && p.file_name() == Some(name))
        })
        .unwrap_or(0);

    // サブフォルダまで読んだときは、開いたフォルダからの相対パスで表示する
    let prefix = if depth > 1 {
        let mut root = dir.to_string_lossy().into_owned();
        if !root.ends_with(std::path::MAIN_SEPARATOR) {
            root.push(std::path::MAIN_SEPARATOR);
        }
        root
    } else {
        String::new()
    };

    let images = entries
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    Ok(ImageList {
        images,
        index,
        archive: None,
        dir: Some(dir.to_string_lossy().into_owned()),
        prefix,
        kind,
        depth: depth.max(1),
    })
}

/// 画像・動画・音楽 / フォルダ / 書庫（zip・cbz）のいずれかを受け取り、
/// 表示対象の一覧と開始位置を返す。
/// - 単体のファイル: 同じフォルダの直下にある、同じ種類のファイルだけ
/// - フォルダ・書庫: MAX_DEPTH 層目までのうち、並べたとき先頭に来るファイルと同じ種類だけ
///
/// `kind` / `depth` は読み直しのときに、最初に開いたときと同じ条件で並べるために渡す
#[tauri::command]
fn list_images(
    path: String,
    kind: Option<MediaKind>,
    depth: Option<usize>,
    cache: State<ArchiveCache>,
) -> Result<ImageList, String> {
    let target = PathBuf::from(&path);
    if target.is_dir() {
        let depth = depth.unwrap_or(MAX_DEPTH).clamp(1, MAX_DEPTH);
        return list_dir_images(&target, depth, kind, None);
    }
    if !target.is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    if is_archive(&target) {
        return list_archive_images(&target, &cache);
    }
    let Some(kind) = media_kind(&target) else {
        return Err(format!("対応していないファイル形式です: {path}"));
    };
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "親フォルダを取得できません".to_string())?;
    list_dir_images(dir, 1, Some(kind), target.file_name())
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

/// 表示中の画像を OS のゴミ箱（Windows: ごみ箱 / macOS: ゴミ箱）へ送る。
/// 完全削除はしないので、取り違えても OS 側から戻せる
#[tauri::command]
fn delete_image(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    if !is_media(&target) {
        return Err(format!("画像・動画・音楽ファイルではありません: {path}"));
    }
    trash::delete(&target).map_err(|e| format!("ゴミ箱へ移動できませんでした: {e}"))?;
    log::info!("ゴミ箱へ移動しました: {path}");
    Ok(())
}

/// 音楽ファイルのタグ情報（表示用）。読めない項目は None
#[derive(serde::Serialize, Default)]
struct AudioInfo {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

/// 音楽ファイルのタグを読む。音声の長さなどは要らないので、タグだけを読む。
/// 形式は拡張子ではなく中身から判定する（拡張子と中身が食い違うファイルもあるため）
fn read_audio_tags(path: &Path) -> Option<lofty::file::TaggedFile> {
    use lofty::config::ParseOptions;
    use lofty::probe::Probe;
    let read = || -> Result<lofty::file::TaggedFile, Box<dyn std::error::Error>> {
        Ok(Probe::open(path)?
            .guess_file_type()?
            .options(ParseOptions::new().read_properties(false))
            .read()?)
    };
    read()
        .map_err(|e| log::warn!("音楽ファイルのタグを読めません ({}): {e}", path.display()))
        .ok()
}

/// 前後の空白を除き、空なら None にする
fn non_empty(text: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// 音楽ファイルの曲名・アーティスト・アルバム名を返す。
/// タグが無い・読めないときもエラーにはせず、空の情報を返す（再生はできるため）
#[tauri::command]
fn audio_info(path: String) -> Result<AudioInfo, String> {
    use lofty::prelude::*;
    let target = PathBuf::from(&path);
    if !target.is_file() || !is_audio(&target) {
        return Err(format!("音楽ファイルではありません: {path}"));
    }
    let Some(file) = read_audio_tags(&target) else {
        return Ok(AudioInfo::default());
    };
    // 主となるタグを先に見て、足りない項目はほかのタグで補う
    let mut info = AudioInfo::default();
    for tag in file.primary_tag().into_iter().chain(file.tags()) {
        info.title = info.title.or_else(|| non_empty(tag.title()));
        info.artist = info.artist.or_else(|| non_empty(tag.artist()));
        info.album = info.album.or_else(|| non_empty(tag.album()));
    }
    Ok(info)
}

/// タグに埋め込まれたアートワーク。表紙（CoverFront）を優先し、無ければ最初の 1 枚
fn embedded_artwork(path: &Path) -> Option<Vec<u8>> {
    use lofty::picture::PictureType;
    use lofty::prelude::*;
    let file = read_audio_tags(path)?;
    let pictures: Vec<_> = file
        .tags()
        .iter()
        .flat_map(|t| t.pictures())
        .filter(|p| !p.data().is_empty() && p.data().len() as u64 <= MAX_ARTWORK_BYTES)
        .collect();
    pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| pictures.first())
        .map(|p| p.data().to_vec())
}

/// 同じフォルダにあるジャケット画像（cover.jpg / folder.png など）。
/// 候補が複数あれば COVER_STEMS の順に選ぶ
fn folder_artwork(path: &Path) -> Option<Vec<u8>> {
    let dir = path.parent()?;
    let mut candidates: Vec<(usize, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p) && !has_ext(p, &["svg"]))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let rank = COVER_STEMS.iter().position(|s| *s == stem)?;
            Some((rank, p))
        })
        .collect();
    candidates.sort();
    candidates.into_iter().find_map(|(_, p)| {
        let size = fs::metadata(&p).ok()?.len();
        if size == 0 || size > MAX_ARTWORK_BYTES {
            return None;
        }
        fs::read(&p).ok()
    })
}

/// 音楽ファイルのアートワークをバイト列のまま返す（IPC の raw payload で転送）。
/// 埋め込みが無ければ同じフォルダのジャケット画像を探し、それも無ければ空を返す
/// （そのときはフロントエンドが既定の絵を出す）
#[tauri::command]
fn audio_artwork(path: String) -> Result<Response, String> {
    let target = PathBuf::from(&path);
    if !target.is_file() || !is_audio(&target) {
        return Err(format!("音楽ファイルではありません: {path}"));
    }
    let bytes = embedded_artwork(&target)
        .or_else(|| folder_artwork(&target))
        .unwrap_or_default();
    Ok(Response::new(bytes))
}

/// `path` が監視中のフォルダ（`roots` のどれか）から数えて何層目か（直下が 1）。
/// 数えられないとき（OS が別の表記のパスを返したなど）は直下とみなす
fn watched_depth(path: &Path, roots: &[PathBuf]) -> usize {
    roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .map(|rel| rel.components().count())
        .unwrap_or(1)
}

/// 一覧を作り直す必要がある変更かどうか。
/// `depth` 層目までの画像・動画・音楽の増減（作成・削除・名前の変更）だけを拾い、
/// 中身の書き換えは無視する。サブフォルダまで見ているときは、その中にあるフォルダ自体の
/// 増減も拾う（フォルダごと移してきたときは、中のファイルの通知が来ないことがあるため）。
/// 種類を判別できない通知（EventKind::Any）は、取りこぼすより拾う方に倒す
fn is_listing_change(event: &notify::Event, roots: &[PathBuf], depth: usize) -> bool {
    use notify::event::{EventKind, ModifyKind};
    let kind_matches = matches!(
        event.kind,
        EventKind::Any
            | EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_))
    );
    // 名前の変更では変更前と変更後の両方が入るので、どちらかが対象なら拾う
    kind_matches
        && event.paths.iter().any(|p| {
            let level = watched_depth(p, roots);
            if level > depth {
                return false;
            }
            if is_media(p) {
                return true;
            }
            // 消えたものはフォルダだったか確かめられないので、拡張子の無いものをフォルダとみなす
            level < depth && (p.is_dir() || (!p.exists() && p.extension().is_none()))
        })
}

/// 表示中のフォルダの監視を開始する（`path` が null なら監視をやめる）。
///
/// OS のネイティブ通知を使うので、変化が無い間は CPU もディスクも使わない
/// （ポーリングのように一定間隔で read_dir する方式とはここが違う）。
/// `depth` は一覧と同じ深さ。1 なら直下だけを見て、2 以上ならサブフォルダも見たうえで
/// `depth` 層目より深いところの通知は捨てる。
/// 通知は数が多くなりがちなので、実際の再スキャンはフロントエンド側で
/// 一定時間まとめてから 1 回だけ行う
#[tauri::command]
fn watch_folder(
    app: AppHandle,
    state: State<FolderWatcher>,
    path: Option<String>,
    depth: Option<usize>,
) -> Result<(), String> {
    let mut current = state
        .0
        .lock()
        .map_err(|_| "監視の状態を取得できません".to_string())?;

    let Some(path) = path else {
        *current = None;
        return Ok(());
    };
    let dir = PathBuf::from(path);
    let depth = depth.unwrap_or(1).clamp(1, MAX_DEPTH);
    if current
        .as_ref()
        .is_some_and(|w| w.path == dir && w.depth == depth)
    {
        return Ok(()); // 同じフォルダを同じ深さで見ているなら張り直さない
    }
    // 先に古い監視を解除してから張り直す（二重に監視しない）
    *current = None;
    if !dir.is_dir() {
        return Err(format!("フォルダが見つかりません: {}", dir.display()));
    }

    // macOS の FSEvents は実体のパス（/private/var/... など）で知らせてくることがあるので、
    // 渡されたパスと実体のパスの両方を基準にして深さを数える
    let mut roots = vec![dir.clone()];
    if let Ok(real) = dir.canonicalize() {
        if real != dir {
            roots.push(real);
        }
    }
    let handle = app.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) if is_listing_change(&event, &roots, depth) => {
                let _ = handle.emit("folder-changed", ());
            }
            // 監視できなくなった場合（フォルダごと消えたなど）は記録だけして続ける。
            // 一覧は R キーで作り直せる
            Err(e) => log::warn!("フォルダの監視でエラーが発生しました: {e}"),
            Ok(_) => {}
        }
    })
    .map_err(|e| format!("フォルダを監視できません: {e}"))?;

    let mode = if depth > 1 {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    watcher
        .watch(&dir, mode)
        .map_err(|e| format!("フォルダを監視できません: {e}"))?;
    log::info!(
        "フォルダの監視を開始しました: {} ({depth} 層目まで)",
        dir.display()
    );
    *current = Some(Watching {
        path: dir,
        depth,
        _watcher: watcher,
    });
    Ok(())
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

/// WebView がキャッシュや localStorage を置くフォルダ。
///
/// Windows: 指定しないと WebView2 は実行ファイルの隣（`C:\Program Files\sView`）に
/// `sview.exe.WebView2` を作ろうとして、書き込めずに起動そのものが失敗する。
/// 明示的に `%LOCALAPPDATA%\sview` を渡す。フォルダ名を bundle ID ではなく
/// `sview` にしているのは CONFIG_DIR_NAME と同じ理由。
///
/// macOS: WKWebView にはデータフォルダを指定する仕組みが無く、置き場所
/// （`~/Library/WebKit/<bundle ID>` など）は OS が bundle ID から決める。
/// None を返して既定の挙動に任せる
#[cfg(target_os = "windows")]
fn webview_data_dir(app: &AppHandle) -> Option<PathBuf> {
    Some(app.path().local_data_dir().ok()?.join(CONFIG_DIR_NAME))
}

#[cfg(not(target_os = "windows"))]
fn webview_data_dir(_app: &AppHandle) -> Option<PathBuf> {
    None
}

/// tauri.conf.json のウィンドウ定義から実際のウィンドウを作る。
/// 定義側を `create: false` にして自前で作っているのは、webview_data_dir を
/// 渡せるようにするため。ウィンドウの見た目や大きさは tauri.conf.json のまま
fn create_configured_windows(app: &AppHandle) -> Result<(), String> {
    let data_dir = webview_data_dir(app);
    for config in &app.config().app.windows {
        let mut builder = WebviewWindowBuilder::from_config(app, config)
            .map_err(|e| format!("ウィンドウ「{}」を作れません: {e}", config.label))?;
        if let Some(dir) = &data_dir {
            builder = builder.data_directory(dir.clone());
        }
        builder
            .build()
            .map_err(|e| format!("ウィンドウ「{}」を作れません: {e}", config.label))?;
    }
    Ok(())
}

/// ウィンドウの大きさと位置を覚えておくファイル（設定本体とは分けて、
/// 設定ウィンドウの「既定に戻す」で消えないようにする）
fn window_state_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("window.json"))
}

/// 各項目は Option。古い window.json（大きさだけ）もそのまま読めるようにし、
/// 保存できなかった項目は既定の挙動（中央・既定サイズ）に任せる
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WindowState {
    width: Option<f64>,
    height: Option<f64>,
    x: Option<f64>,
    y: Option<f64>,
    /// 種類（"image" / "video" / "audio"）ごとの大きさと位置。
    /// 設定「ファイルの種類ごとにウィンドウを保持する」がオンのときだけ使う
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    kinds: BTreeMap<String, WindowState>,
}

/// 種類を window.json のキーにする
fn kind_key(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
    }
}

fn write_window_state(app: &AppHandle, state: &WindowState) {
    let Ok(path) = window_state_file(app) else {
        return;
    };
    if let (Some(dir), Ok(text)) = (path.parent(), serde_json::to_string_pretty(state)) {
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(&path, text);
    }
}

/// 今のウィンドウの位置と大きさ（論理ピクセル）。
/// 最小化中・最大化中・全画面中は普段の姿と違うので None
fn current_geometry(window: &tauri::Window) -> Option<WindowState> {
    let scale = window.scale_factor().ok()?;
    if window.is_minimized().unwrap_or(false)
        || window.is_maximized().unwrap_or(false)
        || window.is_fullscreen().unwrap_or(false)
    {
        return None;
    }
    let mut state = WindowState::default();
    if let Ok(pos) = window.outer_position() {
        let pos = pos.to_logical::<f64>(scale);
        state.x = Some(pos.x);
        state.y = Some(pos.y);
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

/// geometry の大きさ・位置を state に書き写す（kinds はそのまま）
fn copy_geometry(state: &mut WindowState, geometry: &WindowState) {
    state.x = geometry.x.or(state.x);
    state.y = geometry.y.or(state.y);
    state.width = geometry.width.or(state.width);
    state.height = geometry.height.or(state.height);
}

fn read_window_state(app: &AppHandle) -> Option<WindowState> {
    let path = window_state_file(app).ok()?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 前回の続きから開けるよう、閉じるときのウィンドウの位置と大きさを保存する。
/// どちらのサイズ設定でも両方を覚える（次回は必ずこの大きさ・この場所で開く）。
/// 種類ごとに保持する設定なら、表示していた種類の分としても覚える
fn save_window_state(window: &tauri::Window) {
    let app = window.app_handle();
    // 最小化中・最大化中・全画面中は位置も大きさも普段の姿と違うので触らない
    // （その状態のまま閉じたときは、そうする前の姿を覚えたままにする）
    let Some(geometry) = current_geometry(window) else {
        return;
    };
    let mut state = read_window_state(app).unwrap_or_default();
    copy_geometry(&mut state, &geometry);
    if let Ok(kind) = app.state::<WindowKind>().0.lock() {
        if let (true, Some(k)) = (kind.per_kind, kind.kind) {
            state.kinds.insert(kind_key(k).to_string(), geometry);
        }
    }
    write_window_state(app, &state);
}

/// 画面上の長方形。ディスプレイの範囲や作業領域を表す。
/// 物理ピクセルか論理ピクセルかは使う側で揃える
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Area {
    fn from_physical(position: PhysicalPosition<i32>, size: PhysicalSize<u32>) -> Self {
        Self {
            x: f64::from(position.x),
            y: f64::from(position.y),
            width: f64::from(size.width),
            height: f64::from(size.height),
        }
    }

    fn to_logical(self, scale: f64) -> Self {
        Self {
            x: self.x / scale,
            y: self.y / scale,
            width: self.width / scale,
            height: self.height / scale,
        }
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// ディスプレイ全体の範囲（物理ピクセル）
fn monitor_bounds(monitor: &tauri::Monitor) -> Area {
    Area::from_physical(*monitor.position(), *monitor.size())
}

/// ディスプレイの作業領域（物理ピクセル）。
/// タスクバー・メニューバー・Dock を除いた、ウィンドウを置ける範囲
fn monitor_work_area(monitor: &tauri::Monitor) -> Area {
    let work_area = monitor.work_area();
    Area::from_physical(work_area.position, work_area.size)
}

/// 左上 (x, y)・大きさ (width, height) のウィンドウが area に収まるよう位置をずらす。
/// area より大きいときは左上を area の左上に合わせる（右下がはみ出す）
fn clamp_to_area(x: f64, y: f64, width: f64, height: f64, area: &Area) -> (f64, f64) {
    let x = x.min(area.x + area.width - width).max(area.x);
    let y = y.min(area.y + area.height - height).max(area.y);
    (x, y)
}

/// 保存した位置が載っているディスプレイの作業領域（そのディスプレイの倍率での論理ピクセル）。
/// 前回使っていた外部ディスプレイが外れている場合は None（画面外へ開くのを防ぐ）
fn work_area_at(window: &tauri::Window, x: f64, y: f64) -> Option<Area> {
    let monitors = window.available_monitors().ok()?;
    monitors
        .iter()
        .map(|m| (monitor_bounds(m).to_logical(m.scale_factor()), m))
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
        .map(|(_, m)| monitor_work_area(m).to_logical(m.scale_factor()))
}

/// 外枠と中身の大きさの差（物理ピクセル）。
/// Windows の枠なしウィンドウは影のぶん外枠が中身より大きい（macOS では 0）
fn frame_size(outer: PhysicalSize<u32>, inner: PhysicalSize<u32>) -> (u32, u32) {
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
fn fitted_position(
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

/// 設定ファイルから文字列項目を 1 つ読む（Rust 側から設定を参照する用）
fn settings_value(app: &AppHandle, key: &str) -> Option<String> {
    let path = settings_file(app).ok()?;
    let text = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get(key)?.as_str().map(str::to_owned)
}

/// ウィンドウサイズの設定が「画像に合わせる」か。
/// "flexible" は 0.1 系までの古い値（settings.json を書き換えずに読めるようにする）
fn fits_window_to_image(app: &AppHandle) -> bool {
    matches!(
        settings_value(app, "windowSizeMode").as_deref(),
        Some("image") | Some("flexible")
    )
}

/// 起動時に、前回閉じたときのウィンドウを復元する。
/// 大きさも位置も、どちらのサイズ設定でも戻す。
/// 「画像に合わせる」では最初の画像で縦横比を合わせ直すので、そのときも
/// 前回と同じ左上に置けるよう、保存した位置を PendingPosition に残しておく
fn restore_window_state(window: &tauri::Window) {
    let app = window.app_handle();
    let Some(state) = read_window_state(app) else {
        return;
    };
    let placed = place_window(window, &state);
    remember_placement(app, placed);
}

/// 「画像に合わせる」なら、最初の fit_window_to_image が置いた左上に合わせるよう残す
fn remember_placement(app: &AppHandle, placed: Option<RestoredPosition>) {
    if placed.is_none() || !fits_window_to_image(app) {
        return;
    }
    if let Ok(mut pending) = app.state::<PendingPosition>().0.lock() {
        *pending = placed;
    }
}

/// 保存した大きさ・位置にウィンドウを置く。
/// ウィンドウ全体が作業領域に収まるよう寄せる（ディスプレイの構成が
/// 変わっていたり、前回より大きく開いたりしたときに画面外へ出さない）。
/// 位置を戻したときは、保存した左上と実際に置いた左上を返す
fn place_window(window: &tauri::Window, state: &WindowState) -> Option<RestoredPosition> {
    let scale = window.scale_factor().ok()?;
    // 中身の大きさ（論理ピクセル）。保存した大きさがあればそれにする
    let mut inner = window
        .inner_size()
        .map(|s| s.to_logical::<f64>(scale))
        .unwrap_or_else(|_| LogicalSize::new(0.0, 0.0));
    if let (Some(width), Some(height)) = (state.width, state.height) {
        if width >= 1.0 && height >= 1.0 {
            let _ = window.set_size(LogicalSize::new(width, height));
            inner = LogicalSize::new(width, height);
        }
    }

    let (saved_x, saved_y) = (state.x?, state.y?);
    let work_area = work_area_at(window, saved_x, saved_y)?;

    // 外枠の大きさ = 中身 + 枠（Windows の枠なしウィンドウの影のぶん）
    let frame = match (window.outer_size(), window.inner_size()) {
        (Ok(outer), Ok(current)) => frame_size(outer, current),
        _ => (0, 0),
    };
    let outer_width = inner.width + f64::from(frame.0) / scale;
    let outer_height = inner.height + f64::from(frame.1) / scale;
    let (x, y) = clamp_to_area(saved_x, saved_y, outer_width, outer_height, &work_area);
    let _ = window.set_position(LogicalPosition::new(x, y));
    Some(RestoredPosition {
        saved: (saved_x, saved_y),
        placed: (x, y),
    })
}

/// 表示するファイルの種類が変わったことを受け取る。
/// 設定「ファイルの種類ごとにウィンドウを保持する」がオンなら、それまでの種類の
/// 大きさと位置を window.json に覚え、新しい種類で前に使っていた大きさと位置へ戻す
/// （その種類をまだ表示したことがなければ今のまま）。
/// 起動して最初の種類のときは、前回閉じたときの姿（restore_window_state で戻したもの）が
/// 別の種類のものかもしれないので、その種類の分があればそちらへ置き直す。
/// 最大化中・全画面中・最小化中は覚えも戻しもしない（解けてしまう・普段の姿ではない）
#[tauri::command]
fn set_window_kind(
    window: WebviewWindow,
    state: State<WindowKind>,
    lock: State<AspectLock>,
    kind: MediaKind,
    per_kind: bool,
) {
    let previous = {
        let Ok(mut current) = state.0.lock() else {
            return;
        };
        current.per_kind = per_kind;
        current.kind.replace(kind)
    };
    if !per_kind || previous == Some(kind) {
        return;
    }
    let window = window.as_ref().window();
    let Some(geometry) = current_geometry(&window) else {
        return;
    };
    let app = window.app_handle();
    let mut saved = read_window_state(app).unwrap_or_default();
    if let Some(previous) = previous {
        saved
            .kinds
            .insert(kind_key(previous).to_string(), geometry.clone());
        write_window_state(app, &saved);
    }
    let Some(target) = saved.kinds.get(kind_key(kind)) else {
        return;
    };

    // 縦横比の固定を外し、戻した広さを次の縦横比合わせの基準にする
    // （外さないと、set_size の通知を手で変えたものと取り違えて縦横比へ引き戻す。
    // 固定はこのあとフロントエンドが表示する画像に合わせてかけ直す）。
    // ロックは set_size の前に手放す（その場で Resized が呼ばれると止まる）
    if let (Some(width), Some(height)) = (target.width, target.height) {
        if let Ok(mut aspect) = lock.0.lock() {
            *aspect = AspectState {
                area: width * height,
                ..AspectState::default()
            };
        }
    }
    let placed = place_window(&window, target);
    remember_placement(app, placed);
}

/// 立ち上げて最初に開くときの、ウィンドウの大きさの上限（作業領域に対する割合）。
/// 画面ぎりぎりではなく少し余裕を残す
const SCREEN_RATIO: f64 = 0.95;

/// 大きさの制限なし（sized_to_aspect の limit に渡す）
const NO_LIMIT: (f64, f64) = (f64::INFINITY, f64::INFINITY);

/// 立ち上げて最初に開くときの、ウィンドウの大きさの上限（論理ピクセル）。
/// 画面（タスクバーなどを除いた作業領域）からはみ出させないためのもので、
/// 作業領域が分からないときは制限しない
fn screen_limit(work_area: Option<Area>, scale: f64) -> (f64, f64) {
    work_area
        .map(|a| a.to_logical(scale))
        .map(|a| (a.width * SCREEN_RATIO, a.height * SCREEN_RATIO))
        .unwrap_or(NO_LIMIT)
}

/// 今のディスプレイの作業領域（物理ピクセル）。取得できないときは None
fn current_work_area(window: &tauri::Window) -> Option<Area> {
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
fn sized_to_aspect(aspect: f64, extra: (f64, f64), area: f64, limit: (f64, f64)) -> (f64, f64) {
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
/// ratio が null のときは解除する（「自由に変更」や、画像を開いていないとき）。
/// かけ直したときは、そのときのウィンドウの大きさを基準の広さとして覚える
#[tauri::command]
fn set_aspect_lock(
    window: WebviewWindow,
    lock: State<AspectLock>,
    ratio: Option<f64>,
    extra_width: Option<f64>,
    extra_height: Option<f64>,
) {
    let Ok(mut state) = lock.0.lock() else {
        return;
    };
    let Some(ratio) = ratio.filter(|r| *r > 0.0) else {
        *state = AspectState::default();
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
fn dragged_area(
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
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn sizing_area(
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

/// マウスのボタンが押されたままか（どのボタンでも）。
/// ウィンドウの端を引っ張っている間は OS がマウスを握っていて WebView に mouseup が届かないので、
/// フロントエンドはサイズ変更の通知が途切れたときにこれで「まだ引っ張っているか」を確かめ、
/// 手を離すまで表示中のコンテンツの大きさを据え置く
#[tauri::command]
fn mouse_button_down() -> bool {
    #[cfg(target_os = "windows")]
    {
        #[link(name = "user32")]
        extern "system" {
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
fn keep_aspect_on_resize(window: &tauri::Window, size: PhysicalSize<u32>) {
    let app = window.app_handle();
    let lock = app.state::<AspectLock>();
    let Ok(mut state) = lock.0.lock() else {
        return;
    };
    let Some(ratio) = state.ratio else {
        return;
    };
    // 最小化すると 0 を知らせてくる OS があるので、大きさの無い通知は捨てる
    // （そのまま計算すると、戻したときに最小サイズのウィンドウになってしまう）
    if size.width == 0 || size.height == 0 {
        return;
    }
    // 最小化中・最大化中・全画面中は普段の姿ではないので、覚えている大きさも触らない
    if window.is_minimized().unwrap_or(false)
        || window.is_maximized().unwrap_or(false)
        || window.is_fullscreen().unwrap_or(false)
    {
        return;
    }
    // 自分で直したぶんの跳ね返り
    if size == state.last {
        return;
    }
    let Ok(scale) = window.scale_factor() else {
        return;
    };
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
    drop(state);

    let _ = window.set_size(fixed);
}

/// ウィンドウを画像の縦横比に合わせる（「画像に合わせる」のとき）。
/// 大きさは set_aspect_lock が覚えている広さを保ち、縦横比だけを画像に合わせるので、
/// 画像を切り替えても、手でサイズを変えたあとも、大きさの印象は変わらない。
/// 大きさを変えても中心は動かさず、作業領域からはみ出す分は中へ寄せる。
/// 立ち上げて最初の 1 枚だけは、作業領域（タスクバーなどを除いた画面）の 95% に
/// 収まるよう縦横比を保ったまま縮め（縮めた広さを以後の基準にする）、
/// 前回閉じたときと同じ左上に合わせる。
/// 最大化中・全画面中は大きさを変えない（変えると解けてしまう）
#[tauri::command]
fn fit_window_to_image(
    window: WebviewWindow,
    pending: State<PendingPosition>,
    lock: State<AspectLock>,
    startup: State<StartupFit>,
    width: f64,
    height: f64,
    extra_width: Option<f64>,
    extra_height: Option<f64>,
) -> Result<(), String> {
    if !(width > 0.0 && height > 0.0) {
        return Err("画像サイズを取得できません".to_string());
    }
    if window.is_maximized().unwrap_or(false) || window.is_fullscreen().unwrap_or(false) {
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
    let area = lock.0.lock().map(|state| state.area).unwrap_or(0.0);
    let area = if area > 0.0 {
        area
    } else {
        before
            .map(|(_, _, inner)| inner.to_logical::<f64>(scale))
            .map(|inner| inner.width * inner.height)
            .unwrap_or(0.0)
    };

    let work_area = current_work_area(&window.as_ref().window());
    // 画面に収めるのは立ち上げて最初の 1 枚だけ。前回のディスプレイ構成が
    // 変わっていても画面内に開くための保険で、以後は口を出さない
    let first = startup
        .0
        .lock()
        .map(|mut first| std::mem::replace(&mut *first, false))
        .unwrap_or(false);
    let limit = if first {
        screen_limit(work_area, scale)
    } else {
        NO_LIMIT
    };
    let extra = (extra_width.unwrap_or(0.0), extra_height.unwrap_or(0.0));
    let (w, h) = sized_to_aspect(width / height, extra, area, limit);
    let new_inner: PhysicalSize<u32> = LogicalSize::new(w, h).to_physical(scale);

    // 起動して最初の 1 枚は前回の左上に合わせる。ただし起動時に置いた場所から
    // 動いていたら（ユーザーが動かしていたら）そのまま中心を保つ
    let anchor = pending
        .0
        .lock()
        .ok()
        .and_then(|mut p| p.take())
        .and_then(|restored| {
            const TOLERANCE: i32 = 2;
            let (pos, _, _) = before?;
            let placed = LogicalPosition::new(restored.placed.0, restored.placed.1)
                .to_physical::<i32>(scale);
            let unmoved =
                (pos.x - placed.x).abs() <= TOLERANCE && (pos.y - placed.y).abs() <= TOLERANCE;
            unmoved.then(|| {
                LogicalPosition::new(restored.saved.0, restored.saved.1).to_physical::<i32>(scale)
            })
        });

    // set_size の跳ね返りを手動のサイズ変更と取り違えないよう、先に大きさを覚える。
    // 広さは普段は変えないが、立ち上げて最初の 1 枚で画面に収めるために縮めたときは
    // その広さを以後の基準にする（元の広さのままだと、次の画像へ移った途端に
    // 縮める前の大きさへ戻り、ウィンドウが一度だけ大きくなってしまう）
    if let Ok(mut state) = lock.0.lock() {
        state.last = new_inner;
        state.reported = new_inner;
        if first {
            state.area = w * h;
        }
    }
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

/// 設定ファイルの置き場所（OS の設定フォルダ / sview / settings.json）
fn settings_file(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("settings.json"))
}

/// アプリのバージョンを返す（tauri.conf.json の version。
/// リリース時は CI がタグの値を書き込んでからビルドする）
#[tauri::command]
fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
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
    let mut builder =
        WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
            .title("sView の設定")
            .inner_size(470.0, 560.0)
            .min_inner_size(380.0, 300.0)
            .resizable(true)
            .decorations(false)
            .transparent(true)
            .center();
    if let Some(dir) = webview_data_dir(app) {
        builder = builder.data_directory(dir);
    }
    builder
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

/// ログの置き場所（設定フォルダ / sview / logs）。
/// settings.json と同じ場所にまとめて、バグ報告のときに 1 箇所を見れば済むようにする
fn log_dir(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(config_dir(app)?.join("logs"))
}

/// tauri-plugin-log を後から登録する。
/// 出力先の決定に AppHandle が必要なので、Builder ではなく setup() から呼ぶ。
/// 2MB ごとにファイルを切り替え、直近 3 本だけ残す（放置しても膨らまない）
fn init_logging(app: &AppHandle) -> Result<(), String> {
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
fn install_panic_hook() {
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
fn sanitize_dialog_text(message: &str) -> String {
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
fn show_fatal_error(message: &str) {
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
fn open_folder(dir: &Path) -> Result<(), String> {
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
fn open_log_folder(app: AppHandle) -> Result<(), String> {
    let dir = log_dir(&app)?;
    fs::create_dir_all(&dir).map_err(|e| format!("ログフォルダを作れません: {e}"))?;
    open_folder(&dir)
}

/// OS のファイルマネージャーで対象を選択状態にして開く
#[tauri::command]
fn reveal_in_file_manager(path: String) -> Result<(), String> {
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

// ---- 拡張子の関連付け ----
// 設定ウィンドウから、どの拡張子を sView で開くかを選んで OS に登録する。
// OS ごとに「アプリが勝手に既定を奪えるか」が違うので、それぞれの流儀に従う:
// - Windows 8 以降は、アプリが既定のアプリを直接書き換えることを OS が認めていない
//   （UserChoice はハッシュで保護されている）。そこで sView を「この拡張子を開ける
//   アプリ」として登録し、最後の選択は Windows の「既定のアプリ」画面で本人にしてもらう
// - macOS は LaunchServices の API で既定のアプリを直接設定できる

/// 関連付けの対象にできる拡張子（画像・動画・音楽すべて）。
/// 書庫（zip / cbz）はここでは扱わない
fn associable_exts() -> Vec<&'static str> {
    [IMAGE_EXTS, VIDEO_EXTS, AUDIO_EXTS].concat()
}

#[derive(serde::Serialize)]
struct AssociationItem {
    ext: String,
    /// 種類（設定ウィンドウで種類ごとに分けて並べる）
    kind: MediaKind,
    /// いま sView がこの拡張子の既定のアプリになっているか（調べられないときは false）
    associated: bool,
}

#[derive(serde::Serialize)]
struct AssociationStatus {
    /// "windows" / "macos"
    platform: String,
    items: Vec<AssociationItem>,
}

/// 渡された拡張子を検証し、小文字にそろえる（関連付けできないものはエラー）
fn normalize_association_exts(exts: &[String]) -> Result<Vec<String>, String> {
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
fn file_association_status(app: AppHandle) -> AssociationStatus {
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
fn apply_file_associations(app: AppHandle, exts: Vec<String>) -> Result<String, String> {
    let exts = normalize_association_exts(&exts)?;
    let message = apply_associations(&app, &exts)?;
    log::info!("拡張子の関連付けを変更しました: {exts:?}");
    Ok(message)
}

#[cfg(target_os = "windows")]
mod win_assoc {
    use std::path::{Path, PathBuf};
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
    use winreg::RegKey;

    /// 「既定のアプリ」画面や RegisteredApplications に出る名前
    pub const APP_NAME: &str = "sView";
    const CAPABILITIES_KEY: &str = r"Software\sView\Capabilities";
    /// 種類ごとの ProgID と、エクスプローラーに出る種類の名前
    const PROG_IDS: &[(super::MediaKind, &str, &str)] = &[
        (
            super::MediaKind::Image,
            "sView.Image",
            "画像ファイル (sView)",
        ),
        (
            super::MediaKind::Video,
            "sView.Video",
            "動画ファイル (sView)",
        ),
        (
            super::MediaKind::Audio,
            "sView.Audio",
            "音楽ファイル (sView)",
        ),
    ];

    /// 拡張子に対応する sView の ProgID
    fn prog_id_for(ext: &str) -> &'static str {
        let kind = super::media_kind(Path::new(&format!("x.{ext}")));
        PROG_IDS
            .iter()
            .find(|(k, _, _)| Some(*k) == kind)
            .map(|(_, id, _)| *id)
            .unwrap_or(PROG_IDS[0].1)
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(
            event_id: i32,
            flags: u32,
            item1: *const std::ffi::c_void,
            item2: *const std::ffi::c_void,
        );
    }

    fn reg_err(e: std::io::Error) -> String {
        format!("レジストリに書き込めません: {e}")
    }

    fn exe_path() -> Result<PathBuf, String> {
        std::env::current_exe().map_err(|e| format!("sView の場所が分かりません: {e}"))
    }

    /// ProgID の開くコマンドが sView の exe を指しているか
    fn prog_id_is_ours(prog_id: &str) -> bool {
        if PROG_IDS
            .iter()
            .any(|(_, id, _)| prog_id.eq_ignore_ascii_case(id))
        {
            return true;
        }
        let Some(exe) = exe_path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        else {
            return false;
        };
        RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey(format!(r"{prog_id}\shell\open\command"))
            .and_then(|k| k.get_value::<String, _>(""))
            .map(|cmd| cmd.to_lowercase().contains(&exe))
            .unwrap_or(false)
    }

    /// いまエクスプローラーがこの拡張子に使う ProgID
    fn current_prog_id(ext: &str) -> Option<String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let base = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{ext}");
        // 本人が「既定のアプリ」で選んだもの（Windows 11 の新しい版は UserChoiceLatest）
        for sub in ["UserChoiceLatest", "UserChoice"] {
            if let Ok(id) = hkcu
                .open_subkey(format!(r"{base}\{sub}"))
                .and_then(|k| k.get_value::<String, _>("ProgId"))
            {
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
        // 選んでいなければ、クラス登録の既定値が使われる
        RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey(format!(".{ext}"))
            .and_then(|k| k.get_value::<String, _>(""))
            .ok()
            .filter(|id| !id.is_empty())
    }

    pub fn is_associated(ext: &str) -> bool {
        current_prog_id(ext).is_some_and(|id| prog_id_is_ours(&id))
    }

    /// sView を「開けるアプリ」として登録し直す。選ばれなかった拡張子の登録は外す
    pub fn register(exts: &[String], all: &[&str]) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let exe = exe_path()?;
        let exe = exe.to_string_lossy();

        // ProgID（開き方）。インストーラーが HKLM に入れたものとは別に、ユーザー単位で持つ
        for &(_, id, name) in PROG_IDS {
            let (key, _) = hkcu
                .create_subkey(format!(r"Software\Classes\{id}"))
                .map_err(reg_err)?;
            key.set_value("", &name).map_err(reg_err)?;
            let (icon, _) = key.create_subkey("DefaultIcon").map_err(reg_err)?;
            icon.set_value("", &format!("\"{exe}\",0"))
                .map_err(reg_err)?;
            let (command, _) = key.create_subkey(r"shell\open\command").map_err(reg_err)?;
            command
                .set_value("", &format!("\"{exe}\" \"%1\""))
                .map_err(reg_err)?;
        }

        // 「既定のアプリ」画面に sView を出すための登録（Capabilities）
        let (caps, _) = hkcu.create_subkey(CAPABILITIES_KEY).map_err(reg_err)?;
        caps.set_value("ApplicationName", &APP_NAME)
            .map_err(reg_err)?;
        caps.set_value("ApplicationDescription", &"軽量な画像・動画・音楽ビューア")
            .map_err(reg_err)?;
        // 選ばれなかった拡張子を残さないよう、一覧は作り直す
        let _ = caps.delete_subkey_all("FileAssociations");
        let (assoc, _) = caps.create_subkey("FileAssociations").map_err(reg_err)?;
        for ext in exts {
            assoc
                .set_value(format!(".{ext}"), &prog_id_for(ext))
                .map_err(reg_err)?;
        }
        let (apps, _) = hkcu
            .create_subkey(r"Software\RegisteredApplications")
            .map_err(reg_err)?;
        apps.set_value(APP_NAME, &CAPABILITIES_KEY)
            .map_err(reg_err)?;

        // 「プログラムから開く」の候補
        for ext in all {
            let path = format!(r"Software\Classes\.{ext}\OpenWithProgids");
            if exts.iter().any(|e| e == ext) {
                let (key, _) = hkcu.create_subkey(&path).map_err(reg_err)?;
                key.set_value(prog_id_for(ext), &"").map_err(reg_err)?;
            } else if let Ok(key) =
                hkcu.open_subkey_with_flags(&path, winreg::enums::KEY_ALL_ACCESS)
            {
                let _ = key.delete_value(prog_id_for(ext));
            }
        }

        // エクスプローラーに関連付けの変更を知らせる（SHCNE_ASSOCCHANGED / SHCNF_IDLIST）
        unsafe { SHChangeNotify(0x0800_0000, 0, std::ptr::null(), std::ptr::null()) };
        Ok(())
    }

    /// Windows の「既定のアプリ」画面を sView の項目で開く
    /// （registeredAppUser が効かない古い Windows 10 では、既定のアプリの一覧が開く）
    pub fn open_default_apps_settings() -> Result<(), String> {
        std::process::Command::new("explorer")
            .arg(format!(
                "ms-settings:defaultapps?registeredAppUser={APP_NAME}"
            ))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Windows の設定を開けません: {e}"))
    }
}

#[cfg(target_os = "macos")]
mod mac_assoc {
    use std::ffi::{c_char, c_void, CStr};

    type CFStringRef = *const c_void;
    type OSStatus = i32;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// kLSRolesAll。Viewer だけにすると、Editor として登録されたアプリが優先されることがある
    const K_LS_ROLES_ALL: u32 = 0xFFFF_FFFF;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;
        fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        static kUTTagClassFilenameExtension: CFStringRef;
        fn UTTypeCreatePreferredIdentifierForTag(
            tag_class: CFStringRef,
            tag: CFStringRef,
            conforming_to: CFStringRef,
        ) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            handler_bundle_id: CFStringRef,
        ) -> OSStatus;
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
        ) -> CFStringRef;
    }

    /// 解放を忘れないための CFString の入れ物
    struct CfString(CFStringRef);

    impl CfString {
        fn new(s: &str) -> Option<Self> {
            let r = unsafe {
                CFStringCreateWithBytes(
                    std::ptr::null(),
                    s.as_ptr(),
                    s.len() as isize,
                    K_CF_STRING_ENCODING_UTF8,
                    0,
                )
            };
            (!r.is_null()).then_some(CfString(r))
        }

        fn to_string(&self) -> Option<String> {
            let mut buf = [0 as c_char; 512];
            let ok = unsafe {
                CFStringGetCString(
                    self.0,
                    buf.as_mut_ptr(),
                    buf.len() as isize,
                    K_CF_STRING_ENCODING_UTF8,
                )
            };
            (ok != 0).then(|| {
                unsafe { CStr::from_ptr(buf.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            })
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }

    /// 拡張子に対応する UTI（例: jpg → public.jpeg）
    fn uti_for_ext(ext: &str) -> Option<CfString> {
        let tag = CfString::new(ext)?;
        let r = unsafe {
            UTTypeCreatePreferredIdentifierForTag(
                kUTTagClassFilenameExtension,
                tag.0,
                std::ptr::null(),
            )
        };
        (!r.is_null()).then_some(CfString(r))
    }

    pub fn is_associated(ext: &str, bundle_id: &str) -> bool {
        let Some(uti) = uti_for_ext(ext) else {
            return false;
        };
        let r = unsafe { LSCopyDefaultRoleHandlerForContentType(uti.0, K_LS_ROLES_ALL) };
        if r.is_null() {
            return false;
        }
        CfString(r)
            .to_string()
            .is_some_and(|id| id.eq_ignore_ascii_case(bundle_id))
    }

    pub fn set_default(ext: &str, bundle_id: &str) -> Result<(), String> {
        let uti = uti_for_ext(ext).ok_or_else(|| format!(".{ext} の種類を特定できません"))?;
        let bundle = CfString::new(bundle_id).ok_or("バンドル ID を扱えません")?;
        let status =
            unsafe { LSSetDefaultRoleHandlerForContentType(uti.0, K_LS_ROLES_ALL, bundle.0) };
        if status == 0 {
            Ok(())
        } else {
            Err(format!(".{ext} を関連付けられません (OSStatus {status})"))
        }
    }
}

/// Windows: 「メディアに合わせる」で端・角を引っ張っている間、OS が大きさを
/// 決める前（WM_SIZING）に縦横比へ合わせる。
///
/// Resized を受けてから set_size で直すだけだと、マウスごとに「OS が決めた大きさ」と
/// 「直した大きさ」の 2 回描かれるうえ、角を引っ張ったときは直す基準の辺が
/// マウスの細かな動きで入れ替わり、ウィンドウと表示がちらつく。WM_SIZING なら
/// 引っ張られている辺・角が分かり、変わる前の四角形を書き換えられるので、
/// 縦横比どおりの大きさが 1 回だけ描かれる。ここで直した大きさは AspectLock に
/// 書いておくので、続いて届く Resized は keep_aspect_on_resize が跳ね返りとして見送る
#[cfg(target_os = "windows")]
mod win_sizing {
    use super::{sized_to_aspect, sizing_area, AspectLock, NO_LIMIT};
    use tauri::{AppHandle, LogicalSize, Manager, PhysicalSize, WebviewWindow};

    type Hwnd = isize;
    type SubclassProc = unsafe extern "system" fn(Hwnd, u32, usize, isize, usize, usize) -> isize;

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "comctl32")]
    extern "system" {
        fn SetWindowSubclass(hwnd: Hwnd, proc: SubclassProc, id: usize, data: usize) -> i32;
        fn DefSubclassProc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetDpiForWindow(hwnd: Hwnd) -> u32;
    }

    const WM_SIZING: u32 = 0x0214;
    const WMSZ_LEFT: usize = 1;
    const WMSZ_RIGHT: usize = 2;
    const WMSZ_TOP: usize = 3;
    const WMSZ_TOPLEFT: usize = 4;
    const WMSZ_TOPRIGHT: usize = 5;
    const WMSZ_BOTTOM: usize = 6;
    const WMSZ_BOTTOMLEFT: usize = 7;
    const WMSZ_BOTTOMRIGHT: usize = 8;
    const SUBCLASS_ID: usize = 0x5356_4945; // "SVIE"

    pub fn install(window: &WebviewWindow) -> Result<(), String> {
        let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as Hwnd;
        // サブクラスはウィンドウと同じだけ生きるので、AppHandle は手放さない
        let data = Box::into_raw(Box::new(window.app_handle().clone())) as usize;
        if unsafe { SetWindowSubclass(hwnd, proc, SUBCLASS_ID, data) } == 0 {
            drop(unsafe { Box::from_raw(data as *mut AppHandle) });
            return Err("ウィンドウのサブクラス化に失敗しました".to_string());
        }
        Ok(())
    }

    unsafe extern "system" fn proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        data: usize,
    ) -> isize {
        if msg == WM_SIZING && lparam != 0 && data != 0 {
            let app = &*(data as *const AppHandle);
            if constrain(app, hwnd, wparam, &mut *(lparam as *mut Rect)) {
                return 1;
            }
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// 引っ張られている辺・角 edge に合わせて、これからなる四角形 rect（外枠・物理ピクセル）を
    /// 縦横比へ合わせる。反対側の辺・角は動かさない。書き換えたら true
    fn constrain(app: &AppHandle, hwnd: Hwnd, edge: usize, rect: &mut Rect) -> bool {
        let (horizontal, vertical) = match edge {
            WMSZ_LEFT | WMSZ_RIGHT => (true, false),
            WMSZ_TOP | WMSZ_BOTTOM => (false, true),
            WMSZ_TOPLEFT | WMSZ_TOPRIGHT | WMSZ_BOTTOMLEFT | WMSZ_BOTTOMRIGHT => (true, true),
            _ => return false,
        };
        let lock = app.state::<AspectLock>();
        // 取れないときは今回だけ OS に任せる（あとの Resized で直る）
        let Ok(mut state) = lock.0.try_lock() else {
            return false;
        };
        let Some(ratio) = state.ratio else {
            return false;
        };

        // 外枠と中身の差（枠なしでも Windows は見えない影の分だけ外枠が大きい）
        let mut outer = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut client = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { GetWindowRect(hwnd, &mut outer) } == 0
            || unsafe { GetClientRect(hwnd, &mut client) } == 0
        {
            return false;
        }
        let border_w = (outer.right - outer.left) - (client.right - client.left);
        let border_h = (outer.bottom - outer.top) - (client.bottom - client.top);
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi > 0 { dpi as f64 / 96.0 } else { 1.0 };

        let inner = PhysicalSize::new(
            (rect.right - rect.left - border_w).max(1) as u32,
            (rect.bottom - rect.top - border_h).max(1) as u32,
        );
        let now = inner.to_logical::<f64>(scale);
        let area = sizing_area(horizontal, vertical, now, ratio, state.extra);
        let (width, height) = sized_to_aspect(ratio, state.extra, area, NO_LIMIT);
        let fixed: PhysicalSize<u32> = LogicalSize::new(width, height).to_physical(scale);

        // 続く Resized が跳ね返りだと分かるように、ここで決めた大きさを覚えておく
        state.last = fixed;
        state.reported = fixed;
        state.area = width * height;
        drop(state);

        let w = fixed.width as i32 + border_w;
        let h = fixed.height as i32 + border_h;
        // 引っ張っている側を動かし、反対側は据え置く。
        // 辺のときは、縦横比で決まるもう一方は右・下へ伸び縮みさせる
        if matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT) {
            rect.left = rect.right - w;
        } else {
            rect.right = rect.left + w;
        }
        if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
            rect.top = rect.bottom - h;
        } else {
            rect.bottom = rect.top + h;
        }
        true
    }
}

/// macOS: メインウィンドウへのカーソルの出入りと移動をフロントエンドへ知らせる。
///
/// WKWebView 自身の追跡範囲は「キーウィンドウのときだけ」有効なので、ウィンドウが
/// 非アクティブだと mousemove が届かず、アクティブでも外へ出たときの mouseleave が
/// 当てにならない（Windows の WebView2 はどちらも届く）。そこで content view に
/// 常時有効な NSTrackingArea を足し、出入りは `pointer-inside`（bool）、
/// 非アクティブ中の移動は `pointer-moved` として送る。アクティブ中の移動は
/// WebView の mousemove が届くので送らない
#[cfg(target_os = "macos")]
mod mac_pointer {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{
        define_class, msg_send, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
    };
    use objc2_app_kit::{NSEvent, NSTrackingArea, NSTrackingAreaOptions, NSWindow};
    use objc2_foundation::NSRect;
    use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

    /// 非アクティブ中の移動を送る最短間隔（表示を出し直すだけなので粗くてよい）
    const MOVE_INTERVAL: Duration = Duration::from_millis(100);

    struct Ivars {
        app: AppHandle,
        window: Retained<NSWindow>,
        last_move: Cell<Option<Instant>>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SViewPointerTracker"]
        #[ivars = Ivars]
        struct PointerTracker;

        impl PointerTracker {
            #[unsafe(method(mouseEntered:))]
            fn mouse_entered(&self, _event: &NSEvent) {
                self.emit_inside(true);
            }

            #[unsafe(method(mouseExited:))]
            fn mouse_exited(&self, _event: &NSEvent) {
                self.emit_inside(false);
            }

            #[unsafe(method(mouseMoved:))]
            fn mouse_moved(&self, _event: &NSEvent) {
                let ivars = self.ivars();
                if ivars.window.isKeyWindow() {
                    return;
                }
                let now = Instant::now();
                if ivars
                    .last_move
                    .get()
                    .is_some_and(|t| now.duration_since(t) < MOVE_INTERVAL)
                {
                    return;
                }
                ivars.last_move.set(Some(now));
                let _ = ivars.app.emit_to("main", "pointer-moved", ());
            }
        }
    );

    impl PointerTracker {
        fn emit_inside(&self, inside: bool) {
            self.ivars().last_move.set(None);
            let _ = self.ivars().app.emit_to("main", "pointer-inside", inside);
        }
    }

    /// メインウィンドウに追跡範囲を取り付ける。setup() から（メインスレッドで）呼ぶ
    pub fn install(window: &WebviewWindow) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("メインスレッド以外から呼ばれました")?;
        let ptr = window
            .ns_window()
            .map_err(|e| format!("NSWindow を取得できません: {e}"))?;
        // SAFETY: Tauri が返すのは生きている NSWindow へのポインタ
        let ns_window = unsafe { Retained::retain(ptr.cast::<NSWindow>()) }
            .ok_or("NSWindow を取得できません")?;
        let view = ns_window
            .contentView()
            .ok_or("ウィンドウの content view がありません")?;

        let tracker = PointerTracker::alloc(mtm).set_ivars(Ivars {
            app: window.app_handle().clone(),
            window: ns_window,
            last_move: Cell::new(None),
        });
        let tracker: Retained<PointerTracker> = unsafe { msg_send![super(tracker), init] };

        // InVisibleRect: 範囲はビューの見えている部分に自動で追従する（rect は無視される）
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        let owner: &AnyObject = &tracker;
        // SAFETY: owner は mouseEntered: / mouseExited: / mouseMoved: を実装している
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(owner),
                None,
            )
        };
        view.addTrackingArea(&area);
        // NSTrackingArea は owner を保持しない。メインウィンドウはアプリと同じだけ
        // 生きるので、owner はわざと解放しない
        std::mem::forget(tracker);
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn is_associated(_app: &AppHandle, ext: &str) -> bool {
    win_assoc::is_associated(ext)
}

#[cfg(target_os = "macos")]
fn is_associated(app: &AppHandle, ext: &str) -> bool {
    mac_assoc::is_associated(ext, &app.config().identifier)
}

// 対象外の OS（Linux など）は CI でテストをビルドするためだけの分岐
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn is_associated(_app: &AppHandle, _ext: &str) -> bool {
    false
}

#[cfg(target_os = "windows")]
fn apply_associations(_app: &AppHandle, exts: &[String]) -> Result<String, String> {
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
fn apply_associations(app: &AppHandle, exts: &[String]) -> Result<String, String> {
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
fn apply_associations(_app: &AppHandle, _exts: &[String]) -> Result<String, String> {
    Err("この OS には対応していません".into())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    install_panic_hook();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| match event {
            // 「画像に合わせる」のときだけ、ドラッグの最中も縦横比を保つ
            tauri::WindowEvent::Resized(size) if window.label() == "main" => {
                keep_aspect_on_resize(window, *size);
            }
            tauri::WindowEvent::CloseRequested { api, .. } => match window.label() {
                // 設定ウィンドウは閉じずに隠す。破棄してしまうと
                // 開き直すたびに作り直しになり、閉じ忘れるとアプリが残り続ける
                "settings" => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // 本体を閉じたらアプリごと終了する（非表示の設定ウィンドウが
                // 残っていても終了できるようにする）。
                // 閉じる直前の大きさと場所は、次回ここから開くために覚えておく
                "main" => {
                    save_window_state(window);
                    window.app_handle().exit(0);
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

            // tauri.conf.json のウィンドウは create: false にしてあるので、
            // ここで作る（WebView のデータフォルダを指定するため）。
            // 失敗したら起動を諦める（show_fatal_error で理由を出す）
            if let Err(e) = create_configured_windows(app.handle()) {
                log::error!("{e}");
                return Err(e.into());
            }

            if let Some(window) = app.get_webview_window("main") {
                restore_window_state(&window.as_ref().window());
                // 失敗しても、縦横比はあとから直す方法（keep_aspect_on_resize）で保たれる
                #[cfg(target_os = "windows")]
                if let Err(e) = win_sizing::install(&window) {
                    log::warn!("ウィンドウのサイズ変更を縦横比に合わせられません: {e}");
                }
                // 失敗しても表示の自動非表示が待ち時間頼みになるだけなので起動は続ける
                #[cfg(target_os = "macos")]
                if let Err(e) = mac_pointer::install(&window) {
                    log::warn!("カーソルの追跡を開始できません: {e}");
                }
                // サイズを整えてから見せる（起動直後のちらつきを避ける）
                let _ = window.show();
            }
            Ok(())
        })
        .manage(StartupFile(Mutex::new(startup_file_from_args())))
        .manage(ArchiveCache(Mutex::new(None)))
        .manage(PendingPosition(Mutex::new(None)))
        .manage(WindowKind(Mutex::new(KindState::default())))
        .manage(AspectLock(Mutex::new(AspectState::default())))
        .manage(StartupFit(Mutex::new(true)))
        .manage(FolderWatcher(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            set_window_kind,
            list_images,
            read_archive_image,
            get_startup_file,
            app_version,
            load_settings,
            save_settings,
            open_settings_window,
            reveal_in_file_manager,
            open_log_folder,
            fit_window_to_image,
            set_aspect_lock,
            mouse_button_down,
            delete_image,
            audio_info,
            audio_artwork,
            watch_folder,
            file_association_status,
            apply_file_associations
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
        // macOS: Finder / Dock からファイルを開いたときに届く
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = &_event {
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
