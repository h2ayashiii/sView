use std::cmp::Ordering;
use std::path::Path;

/// 対応する画像拡張子（小文字で比較）
pub(crate) const IMAGE_EXTS: &[&str] = &[
    "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg", "png", "svg", "tif", "tiff", "webp",
];

/// 対応する動画拡張子（小文字で比較）。
/// デコードは OS の WebView に任せる（コーデックは同梱しない）ので、
/// どの OS の WebView でもおおむね再生できるコンテナだけに絞っている。
/// MKV は WebView2（Windows）ならそのまま再生できる。macOS の WKWebView は MKV を
/// 再生できないので、MKV のリマックス（mkv_open など）で fragmented MP4 に詰め直して流す
pub(crate) const VIDEO_EXTS: &[&str] = &["m4v", "mkv", "mov", "mp4", "webm"];

/// 対応する音楽拡張子（小文字で比較）。
/// 動画と同じく再生は OS の WebView に任せる（コーデックは同梱しない）
pub(crate) const AUDIO_EXTS: &[&str] = &["aac", "flac", "m4a", "mp3", "ogg", "opus", "wav"];

/// 音楽ファイルと同じフォルダに置かれたジャケット画像とみなすファイル名（拡張子を除く・小文字）。
/// 埋め込みのアートワークが無いときだけ使う
pub(crate) const COVER_STEMS: &[&str] = &["cover", "folder", "front", "album", "albumart"];

/// アートワークとして読む画像の上限（埋め込み・同じフォルダの画像とも）
pub(crate) const MAX_ARTWORK_BYTES: u64 = 64 * 1024 * 1024;

/// 対応する書庫（圧縮フォルダ）拡張子
pub(crate) const ARCHIVE_EXTS: &[&str] = &["cbz", "zip"];

/// フォルダ・書庫を開いたときに読む深さ。開いたフォルダ・書庫の直下を 1 層目とし、
/// その下 2 層（= 3 層目）まで。ファイルが多すぎる場所を開いても一覧が膨らみすぎないようにする
pub(crate) const MAX_DEPTH: usize = 3;

/// 展開後サイズの上限（zip bomb 対策 / 1枚あたり）
pub(crate) const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;

/// ファイルの種類。一覧には 1 種類だけを並べる
/// （単体のファイルを開いたらその種類、フォルダ・書庫なら並び順で先頭のファイルの種類）
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MediaKind {
    Image,
    Video,
    Audio,
}

/// ウィンドウの大きさと位置を種類ごとに覚えるときの区分。
/// ファイルの種類（MediaKind）に、画像を 2 枚並べる本モードを別枠として加えたもの
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum WindowKindKey {
    Image,
    Video,
    Audio,
    Book,
}

/// 開いたばかりのファイルの種類。本モードは開いたあとに切り替えるものなので含まない
impl From<MediaKind> for WindowKindKey {
    fn from(kind: MediaKind) -> Self {
        match kind {
            MediaKind::Image => WindowKindKey::Image,
            MediaKind::Video => WindowKindKey::Video,
            MediaKind::Audio => WindowKindKey::Audio,
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct ImageList {
    /// フォルダの場合は画像のフルパス、書庫の場合は書庫内のエントリ名
    pub(crate) images: Vec<String>,
    pub(crate) index: usize,
    /// 書庫を開いている場合はその書庫のフルパス
    pub(crate) archive: Option<String>,
    /// 監視対象のフォルダ（フォルダを開いた場合のみ。書庫では None）
    pub(crate) dir: Option<String>,
    /// 表示名から取り除く共通フォルダ
    /// （書庫では全エントリの共通フォルダ。例: "book/"。フォルダをサブフォルダごと
    /// 読んだときは開いたフォルダのパス。単体のファイルから開いたときは空）
    pub(crate) prefix: String,
    /// 並べている種類（一覧が空のときは None）
    pub(crate) kind: Option<MediaKind>,
    /// 読んだ深さ（単体のファイルから開いたときは 1、フォルダ・書庫は MAX_DEPTH）。
    /// 読み直しと監視で同じ深さを使うために返す
    pub(crate) depth: usize,
}

pub(crate) fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub(crate) fn is_image(path: &Path) -> bool {
    has_ext(path, IMAGE_EXTS)
}

pub(crate) fn is_video(path: &Path) -> bool {
    has_ext(path, VIDEO_EXTS)
}

pub(crate) fn is_audio(path: &Path) -> bool {
    has_ext(path, AUDIO_EXTS)
}

/// フォルダで一覧に並べるもの（画像・動画・音楽）。書庫の中の動画・音楽は対象外
pub(crate) fn is_media(path: &Path) -> bool {
    media_kind(path).is_some()
}

pub(crate) fn media_kind(path: &Path) -> Option<MediaKind> {
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

pub(crate) fn is_archive(path: &Path) -> bool {
    has_ext(path, ARCHIVE_EXTS)
}
/// 数値部分を数値として比較する自然順ソート（大文字小文字は無視）
/// 例: img2.png < img10.png
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
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
