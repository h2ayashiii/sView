use std::cmp::Ordering;
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

/// 「画像に合わせる」で、ウィンドウを画像の縦横比から外させないための覚え書き。
/// ドラッグの最中も含め、大きさが変わるたびに参照する
#[derive(Default)]
struct AspectState {
    /// 固定する縦横比（横 / 縦）。None のときは固定しない
    ratio: Option<f64>,
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
    /// drop するとネイティブの監視も解除されるので、持っているだけでよい
    _watcher: RecommendedWatcher,
}
struct FolderWatcher(Mutex<Option<Watching>>);

#[derive(serde::Serialize)]
struct ImageList {
    /// フォルダの場合は画像のフルパス、書庫の場合は書庫内のエントリ名
    images: Vec<String>,
    index: usize,
    /// 書庫を開いている場合はその書庫のフルパス
    archive: Option<String>,
    /// 監視対象のフォルダ（フォルダを開いた場合のみ。書庫では None）
    dir: Option<String>,
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
        let (w, h) = sized_to_aspect(16.0 / 9.0, 500.0 * 500.0, limit);
        assert!((w / h - 16.0 / 9.0).abs() < 1e-9);
        assert!((w * h - 250_000.0).abs() < 1e-6);
        // 縦長の画像でも広さは変わらない（切り替えても大きさの印象が揃う）
        let (w, h) = sized_to_aspect(9.0 / 16.0, 500.0 * 500.0, limit);
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
        let (w, h) = sized_to_aspect(ratio, dragged_area(now, before, ratio), limit);
        assert!((w - 1000.0).abs() < 1e-9);
        assert!((h - 1000.0 / ratio).abs() < 1e-9);
        // 下端を引っ張った（縦だけ変わった）→ 縦はそのまま、横が縦横比で決まる
        let now = LogicalSize::new(800.0, 600.0);
        let (w, h) = sized_to_aspect(ratio, dragged_area(now, before, ratio), limit);
        assert!((h - 600.0).abs() < 1e-9);
        assert!((w - 600.0 * ratio).abs() < 1e-9);
    }

    #[test]
    fn sized_to_aspect_shrinks_to_fit_the_screen() {
        let limit = (1728.0, 972.0);
        // 画面に収まらない広さを求められたら、縦横比を保ったまま縮める
        let (w, h) = sized_to_aspect(1.0, 4000.0 * 4000.0, limit);
        assert!((w - 972.0).abs() < 1e-9);
        assert!((h - 972.0).abs() < 1e-9);
    }

    #[test]
    fn sized_to_aspect_grows_to_the_minimum_size() {
        let limit = (1728.0, 972.0);
        // 小さすぎる指定でも最小サイズは下回らない
        let (w, h) = sized_to_aspect(1.0, 1.0, limit);
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
    fn listing_change_only_reacts_to_images_appearing_or_disappearing() {
        use notify::event::{
            CreateKind, DataChange, EventKind, ModifyKind, RemoveKind, RenameMode,
        };
        let event = |kind, paths: &[&str]| notify::Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        };

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
        // 画像以外は無関係
        assert!(!is_listing_change(&event(
            EventKind::Create(CreateKind::File),
            &["/x/notes.txt"]
        )));
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

        let archives = sorted(ARCHIVE_EXTS.to_vec());
        assert_eq!(
            sorted(quoted_items(&main_js, "const ARCHIVE_EXT_FILTER = [", ']')),
            archives,
            "main.js の ARCHIVE_EXT_FILTER が ARCHIVE_EXTS とずれています"
        );
        // 書庫は zip も開けるが、OS の関連付けは cbz だけにしている。
        // zip を取ると解凍ソフトと取り合いになるため（ドラッグ＆ドロップと O キーでは開ける）
        assert_eq!(association("Comic Book Archive"), vec!["cbz"]);
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
        dir: None,
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
        dir: Some(dir.to_string_lossy().into_owned()),
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

/// 表示中の画像を OS のゴミ箱（Windows: ごみ箱 / macOS: ゴミ箱 / Linux: freedesktop の Trash）へ送る。
/// 完全削除はしないので、取り違えても OS 側から戻せる
#[tauri::command]
fn delete_image(path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    if !is_image(&target) {
        return Err(format!("画像ファイルではありません: {path}"));
    }
    trash::delete(&target).map_err(|e| format!("ゴミ箱へ移動できませんでした: {e}"))?;
    log::info!("ゴミ箱へ移動しました: {path}");
    Ok(())
}

/// 一覧を作り直す必要がある変更かどうか。
/// 画像の増減（作成・削除・名前の変更）だけを拾い、中身の書き換えは無視する。
/// 種類を判別できない通知（EventKind::Any）は、取りこぼすより拾う方に倒す
fn is_listing_change(event: &notify::Event) -> bool {
    use notify::event::{EventKind, ModifyKind};
    let kind_matches = matches!(
        event.kind,
        EventKind::Any
            | EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_))
    );
    // 名前の変更では変更前と変更後の両方が入るので、どちらかが画像なら対象
    kind_matches && event.paths.iter().any(|p| is_image(p))
}

/// 表示中のフォルダの監視を開始する（`path` が null なら監視をやめる）。
///
/// OS のネイティブ通知を使うので、変化が無い間は CPU もディスクも使わない
/// （ポーリングのように一定間隔で read_dir する方式とはここが違う）。
/// サブフォルダは見ない（表示対象が同一フォルダ内だけなので）。
/// 通知は数が多くなりがちなので、実際の再スキャンはフロントエンド側で
/// 一定時間まとめてから 1 回だけ行う
#[tauri::command]
fn watch_folder(
    app: AppHandle,
    state: State<FolderWatcher>,
    path: Option<String>,
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
    if current.as_ref().is_some_and(|w| w.path == dir) {
        return Ok(()); // 同じフォルダなら張り直さない
    }
    // 先に古い監視を解除してから張り直す（二重に監視しない）
    *current = None;
    if !dir.is_dir() {
        return Err(format!("フォルダが見つかりません: {}", dir.display()));
    }

    let handle = app.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        match res {
            Ok(event) if is_listing_change(&event) => {
                let _ = handle.emit("folder-changed", ());
            }
            // 監視できなくなった場合（フォルダごと消えたなど）は記録だけして続ける。
            // 一覧は R キーで作り直せる
            Err(e) => log::warn!("フォルダの監視でエラーが発生しました: {e}"),
            Ok(_) => {}
        }
    })
    .map_err(|e| format!("フォルダを監視できません: {e}"))?;

    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("フォルダを監視できません: {e}"))?;
    log::info!("フォルダの監視を開始しました: {}", dir.display());
    *current = Some(Watching {
        path: dir,
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
/// Linux: WebKitGTK が既定で `~/.local/share/sview` と `~/.cache/sview` を
/// 使うので、こちらから指定する必要が無い。
/// どちらも None を返して既定の挙動に任せる
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
/// どちらのサイズ設定でも両方を覚える（次回は必ずこの大きさ・この場所で開く）
fn save_window_state(window: &tauri::Window) {
    let app = window.app_handle();
    let Ok(path) = window_state_file(app) else {
        return;
    };
    let Ok(scale) = window.scale_factor() else {
        return;
    };
    // 最小化中・最大化中・全画面中は位置も大きさも普段の姿と違うので触らない
    // （その状態のまま閉じたときは、そうする前の姿を覚えたままにする）
    if window.is_minimized().unwrap_or(false)
        || window.is_maximized().unwrap_or(false)
        || window.is_fullscreen().unwrap_or(false)
    {
        return;
    }

    let mut state = read_window_state(app).unwrap_or_default();

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

    if let (Some(dir), Ok(text)) = (path.parent(), serde_json::to_string_pretty(&state)) {
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(&path, text);
    }
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
/// ウィンドウ全体が作業領域に収まるよう寄せる（ディスプレイの構成が
/// 変わっていたり、前回より大きく開いたりしたときに画面外へ出さない）。
/// 「画像に合わせる」では最初の画像で縦横比を合わせ直すので、そのときも
/// 前回と同じ左上に置けるよう、保存した位置を PendingPosition に残しておく
fn restore_window_state(window: &tauri::Window) {
    let app = window.app_handle();
    let Some(state) = read_window_state(app) else {
        return;
    };
    let Ok(scale) = window.scale_factor() else {
        return;
    };
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

    let (Some(saved_x), Some(saved_y)) = (state.x, state.y) else {
        return;
    };
    let Some(work_area) = work_area_at(window, saved_x, saved_y) else {
        return;
    };

    // 外枠の大きさ = 中身 + 枠（Windows の枠なしウィンドウの影のぶん）
    let frame = match (window.outer_size(), window.inner_size()) {
        (Ok(outer), Ok(current)) => frame_size(outer, current),
        _ => (0, 0),
    };
    let outer_width = inner.width + f64::from(frame.0) / scale;
    let outer_height = inner.height + f64::from(frame.1) / scale;
    let (x, y) = clamp_to_area(saved_x, saved_y, outer_width, outer_height, &work_area);
    let _ = window.set_position(LogicalPosition::new(x, y));

    if fits_window_to_image(app) {
        if let Ok(mut pending) = app.state::<PendingPosition>().0.lock() {
            *pending = Some(RestoredPosition {
                saved: (saved_x, saved_y),
                placed: (x, y),
            });
        }
    }
}

/// ウィンドウの大きさの上限（論理ピクセル）。画面（タスクバーなどを除いた
/// 作業領域）からはみ出させないためのもので、作業領域が分からないときは制限しない
fn screen_limit(work_area: Option<Area>, scale: f64) -> (f64, f64) {
    work_area
        .map(|a| a.to_logical(scale))
        .map(|a| (a.width, a.height))
        .unwrap_or((f64::INFINITY, f64::INFINITY))
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
/// limit に収まらない場合は縦横比を保ったまま縮め、小さすぎる場合は保ったまま広げる。
/// 極端な縦横比では両立しないことがあるが、そのときは最小サイズを優先する
fn sized_to_aspect(aspect: f64, area: f64, limit: (f64, f64)) -> (f64, f64) {
    let aspect = if aspect > 0.0 { aspect } else { 1.0 };
    let area = area.max(MIN_WINDOW_SIZE.0 * MIN_WINDOW_SIZE.1);

    // area = width * height かつ aspect = width / height を満たす大きさ
    let mut width = (area * aspect).sqrt();
    let mut height = width / aspect;

    let shrink = (limit.0 / width).min(limit.1 / height).min(1.0);
    width *= shrink;
    height *= shrink;

    let grow = (MIN_WINDOW_SIZE.0 / width)
        .max(MIN_WINDOW_SIZE.1 / height)
        .max(1.0);
    (width * grow, height * grow)
}

/// 「画像に合わせる」で、ウィンドウの縦横比を表示中の画像に固定する。
/// ratio が null のときは解除する（「自由に変更」や、画像を開いていないとき）。
/// かけ直したときは、そのときのウィンドウの大きさを基準の広さとして覚える
#[tauri::command]
fn set_aspect_lock(window: WebviewWindow, lock: State<AspectLock>, ratio: Option<f64>) {
    let Ok(mut state) = lock.0.lock() else {
        return;
    };
    let Some(ratio) = ratio.filter(|r| *r > 0.0) else {
        *state = AspectState::default();
        return;
    };
    state.ratio = Some(ratio);
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
fn dragged_area(now: LogicalSize<f64>, before: LogicalSize<f64>, ratio: f64) -> f64 {
    let dw = (now.width - before.width).abs() / before.width.max(1.0);
    let dh = (now.height - before.height).abs() / before.height.max(1.0);
    if dw >= dh {
        now.width * now.width / ratio
    } else {
        now.height * now.height * ratio
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
    let area = dragged_area(now, before, ratio);
    let limit = screen_limit(current_work_area(window), scale);
    let (width, height) = sized_to_aspect(ratio, area, limit);
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
/// 作業領域（タスクバーなどを除いた画面）に収まらないときは、
/// 縦横比を保ったまま縮める。
/// 大きさを変えても中心は動かさず、作業領域からはみ出す分は中へ寄せる。
/// 起動して最初の 1 枚だけは、前回閉じたときと同じ左上に合わせる。
/// 最大化中・全画面中は大きさを変えない（変えると解けてしまう）
#[tauri::command]
fn fit_window_to_image(
    window: WebviewWindow,
    pending: State<PendingPosition>,
    lock: State<AspectLock>,
    width: f64,
    height: f64,
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
    let limit = screen_limit(work_area, scale);
    let (w, h) = sized_to_aspect(width / height, area, limit);
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

    // 覚えている広さは変えない（画面に収めるために縮めても、次の画像で元の広さに戻せる）。
    // set_size の跳ね返りを手動のサイズ変更と取り違えないよう、先に大きさだけ覚える
    if let Ok(mut state) = lock.0.lock() {
        state.last = new_inner;
        state.reported = new_inner;
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

    // Linux には共通のダイアログが無いので、stderr への出力だけで済ませる
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let _ = text;
}

/// OS のファイルマネージャーでフォルダを開く（中身を表示する。選択状態にはしない）
fn open_folder(dir: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let command = "explorer";
    #[cfg(target_os = "macos")]
    let command = "open";
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
                // サイズを整えてから見せる（起動直後のちらつきを避ける）
                let _ = window.show();
            }
            Ok(())
        })
        .manage(StartupFile(Mutex::new(startup_file_from_args())))
        .manage(ArchiveCache(Mutex::new(None)))
        .manage(PendingPosition(Mutex::new(None)))
        .manage(AspectLock(Mutex::new(AspectState::default())))
        .manage(FolderWatcher(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
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
            delete_image,
            watch_folder
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
