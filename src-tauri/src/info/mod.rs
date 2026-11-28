// 表示・再生中のファイルの情報（ファイルの情報ウィンドウ）。
// 画像は解像度と Exif、動画はコンテナ・映像 / 音声 / 字幕トラックのコーデック、
// 音楽は音声の形式とタグを、ファイルの先頭やヘッダだけを読んで集める
// （映像・音声をデコードはしない）。

pub(crate) mod audio;
pub(crate) mod format;
pub(crate) mod image;
pub(crate) mod video;

use std::fs;
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

use crate::archive::{ArchiveCache, with_archive};
use crate::media::{MAX_ENTRY_BYTES, MediaKind, is_image, media_kind};
use format::{format_bytes, format_utc};

/// 情報ウィンドウのラベル（tauri.conf.json / capabilities/info.json と合わせる）
pub(crate) const INFO_LABEL: &str = "info";

/// 情報の 1 行（「解像度: 1920 × 1080」など）
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InfoRow {
    pub(crate) label: String,
    pub(crate) value: String,
    /// 日時の行なら UNIX 時刻（ミリ秒）。フロントエンドが利用者の地域の時刻で出し直す
    /// （value には UTC で書いたものを入れておく）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) time: Option<f64>,
}

/// 情報のまとまり（「ファイル」「映像」「音声 2」など）
#[derive(serde::Serialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct InfoSection {
    pub(crate) title: String,
    pub(crate) rows: Vec<InfoRow>,
}

impl InfoSection {
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
        }
    }

    /// 1 行足す。値が空なら足さない
    pub(crate) fn push(&mut self, label: &str, value: impl Into<String>) {
        let value = value.into();
        let value = value.trim();
        if !value.is_empty() {
            self.rows.push(InfoRow {
                label: label.into(),
                value: value.into(),
                time: None,
            });
        }
    }

    /// 値があるときだけ 1 行足す
    pub(crate) fn opt(&mut self, label: &str, value: Option<impl Into<String>>) {
        if let Some(value) = value {
            self.push(label, value);
        }
    }

    /// 日時の行を足す（UNIX 時刻・ミリ秒）
    pub(crate) fn time(&mut self, label: &str, unix_ms: f64) {
        if !unix_ms.is_finite() {
            return;
        }
        self.rows.push(InfoRow {
            label: label.into(),
            value: format_utc(unix_ms),
            time: Some(unix_ms),
        });
    }

    pub(crate) fn get(&self, label: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.label == label)
            .map(|r| r.value.as_str())
    }
}

/// ファイルの情報（情報ウィンドウに出すもの）
#[derive(serde::Serialize, Clone, Debug, Default)]
pub(crate) struct MediaInfo {
    /// 見出しに出すファイル名
    pub(crate) name: String,
    pub(crate) kind: Option<MediaKind>,
    pub(crate) sections: Vec<InfoSection>,
    /// 一部の情報を読めなかったときの理由（ファイルの情報は出せている）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warning: Option<String>,
}

/// SystemTime を UNIX 時刻（ミリ秒）にする
fn unix_ms(time: std::io::Result<std::time::SystemTime>) -> Option<f64> {
    let time = time.ok()?;
    Some(time.duration_since(UNIX_EPOCH).ok()?.as_secs_f64() * 1000.0)
}

/// 拡張子（大文字）。種類の欄の補足に使う
fn ext_upper(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_uppercase())
}

/// 普通のファイルの「ファイル」欄
fn file_section(path: &Path) -> Result<InfoSection, String> {
    let meta = fs::metadata(path).map_err(|e| format!("ファイルを読めません: {e}"))?;
    let mut section = InfoSection::new("ファイル");
    section.opt(
        "名前",
        path.file_name().map(|n| n.to_string_lossy().into_owned()),
    );
    section.opt(
        "場所",
        path.parent().map(|p| p.to_string_lossy().into_owned()),
    );
    section.push("サイズ", format_bytes(meta.len()));
    if let Some(ms) = unix_ms(meta.modified()) {
        section.time("更新日時", ms);
    }
    if let Some(ms) = unix_ms(meta.created()) {
        section.time("作成日時", ms);
    }
    Ok(section)
}

/// 書庫の中の画像の「ファイル」欄と中身。
/// 情報を読むのに中身が要るので、ここで取り出して返す
fn archive_entry(
    archive: &Path,
    entry: &str,
    cache: &ArchiveCache,
) -> Result<(InfoSection, Vec<u8>), String> {
    if !is_image(Path::new(entry)) {
        return Err(format!("対応していないファイル形式です: {entry}"));
    }
    with_archive(archive, cache, |zip| {
        let mut file = zip
            .by_name(entry)
            .map_err(|e| format!("書庫内のファイルを開けません ({entry}): {e}"))?;
        let size = file.size();
        if size > MAX_ENTRY_BYTES {
            return Err(format!("ファイルが大きすぎます ({entry})"));
        }
        let mut section = InfoSection::new("ファイル");
        let name = entry.rsplit('/').next().unwrap_or(entry);
        section.push("名前", name);
        if name != entry {
            section.push("書庫内の場所", entry);
        }
        section.push("書庫", archive.to_string_lossy());
        section.push("サイズ", format_bytes(size));
        section.push("圧縮後のサイズ", format_bytes(file.compressed_size()));
        // zip の日時はタイムゾーンを持たない（書庫を作った PC の時刻）ので、そのまま出す
        if let Some(t) = file.last_modified() {
            section.push(
                "更新日時",
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    t.year(),
                    t.month(),
                    t.day(),
                    t.hour(),
                    t.minute(),
                    t.second()
                ),
            );
        }
        let mut buf = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buf)
            .map_err(|e| format!("書庫内のファイルを読めません ({entry}): {e}"))?;
        Ok((section, buf))
    })
}

/// 中身を読んで種類ごとの欄を作る（読めなくてもファイルの欄は出す）
fn media_sections<R: BufRead + Seek>(
    kind: MediaKind,
    path: &Path,
    reader: impl FnOnce() -> std::io::Result<R>,
) -> Result<Vec<InfoSection>, String> {
    match kind {
        MediaKind::Image => {
            let mut r = reader().map_err(|e| format!("ファイルを読めません: {e}"))?;
            Ok(image::image_sections(&mut r, path))
        }
        MediaKind::Video => video::video_sections(path),
        MediaKind::Audio => audio::audio_sections(path),
    }
}

/// ファイルの情報を集める。`entry` があれば `path` は書庫で、その中の画像の情報
pub(crate) fn collect_media_info(
    path: &Path,
    entry: Option<&str>,
    cache: &ArchiveCache,
) -> Result<MediaInfo, String> {
    let mut info = MediaInfo::default();
    let result = if let Some(entry) = entry {
        let (file, bytes) = archive_entry(path, entry, cache)?;
        info.name = entry.rsplit('/').next().unwrap_or(entry).to_string();
        info.kind = Some(MediaKind::Image);
        info.sections.push(file);
        let entry_path = Path::new(entry);
        media_sections(MediaKind::Image, entry_path, || {
            Ok(std::io::Cursor::new(bytes))
        })
    } else {
        info.sections.push(file_section(path)?);
        info.name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(kind) = media_kind(path) else {
            return Ok(info);
        };
        info.kind = Some(kind);
        media_sections(kind, path, || {
            Ok(std::io::BufReader::new(fs::File::open(path)?))
        })
    };
    match result {
        Ok(sections) => info.sections.extend(sections),
        Err(e) => {
            log::warn!("ファイルの情報を読めません ({}): {e}", path.display());
            info.warning = Some(e);
        }
    }
    Ok(info)
}

/// ファイルの情報を返す（情報ウィンドウから呼ぶ）。
/// ファイルのヘッダを読むので async にして、メインスレッドを止めない
#[tauri::command(async)]
pub(crate) fn media_info(
    path: String,
    entry: Option<String>,
    cache: State<ArchiveCache>,
) -> Result<MediaInfo, String> {
    collect_media_info(&PathBuf::from(path), entry.as_deref(), &cache)
}

// ---- 情報ウィンドウ ----

/// 情報ウィンドウに出すファイル（ビューアが表示中のもの）
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct InfoTarget {
    pub(crate) path: String,
    pub(crate) entry: Option<String>,
}

/// 情報ウィンドウがいま出しているファイル（読み込み前のウィンドウは get_info_target で受け取る）
pub(crate) struct InfoTargetState(pub(crate) Mutex<Option<InfoTarget>>);

fn set_target(app: &AppHandle, target: Option<InfoTarget>) {
    if let Ok(mut current) = app.state::<InfoTargetState>().0.lock() {
        *current = target.clone();
    }
    let _ = app.emit_to(INFO_LABEL, "info-target", target);
}

/// 情報ウィンドウを開いて、表示中のファイルの情報を出す。
/// `toggle` なら、すでに開いているときは閉じる（ショートカットで開け閉めする）。
/// 設定ウィンドウと同じく、起動時に非表示で作ったものを表示して前面に出すだけ
#[tauri::command]
pub(crate) fn open_info_window(
    app: AppHandle,
    path: Option<String>,
    entry: Option<String>,
    toggle: Option<bool>,
) -> Result<(), String> {
    let window = app
        .get_webview_window(INFO_LABEL)
        .ok_or("ファイルの情報のウィンドウがありません")?;
    if toggle.unwrap_or(false) && window.is_visible().unwrap_or(false) {
        return window
            .hide()
            .map_err(|e| format!("ファイルの情報を閉じられません: {e}"));
    }
    set_target(&app, path.map(|path| InfoTarget { path, entry }));
    window
        .show()
        .map_err(|e| format!("ファイルの情報を表示できません: {e}"))?;
    window
        .set_focus()
        .map_err(|e| format!("ファイルの情報を前面にできません: {e}"))
}

/// ビューアで表示するファイルが変わったら、情報ウィンドウが開いていればその情報に差し替える。
/// 前面のビューア（macOS で複数開いているときは操作中のもの）からの知らせだけを受ける
#[tauri::command]
pub(crate) fn sync_info_window(
    app: AppHandle,
    window: WebviewWindow,
    path: Option<String>,
    entry: Option<String>,
) {
    let Some(info) = app.get_webview_window(INFO_LABEL) else {
        return;
    };
    if !info.is_visible().unwrap_or(false) || !window.is_focused().unwrap_or(false) {
        return;
    }
    let target = path.map(|path| InfoTarget { path, entry });
    let same = app
        .state::<InfoTargetState>()
        .0
        .lock()
        .map(|current| *current == target)
        .unwrap_or(false);
    if !same {
        set_target(&app, target);
    }
}

/// 情報ウィンドウが読み込み時に、出すファイルを受け取る
#[tauri::command]
pub(crate) fn get_info_target(state: State<InfoTargetState>) -> Option<InfoTarget> {
    state.0.lock().ok().and_then(|t| t.clone())
}
