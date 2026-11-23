use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::Path;

use crate::media::{has_ext, is_video};

// ---- MKV のテキスト字幕 ----
// WebView（Chromium）は MKV の中の字幕を textTracks に出さないので、ここで取り出して
// フロントへ渡し、フロントが VTTCue にして表示する。対象は SRT（S_TEXT/UTF8）と
// ASS / SSA のテキスト字幕だけ（PGS・VobSub などの画像字幕は扱わない）。
// トラックの情報（名前・言語・圧縮）は matroska-demuxer で読み、ブロックは自前で走査する。
// matroska-demuxer の next_frame はすべてのフレーム（映像・音声）の中身を読むので、
// 数 GB の動画では丸ごと読むことになる。自前の走査なら字幕以外のブロックは読み飛ばせる

/// 字幕 1 件（時刻は秒）
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct SubtitleCue {
    pub(crate) start: f64,
    pub(crate) end: f64,
    pub(crate) text: String,
}

/// 字幕トラック 1 本
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct SubtitleTrack {
    pub(crate) name: Option<String>,
    pub(crate) language: Option<String>,
    /// 動画で既定とされている字幕か（開いたときに表示する）
    pub(crate) default: bool,
    pub(crate) cues: Vec<SubtitleCue>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SubtitleFormat {
    Srt,
    Ass,
}

pub(crate) fn subtitle_format(codec_id: &str) -> Option<SubtitleFormat> {
    match codec_id {
        "S_TEXT/UTF8" => Some(SubtitleFormat::Srt),
        "S_TEXT/ASS" | "S_TEXT/SSA" | "S_ASS" | "S_SSA" => Some(SubtitleFormat::Ass),
        _ => None,
    }
}

/// ブロックの中身に掛かっている符号化（ContentEncoding）を戻す手順（字幕と MKV のリマックスで使う）
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BlockDecode {
    Zlib,
    /// 各フレームの先頭から取り除かれているバイト列（header stripping）
    Strip(Vec<u8>),
}

/// 字幕 1 件の中身として読む上限（壊れたファイルで巨大な領域を確保しない）
pub(crate) const MAX_SUBTITLE_BLOCK_BYTES: u64 = 1024 * 1024;

/// 符号化の手順（戻す順）。暗号化・未対応の圧縮なら None（そのトラックは読まない）
pub(crate) fn block_decoders(track: &matroska_demuxer::TrackEntry) -> Option<Vec<BlockDecode>> {
    use matroska_demuxer::{ContentCompAlgo, ContentEncodingValue};
    let mut encodings: Vec<_> = track.content_encodings().unwrap_or(&[]).iter().collect();
    // 戻すときは order の大きいものから
    encodings.sort_by_key(|e| std::cmp::Reverse(e.order()));
    let mut out = Vec::new();
    for encoding in encodings {
        // scope の 1 ビット目が立っているものだけがフレームに掛かっている
        if encoding.scope() & 1 == 0 {
            continue;
        }
        match encoding.encoding() {
            ContentEncodingValue::Compression(c) => match c.algo() {
                ContentCompAlgo::Zlib => out.push(BlockDecode::Zlib),
                ContentCompAlgo::Stripping => {
                    out.push(BlockDecode::Strip(c.settings().unwrap_or(&[]).to_vec()))
                }
                _ => return None,
            },
            _ => return None,
        }
    }
    Some(out)
}

pub(crate) fn decode_subtitle_block(data: Vec<u8>, decoders: &[BlockDecode]) -> Option<Vec<u8>> {
    decode_block(data, decoders, MAX_SUBTITLE_BLOCK_BYTES)
}

/// ブロック（フレーム）の符号化を戻す。zlib は展開後 `limit` バイトまでしか読まない
pub(crate) fn decode_block(
    mut data: Vec<u8>,
    decoders: &[BlockDecode],
    limit: u64,
) -> Option<Vec<u8>> {
    for decoder in decoders {
        data = match decoder {
            BlockDecode::Zlib => {
                let mut out = Vec::new();
                flate2::read::ZlibDecoder::new(data.as_slice())
                    .take(limit)
                    .read_to_end(&mut out)
                    .ok()?;
                out
            }
            BlockDecode::Strip(prefix) => [prefix.as_slice(), &data].concat(),
        };
    }
    Some(data)
}

/// ASS の上書きタグ（`{\b1}` など）を外し、改行を直す。
/// 描画命令（`{\p1}` 〜 `{\p0}`）は文字ではないので捨てる
pub(crate) fn clean_ass_text(text: &str) -> String {
    let mut out = String::new();
    let mut drawing = false;
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        if !drawing {
            out.push_str(&rest[..open]);
        }
        let Some(close) = rest[open..].find('}') else {
            rest = &rest[open..];
            break;
        };
        let tags = &rest[open + 1..open + close];
        // \p の後の数字が 0 以外なら描画の始まり、0 なら終わり（\pos などは対象外）
        for (i, _) in tags.match_indices("\\p") {
            let digits: String = tags[i + 2..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                drawing = !digits.trim_start_matches('0').is_empty();
            }
        }
        rest = &rest[open + close + 1..];
    }
    if !drawing {
        out.push_str(rest);
    }
    out.replace("\\N", "\n")
        .replace("\\n", "\n")
        .replace("\\h", "\u{a0}")
}

/// WebVTT の字幕文として解釈されないよう、記号を文字参照にする
pub(crate) fn escape_cue_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// ブロックの中身から表示する文字を取り出す（空なら None）
pub(crate) fn subtitle_text(data: &[u8], format: SubtitleFormat) -> Option<String> {
    let raw = String::from_utf8_lossy(data);
    let raw = raw.trim_end_matches(['\0', '\r', '\n']);
    let text = match format {
        // ReadOrder, Layer, Style, Name, MarginL, MarginR, MarginV, Effect, Text
        SubtitleFormat::Ass => escape_cue_text(&clean_ass_text(raw.splitn(9, ',').nth(8)?)),
        // SRT の <i> <b> <u> は WebVTT でもそのまま使える。
        // {\an8} などの ASS 由来の位置指定だけ外す
        SubtitleFormat::Srt => {
            let mut s = String::new();
            let mut rest = raw;
            while let Some(open) = rest.find("{\\") {
                s.push_str(&rest[..open]);
                match rest[open..].find('}') {
                    Some(close) => rest = &rest[open + close + 1..],
                    None => {
                        rest = &rest[open..];
                        break;
                    }
                }
            }
            s.push_str(rest);
            s.replace("\r\n", "\n")
        }
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

// 走査に使う Matroska の要素 ID
pub(crate) const MKV_CLUSTER: u64 = 0x1F43_B675;
pub(crate) const MKV_TIMESTAMP: u64 = 0xE7;
pub(crate) const MKV_BLOCK_GROUP: u64 = 0xA0;
pub(crate) const MKV_BLOCK: u64 = 0xA1;
pub(crate) const MKV_SIMPLE_BLOCK: u64 = 0xA3;
pub(crate) const MKV_BLOCK_DURATION: u64 = 0x9B;

/// EBML の可変長整数を読む。`keep_marker` は要素 ID（先頭の印のビットも値に含める）。
/// 戻り値は (値, バイト数)。サイズで全ビットが 1 のもの（長さ不明）は None
pub(crate) fn read_ebml_vint<R: Read>(
    r: &mut R,
    keep_marker: bool,
) -> std::io::Result<(Option<u64>, u64)> {
    let mut first = [0u8; 1];
    r.read_exact(&mut first)?;
    let len = first[0].leading_zeros() as usize + 1;
    if len > 8 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "EBML の整数が壊れています",
        ));
    }
    let mut value = if keep_marker {
        first[0] as u64
    } else {
        (first[0] as u64) & (0xFF >> len)
    };
    let mut all_ones = value == (0xFF >> len);
    let mut rest = [0u8; 7];
    r.read_exact(&mut rest[..len - 1])?;
    for &b in &rest[..len - 1] {
        value = (value << 8) | b as u64;
        all_ones &= b == 0xFF;
    }
    let unknown = !keep_marker && all_ones;
    Ok(((!unknown).then_some(value), len as u64))
}

pub(crate) fn read_ebml_uint<R: Read>(r: &mut R, size: u64) -> std::io::Result<u64> {
    if size > 8 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "EBML の整数が長すぎます",
        ));
    }
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf[..size as usize])?;
    Ok(buf[..size as usize]
        .iter()
        .fold(0, |v, &b| (v << 8) | b as u64))
}

/// 字幕として拾ったブロック（時刻は Matroska の単位）
pub(crate) struct RawSubtitle {
    pub(crate) track: u64,
    pub(crate) timestamp: i64,
    pub(crate) duration: Option<u64>,
    pub(crate) data: Vec<u8>,
}

/// 最初の Cluster から末尾までを走査し、`tracks` のトラック番号のブロックだけを読む。
/// 途中で壊れていたら（ファイルが途中で切れているなど）、そこまでに読めた分を返す
pub(crate) fn scan_subtitle_blocks<R: BufRead + Seek>(
    r: &mut R,
    tracks: &[u64],
) -> Vec<RawSubtitle> {
    let mut out = Vec::new();
    let _ = scan_subtitle_blocks_inner(r, tracks, &mut out);
    out
}

pub(crate) fn scan_subtitle_blocks_inner<R: BufRead + Seek>(
    r: &mut R,
    tracks: &[u64],
    out: &mut Vec<RawSubtitle>,
) -> std::io::Result<()> {
    let mut cluster_timestamp: u64 = 0;
    // BlockGroup の終わりの位置と、その中の Block（BlockDuration を待っている）
    let mut group_end: Option<u64> = None;
    let mut pending: Option<RawSubtitle> = None;
    loop {
        if let Some(end) = group_end
            && r.stream_position()? >= end
        {
            group_end = None;
            out.extend(pending.take());
        }
        let (Some(id), _) = (match read_ebml_vint(r, true) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }) else {
            break;
        };
        let (size, _) = read_ebml_vint(r, false)?;
        match id {
            // 中へ入る（長さ不明の Cluster もそのまま続けて読める）
            MKV_CLUSTER => {}
            MKV_BLOCK_GROUP => {
                out.extend(pending.take());
                group_end = match size {
                    Some(size) => Some(r.stream_position()? + size),
                    None => None,
                };
            }
            MKV_TIMESTAMP => cluster_timestamp = read_ebml_uint(r, size.unwrap_or(0))?,
            MKV_BLOCK | MKV_SIMPLE_BLOCK => {
                let Some(size) = size else { break };
                let (track, track_len) = read_ebml_vint(r, false)?;
                let mut header = [0u8; 3];
                r.read_exact(&mut header)?;
                let body = size.saturating_sub(track_len + 3);
                let relative = i16::from_be_bytes([header[0], header[1]]) as i64;
                // レーシング（1 ブロックに複数フレーム）は字幕では使われないので読まない
                let laced = header[2] & 0x06 != 0;
                let wanted = track.is_some_and(|t| tracks.contains(&t));
                if wanted && !laced && body <= MAX_SUBTITLE_BLOCK_BYTES {
                    let mut data = vec![0u8; body as usize];
                    r.read_exact(&mut data)?;
                    let raw = RawSubtitle {
                        track: track.unwrap_or(0),
                        timestamp: cluster_timestamp as i64 + relative,
                        duration: None,
                        data,
                    };
                    if id == MKV_BLOCK && group_end.is_some() {
                        out.extend(pending.replace(raw));
                    } else {
                        out.push(raw);
                    }
                } else {
                    r.seek_relative(body as i64)?;
                }
            }
            MKV_BLOCK_DURATION => {
                let duration = read_ebml_uint(r, size.unwrap_or(0))?;
                if let Some(raw) = pending.as_mut() {
                    raw.duration = Some(duration);
                }
            }
            // ほかの要素（映像・音声の情報、Cues、Tags など）は読み飛ばす
            _ => match size {
                Some(size) => r.seek_relative(size as i64)?,
                None => break,
            },
        }
    }
    out.extend(pending.take());
    Ok(())
}

/// 字幕が終わる時刻が無いものは、次の字幕まで（最後なら 5 秒）表示する
pub(crate) const SUBTITLE_FALLBACK_S: f64 = 5.0;

/// MKV からテキスト字幕のトラックをすべて取り出す（字幕が無ければ空）
pub(crate) fn read_mkv_subtitles(path: &Path) -> Result<Vec<SubtitleTrack>, String> {
    let file = File::open(path).map_err(|e| format!("動画を開けません: {e}"))?;
    let mut reader = BufReader::new(file);
    let mkv = matroska_demuxer::MatroskaFile::open(&mut reader)
        .map_err(|e| format!("MKV を読めません: {e}"))?;
    // 1 単位あたりのナノ秒
    let scale = mkv.info().timestamp_scale().get() as f64;
    struct Wanted {
        number: u64,
        format: SubtitleFormat,
        decoders: Vec<BlockDecode>,
        default_duration: Option<f64>,
        track: SubtitleTrack,
    }
    let mut wanted: Vec<Wanted> = mkv
        .tracks()
        .iter()
        .filter(|t| t.track_type() == matroska_demuxer::TrackType::Subtitle)
        .filter_map(|t| {
            Some(Wanted {
                number: t.track_number().get(),
                format: subtitle_format(t.codec_id())?,
                decoders: block_decoders(t)?,
                default_duration: t.default_duration().map(|d| d.get() as f64 / 1e9),
                track: SubtitleTrack {
                    name: t.name().map(str::to_string),
                    language: t
                        .language_bcp47()
                        .or(t.language())
                        .filter(|l| *l != "und")
                        .map(str::to_string),
                    default: t.flag_default(),
                    cues: Vec::new(),
                },
            })
        })
        .collect();
    // open() のあと、読む位置は最初の Cluster の頭にある
    drop(mkv);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let numbers: Vec<u64> = wanted.iter().map(|w| w.number).collect();
    for raw in scan_subtitle_blocks(&mut reader, &numbers) {
        let Some(w) = wanted.iter_mut().find(|w| w.number == raw.track) else {
            continue;
        };
        let Some(data) = decode_subtitle_block(raw.data, &w.decoders) else {
            continue;
        };
        let Some(text) = subtitle_text(&data, w.format) else {
            continue;
        };
        let start = (raw.timestamp as f64 * scale / 1e9).max(0.0);
        let end = raw
            .duration
            .map(|d| start + d as f64 * scale / 1e9)
            .or(w.default_duration.map(|d| start + d));
        w.track.cues.push(SubtitleCue {
            start,
            // 終わりが分からないものは後で埋める（NaN にしておく）
            end: end.unwrap_or(f64::NAN),
            text,
        });
    }
    Ok(wanted
        .into_iter()
        .map(|mut w| {
            let cues = &mut w.track.cues;
            cues.sort_by(|a, b| a.start.total_cmp(&b.start));
            for i in 0..cues.len() {
                if cues[i].end.is_nan() {
                    let next = cues.get(i + 1).map(|c| c.start);
                    let start = cues[i].start;
                    cues[i].end = next
                        .filter(|n| *n > start)
                        .unwrap_or(start + SUBTITLE_FALLBACK_S)
                        .min(start + SUBTITLE_FALLBACK_S);
                }
            }
            w.track
        })
        .collect())
}

/// 動画に入っているテキスト字幕を返す。MKV だけが対象で、それ以外の動画では空
#[tauri::command(async)]
pub(crate) fn video_subtitles(path: String) -> Result<Vec<SubtitleTrack>, String> {
    let path = Path::new(&path);
    if !(has_ext(path, &["mkv"]) && is_video(path)) {
        return Ok(Vec::new());
    }
    read_mkv_subtitles(path)
}
