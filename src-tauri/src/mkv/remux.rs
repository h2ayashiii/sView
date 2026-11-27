#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::Path;

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
use super::av1::{Av1Transcoder, EncoderFactory};
use super::av1::{
    ColorInfo, av1_codec_string, av1c_from_obu_stream, can_transcode_av1, hw_av1_decode_supported,
};
use super::fmp4::{Sample, TrackRun, build_fragment, build_init_segment};
use super::mp4::{Mp4Source, is_mp4_av1, looks_like_mp4};
use super::subtitles::{
    BlockDecode, MKV_BLOCK, MKV_BLOCK_GROUP, MKV_CLUSTER, MKV_SIMPLE_BLOCK, MKV_TIMESTAMP,
    block_decoders, decode_block, read_ebml_uint, read_ebml_vint,
};

// ---- MKV / WebM / MP4 のリマックスと AV1 変換（macOS） ----
// macOS の WKWebView は MKV コンテナを再生できず、M1 / M2 世代では AV1 も再生できない。
// - MKV（H.264 / HEVC、および M3 以降の AV1）は再エンコードせず fragmented MP4 へ詰め直す。
// - AV1（MP4 / WebM / MKV）でハードウェアデコードできない環境では、rav1d でデコードして
//   VideoToolbox の H.264 ハードウェアエンコーダ（B フレームなし）でリアルタイム変換し、
//   音声はそのままリマックスして MSE（mkv-player.js）へ流す。
// 一時ファイルは作らず、断片は要求されるたびにファイルの続きから作る。
// Windows は WebView2 が MKV・AV1 ともそのまま再生できるので、この経路は使わない

/// 映像の時間の単位（1 秒あたり）。MKV の時刻（多くはミリ秒）をそのまま表せる細かさにする
pub(crate) const REMUX_VIDEO_TIMESCALE: u64 = 90_000;

/// 断片 1 つに入れる映像の長さの目安（Matroska の単位に直して使う）。
/// 断片はキーフレーム（GOP の頭）でだけ区切るので、実際は GOP の長さぶん延びる
pub(crate) const REMUX_FRAGMENT_SECONDS: f64 = 1.0;

/// AV1 変換の起動・シーク直後の最初の断片に入れる映像の長さ（秒）。
/// 最初の 1 断片だけ短く返すことで、シーク後の最初の描画と再生再開までの待ち時間を抑える
#[cfg_attr(
    not(any(target_os = "macos", all(test, target_os = "linux"))),
    allow(dead_code)
)]
pub(crate) const TRANSCODE_INITIAL_FRAGMENT_SECONDS: f64 = 0.25;

/// AV1 変換時に 2 断片目以降に入れる映像の長さの目安（秒）。
/// 出力側でキーフレームを打てるため、元動画の GOP が長くても 1 秒ごとに細切れにして応答を速く保つ
#[cfg_attr(
    not(any(target_os = "macos", all(test, target_os = "linux"))),
    allow(dead_code)
)]
pub(crate) const TRANSCODE_FRAGMENT_SECONDS: f64 = 1.0;

/// AV1 変換時に 1 断片へ入れる映像フレーム数の上限（高フレームレートでもメモリと遅延を抑える）
#[cfg_attr(
    not(any(target_os = "macos", all(test, target_os = "linux"))),
    allow(dead_code)
)]
pub(crate) const TRANSCODE_FRAGMENT_MAX_FRAMES: usize = 120;

/// 断片の大きさの目安。超えたら次のキーフレームで区切る
pub(crate) const REMUX_FRAGMENT_SOFT_BYTES: usize = 16 * 1024 * 1024;

/// 断片の大きさの上限。GOP が極端に長いファイルでも、これを超えたらキーフレームを待たずに区切る
pub(crate) const REMUX_FRAGMENT_MAX_BYTES: usize = 48 * 1024 * 1024;

/// フレーム 1 つの上限（壊れたファイルで巨大な領域を確保しない）
pub(crate) const REMUX_MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;

/// 前の断片の続きとして音声の時刻をつなぐ範囲（秒）。これより離れていたら時刻を付け直す
pub(crate) const REMUX_AUDIO_RESYNC_SECONDS: f64 = 0.1;

/// B フレームの並べ替えの深さを見積もるのに読む映像のフレーム数の上限
pub(crate) const REMUX_REORDER_PROBE_FRAMES: usize = 600;

// リマックスで読む Matroska の要素 ID（字幕の走査と共通のものは上で定義している）
pub(crate) const MKV_EBML: u64 = 0x1A45_DFA3;
pub(crate) const MKV_SEGMENT: u64 = 0x1853_8067;
pub(crate) const MKV_SEEK_HEAD: u64 = 0x114D_9B74;
pub(crate) const MKV_SEEK: u64 = 0x4DBB;
pub(crate) const MKV_SEEK_ID: u64 = 0x53AB;
pub(crate) const MKV_SEEK_POSITION: u64 = 0x53AC;
pub(crate) const MKV_CUES: u64 = 0x1C53_BB6B;
pub(crate) const MKV_CUE_POINT: u64 = 0xBB;
pub(crate) const MKV_CUE_TIME: u64 = 0xB3;
pub(crate) const MKV_CUE_TRACK_POSITIONS: u64 = 0xB7;
pub(crate) const MKV_CUE_TRACK: u64 = 0xF7;
pub(crate) const MKV_CUE_CLUSTER_POSITION: u64 = 0xF1;
pub(crate) const MKV_REFERENCE_BLOCK: u64 = 0xFB;

pub(crate) fn invalid_data(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_string())
}

/// 要素の頭（ID とサイズ）を読む。サイズが不明（全ビット 1）なら None
pub(crate) fn read_element_header<R: Read>(r: &mut R) -> std::io::Result<(u64, Option<u64>)> {
    let (id, _) = read_ebml_vint(r, true)?;
    let id = id.ok_or_else(|| invalid_data("要素 ID が壊れています"))?;
    let (size, _) = read_ebml_vint(r, false)?;
    Ok((id, size))
}

/// バイト列の中の EBML 可変長整数（レーシングのサイズ）を読む。戻り値は (値, バイト数)
pub(crate) fn slice_vint(data: &[u8]) -> Option<(u64, usize)> {
    let first = *data.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || data.len() < len {
        return None;
    }
    let mut value = (first as u64) & (0xFF >> len);
    for &b in &data[1..len] {
        value = (value << 8) | b as u64;
    }
    Some((value, len))
}

/// レーシング（1 ブロックに複数フレーム）を分ける。`lacing` はフラグの 2 ビット
/// （1: Xiph、2: 固定長、3: EBML）。壊れていれば None
pub(crate) fn split_laced_frames(data: &[u8], lacing: u8) -> Option<Vec<Vec<u8>>> {
    let count = *data.first()? as usize + 1;
    let mut p = 1;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        1 => {
            for _ in 0..count - 1 {
                let mut size = 0usize;
                loop {
                    let b = *data.get(p)?;
                    p += 1;
                    size += b as usize;
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        2 => {
            let body = data.len() - 1;
            if !body.is_multiple_of(count) {
                return None;
            }
            sizes = vec![body / count; count - 1];
        }
        3 => {
            if count > 1 {
                let (first, len) = slice_vint(&data[p..])?;
                p += len;
                let mut size = first as i64;
                sizes.push(size as usize);
                for _ in 0..count - 2 {
                    let (raw, len) = slice_vint(&data[p..])?;
                    p += len;
                    // 符号付き: 値から (2^(7*len-1) - 1) を引く
                    let bias = (1i64 << (7 * len - 1)) - 1;
                    size += raw as i64 - bias;
                    if size < 0 {
                        return None;
                    }
                    sizes.push(size as usize);
                }
            }
        }
        _ => return None,
    }
    let used: usize = sizes.iter().sum();
    let rest = data.len().checked_sub(p)?.checked_sub(used)?;
    sizes.push(rest);
    let mut out = Vec::with_capacity(count);
    for size in sizes {
        out.push(data.get(p..p + size)?.to_vec());
        p += size;
    }
    Some(out)
}

/// MKV から取り出したフレーム 1 つ（時刻は Matroska の単位・表示順の時刻）
#[derive(Clone, Debug)]
pub(crate) struct MkvFrame {
    pub(crate) track: u64,
    pub(crate) timestamp: i64,
    pub(crate) keyframe: bool,
    pub(crate) data: Vec<u8>,
    /// このフレームのブロック（BlockGroup / SimpleBlock）の頭の位置と、その Cluster の時刻。
    /// シークでここから読み直すのに使う
    pub(crate) pos: u64,
    pub(crate) cluster_timestamp: i64,
}

/// Cluster を頭から順に読み、指定したトラックのフレームを 1 つずつ返す
pub(crate) struct MkvWalker<R> {
    r: R,
    cluster_timestamp: i64,
    queue: std::collections::VecDeque<MkvFrame>,
    /// 読むトラックと、レーシングされたフレームどうしの時刻の間隔（Matroska の単位）
    tracks: Vec<(u64, i64)>,
}

impl<R: BufRead + Seek> MkvWalker<R> {
    pub(crate) fn new(r: R, tracks: Vec<(u64, i64)>) -> Self {
        Self {
            r,
            cluster_timestamp: 0,
            queue: Default::default(),
            tracks,
        }
    }

    /// 要素の頭 `pos` から読み直す（`cluster_timestamp` はその位置が属する Cluster の時刻）
    pub(crate) fn seek_to(&mut self, pos: u64, cluster_timestamp: i64) -> std::io::Result<()> {
        self.r.seek(std::io::SeekFrom::Start(pos))?;
        self.cluster_timestamp = cluster_timestamp;
        self.queue.clear();
        Ok(())
    }

    /// `pos` へ移る。BufReader の seek は読み込み済みの分を捨てるので、近くなら seek_relative で動く
    pub(crate) fn move_to(&mut self, pos: u64) -> std::io::Result<()> {
        let here = self.r.stream_position()?;
        self.r.seek_relative(pos as i64 - here as i64)
    }

    /// 読み過ぎたフレームを戻す（次の next で最初に返る）
    pub(crate) fn push_back(&mut self, frame: MkvFrame) {
        self.queue.push_front(frame);
    }

    /// 次のフレーム。`with_data` が false なら中身は読まない（時刻とキーフレームかどうかだけ）。
    /// ファイルの終わり・壊れた所まで来たら None
    pub(crate) fn next(&mut self, with_data: bool) -> std::io::Result<Option<MkvFrame>> {
        match self.next_inner(with_data) {
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            other => other,
        }
    }

    pub(crate) fn next_inner(&mut self, with_data: bool) -> std::io::Result<Option<MkvFrame>> {
        loop {
            if let Some(frame) = self.queue.pop_front() {
                return Ok(Some(frame));
            }
            let pos = self.r.stream_position()?;
            let (id, size) = read_element_header(&mut self.r)?;
            match id {
                // 中へ入る（長さ不明の Cluster もそのまま続けて読める）
                MKV_CLUSTER => {}
                MKV_TIMESTAMP => {
                    self.cluster_timestamp = read_ebml_uint(&mut self.r, size.unwrap_or(0))? as i64
                }
                MKV_SIMPLE_BLOCK => {
                    let Some(size) = size else { return Ok(None) };
                    self.read_block(pos, size, None, with_data)?;
                }
                MKV_BLOCK_GROUP => {
                    let Some(size) = size else { return Ok(None) };
                    let end = self.r.stream_position()? + size;
                    let mut block = None;
                    let mut referenced = false;
                    while self.r.stream_position()? < end {
                        let (child, child_size) = read_element_header(&mut self.r)?;
                        let Some(child_size) = child_size else {
                            return Ok(None);
                        };
                        match child {
                            MKV_BLOCK => block = Some((self.r.stream_position()?, child_size)),
                            // 別のフレームを参照している = キーフレームではない
                            MKV_REFERENCE_BLOCK => referenced = true,
                            _ => {}
                        }
                        self.r.seek_relative(child_size as i64)?;
                    }
                    if let Some((start, block_size)) = block {
                        self.move_to(start)?;
                        self.read_block(pos, block_size, Some(!referenced), with_data)?;
                    }
                    self.move_to(end)?;
                }
                // ほかの要素（Cues、Tags など）は読み飛ばす
                _ => match size {
                    Some(size) => self.r.seek_relative(size as i64)?,
                    None => return Ok(None),
                },
            }
        }
    }

    /// Block / SimpleBlock の中身を読んで queue に積む（読まないトラックなら読み飛ばす）
    pub(crate) fn read_block(
        &mut self,
        pos: u64,
        size: u64,
        keyframe: Option<bool>,
        with_data: bool,
    ) -> std::io::Result<()> {
        let (track, track_len) = read_ebml_vint(&mut self.r, false)?;
        let Some(body) = size.checked_sub(track_len + 3) else {
            return self.r.seek_relative(size.saturating_sub(track_len) as i64);
        };
        let mut header = [0u8; 3];
        self.r.read_exact(&mut header)?;
        let wanted = track.and_then(|t| self.tracks.iter().find(|(n, _)| *n == t).copied());
        let Some((track, step)) = wanted.filter(|_| body <= REMUX_MAX_FRAME_BYTES) else {
            return self.r.seek_relative(body as i64);
        };
        let cluster_timestamp = self.cluster_timestamp;
        let timestamp = cluster_timestamp + i16::from_be_bytes([header[0], header[1]]) as i64;
        let keyframe = keyframe.unwrap_or(header[2] & 0x80 != 0);
        let lacing = (header[2] >> 1) & 3;
        let frame = |data: Vec<u8>, i: usize| MkvFrame {
            track,
            timestamp: timestamp + step * i as i64,
            keyframe,
            data,
            pos,
            cluster_timestamp,
        };
        if lacing == 0 {
            let data = if with_data {
                let mut data = vec![0u8; body as usize];
                self.r.read_exact(&mut data)?;
                data
            } else {
                self.r.seek_relative(body as i64)?;
                Vec::new()
            };
            self.queue.push_back(frame(data, 0));
            return Ok(());
        }
        // レーシングは中身を読まないと分けられない（音声で使われ、1 ブロックは小さい）
        let mut data = vec![0u8; body as usize];
        self.r.read_exact(&mut data)?;
        let frames: Vec<MkvFrame> = split_laced_frames(&data, lacing)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, data)| frame(if with_data { data } else { Vec::new() }, i))
            .collect();
        self.queue.extend(frames);
        Ok(())
    }
}

/// Segment の中の配置（最初の Cluster の位置と、映像トラックの Cues）
pub(crate) struct MkvLayout {
    pub(crate) first_cluster: u64,
    /// 映像のキーフレームの (時刻, Cluster の位置)。時刻順
    pub(crate) cues: Vec<(i64, u64)>,
}

/// EBML ヘッダと Segment の頭を読み、SeekHead から Cues を探して読む
pub(crate) fn read_mkv_layout<R: Read + Seek>(
    r: &mut R,
    video_track: u64,
) -> std::io::Result<MkvLayout> {
    use std::io::SeekFrom;
    r.seek(SeekFrom::Start(0))?;
    let (id, size) = read_element_header(r)?;
    if id != MKV_EBML {
        return Err(invalid_data("MKV ではありません"));
    }
    r.seek_relative(size.ok_or_else(|| invalid_data("EBML ヘッダが壊れています"))? as i64)?;
    let segment_start = loop {
        let (id, size) = read_element_header(r)?;
        if id == MKV_SEGMENT {
            break r.stream_position()?;
        }
        r.seek_relative(size.ok_or_else(|| invalid_data("Segment がありません"))? as i64)?;
    };
    let mut cues_pos = None;
    let first_cluster = loop {
        let pos = r.stream_position()?;
        let (id, size) = read_element_header(r)?;
        let body = r.stream_position()?;
        match id {
            MKV_CLUSTER => break pos,
            MKV_CUES => cues_pos = Some(pos),
            MKV_SEEK_HEAD => {
                let end = body + size.ok_or_else(|| invalid_data("SeekHead が壊れています"))?;
                while r.stream_position()? < end {
                    let (id, size) = read_element_header(r)?;
                    let size = size.ok_or_else(|| invalid_data("SeekHead が壊れています"))?;
                    if id != MKV_SEEK {
                        r.seek_relative(size as i64)?;
                        continue;
                    }
                    let seek_end = r.stream_position()? + size;
                    let (mut target, mut position) = (None, None);
                    while r.stream_position()? < seek_end {
                        let (id, size) = read_element_header(r)?;
                        let size = size.unwrap_or(0);
                        match id {
                            MKV_SEEK_ID | MKV_SEEK_POSITION => {
                                let value = read_ebml_uint(r, size)?;
                                if id == MKV_SEEK_ID {
                                    target = Some(value);
                                } else {
                                    position = Some(value);
                                }
                            }
                            _ => r.seek_relative(size as i64)?,
                        }
                    }
                    if target == Some(MKV_CUES) {
                        cues_pos = cues_pos.or(position.map(|p| segment_start + p));
                    }
                    r.seek(SeekFrom::Start(seek_end))?;
                }
            }
            _ => {}
        }
        let size = size.ok_or_else(|| invalid_data("Cluster が見つかりません"))?;
        r.seek(SeekFrom::Start(body + size))?;
    };
    // Cues が壊れていても再生はできる（シークが遅くなるだけ）ので、読めた分だけ使う
    let mut cues = Vec::new();
    if let Some(pos) = cues_pos {
        let _ = read_cues(r, pos, segment_start, video_track, &mut cues);
    }
    cues.sort_unstable();
    cues.dedup_by_key(|c| c.0);
    Ok(MkvLayout {
        first_cluster,
        cues,
    })
}

pub(crate) fn read_cues<R: Read + Seek>(
    r: &mut R,
    pos: u64,
    segment_start: u64,
    video_track: u64,
    out: &mut Vec<(i64, u64)>,
) -> std::io::Result<()> {
    r.seek(std::io::SeekFrom::Start(pos))?;
    let (id, size) = read_element_header(r)?;
    if id != MKV_CUES {
        return Err(invalid_data("Cues ではありません"));
    }
    let end = r.stream_position()? + size.ok_or_else(|| invalid_data("Cues が壊れています"))?;
    while r.stream_position()? < end {
        let (id, size) = read_element_header(r)?;
        let size = size.ok_or_else(|| invalid_data("Cues が壊れています"))?;
        if id != MKV_CUE_POINT {
            r.seek_relative(size as i64)?;
            continue;
        }
        let point_end = r.stream_position()? + size;
        let mut time = None;
        let mut clusters = Vec::new();
        while r.stream_position()? < point_end {
            let (id, size) = read_element_header(r)?;
            let size = size.ok_or_else(|| invalid_data("Cues が壊れています"))?;
            match id {
                MKV_CUE_TIME => time = Some(read_ebml_uint(r, size)?),
                MKV_CUE_TRACK_POSITIONS => {
                    let positions_end = r.stream_position()? + size;
                    let (mut track, mut cluster) = (None, None);
                    while r.stream_position()? < positions_end {
                        let (id, size) = read_element_header(r)?;
                        let size = size.unwrap_or(0);
                        match id {
                            MKV_CUE_TRACK => track = Some(read_ebml_uint(r, size)?),
                            MKV_CUE_CLUSTER_POSITION => cluster = Some(read_ebml_uint(r, size)?),
                            _ => r.seek_relative(size as i64)?,
                        }
                    }
                    if let (Some(track), Some(cluster)) = (track, cluster)
                        && track == video_track
                    {
                        clusters.push(segment_start + cluster);
                    }
                }
                _ => r.seek_relative(size as i64)?,
            }
        }
        if let (Some(time), Some(&cluster)) = (time, clusters.first()) {
            out.push((time as i64, cluster));
        }
        r.seek(std::io::SeekFrom::Start(point_end))?;
    }
    Ok(())
}

/// Cues の無いファイル用に、各 Cluster の (時刻, 位置) を集める（中身は読み飛ばす）。
/// 長さ不明の Cluster があればそこで打ち切る
pub(crate) fn scan_cluster_index<R: Read + Seek>(r: &mut R, first_cluster: u64) -> Vec<(i64, u64)> {
    let mut out = Vec::new();
    let _ = (|| -> std::io::Result<()> {
        r.seek(std::io::SeekFrom::Start(first_cluster))?;
        loop {
            let pos = r.stream_position()?;
            let (id, size) = read_element_header(r)?;
            let Some(size) = size else { return Ok(()) };
            let body = r.stream_position()?;
            if id == MKV_CLUSTER {
                // Timestamp は Cluster の最初の方にある
                while r.stream_position()? < body + size {
                    let (child, child_size) = read_element_header(r)?;
                    let child_size = child_size.unwrap_or(0);
                    if child == MKV_TIMESTAMP {
                        out.push((read_ebml_uint(r, child_size)? as i64, pos));
                        break;
                    }
                    if child == MKV_SIMPLE_BLOCK || child == MKV_BLOCK_GROUP {
                        break;
                    }
                    r.seek_relative(child_size as i64)?;
                }
            }
            r.seek(std::io::SeekFrom::Start(body + size))?;
        }
    })();
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RemuxVideoCodec {
    Avc,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RemuxAudioCodec {
    Aac,
    Opus,
}

/// リマックスする映像トラック
#[derive(Clone, Debug)]
pub(crate) struct RemuxVideo {
    pub(crate) number: u64,
    pub(crate) codec: RemuxVideoCodec,
    /// avcC / hvcC / av1C の中身（MKV の CodecPrivate や MP4 の設定ボックス）
    pub(crate) config: Vec<u8>,
    pub(crate) codec_string: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// 表示する大きさ（画素の縦横比が 1:1 でない動画は pasp で伝える）
    pub(crate) display: (u32, u32),
    /// 色空間情報（AV1 のリアルタイム変換時に colr ボックスへ書き出す）
    pub(crate) color: Option<ColorInfo>,
    pub(crate) decoders: Vec<BlockDecode>,
    /// 1 フレームの長さ（Matroska の単位または 90 kHz。分からなければ 0）
    pub(crate) frame_ticks: i64,
}

/// リマックスする音声トラック
#[derive(Clone, Debug)]
pub(crate) struct RemuxAudio {
    pub(crate) number: u64,
    pub(crate) codec: RemuxAudioCodec,
    /// AAC は AudioSpecificConfig、Opus は dOps の中身
    pub(crate) config: Vec<u8>,
    pub(crate) codec_string: String,
    pub(crate) channels: u16,
    /// 音声の時間の単位（サンプリング周波数）
    pub(crate) timescale: u32,
    /// 1 フレームのサンプル数（AAC。Opus はフレームごとに TOC から求める）
    pub(crate) frame_samples: u32,
    /// デコーダの遅れ（CodecDelay、Matroska の単位）。ブロックの時刻からこれを引いた時刻に鳴らす
    pub(crate) delay_ticks: i64,
    pub(crate) decoders: Vec<BlockDecode>,
}

/// フロントエンドに渡す音声トラックの情報（コンテナに入っている順）
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemuxAudioTrack {
    pub(crate) name: Option<String>,
    pub(crate) language: Option<String>,
    /// コーデックの名前（AAC、AC-3 など）
    pub(crate) codec: String,
    /// MP4 の codecs 文字列（mp4a.40.2 など）。リマックスできないコーデックなら None
    pub(crate) codec_string: Option<String>,
    pub(crate) default: bool,
}

/// 動画ストリームを開いた結果
#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MkvOpened {
    pub(crate) id: u64,
    /// 映像の MP4 の codecs 文字列
    pub(crate) video_codec: String,
    /// 鳴らす音声の MP4 の codecs 文字列（音声なしなら None）
    pub(crate) audio_codec: Option<String>,
    /// 長さ（秒）。分からなければ None
    pub(crate) duration: Option<f64>,
    pub(crate) audio_tracks: Vec<RemuxAudioTrack>,
    /// 鳴らす音声（audio_tracks の番号）
    pub(crate) audio: Option<usize>,
    /// AV1 をソフトウェアデコード＋ハードウェアエンコードで変換して流しているか
    pub(crate) transcoded: bool,
    /// 現在は無変換だが、WebView が非対応なら変換モードで開き直せるか
    pub(crate) can_transcode: bool,
}

/// コーデック ID を人が読む名前にする（非対応の知らせに出す）
pub(crate) fn mkv_codec_name(codec_id: &str) -> String {
    let name = match codec_id {
        "V_MPEG4/ISO/AVC" => "H.264",
        "V_MPEGH/ISO/HEVC" => "HEVC",
        "V_VP8" => "VP8",
        "V_VP9" => "VP9",
        "V_AV1" => "AV1",
        "V_MPEG2" => "MPEG-2",
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/AP" => "MPEG-4 Visual",
        "V_MS/VFW/FOURCC" => "VfW",
        "A_OPUS" => "Opus",
        "A_AC3" => "AC-3",
        "A_EAC3" => "E-AC-3",
        "A_TRUEHD" => "TrueHD",
        "A_FLAC" => "FLAC",
        "A_VORBIS" => "Vorbis",
        "A_MPEG/L3" => "MP3",
        "A_MPEG/L2" => "MP2",
        id if id.starts_with("A_AAC") => "AAC",
        id if id.starts_with("A_DTS") => "DTS",
        id if id.starts_with("A_PCM") => "PCM",
        id => id,
    };
    name.to_string()
}

/// avcC から codecs 文字列（avc1.PPCCLL）を作る
pub(crate) fn avc_codec_string(avcc: &[u8]) -> Option<String> {
    if avcc.len() < 7 || avcc[0] != 1 {
        return None;
    }
    Some(format!(
        "avc1.{:02X}{:02X}{:02X}",
        avcc[1], avcc[2], avcc[3]
    ))
}

/// hvcC から codecs 文字列（hvc1.1.6.L93.B0 など。ISO/IEC 14496-15 の付属書 E）を作る
pub(crate) fn hevc_codec_string(hvcc: &[u8]) -> Option<String> {
    if hvcc.len() < 23 || hvcc[0] != 1 {
        return None;
    }
    let space = ["", "A", "B", "C"][(hvcc[1] >> 6) as usize];
    let tier = if hvcc[1] & 0x20 != 0 { "H" } else { "L" };
    let profile = hvcc[1] & 0x1F;
    let compat = u32::from_be_bytes([hvcc[2], hvcc[3], hvcc[4], hvcc[5]]).reverse_bits();
    let mut out = format!("hvc1.{space}{profile}.{compat:X}.{tier}{}", hvcc[12]);
    // 制約フラグ（6 バイト）。末尾の 0 のバイトは省く
    let constraints = &hvcc[6..12];
    let used = constraints
        .iter()
        .rposition(|&b| b != 0)
        .map_or(0, |i| i + 1);
    for b in &constraints[..used] {
        out.push_str(&format!(".{b:X}"));
    }
    Some(out)
}

/// AAC のサンプリング周波数の表（AudioSpecificConfig の samplingFrequencyIndex）
pub(crate) const AAC_SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// AudioSpecificConfig から (オブジェクトタイプ, サンプリング周波数, 1 フレームのサンプル数) を読む
pub(crate) fn parse_aac_config(asc: &[u8]) -> Option<(u8, u32, u32)> {
    if asc.len() < 2 {
        return None;
    }
    let object = asc[0] >> 3;
    if object == 0 || object == 31 {
        return None;
    }
    let index = ((asc[0] & 7) << 1) | (asc[1] >> 7);
    let (rate, rest_bit) = if index == 15 {
        // 24 ビットで直接書かれている
        if asc.len() < 5 {
            return None;
        }
        let bits = u64::from_be_bytes([0, 0, 0, asc[1], asc[2], asc[3], asc[4], 0]);
        (((bits >> 15) & 0xFF_FFFF) as u32, None)
    } else {
        (*AAC_SAMPLE_RATES.get(index as usize)?, Some(asc[1]))
    };
    // GASpecificConfig の frameLengthFlag（立っていれば 960 サンプル）
    let short =
        matches!(object, 1..=4 | 6 | 7 | 17 | 19..=23) && rest_bit.is_some_and(|b| b & 0x04 != 0);
    Some((object, rate, if short { 960 } else { 1024 }))
}

/// CodecPrivate の無い古い AAC のコーデック ID（A_AAC/MPEG4/LC など）から AudioSpecificConfig を作る
pub(crate) fn legacy_aac_config(codec_id: &str, rate: u32, channels: u16) -> Option<Vec<u8>> {
    let profile = codec_id.rsplit('/').next()?;
    let (object, rate) = match profile {
        "MAIN" => (1u8, rate),
        "LC" => (2, rate),
        "SSR" => (3, rate),
        "LTP" => (4, rate),
        // SBR は中核の LC を半分の周波数で鳴らし、デコーダが倍に戻す
        "SBR" => (2, rate / 2),
        _ => return None,
    };
    let index = AAC_SAMPLE_RATES.iter().position(|&r| r == rate)? as u8;
    let channels = channels.min(7) as u8;
    Some(vec![
        (object << 3) | (index >> 1),
        ((index & 1) << 7) | (channels << 3),
    ])
}

/// OpusHead（MKV の CodecPrivate、リトルエンディアン）を dOps の中身（ビッグエンディアン）にする
pub(crate) fn opus_dops(head: &[u8]) -> Option<Vec<u8>> {
    if head.len() < 19 || &head[..8] != b"OpusHead" {
        return None;
    }
    let channels = head[9];
    let family = head[18];
    let mut out = vec![0, channels];
    out.extend_from_slice(&u16::from_le_bytes([head[10], head[11]]).to_be_bytes());
    out.extend_from_slice(
        &u32::from_le_bytes([head[12], head[13], head[14], head[15]]).to_be_bytes(),
    );
    out.extend_from_slice(&i16::from_le_bytes([head[16], head[17]]).to_be_bytes());
    out.push(family);
    if family != 0 {
        // StreamCount, CoupledCount, ChannelMapping[channels]
        out.extend_from_slice(head.get(19..21 + channels as usize)?);
    }
    Some(out)
}

/// Opus のパケット 1 つの長さ（48 kHz のサンプル数。RFC 6716 の TOC）
pub(crate) fn opus_packet_samples(packet: &[u8]) -> Option<u32> {
    let toc = *packet.first()?;
    let config = toc >> 3;
    // 1 フレームの長さ（48 kHz で 2.5 ms = 120 サンプル）
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12..=15 => [480, 960][(config % 2) as usize],
        _ => [120, 240, 480, 960][(config % 4) as usize],
    };
    let count = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => (*packet.get(1)? & 0x3F) as u32,
    };
    Some(frame * count)
}

/// 映像トラックをリマックス・変換できる形に読む。できなければ理由（コーデック名）を返す
pub(crate) fn remux_video_track(
    track: &matroska_demuxer::TrackEntry,
) -> Result<RemuxVideo, String> {
    let name = mkv_codec_name(track.codec_id());
    let unsupported = || format!("この MKV の映像（{name}）には対応していません");
    let codec = match track.codec_id() {
        "V_MPEG4/ISO/AVC" => RemuxVideoCodec::Avc,
        "V_MPEGH/ISO/HEVC" => RemuxVideoCodec::Hevc,
        "V_AV1" => RemuxVideoCodec::Av1,
        _ => return Err(unsupported()),
    };
    let (config, codec_string) = match codec {
        RemuxVideoCodec::Avc => {
            let config = track.codec_private().ok_or_else(unsupported)?.to_vec();
            let cs = avc_codec_string(&config).ok_or_else(unsupported)?;
            (config, cs)
        }
        RemuxVideoCodec::Hevc => {
            let config = track.codec_private().ok_or_else(unsupported)?.to_vec();
            let cs = hevc_codec_string(&config).ok_or_else(unsupported)?;
            (config, cs)
        }
        RemuxVideoCodec::Av1 => {
            // AV1 は CodecPrivate（av1C）が省略されているファイルもある（先頭 OBU や変換で補える）
            let config = track.codec_private().unwrap_or(&[]).to_vec();
            let cs = av1_codec_string(&config).unwrap_or_else(|| "av01.0.04M.08".into());
            (config, cs)
        }
    };
    let decoders = block_decoders(track).ok_or_else(unsupported)?;
    let video = track.video().ok_or_else(unsupported)?;
    let width = video.pixel_width().get().min(u16::MAX as u64) as u32;
    let height = video.pixel_height().get().min(u16::MAX as u64) as u32;
    // DisplayUnit が画素（0）か未指定ならそのまま、縦横比（3）なら高さを保って幅を合わせる
    use matroska_demuxer::DisplayUnit;
    let clamp = |v: u64| v.clamp(1, u16::MAX as u64) as u32;
    let display = match (
        video.display_width(),
        video.display_height(),
        video.display_unit(),
    ) {
        (Some(w), Some(h), None | Some(DisplayUnit::Pixels)) => (clamp(w.get()), clamp(h.get())),
        (Some(w), Some(h), Some(DisplayUnit::DisplayAspectRatio)) => {
            (clamp((height as u64 * w.get() + h.get() / 2) / h), height)
        }
        _ => (width, height),
    };
    Ok(RemuxVideo {
        number: track.track_number().get(),
        codec,
        config,
        codec_string,
        width,
        height,
        display,
        color: None,
        decoders,
        frame_ticks: 0,
    })
}

/// 音声トラックをリマックスできる形に読む。できなければ None
pub(crate) fn remux_audio_track(track: &matroska_demuxer::TrackEntry) -> Option<RemuxAudio> {
    let audio = track.audio()?;
    let channels = audio.channels().get().min(255) as u16;
    let rate = audio.sampling_frequency().round() as u32;
    let decoders = block_decoders(track)?;
    let number = track.track_number().get();
    let codec_id = track.codec_id();
    if codec_id == "A_OPUS" {
        let config = opus_dops(track.codec_private()?)?;
        return Some(RemuxAudio {
            number,
            codec: RemuxAudioCodec::Opus,
            channels: config[1] as u16,
            config,
            codec_string: "opus".into(),
            timescale: 48000,
            frame_samples: 960,
            delay_ticks: 0,
            decoders,
        });
    }
    if !codec_id.starts_with("A_AAC") {
        return None;
    }
    let config = match track.codec_private() {
        Some(asc) if !asc.is_empty() => asc.to_vec(),
        _ => legacy_aac_config(codec_id, rate, channels)?,
    };
    let (object, core_rate, frame_samples) = parse_aac_config(&config)?;
    Some(RemuxAudio {
        number,
        codec: RemuxAudioCodec::Aac,
        config,
        codec_string: format!("mp4a.40.{object}"),
        channels,
        timescale: core_rate,
        frame_samples,
        delay_ticks: 0,
        decoders,
    })
}

/// コンテナヘッダだけを高速に覗き、映像トラックが AV1（MP4 の av01 / WebM・MKV の V_AV1）かを判定する。
/// フレーム本体は読まないため大きな動画でも 1 ms 未満で終わる
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn is_av1_video(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut head = [0u8; 8];
    if file.read_exact(&mut head).is_err() {
        return false;
    }
    // EBML ヘッダ（MKV / WebM）
    if head[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        if file.seek(std::io::SeekFrom::Start(0)).is_err() {
            return false;
        }
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let Ok(mkv) = matroska_demuxer::MatroskaFile::open(&mut reader) else {
            return false;
        };
        return mkv.tracks().iter().any(|t| {
            t.track_type() == matroska_demuxer::TrackType::Video && t.codec_id() == "V_AV1"
        });
    }
    if looks_like_mp4(&head) {
        return is_mp4_av1(path);
    }
    false
}

/// Matroska / WebM 側の読み取り状態
struct MatroskaSource {
    walker: MkvWalker<BufReader<File>>,
    /// Matroska の 1 単位あたりのナノ秒
    scale_ns: u64,
    first_cluster: u64,
    cues: Vec<(i64, u64)>,
    /// Cues が無いときに作る Cluster の索引（最初のシークで作る）
    clusters: Option<Vec<(i64, u64)>>,
    /// B フレームの並べ替えの深さ（Matroska の単位）。DTS = 表示時刻の昇順 − これ
    reorder: i64,
}

impl MatroskaSource {
    fn ticks_to_seconds(&self, ticks: i64) -> f64 {
        ticks as f64 * self.scale_ns as f64 / 1e9
    }

    fn ticks_to_units(&self, ticks: i64, timescale: u64) -> i64 {
        ((ticks as i128 * self.scale_ns as i128 * timescale as i128 + 500_000_000) / 1_000_000_000)
            as i64
    }

    fn probe_reorder(&mut self, video_track: u64) -> std::io::Result<i64> {
        self.walker.seek_to(self.first_cluster, 0)?;
        let mut gops: Vec<Vec<i64>> = Vec::new();
        let mut frames = 0;
        while let Some(frame) = self.walker.next(false)? {
            if frame.track != video_track {
                continue;
            }
            if frame.keyframe || gops.is_empty() {
                // 最後の GOP は途中で終わっているかもしれないので、4 つ目の頭で止める
                if gops.len() == 4 {
                    break;
                }
                gops.push(Vec::new());
            }
            gops.last_mut().unwrap().push(frame.timestamp);
            frames += 1;
            if frames >= REMUX_REORDER_PROBE_FRAMES {
                break;
            }
        }
        Ok(gops.iter().map(|gop| reorder_depth(gop)).max().unwrap_or(0))
    }

    fn seek(&mut self, video_track: u64, seconds: f64) -> std::io::Result<f64> {
        let target = (seconds.max(0.0) * 1e9 / self.scale_ns as f64) as i64;
        let starts: Vec<u64> = if !self.cues.is_empty() {
            let i = self
                .cues
                .partition_point(|c| c.0 <= target)
                .saturating_sub(1);
            vec![self.cues[i].1, self.first_cluster]
        } else {
            if self.clusters.is_none() {
                let index = scan_cluster_index(&mut self.walker.r, self.first_cluster);
                self.clusters = Some(index);
            }
            let clusters = self.clusters.as_deref().unwrap_or(&[]);
            let i = clusters.partition_point(|c| c.0 <= target);
            let mut starts = Vec::new();
            let mut back = 1;
            while back <= i {
                starts.push(clusters[i - back].1);
                back *= 2;
            }
            starts.push(self.first_cluster);
            starts.dedup();
            starts
        };
        let mut fallback = None;
        for start in starts {
            self.walker.seek_to(start, 0)?;
            let mut best: Option<(u64, i64, i64)> = None;
            while let Some(frame) = self.walker.next(false)? {
                if frame.track != video_track {
                    continue;
                }
                if frame.keyframe && frame.timestamp <= target {
                    best = Some((frame.pos, frame.cluster_timestamp, frame.timestamp));
                } else if frame.timestamp > target {
                    if best.is_none() && frame.keyframe && fallback.is_none() {
                        fallback = Some((frame.pos, frame.cluster_timestamp, frame.timestamp));
                    }
                    if best.is_some() || fallback.is_some() {
                        break;
                    }
                }
            }
            if let Some(found) = best {
                return self.start_at(found);
            }
        }
        match fallback {
            Some(found) => self.start_at(found),
            None => {
                self.walker.r.seek(std::io::SeekFrom::End(0))?;
                self.walker.queue.clear();
                Ok(seconds)
            }
        }
    }

    fn start_at(
        &mut self,
        (pos, cluster_timestamp, timestamp): (u64, i64, i64),
    ) -> std::io::Result<f64> {
        self.walker.seek_to(pos, cluster_timestamp)?;
        Ok(self.ticks_to_seconds(timestamp))
    }
}

enum StreamSource {
    Matroska(MatroskaSource),
    Mp4(Mp4Source),
}

/// AV1 リアルタイム変換（rav1d → VideoToolbox H.264）の状態
#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
struct TranscodeState {
    transcoder: Av1Transcoder,
    /// open() で avcC を確定した際に作った最初の断片（1 回目の read_fragment でそのまま返す）
    prefetched: Option<Vec<u8>>,
    /// Matroska / WebM でインターリーブ音声を拾う途中に先読みした映像パケット（90 kHz PTS, ペイロード）
    pending_video: VecDeque<(i64, Vec<u8>)>,
    /// 先読み済みの音声フレーム（出力 timescale 単位の PTS, 復号済みペイロード）
    pending_audio: VecDeque<(i64, Vec<u8>)>,
    /// シーク直後にエンコードをスキップする閾値（90 kHz PTS）
    skip_before_pts: Option<i64>,
    /// 起動・シーク直後の最初の断片か（最初の 1 断片だけ短く返して応答を速める）
    first_fragment: bool,
    /// コンテナから映像を最後まで読み切ったか
    video_eof: bool,
    /// デフォルトの 1 フレーム長（90 kHz）
    default_frame_ticks: i64,
    /// 動画の総再生時間（秒）。末尾付近・末尾以降のシーク時にスキップし過ぎないために保持する
    duration: Option<f64>,
}

/// 開いている動画ストリーム 1 本（MKV / WebM / MP4 のリマックスまたは AV1 リアルタイム変換）
pub(crate) struct MkvStream {
    source: StreamSource,
    video: RemuxVideo,
    audio: Option<RemuxAudio>,
    /// 前の断片の続きの音声の時刻（音声の単位）。シークしたら付け直す
    audio_next: Option<u64>,
    sequence: u32,
    #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
    transcode: Option<TranscodeState>,
}

impl MkvStream {
    /// 動画を開く（既定のモード: MKV の H.264/HEVC は無変換、AV1 はハードウェア非対応環境なら変換）。
    /// `audio` は鳴らす音声トラックの番号（None なら既定、負なら音声なし）
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn open(path: &Path, audio: Option<i64>) -> Result<(Self, MkvOpened), String> {
        Self::open_with_options(path, audio, None)
    }

    /// 変換モードを明示指定して動画を開く（`force_transcode: Some(true)` で AV1 を強制的にソフト変換）
    pub(crate) fn open_with_options(
        path: &Path,
        audio: Option<i64>,
        force_transcode: Option<bool>,
    ) -> Result<(Self, MkvOpened), String> {
        #[cfg(target_os = "macos")]
        let factory: Option<EncoderFactory> = None;
        #[cfg(all(test, target_os = "linux"))]
        let factory: Option<EncoderFactory> = None;
        #[cfg(not(any(target_os = "macos", all(test, target_os = "linux"))))]
        let factory = ();
        Self::open_inner(path, audio, force_transcode, factory)
    }

    /// テスト用: モック H.264 エンコーダを差し込んで rav1d 実デコード〜fMP4 生成を検証する
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn open_with_test_encoder(
        path: &Path,
        audio: Option<i64>,
        factory: EncoderFactory,
    ) -> Result<(Self, MkvOpened), String> {
        Self::open_inner(path, audio, Some(true), Some(factory))
    }

    fn open_inner(
        path: &Path,
        audio: Option<i64>,
        force_transcode: Option<bool>,
        #[cfg(any(target_os = "macos", all(test, target_os = "linux")))] custom_factory: Option<
            EncoderFactory,
        >,
        #[cfg(not(any(target_os = "macos", all(test, target_os = "linux"))))] _custom_factory: (),
    ) -> Result<(Self, MkvOpened), String> {
        let mut probe = File::open(path).map_err(|e| format!("動画を開けません: {e}"))?;
        let mut head = [0u8; 8];
        probe
            .read_exact(&mut head)
            .map_err(|e| format!("動画を読めません: {e}"))?;
        drop(probe);

        let (source, mut video, audio_track, audio_tracks, chosen_audio, duration, fps) =
            if looks_like_mp4(&head) {
                let (mp4, meta) = Mp4Source::open(path, audio)?;
                (
                    StreamSource::Mp4(mp4),
                    meta.video,
                    meta.audio,
                    meta.audio_tracks,
                    meta.chosen_audio,
                    meta.duration,
                    meta.fps,
                )
            } else {
                Self::open_matroska(path, audio)?
            };

        let want_transcode = video.codec == RemuxVideoCodec::Av1
            && match force_transcode {
                Some(v) => v,
                None => !hw_av1_decode_supported(),
            };

        let mut stream = MkvStream {
            source,
            video: video.clone(),
            audio: audio_track.clone(),
            audio_next: None,
            sequence: 0,
            #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
            transcode: None,
        };

        if want_transcode {
            #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
            {
                let transcoder = match custom_factory {
                    Some(f) => Av1Transcoder::with_factory(fps, f)?,
                    None => {
                        #[cfg(target_os = "macos")]
                        {
                            Av1Transcoder::new_videotoolbox(fps)?
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            return Err("この動画の映像（AV1）には対応していません".into());
                        }
                    }
                };
                let default_frame_ticks = (REMUX_VIDEO_TIMESCALE as f64 / fps.clamp(1.0, 240.0))
                    .round()
                    .max(1.0) as i64;
                stream.transcode = Some(TranscodeState {
                    transcoder,
                    prefetched: None,
                    pending_video: VecDeque::new(),
                    pending_audio: VecDeque::new(),
                    skip_before_pts: None,
                    first_fragment: true,
                    video_eof: false,
                    default_frame_ticks,
                    duration,
                });
                // 先頭の断片を 1 つエンコードして avcC・解像度・色空間を確定し、
                // その断片をキャッシュして 1 回目の read_fragment で無駄なく返す
                let first_fragment = stream
                    .build_transcode_fragment()
                    .map_err(|e| format!("AV1 動画を変換できません: {e}"))?;
                let tc = stream.transcode.as_mut().unwrap();
                let avcc = tc
                    .transcoder
                    .avcc()
                    .ok_or("AV1 動画の映像を変換できませんでした")?
                    .to_vec();
                let codec_string =
                    avc_codec_string(&avcc).ok_or("変換後の H.264 形式を特定できませんでした")?;
                if let Some((w, h)) = tc.transcoder.encoded_size() {
                    if stream.video.display == (stream.video.width, stream.video.height) {
                        stream.video.display = (w, h);
                    }
                    stream.video.width = w;
                    stream.video.height = h;
                }
                stream.video.codec = RemuxVideoCodec::Avc;
                stream.video.config = avcc;
                stream.video.codec_string = codec_string;
                stream.video.color = tc.transcoder.encoded_color();
                if !first_fragment.is_empty() {
                    tc.prefetched = Some(first_fragment);
                }
                video = stream.video.clone();
            }
            #[cfg(not(any(target_os = "macos", all(test, target_os = "linux"))))]
            {
                let _ = fps;
                return Err("この動画の映像（AV1）には対応していません".into());
            }
        } else if video.codec == RemuxVideoCodec::Av1 && video.config.is_empty() {
            // 無変換 AV1 リマックスで CodecPrivate が空のときは先頭キーフレームの OBU から av1C を補う
            if let Some(av1c) = stream.probe_first_av1c() {
                if let Some(cs) = av1_codec_string(&av1c) {
                    stream.video.codec_string = cs;
                }
                stream.video.config = av1c;
                video = stream.video.clone();
            }
        }

        let transcoded = {
            #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
            {
                stream.transcode.is_some()
            }
            #[cfg(not(any(target_os = "macos", all(test, target_os = "linux"))))]
            {
                false
            }
        };
        let can_transcode =
            !transcoded && video.codec == RemuxVideoCodec::Av1 && can_transcode_av1();

        let opened = MkvOpened {
            id: 0,
            video_codec: video.codec_string.clone(),
            audio_codec: audio_track.as_ref().map(|a| a.codec_string.clone()),
            duration,
            audio_tracks,
            audio: chosen_audio,
            transcoded,
            can_transcode,
        };
        Ok((stream, opened))
    }

    #[allow(clippy::type_complexity)]
    fn open_matroska(
        path: &Path,
        audio: Option<i64>,
    ) -> Result<
        (
            StreamSource,
            RemuxVideo,
            Option<RemuxAudio>,
            Vec<RemuxAudioTrack>,
            Option<usize>,
            Option<f64>,
            f64,
        ),
        String,
    > {
        let file = File::open(path).map_err(|e| format!("動画を開けません: {e}"))?;
        let mut reader = BufReader::with_capacity(256 * 1024, file);
        let mkv = matroska_demuxer::MatroskaFile::open(&mut reader)
            .map_err(|e| format!("MKV を読めません: {e}"))?;
        let scale_ns = mkv.info().timestamp_scale().get();
        let duration = mkv
            .info()
            .duration()
            .map(|d| d * scale_ns as f64 / 1e9)
            .filter(|d| d.is_finite() && *d > 0.0);
        let video_track = mkv
            .tracks()
            .iter()
            .find(|t| t.track_type() == matroska_demuxer::TrackType::Video)
            .ok_or("この MKV には映像がありません")?;
        let mut video = remux_video_track(video_track)?;
        let ns_to_ticks = |ns: u64| (ns / scale_ns) as i64;
        video.frame_ticks = video_track
            .default_duration()
            .map_or(0, |d| ns_to_ticks(d.get()));
        let fps = video_track
            .default_duration()
            .map(|d| (1e9 / d.get() as f64).clamp(1.0, 240.0))
            .unwrap_or(30.0);
        let audio_entries: Vec<_> = mkv
            .tracks()
            .iter()
            .filter(|t| t.track_type() == matroska_demuxer::TrackType::Audio)
            .collect();
        let remuxable: Vec<Option<RemuxAudio>> =
            audio_entries.iter().map(|t| remux_audio_track(t)).collect();
        let audio_tracks: Vec<RemuxAudioTrack> = audio_entries
            .iter()
            .zip(&remuxable)
            .map(|(t, r)| RemuxAudioTrack {
                name: t.name().map(str::to_string),
                language: t
                    .language_bcp47()
                    .or(t.language())
                    .filter(|l| *l != "und")
                    .map(str::to_string),
                codec: mkv_codec_name(t.codec_id()),
                codec_string: r.as_ref().map(|r| r.codec_string.clone()),
                default: t.flag_default(),
            })
            .collect();
        let chosen = match audio {
            Some(i) => usize::try_from(i).ok().filter(|&i| i < audio_tracks.len()),
            None => (0..audio_tracks.len())
                .find(|&i| audio_tracks[i].default && remuxable[i].is_some())
                .or_else(|| (0..audio_tracks.len()).find(|&i| remuxable[i].is_some())),
        };
        let audio_track = chosen.and_then(|i| {
            let mut track = remuxable[i].clone()?;
            track.delay_ticks = audio_entries[i].codec_delay().map_or(0, ns_to_ticks);
            Some(track)
        });
        let lace_step = audio_track.as_ref().map_or(0, |a| {
            let samples = if a.codec == RemuxAudioCodec::Opus {
                960
            } else {
                a.frame_samples
            };
            ns_to_ticks(samples as u64 * 1_000_000_000 / a.timescale as u64)
        });
        drop(mkv);

        let layout = read_mkv_layout(&mut reader, video.number)
            .map_err(|e| format!("MKV を読めません: {e}"))?;
        let mut tracks = vec![(video.number, video.frame_ticks)];
        if let Some(a) = &audio_track {
            tracks.push((a.number, lace_step));
        }
        let mut mkv_src = MatroskaSource {
            walker: MkvWalker::new(reader, tracks),
            scale_ns,
            first_cluster: layout.first_cluster,
            cues: layout.cues,
            clusters: None,
            reorder: 0,
        };
        // AV1 はコンテナ上で表示順に並ぶため並べ替えの深さ調査は不要（H.264 / HEVC のみ調査）
        if video.codec != RemuxVideoCodec::Av1 {
            mkv_src.reorder = mkv_src
                .probe_reorder(video.number)
                .map_err(|e| format!("MKV を読めません: {e}"))?;
        }
        mkv_src
            .seek(video.number, 0.0)
            .map_err(|e| format!("MKV を読めません: {e}"))?;
        Ok((
            StreamSource::Matroska(mkv_src),
            video,
            audio_track.clone(),
            audio_tracks,
            chosen.filter(|_| audio_track.is_some()),
            duration,
            fps,
        ))
    }

    fn probe_first_av1c(&mut self) -> Option<Vec<u8>> {
        let StreamSource::Matroska(mkv) = &mut self.source else {
            return None;
        };
        let mut av1c = None;
        while let Ok(Some(frame)) = mkv.walker.next(true) {
            if frame.track == self.video.number {
                let raw = decode_block(frame.data, &self.video.decoders, REMUX_MAX_FRAME_BYTES)?;
                av1c = av1c_from_obu_stream(&raw);
                break;
            }
        }
        let _ = mkv.seek(self.video.number, 0.0);
        av1c
    }

    /// `seconds` の直前のキーフレームから読むようにする。戻り値は開始位置（秒）
    pub(crate) fn seek(&mut self, seconds: f64) -> std::io::Result<f64> {
        self.audio_next = None;
        let keyframe_sec = match &mut self.source {
            StreamSource::Matroska(mkv) => mkv.seek(self.video.number, seconds)?,
            StreamSource::Mp4(mp4) => mp4.seek(seconds),
        };
        #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
        if let Some(tc) = self.transcode.as_mut() {
            tc.prefetched = None;
            tc.pending_video.clear();
            tc.pending_audio.clear();
            tc.video_eof = false;
            tc.first_fragment = true;
            tc.transcoder.reset_for_seek();
            // 動画の末尾以降へのシークでは最後のキーフレームからそのまま再生する（通常リマックスと揃える）。
            // 途中のシークではキーフレームから目標時刻の 1 フレーム手前までを復号のみにしてエンコードを省き、
            // 最初の断片が必ず目標時刻（video.currentTime）を覆うようにして WebKit 側の二重シークを防ぐ
            let past_end = tc.duration.is_some_and(|d| seconds >= d);
            if !past_end {
                let target = match tc.duration {
                    Some(d) => seconds.min((d - 0.05).max(keyframe_sec)),
                    None => seconds,
                };
                let target_pts = (target.max(0.0) * REMUX_VIDEO_TIMESCALE as f64).round() as i64;
                let kf_pts = (keyframe_sec.max(0.0) * REMUX_VIDEO_TIMESCALE as f64).round() as i64;
                let start_pts = (target_pts - tc.default_frame_ticks - 450).max(kf_pts);
                if start_pts > kf_pts + 450 {
                    tc.skip_before_pts = Some(start_pts);
                    let start_sec = start_pts as f64 / REMUX_VIDEO_TIMESCALE as f64;
                    if let StreamSource::Mp4(mp4) = &mut self.source {
                        mp4.seek_audio((start_sec - 0.1).max(0.0));
                    }
                    return Ok(start_sec);
                }
            }
            tc.skip_before_pts = None;
        }
        Ok(keyframe_sec)
    }

    /// 初期化セグメント（ftyp + moov）
    pub(crate) fn init_segment(&self) -> Vec<u8> {
        build_init_segment(&self.video, self.audio.as_ref())
    }

    /// 次の断片（moof + mdat）。終わりまで読んだら空
    pub(crate) fn read_fragment(&mut self) -> std::io::Result<Vec<u8>> {
        #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
        if self.transcode.is_some() {
            if let Some(prefetched) = self.transcode.as_mut().unwrap().prefetched.take() {
                return Ok(prefetched);
            }
            return self
                .build_transcode_fragment()
                .map_err(std::io::Error::other);
        }
        match &self.source {
            StreamSource::Matroska(_) => self.read_matroska_remux_fragment(),
            StreamSource::Mp4(_) => self.read_mp4_remux_fragment(),
        }
    }

    /// Matroska / WebM の無変換リマックス（H.264 / HEVC / M3+ AV1）
    fn read_matroska_remux_fragment(&mut self) -> std::io::Result<Vec<u8>> {
        let StreamSource::Matroska(mkv) = &mut self.source else {
            unreachable!();
        };
        let min_span = (REMUX_FRAGMENT_SECONDS * 1e9 / mkv.scale_ns as f64) as i64;
        let mut video: Vec<MkvFrame> = Vec::new();
        let mut audio: Vec<MkvFrame> = Vec::new();
        let mut bytes = 0usize;
        let mut next_keyframe = None;
        while let Some(frame) = mkv.walker.next(true)? {
            if frame.track == self.video.number {
                if frame.keyframe && !video.is_empty() {
                    let first = video[0].timestamp;
                    let last = video.iter().map(|f| f.timestamp).max().unwrap_or(first);
                    if last - first >= min_span || bytes >= REMUX_FRAGMENT_SOFT_BYTES {
                        next_keyframe = Some(frame.timestamp);
                        mkv.walker.push_back(frame);
                        break;
                    }
                }
                bytes += frame.data.len();
                video.push(frame);
            } else {
                bytes += frame.data.len();
                audio.push(frame);
            }
            if bytes >= REMUX_FRAGMENT_MAX_BYTES {
                break;
            }
        }
        if video.is_empty() && audio.is_empty() {
            return Ok(Vec::new());
        }
        let video_samples = self.video_samples_matroska(video, next_keyframe);
        let audio_samples = self.audio_samples_matroska(audio);
        self.sequence += 1;
        Ok(build_fragment(
            self.sequence,
            video_samples.as_ref(),
            audio_samples.as_ref(),
        ))
    }

    /// MP4 の無変換リマックス（M3+ での AV1 無変換テスト・フォールバック用）
    fn read_mp4_remux_fragment(&mut self) -> std::io::Result<Vec<u8>> {
        let StreamSource::Mp4(mp4) = &mut self.source else {
            unreachable!();
        };
        let min_span = (REMUX_FRAGMENT_SECONDS * REMUX_VIDEO_TIMESCALE as f64) as i64;
        let mut v_frames: Vec<(i64, bool, Vec<u8>)> = Vec::new();
        let mut bytes = 0usize;
        let mut next_pts = None;
        while let Some((pts, kf, data)) = mp4.next_video()? {
            bytes += data.len();
            v_frames.push((pts, kf, data));
            if let Some(peek_pts) = mp4.peek_video_pts_90k() {
                let peek_kf = mp4.peek_video_keyframe().unwrap_or(false);
                if peek_kf
                    && (peek_pts - v_frames[0].0 >= min_span || bytes >= REMUX_FRAGMENT_SOFT_BYTES)
                {
                    next_pts = Some(peek_pts);
                    break;
                }
            }
            if bytes >= REMUX_FRAGMENT_MAX_BYTES {
                next_pts = mp4.peek_video_pts_90k();
                break;
            }
        }
        if v_frames.is_empty() {
            return Ok(Vec::new());
        }
        let fallback = self.video.frame_ticks.max(1);
        let base_pts = v_frames[0].0.max(0);
        let mut durations = Vec::with_capacity(v_frames.len());
        for i in 0..v_frames.len() {
            let d = if i + 1 < v_frames.len() {
                (v_frames[i + 1].0 - v_frames[i].0).max(1)
            } else {
                next_pts
                    .map(|n| n - v_frames[i].0)
                    .filter(|&d| d > 0)
                    .or_else(|| durations.last().copied())
                    .unwrap_or(fallback)
                    .max(1)
            };
            durations.push(d);
        }
        let end_pts = v_frames.last().unwrap().0 + *durations.last().unwrap();
        let until_sec = if mp4.peek_video_pts_90k().is_none() {
            None
        } else {
            Some(end_pts as f64 / REMUX_VIDEO_TIMESCALE as f64)
        };
        let raw_audio = match &self.audio {
            Some(a) => mp4.read_audio_until(until_sec, a.timescale)?,
            None => Vec::new(),
        };
        let video_run = TrackRun {
            track_id: 1,
            base_decode_time: base_pts as u64,
            samples: v_frames
                .into_iter()
                .zip(durations)
                .map(|((_, keyframe, data), duration)| Sample {
                    data,
                    duration: duration as u32,
                    composition_offset: 0,
                    keyframe,
                })
                .collect(),
            has_composition_offsets: false,
        };
        let audio_run = self.audio_samples_units(raw_audio);
        self.sequence += 1;
        Ok(build_fragment(
            self.sequence,
            Some(&video_run),
            audio_run.as_ref(),
        ))
    }

    /// AV1 リアルタイム変換（rav1d → VideoToolbox H.264）で次の 1 断片を作る。
    /// - 起動・シーク直後の最初の 1 断片は 0.25 秒で素早く返し、以降は 1 秒単位で返す
    /// - 次の断片の先頭フレーム PTS で前の断片を閉じるため、VFR や WebM でも断片境界の隙間が 0 になる
    /// - B フレームなし（PTS == DTS）で元フレームごとの PTS をそのまま引き継ぎ、音声も同じ区間へ揃える
    #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
    fn build_transcode_fragment(&mut self) -> Result<Vec<u8>, String> {
        let Some(tc) = self.transcode.as_ref() else {
            return Ok(Vec::new());
        };
        if tc.video_eof && !tc.transcoder.has_started_fragment() {
            return Ok(Vec::new());
        }
        let span_sec = if tc.first_fragment {
            TRANSCODE_INITIAL_FRAGMENT_SECONDS
        } else {
            TRANSCODE_FRAGMENT_SECONDS
        };
        let target_span = (span_sec * REMUX_VIDEO_TIMESCALE as f64) as i64;

        while !self
            .transcode
            .as_ref()
            .unwrap()
            .transcoder
            .has_completed_fragment()
        {
            if self.transcode.as_ref().unwrap().video_eof {
                break;
            }
            let skip_before = self.transcode.as_ref().unwrap().skip_before_pts;
            let next_video = self.read_next_av1_packet()?;
            let tc = self.transcode.as_mut().unwrap();
            match next_video {
                Some((pts_90k, payload)) => {
                    tc.transcoder.push_packet(
                        &payload,
                        pts_90k,
                        skip_before,
                        target_span,
                        TRANSCODE_FRAGMENT_MAX_FRAMES,
                    )?;
                    if tc.transcoder.has_started_fragment() {
                        tc.skip_before_pts = None;
                    }
                }
                None => {
                    let default_ticks = tc.default_frame_ticks;
                    tc.transcoder.finish_eof(
                        skip_before,
                        target_span,
                        TRANSCODE_FRAGMENT_MAX_FRAMES,
                        default_ticks,
                    )?;
                    tc.skip_before_pts = None;
                    tc.video_eof = true;
                    break;
                }
            }
        }

        let tc = self.transcode.as_mut().unwrap();
        let Some((encoded, video_end_pts)) = tc.transcoder.pop_completed_fragment() else {
            return Ok(Vec::new());
        };
        tc.first_fragment = false;
        let is_final_fragment = tc.video_eof && !tc.transcoder.has_started_fragment();

        let first_video_pts = encoded[0].pts.max(0);
        let mut durations = Vec::with_capacity(encoded.len());
        for i in 0..encoded.len() {
            let next_pts = if i + 1 < encoded.len() {
                encoded[i + 1].pts
            } else {
                video_end_pts
            };
            durations.push((next_pts - encoded[i].pts).max(1));
        }

        let video_run = TrackRun {
            track_id: 1,
            base_decode_time: first_video_pts as u64,
            samples: encoded
                .into_iter()
                .zip(durations)
                .map(|(frame, duration)| Sample {
                    data: frame.data,
                    duration: duration as u32,
                    composition_offset: 0,
                    keyframe: frame.keyframe,
                })
                .collect(),
            has_composition_offsets: false,
        };

        // 映像と同じ区間 [first_video_pts, video_end_pts] の音声を揃えて入れる
        let audio_run =
            self.collect_transcode_audio(first_video_pts, video_end_pts, is_final_fragment)?;

        self.sequence += 1;
        Ok(build_fragment(
            self.sequence,
            Some(&video_run),
            audio_run.as_ref(),
        ))
    }

    /// 変換用: コンテナから次の AV1 映像パケット 1 つを読む。
    /// Matroska / WebM では途中で出会った音声パケットを pending_audio に蓄える
    #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
    fn read_next_av1_packet(&mut self) -> Result<Option<(i64, Vec<u8>)>, String> {
        if let Some(pkt) = self.transcode.as_mut().unwrap().pending_video.pop_front() {
            return Ok(Some(pkt));
        }
        match &mut self.source {
            StreamSource::Mp4(mp4) => mp4
                .next_video()
                .map(|opt| opt.map(|(pts, _, data)| (pts, data)))
                .map_err(|e| format!("動画を読めません: {e}")),
            StreamSource::Matroska(mkv) => {
                while let Some(frame) = mkv
                    .walker
                    .next(true)
                    .map_err(|e| format!("動画を読めません: {e}"))?
                {
                    if frame.track == self.video.number {
                        let pts = mkv.ticks_to_units(frame.timestamp, REMUX_VIDEO_TIMESCALE);
                        let data =
                            decode_block(frame.data, &self.video.decoders, REMUX_MAX_FRAME_BYTES)
                                .unwrap_or_default();
                        return Ok(Some((pts, data)));
                    }
                    if let Some(audio) = &self.audio {
                        let ts = frame.timestamp - audio.delay_ticks;
                        if ts >= 0 {
                            let a_units = mkv.ticks_to_units(ts, audio.timescale as u64);
                            let data =
                                decode_block(frame.data, &audio.decoders, REMUX_MAX_FRAME_BYTES)
                                    .unwrap_or_default();
                            self.transcode
                                .as_mut()
                                .unwrap()
                                .pending_audio
                                .push_back((a_units, data));
                        }
                    }
                }
                Ok(None)
            }
        }
    }

    /// 変換用: 映像断片 [first_video_pts, video_end_pts] に対応する音声フレームを集めて TrackRun を作る
    #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
    fn collect_transcode_audio(
        &mut self,
        first_video_pts: i64,
        video_end_pts: i64,
        is_final_fragment: bool,
    ) -> Result<Option<TrackRun>, String> {
        let Some(audio) = self.audio.clone() else {
            return Ok(None);
        };
        let timescale = audio.timescale as i64;
        let end_units = if is_final_fragment {
            i64::MAX
        } else {
            ((video_end_pts as i128 * timescale as i128) / REMUX_VIDEO_TIMESCALE as i128) as i64
        };

        // コンテナから video_end_pts までの音声を補充する
        match &mut self.source {
            StreamSource::Mp4(mp4) => {
                let until_sec = (!is_final_fragment)
                    .then_some(video_end_pts as f64 / REMUX_VIDEO_TIMESCALE as f64);
                let more = mp4
                    .read_audio_until(until_sec, audio.timescale)
                    .map_err(|e| format!("音声を読めません: {e}"))?;
                self.transcode.as_mut().unwrap().pending_audio.extend(more);
            }
            StreamSource::Matroska(mkv) => {
                // WebM / MKV で音声ブロックが映像ブロックより後ろにインターリーブされていても取りこぼさないよう、
                // 途中の映像ブロックは pending_video に退避しつつ end_units まで音声を拾う
                let video_track = self.video.number;
                let video_decoders = &self.video.decoders;
                let max_video_ahead = video_end_pts.saturating_add(REMUX_VIDEO_TIMESCALE as i64);
                loop {
                    let tail_ok = !is_final_fragment
                        && self
                            .transcode
                            .as_ref()
                            .unwrap()
                            .pending_audio
                            .back()
                            .is_some_and(|(pts, _)| *pts >= end_units);
                    if tail_ok {
                        break;
                    }
                    let Some(frame) = mkv
                        .walker
                        .next(true)
                        .map_err(|e| format!("音声を読めません: {e}"))?
                    else {
                        break;
                    };
                    if frame.track == video_track {
                        let v_pts = mkv.ticks_to_units(frame.timestamp, REMUX_VIDEO_TIMESCALE);
                        let data = decode_block(frame.data, video_decoders, REMUX_MAX_FRAME_BYTES)
                            .unwrap_or_default();
                        self.transcode
                            .as_mut()
                            .unwrap()
                            .pending_video
                            .push_back((v_pts, data));
                        if !is_final_fragment && v_pts > max_video_ahead {
                            break;
                        }
                        continue;
                    }
                    let ts = frame.timestamp - audio.delay_ticks;
                    if ts >= 0 {
                        let a_units = mkv.ticks_to_units(ts, audio.timescale as u64);
                        let data = decode_block(frame.data, &audio.decoders, REMUX_MAX_FRAME_BYTES)
                            .unwrap_or_default();
                        self.transcode
                            .as_mut()
                            .unwrap()
                            .pending_audio
                            .push_back((a_units, data));
                    }
                }
            }
        }

        // シーク直後の最初の断片では、先頭映像フレームを覆う音声フレーム（最大 1 パケット手前）から始める
        let min_units = if self.audio_next.is_none() && first_video_pts > 0 {
            let v_in_audio = ((first_video_pts as i128 * timescale as i128)
                / REMUX_VIDEO_TIMESCALE as i128) as i64;
            let frame_dur = audio.frame_samples.max(960) as i64;
            (v_in_audio - frame_dur).max(0)
        } else {
            0
        };

        let pending = &mut self.transcode.as_mut().unwrap().pending_audio;
        let mut chosen = Vec::new();
        while let Some(&(pts, _)) = pending.front() {
            if pts > end_units {
                break;
            }
            let (pts, data) = pending.pop_front().unwrap();
            if pts >= min_units {
                chosen.push((pts, data));
            }
        }
        Ok(self.audio_samples_units(chosen))
    }

    /// Matroska の映像フレームを MP4 のサンプルにする（DTS を作る）
    fn video_samples_matroska(
        &self,
        frames: Vec<MkvFrame>,
        next_keyframe: Option<i64>,
    ) -> Option<TrackRun> {
        if frames.is_empty() {
            return None;
        }
        let StreamSource::Matroska(mkv) = &self.source else {
            return None;
        };
        let units = |t: i64| mkv.ticks_to_units(t, REMUX_VIDEO_TIMESCALE);
        let pts: Vec<i64> = frames.iter().map(|f| units(f.timestamp.max(0))).collect();
        let reorder = units(mkv.reorder);
        let next_dts = next_keyframe.map(|t| units(t.max(0)) - reorder);
        let fallback = units(self.video.frame_ticks).max(1);
        let timing = video_decode_times(&pts, reorder, next_dts, fallback);
        let samples = frames
            .into_iter()
            .zip(timing.durations)
            .zip(pts.iter().zip(&timing.dts))
            .map(|((frame, duration), (pts, dts))| Sample {
                data: decode_block(frame.data, &self.video.decoders, REMUX_MAX_FRAME_BYTES)
                    .unwrap_or_default(),
                duration: duration as u32,
                composition_offset: (pts - dts).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                keyframe: frame.keyframe,
            })
            .collect();
        Some(TrackRun {
            track_id: 1,
            base_decode_time: timing.dts[0] as u64,
            samples,
            has_composition_offsets: self.video.codec != RemuxVideoCodec::Av1,
        })
    }

    /// Matroska の音声フレームを MP4 のサンプルにする
    fn audio_samples_matroska(&mut self, mut frames: Vec<MkvFrame>) -> Option<TrackRun> {
        let audio = self.audio.clone()?;
        let StreamSource::Matroska(mkv) = &self.source else {
            return None;
        };
        // CodecDelay を引いて 0 秒より前になるフレーム（AAC のプライミングなど）は捨てる。
        // 0 に寄せると音声全体が遅れる
        for frame in &mut frames {
            frame.timestamp -= audio.delay_ticks;
        }
        frames.retain(|f| f.timestamp >= 0);
        let timescale = audio.timescale as u64;
        let units: Vec<(i64, Vec<u8>)> = frames
            .into_iter()
            .map(|frame| {
                let ts = mkv.ticks_to_units(frame.timestamp, timescale);
                let data = decode_block(frame.data, &audio.decoders, REMUX_MAX_FRAME_BYTES)
                    .unwrap_or_default();
                (ts, data)
            })
            .collect();
        self.audio_samples_units(units)
    }

    /// 出力 timescale 単位の (PTS, payload) 列から音声 TrackRun を作る。
    /// 前の断片から続くときは時刻をつなげ、継ぎ目に隙間・重なりを作らない
    fn audio_samples_units(&mut self, mut frames: Vec<(i64, Vec<u8>)>) -> Option<TrackRun> {
        let audio = self.audio.clone()?;
        frames.retain(|(pts, _)| *pts >= 0);
        let first_pts = frames.first()?.0;
        let timescale = audio.timescale as u64;
        let actual = first_pts.max(0) as u64;
        let tolerance = (REMUX_AUDIO_RESYNC_SECONDS * timescale as f64) as u64;
        let base = match self.audio_next {
            Some(next) if next.abs_diff(actual) <= tolerance => next,
            _ => actual,
        };
        let samples: Vec<Sample> = frames
            .into_iter()
            .map(|(_, data)| {
                let duration = match audio.codec {
                    RemuxAudioCodec::Aac => audio.frame_samples,
                    RemuxAudioCodec::Opus => opus_packet_samples(&data).unwrap_or(960),
                };
                Sample {
                    data,
                    duration,
                    composition_offset: 0,
                    keyframe: true,
                }
            })
            .collect();
        self.audio_next = Some(base + samples.iter().map(|s| s.duration as u64).sum::<u64>());
        Some(TrackRun {
            track_id: 2,
            base_decode_time: base,
            samples,
            has_composition_offsets: false,
        })
    }
}

/// GOP 1 つ（デコード順の表示時刻）の並べ替えの深さ: 昇順に並べた時刻 − 表示時刻 の最大
pub(crate) fn reorder_depth(pts: &[i64]) -> i64 {
    let mut sorted = pts.to_vec();
    sorted.sort_unstable();
    sorted
        .iter()
        .zip(pts)
        .map(|(s, p)| s - p)
        .max()
        .unwrap_or(0)
        .max(0)
}

pub(crate) struct DecodeTimes {
    pub(crate) dts: Vec<i64>,
    pub(crate) durations: Vec<i64>,
}

/// 映像の DTS を作る。MKV は表示時刻（PTS）しか持たないので、断片（GOP 単位）の PTS を
/// 昇順に並べてデコード順のフレームに割り当て、並べ替えの深さ `reorder` だけ全体を前へずらす
/// （DTS ≤ PTS になる）。ファイルの頭ではずらすと負になるので、最初の正の DTS までを
/// 0 から詰めて割り振る。`next_dts` は次の断片の最初の DTS（最後のフレームの長さに使う）
pub(crate) fn video_decode_times(
    pts: &[i64],
    reorder: i64,
    next_dts: Option<i64>,
    fallback: i64,
) -> DecodeTimes {
    let n = pts.len();
    let mut sorted = pts.to_vec();
    sorted.sort_unstable();
    let mut dts: Vec<i64> = sorted.iter().map(|s| s - reorder).collect();
    if dts[0] < 0 {
        // 頭から k 個が負。k 番目（最初の正の DTS）と、その間のフレームの PTS を超えないように詰める
        let k = dts.iter().position(|&d| d > 0).unwrap_or(n);
        let mut limit = if k < n { dts[k] } else { i64::MAX };
        for &p in &pts[1..k] {
            limit = limit.min(p);
        }
        if limit == i64::MAX {
            limit = 0;
        }
        for (i, d) in dts.iter_mut().take(k).enumerate() {
            *d = limit * i as i64 / k as i64;
        }
    }
    // 必ず増えていくようにする（時刻の重なったフレームがあっても壊れないように）
    for i in 1..n {
        if dts[i] <= dts[i - 1] {
            dts[i] = dts[i - 1] + 1;
        }
    }
    let mut durations: Vec<i64> = dts.windows(2).map(|w| w[1] - w[0]).collect();
    let last = next_dts
        .map(|next| next - dts[n - 1])
        .filter(|d| *d > 0)
        .or_else(|| durations.last().copied())
        .unwrap_or(fallback);
    durations.push(last.max(1));
    DecodeTimes { dts, durations }
}
