use std::fs;
use std::path::{Path, PathBuf};

use tauri::ipc::Response;

use crate::media::{has_ext, is_audio, is_image, COVER_STEMS, MAX_ARTWORK_BYTES};

/// 音楽ファイルのタグ情報（表示用）。読めない項目は None
#[derive(serde::Serialize, Default)]
pub(crate) struct AudioInfo {
    pub(crate) title: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) album: Option<String>,
}

/// 音楽ファイルのタグを読む。音声の長さなどは要らないので、タグだけを読む。
/// 形式は拡張子ではなく中身から判定する（拡張子と中身が食い違うファイルもあるため）
pub(crate) fn read_audio_tags(path: &Path) -> Option<lofty::file::TaggedFile> {
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
pub(crate) fn non_empty(text: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// 音楽ファイルの曲名・アーティスト・アルバム名を返す。
/// タグが無い・読めないときもエラーにはせず、空の情報を返す（再生はできるため）
/// 重い処理（フォルダ・書庫・タグの読み込み）なので async にして、メインスレッド
/// （ウィンドウの描画・移動・入力）を止めないようにする
#[tauri::command(async)]
pub(crate) fn audio_info(path: String) -> Result<AudioInfo, String> {
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
pub(crate) fn embedded_artwork(path: &Path) -> Option<Vec<u8>> {
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
pub(crate) fn folder_artwork(path: &Path) -> Option<Vec<u8>> {
    let dir = path.parent()?;
    // 名前で絞ってから種類を問い合わせる（フォルダの全ファイルに問い合わせない）
    let mut candidates: Vec<(usize, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_image(p) && !has_ext(p, &["svg"]))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
            let rank = COVER_STEMS.iter().position(|s| *s == stem)?;
            Some((rank, p))
        })
        .filter(|(_, p)| p.is_file())
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
/// 重い処理（フォルダ・書庫・タグの読み込み）なので async にして、メインスレッド
/// （ウィンドウの描画・移動・入力）を止めないようにする
#[tauri::command(async)]
pub(crate) fn audio_artwork(path: String) -> Result<Response, String> {
    let target = PathBuf::from(&path);
    if !target.is_file() || !is_audio(&target) {
        return Err(format!("音楽ファイルではありません: {path}"));
    }
    let bytes = embedded_artwork(&target)
        .or_else(|| folder_artwork(&target))
        .unwrap_or_default();
    Ok(Response::new(bytes))
}
