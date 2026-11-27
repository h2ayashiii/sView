// MP4 / M4V / MOV（ISO BMFF）の軽量デマルチプレクサ。
// macOS で AV1 ハードウェアデコーダを持たない環境（M1 / M2 など）向けに、
// AV1 を含む MP4 / M4V / MOV から映像（av01）と音声（AAC / Opus）のパケットを
// 正確な PTS（elst のプライミング遅延・開始オフセットを含む）付きで取り出す。
// 通常の moov 形式と fragmented MP4（moof + mdat）の両方に対応する。

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use super::av1::{av1_codec_string, av1c_from_obu_stream};
use super::remux::{
    REMUX_MAX_FRAME_BYTES, REMUX_VIDEO_TIMESCALE, RemuxAudio, RemuxAudioCodec, RemuxAudioTrack,
    RemuxVideo, RemuxVideoCodec, invalid_data, parse_aac_config,
};

/// moov ボックス読み込みの上限（壊れたファイルで巨大な領域を確保しない）
const MAX_MOOV_BYTES: u64 = 128 * 1024 * 1024;
/// moof ボックス読み込みの上限
const MAX_MOOF_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct BoxHeader {
    kind: [u8; 4],
    /// ファイル先頭からのヘッダ開始位置
    start: u64,
    /// ペイロード開始位置
    body: u64,
    /// ボックス終端位置（ファイルサイズ上限でクランプ済み）
    end: u64,
}

fn read_box_header<R: Read + Seek>(r: &mut R, limit: u64) -> std::io::Result<Option<BoxHeader>> {
    let start = r.stream_position()?;
    if start + 8 > limit {
        return Ok(None);
    }
    let mut head = [0u8; 8];
    r.read_exact(&mut head)?;
    let size32 = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
    let kind = [head[4], head[5], head[6], head[7]];
    let (body, end) = match size32 {
        0 => (start + 8, limit),
        1 => {
            if start + 16 > limit {
                return Ok(None);
            }
            let mut ext = [0u8; 8];
            r.read_exact(&mut ext)?;
            let size64 = u64::from_be_bytes(ext);
            if size64 < 16 {
                return Err(invalid_data("MP4 ボックスの大きさが不正です"));
            }
            (start + 16, start.saturating_add(size64).min(limit))
        }
        s if s >= 8 => (start + 8, start.saturating_add(s).min(limit)),
        _ => return Err(invalid_data("MP4 ボックスの大きさが不正です")),
    };
    Ok(Some(BoxHeader {
        kind,
        start,
        body,
        end,
    }))
}

/// メモリ上のスライスから子ボックスを順に辿るイテレータ
struct SliceBoxes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceBoxes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl<'a> Iterator for SliceBoxes<'a> {
    type Item = ([u8; 4], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 8 > self.data.len() {
            return None;
        }
        let size32 = u32::from_be_bytes(self.data[self.pos..self.pos + 4].try_into().ok()?) as u64;
        let kind: [u8; 4] = self.data[self.pos + 4..self.pos + 8].try_into().ok()?;
        let (body, end) = match size32 {
            0 => (self.pos + 8, self.data.len()),
            1 => {
                if self.pos + 16 > self.data.len() {
                    return None;
                }
                let size64 =
                    u64::from_be_bytes(self.data[self.pos + 8..self.pos + 16].try_into().ok()?);
                if size64 < 16 {
                    return None;
                }
                (
                    self.pos + 16,
                    (self.pos as u64)
                        .saturating_add(size64)
                        .min(self.data.len() as u64) as usize,
                )
            }
            s if s >= 8 => (
                self.pos + 8,
                (self.pos as u64)
                    .saturating_add(s)
                    .min(self.data.len() as u64) as usize,
            ),
            _ => return None,
        };
        if body > end || end > self.data.len() {
            return None;
        }
        self.pos = end;
        Some((kind, &self.data[body..end]))
    }
}

fn find_box<'a>(data: &'a [u8], target: &[u8; 4]) -> Option<&'a [u8]> {
    SliceBoxes::new(data).find_map(|(k, b)| (&k == target).then_some(b))
}

fn read_u16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn read_u32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn read_i32(b: &[u8], off: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn read_u64(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

fn read_i64(b: &[u8], off: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// MP4 ファイルの先頭付近から moov を探し、AV1（av01）映像トラックがあるかを高速に調べる。
/// mdat は読み飛ばすため大きなファイルでも 1 ms 未満で終わる
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn is_mp4_av1(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    let mut r = BufReader::with_capacity(64 * 1024, file);
    let Some(moov) = read_moov_bytes(&mut r, len).ok().flatten() else {
        return false;
    };
    for (kind, trak) in SliceBoxes::new(&moov) {
        if &kind != b"trak" {
            continue;
        }
        let Some(mdia) = find_box(trak, b"mdia") else {
            continue;
        };
        let Some(minf) = find_box(mdia, b"minf") else {
            continue;
        };
        let Some(stbl) = find_box(minf, b"stbl") else {
            continue;
        };
        let Some(stsd) = find_box(stbl, b"stsd") else {
            continue;
        };
        // stsd: version(1) + flags(3) + entry_count(4) + entries...
        if let Some(entries) = stsd.get(8..)
            && SliceBoxes::new(entries).any(|(k, _)| &k == b"av01")
        {
            return true;
        }
    }
    false
}

/// ファイルが MP4 / MOV コンテナらしいか（先頭ボックスが ftyp / moov / mdat / free / wide / skip）
pub(crate) fn looks_like_mp4(header: &[u8]) -> bool {
    if header.len() < 8 {
        return false;
    }
    matches!(
        &header[4..8],
        b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip" | b"styp"
    )
}

fn read_moov_bytes<R: Read + Seek>(r: &mut R, file_len: u64) -> std::io::Result<Option<Vec<u8>>> {
    r.seek(SeekFrom::Start(0))?;
    while let Some(bx) = read_box_header(r, file_len)? {
        if &bx.kind == b"moov" {
            let size = bx.end.saturating_sub(bx.body);
            if size > MAX_MOOV_BYTES {
                return Err(invalid_data("MP4 のヘッダが大きすぎます"));
            }
            let mut buf = vec![0u8; size as usize];
            r.read_exact(&mut buf)?;
            return Ok(Some(buf));
        }
        r.seek(SeekFrom::Start(bx.end))?;
    }
    Ok(None)
}

/// esds ボックスのペイロード（version/flags 込み）から AudioSpecificConfig（タグ 0x05）を取り出す
pub(crate) fn parse_esds_asc(esds: &[u8]) -> Option<Vec<u8>> {
    let body = esds.get(4..)?;
    find_mpeg4_descriptor(body, 0x05)
}

fn read_desc_length(data: &[u8]) -> Option<(usize, usize)> {
    let mut len = 0usize;
    for (i, &b) in data.iter().take(4).enumerate() {
        len = (len << 7) | (b & 0x7F) as usize;
        if b & 0x80 == 0 {
            return Some((len, i + 1));
        }
    }
    None
}

fn find_mpeg4_descriptor(data: &[u8], target_tag: u8) -> Option<Vec<u8>> {
    let mut p = 0;
    while p < data.len() {
        let tag = data[p];
        p += 1;
        let (len, used) = read_desc_length(&data[p..])?;
        p += used;
        let payload = data.get(p..p + len)?;
        p += len;
        if tag == target_tag {
            return Some(payload.to_vec());
        }
        match tag {
            // ES_DescrTag: ES_ID(2) + flags(1) + 任意フィールド
            0x03 => {
                let mut inner = 3usize;
                let flags = *payload.get(2)?;
                if flags & 0x80 != 0 {
                    inner += 2; // streamDependenceFlag
                }
                if flags & 0x40 != 0 {
                    let url_len = *payload.get(inner)? as usize;
                    inner += 1 + url_len; // URL_Flag
                }
                if flags & 0x20 != 0 {
                    inner += 2; // OCRstreamFlag
                }
                if let Some(found) = find_mpeg4_descriptor(payload.get(inner..)?, target_tag) {
                    return Some(found);
                }
            }
            // DecoderConfigDescrTag: 13 バイト固定ヘッダの後に DecSpecificInfoTag
            0x04 => {
                if let Some(found) = find_mpeg4_descriptor(payload.get(13..)?, target_tag) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// mdhd の 16 ビット言語コード（ISO-639-2/T の 5 ビット × 3）を文字列へ戻す
fn parse_mdhd_language(packed: u16) -> Option<String> {
    let c1 = ((packed >> 10) & 0x1F) as u8;
    let c2 = ((packed >> 5) & 0x1F) as u8;
    let c3 = (packed & 0x1F) as u8;
    if c1 == 0 || c2 == 0 || c3 == 0 || c1 > 26 || c2 > 26 || c3 > 26 {
        return None;
    }
    let s = String::from_utf8(vec![c1 + 0x60, c2 + 0x60, c3 + 0x60]).ok()?;
    (s != "und").then_some(s)
}

fn mp4_audio_codec_name(kind: &[u8; 4]) -> String {
    match kind {
        b"mp4a" => "AAC".into(),
        b"Opus" => "Opus".into(),
        b"ac-3" => "AC-3".into(),
        b"ec-3" => "E-AC-3".into(),
        b"fLaC" => "FLAC".into(),
        b"alac" => "ALAC".into(),
        b".mp3" => "MP3".into(),
        b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"fl32" => "PCM".into(),
        other => String::from_utf8_lossy(other).into_owned(),
    }
}

/// Edit List（elst）から算出した時刻オフセット（トラック timescale 単位）。
/// `adjusted_pts = raw_pts - media_offset + empty_delay`
#[derive(Clone, Copy, Debug, Default)]
struct EditOffset {
    /// 先頭の空エディット（media_time == -1）による開始遅延（トラック timescale 単位）
    empty_delay: i64,
    /// 実メディアの切り出し開始時刻（AAC のプライミングサンプル数など。トラック timescale 単位）
    media_offset: i64,
}

fn parse_elst(trak: &[u8], movie_timescale: u32, track_timescale: u32) -> EditOffset {
    let Some(edts) = find_box(trak, b"edts") else {
        return EditOffset::default();
    };
    let Some(elst) = find_box(edts, b"elst") else {
        return EditOffset::default();
    };
    let Some(&version) = elst.first() else {
        return EditOffset::default();
    };
    let Some(count) = read_u32(elst, 4) else {
        return EditOffset::default();
    };
    let mut off = 8usize;
    let mut out = EditOffset::default();
    for _ in 0..count.min(16) {
        let (seg_dur, media_time) = if version == 1 {
            let (Some(d), Some(t)) = (read_u64(elst, off), read_i64(elst, off + 8)) else {
                break;
            };
            off += 20; // duration(8) + media_time(8) + rate(4)
            (d, t)
        } else {
            let (Some(d), Some(t)) = (read_u32(elst, off), read_i32(elst, off + 4)) else {
                break;
            };
            off += 12; // duration(4) + media_time(4) + rate(4)
            (d as u64, t as i64)
        };
        if media_time == -1 {
            if movie_timescale > 0 {
                out.empty_delay +=
                    (seg_dur as i128 * track_timescale as i128 / movie_timescale as i128) as i64;
            }
        } else {
            if media_time > 0 {
                out.media_offset = media_time;
            }
            break;
        }
    }
    out
}

/// MP4 のサンプル 1 つ（時刻は各トラックの timescale 単位、elst 適用済み）
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mp4Sample {
    pub(crate) offset: u64,
    pub(crate) size: u32,
    pub(crate) pts: i64,
    pub(crate) duration: u32,
    pub(crate) keyframe: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct TrexDefaults {
    track_id: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

/// moov のトラック 1 本分の解析結果
struct ParsedTrack {
    track_id: u32,
    handler: [u8; 4],
    timescale: u32,
    duration: u64,
    language: Option<String>,
    name: Option<String>,
    enabled: bool,
    display: (u32, u32),
    edit: EditOffset,
    stsd_entry: Option<([u8; 4], Vec<u8>)>,
    samples: Vec<Mp4Sample>,
}

fn parse_mvhd(moov: &[u8]) -> Option<(u32, Option<f64>)> {
    let mvhd = find_box(moov, b"mvhd")?;
    let version = *mvhd.first()?;
    let (timescale, duration) = if version == 1 {
        (read_u32(mvhd, 20)?, read_u64(mvhd, 24)?)
    } else {
        (read_u32(mvhd, 12)?, read_u32(mvhd, 16)? as u64)
    };
    if timescale == 0 {
        return None;
    }
    let dur_sec = (duration > 0).then_some(duration as f64 / timescale as f64);
    Some((timescale, dur_sec))
}

fn parse_trex_list(moov: &[u8]) -> Vec<TrexDefaults> {
    let Some(mvex) = find_box(moov, b"mvex") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (kind, body) in SliceBoxes::new(mvex) {
        if &kind != b"trex" || body.len() < 24 {
            continue;
        }
        if let (Some(track_id), Some(duration), Some(size), Some(flags)) = (
            read_u32(body, 4),
            read_u32(body, 12),
            read_u32(body, 16),
            read_u32(body, 20),
        ) {
            out.push(TrexDefaults {
                track_id,
                duration,
                size,
                flags,
            });
        }
    }
    out
}

fn parse_trak(trak: &[u8], movie_timescale: u32) -> Option<ParsedTrack> {
    let tkhd = find_box(trak, b"tkhd")?;
    let tkhd_ver = *tkhd.first()?;
    let tkhd_flags = u32::from_be_bytes([0, tkhd[1], tkhd[2], tkhd[3]]);
    let enabled = tkhd_flags & 1 != 0;
    let (track_id, wh_off) = if tkhd_ver == 1 {
        (read_u32(tkhd, 20)?, 88usize)
    } else {
        (read_u32(tkhd, 12)?, 76usize)
    };
    let disp_w = read_u32(tkhd, wh_off).map_or(0, |v| v >> 16);
    let disp_h = read_u32(tkhd, wh_off + 4).map_or(0, |v| v >> 16);

    let mdia = find_box(trak, b"mdia")?;
    let mdhd = find_box(mdia, b"mdhd")?;
    let mdhd_ver = *mdhd.first()?;
    let (timescale, duration, lang_packed) = if mdhd_ver == 1 {
        (
            read_u32(mdhd, 20)?,
            read_u64(mdhd, 24)?,
            read_u16(mdhd, 32)?,
        )
    } else {
        (
            read_u32(mdhd, 12)?,
            read_u32(mdhd, 16)? as u64,
            read_u16(mdhd, 20)?,
        )
    };
    if timescale == 0 {
        return None;
    }
    let language = parse_mdhd_language(lang_packed);

    let hdlr = find_box(mdia, b"hdlr")?;
    let handler: [u8; 4] = hdlr.get(8..12)?.try_into().ok()?;
    let name = hdlr.get(24..).and_then(|raw| {
        let s = String::from_utf8_lossy(raw);
        let s = s.trim_matches('\0').trim();
        // 既定の無意味なハンドラ名はトラック名として出さない
        if s.is_empty()
            || matches!(
                s,
                "SoundHandler" | "VideoHandler" | "Core Media Audio" | "Core Media Video"
            )
        {
            None
        } else {
            Some(s.to_string())
        }
    });

    let minf = find_box(mdia, b"minf")?;
    let stbl = find_box(minf, b"stbl")?;
    let stsd = find_box(stbl, b"stsd")?;
    let stsd_entry = stsd
        .get(8..)
        .and_then(|entries| SliceBoxes::new(entries).next())
        .map(|(k, b)| (k, b.to_vec()));

    let edit = parse_elst(trak, movie_timescale, timescale);
    let samples = parse_sample_table(stbl, edit);

    Some(ParsedTrack {
        track_id,
        handler,
        timescale,
        duration,
        language,
        name,
        enabled,
        display: (disp_w, disp_h),
        edit,
        stsd_entry,
        samples,
    })
}

/// 通常 MP4 の stbl（stts / ctts / stsc / stsz / stco / co64 / stss）からサンプル一覧を構築する
fn parse_sample_table(stbl: &[u8], edit: EditOffset) -> Vec<Mp4Sample> {
    // 1. サンプルサイズ（stsz）
    let Some(stsz) = find_box(stbl, b"stsz") else {
        return Vec::new();
    };
    let (Some(default_size), Some(sample_count)) = (read_u32(stsz, 4), read_u32(stsz, 8)) else {
        return Vec::new();
    };
    let sample_count = sample_count as usize;
    if sample_count == 0 {
        return Vec::new();
    }
    let mut sizes = Vec::with_capacity(sample_count);
    if default_size != 0 {
        sizes.resize(sample_count, default_size);
    } else {
        for i in 0..sample_count {
            let Some(s) = read_u32(stsz, 12 + i * 4) else {
                return Vec::new();
            };
            sizes.push(s);
        }
    }

    // 2. チャンクオフセット（stco または co64）
    let chunk_offsets: Vec<u64> = if let Some(stco) = find_box(stbl, b"stco") {
        let Some(cnt) = read_u32(stco, 4) else {
            return Vec::new();
        };
        (0..cnt as usize)
            .filter_map(|i| read_u32(stco, 8 + i * 4).map(|v| v as u64))
            .collect()
    } else if let Some(co64) = find_box(stbl, b"co64") {
        let Some(cnt) = read_u32(co64, 4) else {
            return Vec::new();
        };
        (0..cnt as usize)
            .filter_map(|i| read_u64(co64, 8 + i * 8))
            .collect()
    } else {
        return Vec::new();
    };

    // 3. Sample-to-Chunk（stsc）
    let Some(stsc) = find_box(stbl, b"stsc") else {
        return Vec::new();
    };
    let Some(stsc_cnt) = read_u32(stsc, 4) else {
        return Vec::new();
    };
    let mut stsc_entries = Vec::with_capacity(stsc_cnt as usize);
    for i in 0..stsc_cnt as usize {
        let off = 8 + i * 12;
        let (Some(first_chunk), Some(spc)) = (read_u32(stsc, off), read_u32(stsc, off + 4)) else {
            break;
        };
        if first_chunk >= 1 && spc > 0 {
            stsc_entries.push((first_chunk as usize - 1, spc as usize));
        }
    }
    let mut offsets = Vec::with_capacity(sample_count);
    let mut s_idx = 0usize;
    for (ei, &(first_chunk, spc)) in stsc_entries.iter().enumerate() {
        let next_chunk = stsc_entries
            .get(ei + 1)
            .map_or(chunk_offsets.len(), |&(c, _)| c)
            .min(chunk_offsets.len());
        for &chunk_off in &chunk_offsets[first_chunk.min(chunk_offsets.len())..next_chunk] {
            let mut cur_off = chunk_off;
            for _ in 0..spc {
                if s_idx >= sample_count {
                    break;
                }
                offsets.push(cur_off);
                cur_off += sizes[s_idx] as u64;
                s_idx += 1;
            }
            if s_idx >= sample_count {
                break;
            }
        }
        if s_idx >= sample_count {
            break;
        }
    }
    if offsets.len() < sample_count {
        return Vec::new();
    }

    // 4. Decoding Time to Sample（stts）
    let Some(stts) = find_box(stbl, b"stts") else {
        return Vec::new();
    };
    let Some(stts_cnt) = read_u32(stts, 4) else {
        return Vec::new();
    };
    let mut dts_list = Vec::with_capacity(sample_count);
    let mut dur_list = Vec::with_capacity(sample_count);
    let mut cur_dts = 0i64;
    for i in 0..stts_cnt as usize {
        let off = 8 + i * 8;
        let (Some(cnt), Some(delta)) = (read_u32(stts, off), read_u32(stts, off + 4)) else {
            break;
        };
        for _ in 0..cnt {
            if dts_list.len() >= sample_count {
                break;
            }
            dts_list.push(cur_dts);
            dur_list.push(delta);
            cur_dts += delta as i64;
        }
    }
    while dts_list.len() < sample_count {
        let delta = dur_list.last().copied().unwrap_or(1);
        dts_list.push(cur_dts);
        dur_list.push(delta);
        cur_dts += delta as i64;
    }

    // 5. Composition Time to Sample（ctts。任意）
    let mut ctts_list = vec![0i64; sample_count];
    if let Some(ctts) = find_box(stbl, b"ctts")
        && let (Some(&ver), Some(cnt)) = (ctts.first(), read_u32(ctts, 4))
    {
        let mut idx = 0usize;
        for i in 0..cnt as usize {
            let off = 8 + i * 8;
            let Some(n) = read_u32(ctts, off) else { break };
            let cto = if ver == 1 {
                read_i32(ctts, off + 4).unwrap_or(0) as i64
            } else {
                read_u32(ctts, off + 4).unwrap_or(0) as i64
            };
            for _ in 0..n {
                if idx >= sample_count {
                    break;
                }
                ctts_list[idx] = cto;
                idx += 1;
            }
        }
    }

    // 6. Sync Sample（stss。無ければ全サンプルがキーフレーム）
    let mut keyframes = if let Some(stss) = find_box(stbl, b"stss") {
        let mut flags = vec![false; sample_count];
        if let Some(cnt) = read_u32(stss, 4) {
            for i in 0..cnt as usize {
                if let Some(n) = read_u32(stss, 8 + i * 4)
                    && n >= 1
                    && (n as usize) <= sample_count
                {
                    flags[n as usize - 1] = true;
                }
            }
        }
        flags
    } else {
        vec![true; sample_count]
    };
    if !keyframes.is_empty() && !keyframes.iter().any(|&k| k) {
        keyframes[0] = true;
    }

    (0..sample_count)
        .map(|i| Mp4Sample {
            offset: offsets[i],
            size: sizes[i],
            pts: dts_list[i] + ctts_list[i] - edit.media_offset + edit.empty_delay,
            duration: dur_list[i],
            keyframe: keyframes[i],
        })
        .collect()
}

/// VisualSampleEntry（av01）から映像トラック情報を作る
fn parse_av1_video_track(trak: &ParsedTrack) -> Result<RemuxVideo, String> {
    let Some((kind, body)) = &trak.stsd_entry else {
        return Err("この動画の映像形式を読めません".into());
    };
    if kind != b"av01" {
        let name = String::from_utf8_lossy(kind);
        return Err(format!("この MP4 の映像（{name}）には対応していません"));
    }
    // VisualSampleEntry は先頭 78 バイト固定ヘッダの後に av1C / pasp などの子ボックスが並ぶ
    if body.len() < 78 {
        return Err("AV1 のヘッダが壊れています".into());
    }
    let width = read_u16(body, 24).unwrap_or(0) as u32;
    let height = read_u16(body, 26).unwrap_or(0) as u32;
    if width == 0 || height == 0 {
        return Err("AV1 の解像度が不正です".into());
    }
    let children = &body[78..];
    let av1c = find_box(children, b"av1C")
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let codec_string = av1_codec_string(&av1c).unwrap_or_else(|| "av01.0.04M.08".into());

    let mut display = if trak.display.0 > 0 && trak.display.1 > 0 {
        trak.display
    } else {
        (width, height)
    };
    if let Some(pasp) = find_box(children, b"pasp")
        && let (Some(hs), Some(vs)) = (read_u32(pasp, 0), read_u32(pasp, 4))
        && hs > 0
        && vs > 0
        && hs != vs
    {
        let dw = ((width as u64 * hs as u64 + vs as u64 / 2) / vs as u64).clamp(1, u16::MAX as u64);
        display = (dw as u32, height);
    }

    let frame_ticks = trak.samples.first().map_or(0, |s| s.duration as i64);

    Ok(RemuxVideo {
        number: trak.track_id as u64,
        codec: RemuxVideoCodec::Av1,
        config: av1c,
        codec_string,
        width,
        height,
        display,
        color: None,
        decoders: Vec::new(),
        frame_ticks,
    })
}

/// AudioSampleEntry（mp4a / Opus）から音声トラック情報を作る
fn parse_mp4_audio_track(trak: &ParsedTrack) -> Option<RemuxAudio> {
    let (kind, body) = trak.stsd_entry.as_ref()?;
    // AudioSampleEntry: version 0 = 28 bytes, version 1 (QuickTime) = 44 bytes, version 2 = 64 bytes
    let version = read_u16(body, 8)?;
    let header_len = match version {
        0 => 28usize,
        1 => 44,
        2 => 64,
        _ => return None,
    };
    let children = body.get(header_len..)?;
    let channels = read_u16(body, 16).unwrap_or(2).clamp(1, 255);
    match kind {
        b"mp4a" => {
            let esds = find_box(children, b"esds")?;
            let asc = parse_esds_asc(esds)?;
            let (object, core_rate, frame_samples) = parse_aac_config(&asc)?;
            Some(RemuxAudio {
                number: trak.track_id as u64,
                codec: RemuxAudioCodec::Aac,
                config: asc,
                codec_string: format!("mp4a.40.{object}"),
                channels,
                timescale: core_rate,
                frame_samples,
                delay_ticks: 0,
                decoders: Vec::new(),
            })
        }
        b"Opus" => {
            let dops = find_box(children, b"dOps")?;
            if dops.len() < 11 {
                return None;
            }
            Some(RemuxAudio {
                number: trak.track_id as u64,
                codec: RemuxAudioCodec::Opus,
                config: dops.to_vec(),
                codec_string: "opus".into(),
                channels: dops[1] as u16,
                timescale: 48000,
                frame_samples: 960,
                delay_ticks: 0,
                decoders: Vec::new(),
            })
        }
        _ => None,
    }
}

/// Fragmented MP4 の moof 1 つ分のサンプル一覧をパースする
fn parse_moof_samples(
    moof: &[u8],
    moof_file_pos: u64,
    video_id: u32,
    video_edit: EditOffset,
    audio_id: Option<u32>,
    audio_edit: EditOffset,
    trex: &[TrexDefaults],
) -> (Vec<Mp4Sample>, Vec<Mp4Sample>) {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for (kind, traf) in SliceBoxes::new(moof) {
        if &kind != b"traf" {
            continue;
        }
        let Some(tfhd) = find_box(traf, b"tfhd") else {
            continue;
        };
        if tfhd.len() < 8 {
            continue;
        }
        let tf_flags = u32::from_be_bytes([0, tfhd[1], tfhd[2], tfhd[3]]);
        let Some(track_id) = read_u32(tfhd, 4) else {
            continue;
        };
        let is_video = track_id == video_id;
        let is_audio = audio_id == Some(track_id);
        if !is_video && !is_audio {
            continue;
        }
        let defaults = trex
            .iter()
            .find(|t| t.track_id == track_id)
            .copied()
            .unwrap_or_default();
        let mut p = 8usize;
        let mut base_data_offset = if tf_flags & 0x00_0001 != 0 {
            let Some(v) = read_u64(tfhd, p) else { continue };
            p += 8;
            v
        } else {
            moof_file_pos
        };
        if tf_flags & 0x00_0002 != 0 {
            p += 4; // sample_description_index
        }
        let default_duration = if tf_flags & 0x00_0008 != 0 {
            let v = read_u32(tfhd, p).unwrap_or(defaults.duration);
            p += 4;
            v
        } else {
            defaults.duration
        };
        let default_size = if tf_flags & 0x00_0010 != 0 {
            let v = read_u32(tfhd, p).unwrap_or(defaults.size);
            p += 4;
            v
        } else {
            defaults.size
        };
        let default_flags = if tf_flags & 0x00_0020 != 0 {
            read_u32(tfhd, p).unwrap_or(defaults.flags)
        } else {
            defaults.flags
        };

        let base_decode_time = find_box(traf, b"tfdt").map_or(0i64, |tfdt| {
            let ver = tfdt.first().copied().unwrap_or(0);
            if ver == 1 {
                read_u64(tfdt, 4).unwrap_or(0) as i64
            } else {
                read_u32(tfdt, 4).unwrap_or(0) as i64
            }
        });
        let edit = if is_video { video_edit } else { audio_edit };

        let mut cur_dts = base_decode_time;
        for (ck, trun) in SliceBoxes::new(traf) {
            if &ck != b"trun" || trun.len() < 8 {
                continue;
            }
            let ver = trun[0];
            let tr_flags = u32::from_be_bytes([0, trun[1], trun[2], trun[3]]);
            let Some(count) = read_u32(trun, 4) else {
                continue;
            };
            let mut tp = 8usize;
            let mut cur_off = if tr_flags & 0x0001 != 0 {
                let data_off = read_i32(trun, tp).unwrap_or(0) as i64;
                tp += 4;
                (base_data_offset as i64 + data_off).max(0) as u64
            } else {
                base_data_offset
            };
            let first_flags = if tr_flags & 0x0004 != 0 {
                let f = read_u32(trun, tp);
                tp += 4;
                f
            } else {
                None
            };
            for i in 0..count {
                let dur = if tr_flags & 0x0100 != 0 {
                    let Some(v) = read_u32(trun, tp) else { break };
                    tp += 4;
                    v
                } else {
                    default_duration
                };
                let size = if tr_flags & 0x0200 != 0 {
                    let Some(v) = read_u32(trun, tp) else { break };
                    tp += 4;
                    v
                } else {
                    default_size
                };
                let s_flags = if tr_flags & 0x0400 != 0 {
                    let Some(v) = read_u32(trun, tp) else { break };
                    tp += 4;
                    v
                } else if i == 0 {
                    first_flags.unwrap_or(default_flags)
                } else {
                    default_flags
                };
                let cto = if tr_flags & 0x0800 != 0 {
                    let v = if ver == 1 {
                        read_i32(trun, tp).unwrap_or(0) as i64
                    } else {
                        read_u32(trun, tp).unwrap_or(0) as i64
                    };
                    tp += 4;
                    v
                } else {
                    0
                };
                let keyframe = (s_flags & 0x0001_0000) == 0;
                let sample = Mp4Sample {
                    offset: cur_off,
                    size,
                    pts: cur_dts + cto - edit.media_offset + edit.empty_delay,
                    duration: dur,
                    keyframe,
                };
                if is_video {
                    video.push(sample);
                } else {
                    audio.push(sample);
                }
                cur_off += size as u64;
                cur_dts += dur as i64;
            }
            base_data_offset = cur_off;
        }
    }
    (video, audio)
}

/// 開いた MP4 ファイルからのサンプル読み取り器
pub(crate) struct Mp4Source {
    reader: BufReader<File>,
    pub(crate) video_timescale: u32,
    pub(crate) audio_timescale: u32,
    video_samples: Vec<Mp4Sample>,
    audio_samples: Vec<Mp4Sample>,
    video_idx: usize,
    audio_idx: usize,
}

/// MP4 を開いたときのメタデータ
pub(crate) struct Mp4Meta {
    pub(crate) video: RemuxVideo,
    pub(crate) audio: Option<RemuxAudio>,
    pub(crate) audio_tracks: Vec<RemuxAudioTrack>,
    pub(crate) chosen_audio: Option<usize>,
    pub(crate) duration: Option<f64>,
    pub(crate) fps: f64,
}

impl Mp4Source {
    pub(crate) fn open(path: &Path, audio_choice: Option<i64>) -> Result<(Self, Mp4Meta), String> {
        let file = File::open(path).map_err(|e| format!("動画を開けません: {e}"))?;
        let file_len = file
            .metadata()
            .map_err(|e| format!("動画を開けません: {e}"))?
            .len();
        let mut reader = BufReader::with_capacity(256 * 1024, file);
        let moov = read_moov_bytes(&mut reader, file_len)
            .map_err(|e| format!("MP4 を読めません: {e}"))?
            .ok_or("MP4 のヘッダ（moov）が見つかりません")?;
        let (movie_timescale, mut duration) =
            parse_mvhd(&moov).ok_or("MP4 のヘッダ（mvhd）が壊れています")?;
        let trex = parse_trex_list(&moov);

        let tracks: Vec<ParsedTrack> = SliceBoxes::new(&moov)
            .filter(|(k, _)| k == b"trak")
            .filter_map(|(_, b)| parse_trak(b, movie_timescale))
            .collect();
        let video_trak = tracks
            .iter()
            .find(|t| &t.handler == b"vide")
            .ok_or("この動画には映像がありません")?;
        let mut video = parse_av1_video_track(video_trak)?;

        let audio_traks: Vec<&ParsedTrack> =
            tracks.iter().filter(|t| &t.handler == b"soun").collect();
        let remuxable: Vec<Option<RemuxAudio>> = audio_traks
            .iter()
            .map(|t| parse_mp4_audio_track(t))
            .collect();
        let any_enabled = audio_traks.iter().any(|t| t.enabled);
        let audio_tracks: Vec<RemuxAudioTrack> = audio_traks
            .iter()
            .zip(&remuxable)
            .enumerate()
            .map(|(i, (t, r))| RemuxAudioTrack {
                name: t.name.clone(),
                language: t.language.clone(),
                codec: t
                    .stsd_entry
                    .as_ref()
                    .map_or_else(|| "不明".into(), |(k, _)| mp4_audio_codec_name(k)),
                codec_string: r.as_ref().map(|r| r.codec_string.clone()),
                default: if any_enabled { t.enabled } else { i == 0 },
            })
            .collect();
        let chosen_audio = match audio_choice {
            Some(i) => usize::try_from(i).ok().filter(|&i| i < audio_tracks.len()),
            None => (0..audio_tracks.len())
                .find(|&i| audio_tracks[i].default && remuxable[i].is_some())
                .or_else(|| (0..audio_tracks.len()).find(|&i| remuxable[i].is_some())),
        };
        let audio_track = chosen_audio.and_then(|i| remuxable[i].clone());
        let selected_audio_trak = chosen_audio
            .filter(|_| audio_track.is_some())
            .map(|i| audio_traks[i]);

        let mut video_samples = video_trak.samples.clone();
        let mut audio_samples = selected_audio_trak
            .map(|t| t.samples.clone())
            .unwrap_or_default();

        // 通常 MP4 のサンプルが空で fragmented MP4（moof + mdat）なら moof ヘッダを集める
        if video_samples.is_empty() {
            reader
                .seek(SeekFrom::Start(0))
                .map_err(|e| format!("MP4 を読めません: {e}"))?;
            let audio_id = selected_audio_trak.map(|t| t.track_id);
            let audio_edit = selected_audio_trak.map_or(EditOffset::default(), |t| t.edit);
            while let Some(bx) = read_box_header(&mut reader, file_len)
                .map_err(|e| format!("MP4 を読めません: {e}"))?
            {
                if &bx.kind == b"moof" {
                    let sz = bx.end.saturating_sub(bx.body);
                    if sz <= MAX_MOOF_BYTES {
                        let mut moof = vec![0u8; sz as usize];
                        reader
                            .read_exact(&mut moof)
                            .map_err(|e| format!("MP4 を読めません: {e}"))?;
                        let (vs, as_) = parse_moof_samples(
                            &moof,
                            bx.start,
                            video_trak.track_id,
                            video_trak.edit,
                            audio_id,
                            audio_edit,
                            &trex,
                        );
                        video_samples.extend(vs);
                        audio_samples.extend(as_);
                    }
                }
                reader
                    .seek(SeekFrom::Start(bx.end))
                    .map_err(|e| format!("MP4 を読めません: {e}"))?;
            }
        }
        if video_samples.is_empty() {
            return Err("この動画に映像フレームが見つかりません".into());
        }

        // av1C がコンテナに無ければ最初のフレームの OBU から補う
        if video.config.is_empty()
            && let Some(first) = video_samples.first()
            && (first.size as u64) <= REMUX_MAX_FRAME_BYTES
        {
            let mut buf = vec![0u8; first.size as usize];
            if reader.seek(SeekFrom::Start(first.offset)).is_ok()
                && reader.read_exact(&mut buf).is_ok()
                && let Some(av1c) = av1c_from_obu_stream(&buf)
            {
                if let Some(cs) = av1_codec_string(&av1c) {
                    video.codec_string = cs;
                }
                video.config = av1c;
            }
        }

        // トラックの実際の長さから duration を補完（fragmented MP4 では mvhd.duration が 0 になりがち）
        if duration.is_none() {
            if video_trak.duration > 0 {
                duration = Some(video_trak.duration as f64 / video_trak.timescale as f64);
            } else if let Some(last) = video_samples.last() {
                let end =
                    (last.pts + last.duration as i64).max(0) as f64 / video_trak.timescale as f64;
                if end > 0.0 {
                    duration = Some(end);
                }
            }
        }

        // 1 フレームあたりの長さ（90 kHz 単位）と fps を推定する
        let avg_dur = video_samples
            .iter()
            .take(30)
            .map(|s| s.duration as f64)
            .filter(|&d| d > 0.0)
            .collect::<Vec<_>>();
        let fps = if !avg_dur.is_empty() {
            let mean = avg_dur.iter().sum::<f64>() / avg_dur.len() as f64;
            (video_trak.timescale as f64 / mean).clamp(1.0, 240.0)
        } else {
            30.0
        };
        video.frame_ticks = (REMUX_VIDEO_TIMESCALE as f64 / fps).round().max(1.0) as i64;

        let audio_timescale = selected_audio_trak
            .map_or(audio_track.as_ref().map_or(48_000, |a| a.timescale), |t| {
                t.timescale
            });

        let meta = Mp4Meta {
            video,
            audio: audio_track.clone(),
            audio_tracks,
            chosen_audio: chosen_audio.filter(|_| audio_track.is_some()),
            duration,
            fps,
        };
        let source = Self {
            reader,
            video_timescale: video_trak.timescale,
            audio_timescale,
            video_samples,
            audio_samples,
            video_idx: 0,
            audio_idx: 0,
        };
        Ok((source, meta))
    }

    /// 映像のサンプル時刻（トラック timescale 単位）を 90 kHz に換算する
    pub(crate) fn video_pts_90k(&self, raw_pts: i64) -> i64 {
        let num = raw_pts as i128 * REMUX_VIDEO_TIMESCALE as i128;
        let den = self.video_timescale as i128;
        let half = if num >= 0 { den / 2 } else { -(den / 2) };
        ((num + half) / den) as i64
    }

    /// 音声のサンプル時刻（トラック timescale 単位）を出力音声 timescale に換算する
    pub(crate) fn audio_pts_units(&self, raw_pts: i64, out_timescale: u32) -> i64 {
        if self.audio_timescale == out_timescale {
            raw_pts
        } else {
            let num = raw_pts as i128 * out_timescale as i128;
            let den = self.audio_timescale as i128;
            let half = if num >= 0 { den / 2 } else { -(den / 2) };
            ((num + half) / den) as i64
        }
    }

    /// `seconds` の直前の映像キーフレームへシークし、そのキーフレームの時刻（秒）を返す。
    /// 音声の読み込み位置も目標時刻の少し手前から揃える
    pub(crate) fn seek(&mut self, seconds: f64) -> f64 {
        let target_v = (seconds.max(0.0) * self.video_timescale as f64) as i64;
        // 目標時刻以下で最も後ろのキーフレームを探す
        let mut kf_idx = 0usize;
        for (i, s) in self.video_samples.iter().enumerate() {
            if s.pts > target_v {
                break;
            }
            if s.keyframe {
                kf_idx = i;
            }
        }
        self.video_idx = kf_idx;
        let kf_sec = self.video_samples[kf_idx].pts.max(0) as f64 / self.video_timescale as f64;

        // 音声はキーフレーム時刻と目標時刻の早い方から読める位置に合わせる
        // （トランスコード時は映像の最初の出力フレームに合わせて不要な先頭音声を間引く）
        let audio_target_sec = kf_sec.min(seconds.max(0.0));
        self.seek_audio(audio_target_sec);
        kf_sec
    }

    /// 音声の読み込み位置だけを `seconds` の直前へ移す（変換シーク時にキーフレーム〜目標時刻の不要な音声読み込みを省く）
    pub(crate) fn seek_audio(&mut self, seconds: f64) {
        let target_a = (seconds.max(0.0) * self.audio_timescale as f64) as i64;
        self.audio_idx = self
            .audio_samples
            .partition_point(|s| s.pts < target_a)
            .saturating_sub(1);
    }

    /// BufReader の 256 KB バッファを無駄に捨てないよう seek_relative で移動する
    fn move_to(&mut self, pos: u64) -> std::io::Result<()> {
        let here = self.reader.stream_position()?;
        if let Ok(delta) = i64::try_from(pos as i128 - here as i128) {
            self.reader.seek_relative(delta)
        } else {
            self.reader.seek(SeekFrom::Start(pos)).map(|_| ())
        }
    }

    /// 次の映像サンプルを読む。戻り値は (90 kHz PTS, keyframe, payload)
    pub(crate) fn next_video(&mut self) -> std::io::Result<Option<(i64, bool, Vec<u8>)>> {
        let Some(&sample) = self.video_samples.get(self.video_idx) else {
            return Ok(None);
        };
        self.video_idx += 1;
        if sample.size as u64 > REMUX_MAX_FRAME_BYTES {
            return Ok(Some((
                self.video_pts_90k(sample.pts),
                sample.keyframe,
                Vec::new(),
            )));
        }
        let mut data = vec![0u8; sample.size as usize];
        self.move_to(sample.offset)?;
        self.reader.read_exact(&mut data)?;
        Ok(Some((
            self.video_pts_90k(sample.pts),
            sample.keyframe,
            data,
        )))
    }

    /// 次に読む映像サンプルの 90 kHz PTS（先読み用・カーソルは進めない）
    pub(crate) fn peek_video_pts_90k(&self) -> Option<i64> {
        self.video_samples
            .get(self.video_idx)
            .map(|s| self.video_pts_90k(s.pts))
    }

    /// 次に読む映像サンプルがキーフレームかどうか（先読み用）
    pub(crate) fn peek_video_keyframe(&self) -> Option<bool> {
        self.video_samples.get(self.video_idx).map(|s| s.keyframe)
    }

    /// `until_sec`（None なら最後まで）以下の音声サンプルをすべて読み出す。
    /// 戻り値は (出力 timescale 単位の PTS, payload)
    pub(crate) fn read_audio_until(
        &mut self,
        until_sec: Option<f64>,
        out_timescale: u32,
    ) -> std::io::Result<Vec<(i64, Vec<u8>)>> {
        let limit = until_sec.map(|s| (s * self.audio_timescale as f64).ceil() as i64);
        let mut out = Vec::new();
        while let Some(&sample) = self.audio_samples.get(self.audio_idx) {
            if let Some(lim) = limit
                && sample.pts > lim
            {
                break;
            }
            self.audio_idx += 1;
            if sample.size as u64 > REMUX_MAX_FRAME_BYTES {
                continue;
            }
            let mut data = vec![0u8; sample.size as usize];
            self.move_to(sample.offset)?;
            self.reader.read_exact(&mut data)?;
            out.push((self.audio_pts_units(sample.pts, out_timescale), data));
        }
        Ok(out)
    }
}
