pub(crate) mod av1;
pub(crate) mod fmp4;
pub(crate) mod mp4;
pub(crate) mod remux;
pub(crate) mod subtitles;
#[cfg(target_os = "macos")]
pub(crate) mod vt;

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::Ordering as AtomicOrdering;

use tauri::State;
use tauri::ipc::Response;

use crate::state::PerWindow;
#[cfg(test)]
pub(crate) use av1::*;
#[cfg(test)]
pub(crate) use fmp4::*;
#[cfg(test)]
pub(crate) use mp4::*;
#[cfg(test)]
pub(crate) use remux::*;
pub(crate) use remux::{MkvOpened, MkvStream};
#[cfg(test)]
pub(crate) use subtitles::*;

// ---- 動画ストリーム（MKV リマックス・macOS AV1 変換）のコマンド ----

/// ウィンドウごとに開いている動画ストリーム（1 ウィンドウ 1 本。開き直したら前のものは閉じる）
pub(crate) struct MkvStreams(pub(crate) PerWindow<Option<(u64, std::sync::Arc<Mutex<MkvStream>>)>>);

static NEXT_MKV_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl MkvStreams {
    pub(crate) fn get(
        &self,
        label: &str,
        id: u64,
    ) -> Result<std::sync::Arc<Mutex<MkvStream>>, String> {
        self.0
            .with(label, |s| {
                s.as_ref().filter(|(i, _)| *i == id).map(|(_, s)| s.clone())
            })
            .flatten()
            .ok_or_else(|| "動画はもう閉じています".to_string())
    }
}

/// この動画を Rust 側のストリーム経路（MKV リマックス / AV1 変換）で再生する必要があるかを返す。
/// - Windows 等では常に false（OS の WebView がそのまま再生する）
/// - macOS の M3 以降（AV1 ハードウェアデコード対応）では MKV 以外 false（ファイル I/O なし）
/// - macOS の M1 / M2 世代など非対応機ではコンテナヘッダだけを高速に覗き、AV1 なら true を返す
#[tauri::command(async)]
pub(crate) fn video_needs_stream(path: String) -> bool {
    #[cfg(target_os = "macos")]
    {
        let p = Path::new(&path);
        if crate::media::has_ext(p, &["mkv"]) {
            return true;
        }
        if av1::hw_av1_decode_supported() {
            return false;
        }
        remux::is_av1_video(p)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

/// 動画を開いて fMP4 ストリーム（リマックスまたは AV1 変換）を始める。
/// `audio` は鳴らす音声の番号（None なら既定の音声、負なら音声なし）。
/// `transcode` は AV1 の強制ソフトウェア変換指定（None なら環境に応じて自動判定）
#[tauri::command(async)]
pub(crate) fn mkv_open(
    window: tauri::Window,
    streams: State<MkvStreams>,
    path: String,
    audio: Option<i64>,
    transcode: Option<bool>,
) -> Result<MkvOpened, String> {
    let (stream, mut opened) = MkvStream::open_with_options(Path::new(&path), audio, transcode)?;
    opened.id = NEXT_MKV_ID.fetch_add(1, AtomicOrdering::Relaxed);
    let entry = (opened.id, std::sync::Arc::new(Mutex::new(stream)));
    streams.0.with(window.label(), |s| *s = Some(entry));
    Ok(opened)
}

/// 初期化セグメント（ftyp + moov）
#[tauri::command(async)]
pub(crate) fn mkv_init(
    window: tauri::Window,
    streams: State<MkvStreams>,
    id: u64,
) -> Result<Response, String> {
    let stream = streams.get(window.label(), id)?;
    let stream = stream.lock().map_err(|_| "動画を読めません".to_string())?;
    Ok(Response::new(stream.init_segment()))
}

/// `time` 秒の直前のキーフレームから読むようにする。戻り値はそのキーフレームの時刻（秒）。
/// フロントエンド（mkv-player.js）は mkv_read / mkv_seek を同時に 1 つしか出さない
#[tauri::command(async)]
pub(crate) fn mkv_seek(
    window: tauri::Window,
    streams: State<MkvStreams>,
    id: u64,
    time: f64,
) -> Result<f64, String> {
    let stream = streams.get(window.label(), id)?;
    let mut stream = stream.lock().map_err(|_| "動画を読めません".to_string())?;
    stream
        .seek(time)
        .map_err(|e| format!("動画を読めません: {e}"))
}

/// 次の断片（moof + mdat）。終わりまで読んだら空
#[tauri::command(async)]
pub(crate) fn mkv_read(
    window: tauri::Window,
    streams: State<MkvStreams>,
    id: u64,
) -> Result<Response, String> {
    let stream = streams.get(window.label(), id)?;
    let mut stream = stream.lock().map_err(|_| "動画を読めません".to_string())?;
    stream
        .read_fragment()
        .map(Response::new)
        .map_err(|e| format!("動画を読めません: {e}"))
}

/// MKV を閉じる（ファイルを手放す）
#[tauri::command]
pub(crate) fn mkv_close(window: tauri::Window, streams: State<MkvStreams>, id: u64) {
    streams.0.with(window.label(), |s| {
        if s.as_ref().is_some_and(|(i, _)| *i == id) {
            *s = None;
        }
    });
}
