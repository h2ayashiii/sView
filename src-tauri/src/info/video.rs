// 動画の情報: コンテナ（MP4 / MOV / Matroska / WebM）のヘッダから、
// 映像・音声・字幕トラックのコーデックと形式を読む（mdat / Cluster の中身は読まない）

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use matroska_demuxer::{MatroskaFile, Primaries, TrackType, TransferCharacteristics};

use super::InfoSection;
use super::audio::read_tags_quiet;
use super::format::{
    format_aspect, format_bitrate, format_channels, format_duration, format_fps,
    format_sample_rate, format_size, group_digits,
};
use crate::mkv::mp4::{
    ParsedTrack, SliceBoxes, find_box, looks_like_mp4, mp4_audio_codec_name, parse_esds_asc,
    parse_mvhd, parse_trak, read_moov_bytes, read_u16, read_u32, read_u64,
};
use crate::mkv::remux::parse_aac_config;

/// MP4 の時刻の起点（1904-01-01）から UNIX 時刻の起点（1970-01-01）までの秒数
const MP4_EPOCH_OFFSET: u64 = 2_082_844_800;
/// Matroska の時刻の起点（2001-01-01）の UNIX 時刻（ミリ秒）
const MKV_EPOCH_MS: f64 = 978_307_200_000.0;

/// 動画ファイルの欄（「全般」「映像」「音声」「字幕」）
pub(crate) fn video_sections(path: &Path) -> Result<Vec<InfoSection>, String> {
    let mut file = File::open(path).map_err(|e| format!("ファイルを読めません: {e}"))?;
    let mut head = [0u8; 12];
    let read = file
        .read(&mut head)
        .map_err(|e| format!("ファイルを読めません: {e}"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| format!("ファイルを読めません: {e}"))?;
    if head[..read].starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        mkv_sections(file)
    } else if looks_like_mp4(&head[..read]) {
        mp4_sections(path, file)
    } else {
        Err("動画の形式を判定できません".into())
    }
}

/// 番号付きの見出し（トラックが 1 つなら番号を付けない）
fn numbered(title: &str, index: usize, count: usize) -> String {
    if count > 1 {
        format!("{title} {}", index + 1)
    } else {
        title.to_string()
    }
}

/// 言語コード（und は出さない）
fn language(lang: Option<&str>) -> Option<String> {
    lang.map(str::trim)
        .filter(|l| !l.is_empty() && *l != "und")
        .map(str::to_string)
}

// ---- コーデックの詳細（プロファイル・レベル・ビット深度） ----

/// コーデックの説明
#[derive(Default, Debug, PartialEq)]
pub(crate) struct CodecDetail {
    pub(crate) name: String,
    pub(crate) profile: Option<String>,
    pub(crate) bit_depth: Option<u8>,
}

impl CodecDetail {
    fn named(name: &str) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// H.264 の avcC（AVCDecoderConfigurationRecord）から「High@L4.1」
pub(crate) fn avc_detail(avcc: Option<&[u8]>) -> CodecDetail {
    let mut d = CodecDetail::named("H.264 (AVC)");
    if let Some(c) = avcc.filter(|c| c.len() >= 4) {
        let profile = match c[1] {
            66 => "Baseline".to_string(),
            77 => "Main".into(),
            88 => "Extended".into(),
            100 => "High".into(),
            110 => "High 10".into(),
            122 => "High 4:2:2".into(),
            244 => "High 4:4:4".into(),
            p => format!("Profile {p}"),
        };
        // level_idc 9 は Level 1b
        let level = match c[3] {
            9 => "1b".to_string(),
            l => format!("{}.{}", l / 10, l % 10),
        };
        d.profile = Some(format!("{profile}@L{level}"));
        if c[1] == 110 {
            d.bit_depth = Some(10);
        }
    }
    d
}

/// H.265 の hvcC（HEVCDecoderConfigurationRecord）から「Main 10@L5.1」とビット深度
pub(crate) fn hevc_detail(hvcc: Option<&[u8]>, name: &str) -> CodecDetail {
    let mut d = CodecDetail::named(name);
    if let Some(c) = hvcc.filter(|c| c.len() >= 19) {
        let profile = match c[1] & 0x1F {
            1 => "Main".to_string(),
            2 => "Main 10".into(),
            3 => "Main Still Picture".into(),
            4 => "Range Extensions".into(),
            p => format!("Profile {p}"),
        };
        let tier = if c[1] & 0x20 != 0 { " High tier" } else { "" };
        let level = c[12] as u32;
        let level = if level.is_multiple_of(30) {
            format!("{}", level / 30)
        } else {
            format!("{}.{}", level / 30, level % 30 / 3)
        };
        d.profile = Some(format!("{profile}@L{level}{tier}"));
        d.bit_depth = Some((c[17] & 0x07) + 8);
    }
    d
}

/// AV1 の av1C（AV1CodecConfigurationRecord）から「Main@L4.0」とビット深度
pub(crate) fn av1_detail(av1c: Option<&[u8]>) -> CodecDetail {
    let mut d = CodecDetail::named("AV1");
    if let Some(c) = av1c.filter(|c| c.len() >= 3 && c[0] & 0x80 != 0) {
        let profile = match c[1] >> 5 {
            0 => "Main",
            1 => "High",
            2 => "Professional",
            _ => "",
        };
        let level = c[1] & 0x1F;
        let level = if level == 31 {
            String::new()
        } else {
            format!("@L{}.{}", 2 + (level >> 2), level & 3)
        };
        if !profile.is_empty() {
            d.profile = Some(format!("{profile}{level}"));
        }
        let high = c[2] & 0x40 != 0;
        let twelve = c[2] & 0x20 != 0;
        d.bit_depth = Some(match (high, twelve) {
            (true, true) => 12,
            (true, false) => 10,
            _ => 8,
        });
    }
    d
}

/// VP9 のプロファイル・レベル・ビット深度
fn vp9_detail(profile: Option<u8>, level: Option<u8>, bit_depth: Option<u8>) -> CodecDetail {
    let mut d = CodecDetail::named("VP9");
    if let Some(p) = profile {
        let mut text = format!("Profile {p}");
        if let Some(l) = level.filter(|l| *l > 0) {
            text.push_str(&format!("@L{}.{}", l / 10, l % 10));
        }
        d.profile = Some(text);
    }
    d.bit_depth = bit_depth.filter(|b| *b > 0);
    d
}

/// Apple ProRes の種類
fn prores_name(fourcc: &[u8; 4]) -> Option<&'static str> {
    Some(match fourcc {
        b"apco" => "Apple ProRes 422 Proxy",
        b"apcs" => "Apple ProRes 422 LT",
        b"apcn" => "Apple ProRes 422",
        b"apch" => "Apple ProRes 422 HQ",
        b"ap4h" => "Apple ProRes 4444",
        b"ap4x" => "Apple ProRes 4444 XQ",
        _ => return None,
    })
}

/// 色域の名前（ISO/IEC 23091-2 の ColourPrimaries）
fn primaries_name(code: u16) -> Option<&'static str> {
    Some(match code {
        1 => "BT.709",
        5 | 6 => "BT.601",
        9 => "BT.2020",
        11 => "DCI-P3",
        12 => "Display P3",
        _ => return None,
    })
}

/// HDR の種類（ISO/IEC 23091-2 の TransferCharacteristics）
fn hdr_name(transfer: u16) -> Option<&'static str> {
    Some(match transfer {
        16 => "HDR10 (PQ)",
        18 => "HLG",
        _ => return None,
    })
}

/// AAC のオブジェクトタイプ名（AudioSpecificConfig から）
pub(crate) fn aac_name(asc: Option<&[u8]>) -> String {
    let object = asc.and_then(parse_aac_config).map(|(o, _, _)| o);
    match object {
        Some(1) => "AAC Main".into(),
        Some(2) => "AAC LC".into(),
        Some(3) => "AAC SSR".into(),
        Some(4) => "AAC LTP".into(),
        Some(5) => "HE-AAC".into(),
        Some(23) => "AAC LD".into(),
        Some(29) => "HE-AAC v2".into(),
        Some(39) => "AAC ELD".into(),
        _ => "AAC".into(),
    }
}

/// 映像の欄に共通の行
#[allow(clippy::too_many_arguments)]
fn push_video_rows(
    s: &mut InfoSection,
    codec: &CodecDetail,
    size: (u64, u64),
    display: Option<(u64, u64)>,
    fps: Option<f64>,
    bitrate: Option<f64>,
    primaries: Option<u16>,
    transfer: Option<u16>,
    dolby_vision: bool,
) {
    s.push("コーデック", codec.name.as_str());
    s.opt("プロファイル", codec.profile.as_deref());
    s.opt("解像度", format_size(size.0, size.1));
    let (dw, dh) = display.filter(|d| d.0 > 0 && d.1 > 0).unwrap_or(size);
    if (dw, dh) != size
        && let Some(text) = format_size(dw, dh)
    {
        s.push("表示サイズ", text);
    }
    s.opt("縦横比", format_aspect(dw, dh));
    s.opt("フレームレート", fps.and_then(format_fps));
    s.opt("ビットレート", bitrate.and_then(format_bitrate));
    s.opt("ビット深度", codec.bit_depth.map(|b| format!("{b} bit")));
    s.opt("色域", primaries.and_then(primaries_name));
    let hdr = transfer.and_then(hdr_name);
    s.opt(
        "HDR",
        match (dolby_vision, hdr) {
            (true, Some(h)) => Some(format!("Dolby Vision / {h}")),
            (true, None) => Some("Dolby Vision".into()),
            (false, h) => h.map(str::to_string),
        },
    );
}

// ---- MP4 / MOV ----

/// ftyp の major brand からコンテナの名前
fn mp4_container_name(brand: Option<[u8; 4]>) -> String {
    match brand.as_ref() {
        Some(b"qt  ") => "QuickTime (MOV)".into(),
        Some(b"M4V " | b"M4VH" | b"M4VP") => "MPEG-4 (M4V)".into(),
        Some(b"M4A " | b"M4B ") => "MPEG-4 Audio (M4A)".into(),
        Some(b) => {
            let brand = String::from_utf8_lossy(b).trim().to_string();
            format!("MPEG-4 ({brand})")
        }
        None => "MPEG-4".into(),
    }
}

/// 先頭の ftyp ボックスの major brand
fn read_major_brand<R: Read + Seek>(r: &mut R) -> Option<[u8; 4]> {
    r.seek(SeekFrom::Start(0)).ok()?;
    let mut head = [0u8; 12];
    r.read_exact(&mut head).ok()?;
    (&head[4..8] == b"ftyp").then(|| [head[8], head[9], head[10], head[11]])
}

/// stsz からサンプル数と合計バイト数（fragmented MP4 では 0）
fn sample_stats(stbl: &[u8]) -> (u64, u64) {
    let Some(stsz) = find_box(stbl, b"stsz") else {
        return (0, 0);
    };
    let (Some(fixed), Some(count)) = (read_u32(stsz, 4), read_u32(stsz, 8)) else {
        return (0, 0);
    };
    let count = count as u64;
    if fixed != 0 {
        return (count, fixed as u64 * count);
    }
    let total = stsz
        .get(12..)
        .unwrap_or_default()
        .chunks_exact(4)
        .take(count as usize)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]) as u64)
        .sum();
    (count, total)
}

/// mvhd の作成日時（UNIX 時刻・ミリ秒）。0（未設定）や 1970 年より前なら None
fn mvhd_creation_ms(moov: &[u8]) -> Option<f64> {
    let mvhd = find_box(moov, b"mvhd")?;
    let created = if *mvhd.first()? == 1 {
        read_u64(mvhd, 4)?
    } else {
        read_u32(mvhd, 4)? as u64
    };
    let unix = created.checked_sub(MP4_EPOCH_OFFSET).filter(|s| *s > 0)?;
    Some(unix as f64 * 1000.0)
}

/// 映像のサンプルエントリ（VisualSampleEntry）の欄
fn mp4_video_section(title: String, trak: &ParsedTrack<'_>) -> InfoSection {
    let mut s = InfoSection::new(title);
    let Some((kind, body)) = &trak.stsd_entry else {
        return s;
    };
    // VisualSampleEntry は先頭 78 バイト固定ヘッダの後に avcC / colr などの子ボックスが並ぶ
    let children = body.get(78..).unwrap_or_default();
    let dolby_vision = find_box(children, b"dvcC").is_some()
        || find_box(children, b"dvvC").is_some()
        || matches!(kind, b"dvh1" | b"dvhe" | b"dva1" | b"dvav");
    let codec = match kind {
        b"avc1" | b"avc3" | b"dva1" | b"dvav" => avc_detail(find_box(children, b"avcC")),
        b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => {
            hevc_detail(find_box(children, b"hvcC"), "H.265 (HEVC)")
        }
        b"av01" => av1_detail(find_box(children, b"av1C")),
        b"vp09" => {
            // vpcC: version / flags の後に profile, level, bitDepth(4) …
            let c = find_box(children, b"vpcC");
            vp9_detail(
                c.and_then(|c| c.get(4).copied()),
                c.and_then(|c| c.get(5).copied()),
                c.and_then(|c| c.get(6)).map(|b| b >> 4),
            )
        }
        b"vp08" => CodecDetail::named("VP8"),
        b"mp4v" => CodecDetail::named("MPEG-4 Visual"),
        b"jpeg" | b"mjpa" | b"mjpb" => CodecDetail::named("Motion JPEG"),
        b"s263" | b"h263" => CodecDetail::named("H.263"),
        other => {
            CodecDetail::named(prores_name(other).unwrap_or(String::from_utf8_lossy(other).trim()))
        }
    };
    let width = read_u16(body, 24).unwrap_or(0) as u64;
    let height = read_u16(body, 26).unwrap_or(0) as u64;
    let display = (trak.display.0 as u64, trak.display.1 as u64);
    let seconds = trak.duration as f64 / trak.timescale as f64;
    let (count, bytes) = sample_stats(trak.stbl);
    let fps = (count > 1 && seconds > 0.0).then(|| count as f64 / seconds);
    let bitrate = (bytes > 0 && seconds > 0.0).then(|| bytes as f64 * 8.0 / seconds);
    // colr（nclx / nclc）: colour_type(4) + primaries(2) + transfer(2) + matrix(2)
    let colr =
        find_box(children, b"colr").filter(|c| matches!(c.get(..4), Some(b"nclx" | b"nclc")));
    push_video_rows(
        &mut s,
        &codec,
        (width, height),
        Some(display),
        fps,
        bitrate,
        colr.and_then(|c| read_u16(c, 4)),
        colr.and_then(|c| read_u16(c, 6)),
        dolby_vision,
    );
    s.opt("言語", language(trak.language.as_deref()));
    s.opt("名前", trak.name.as_deref());
    s
}

/// 音声のサンプルエントリ（AudioSampleEntry）からコーデック名
fn mp4_audio_codec(kind: &[u8; 4], children: &[u8]) -> String {
    match kind {
        b"mp4a" => {
            let asc = find_box(children, b"esds").and_then(parse_esds_asc);
            aac_name(asc.as_deref())
        }
        other => mp4_audio_codec_name(other),
    }
}

/// 音声のサンプルエントリ（AudioSampleEntry）の欄
fn mp4_audio_section(title: String, trak: &ParsedTrack<'_>) -> InfoSection {
    let mut s = InfoSection::new(title);
    let Some((kind, body)) = &trak.stsd_entry else {
        return s;
    };
    // version 0 = 28 バイト、QuickTime の version 1 = 44 バイト、version 2 = 64 バイト
    let version = read_u16(body, 8).unwrap_or(0);
    let header_len = match version {
        1 => 44,
        2 => 64,
        _ => 28,
    };
    let children = body.get(header_len..).unwrap_or_default();
    let (mut channels, mut rate) = if version == 2 {
        (
            read_u32(body, 40).unwrap_or(0) as u64,
            read_u64(body, 32).map_or(0.0, f64::from_bits),
        )
    } else {
        (
            read_u16(body, 16).unwrap_or(0) as u64,
            read_u32(body, 24).map_or(0.0, |v| (v >> 16) as f64),
        )
    };
    let sample_bits = read_u16(body, 18).unwrap_or(0);
    // Opus は dOps の値の方が正しい（サンプルエントリの周波数は 48000 固定）
    if kind == b"Opus"
        && let Some(dops) = find_box(children, b"dOps")
        && dops.len() >= 11
    {
        channels = dops[1] as u64;
        rate = u32::from_be_bytes([dops[4], dops[5], dops[6], dops[7]]) as f64;
    }
    if rate <= 0.0 {
        rate = trak.timescale as f64;
    }
    s.push("コーデック", mp4_audio_codec(kind, children));
    s.opt("チャンネル", format_channels(channels));
    s.opt("サンプリング周波数", format_sample_rate(rate));
    // 圧縮形式の sample size は意味を持たない（多くは 16 固定）ので、非圧縮・可逆のときだけ出す
    if matches!(
        kind,
        b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"fl32" | b"alac" | b"fLaC"
    ) && sample_bits > 0
    {
        s.push("ビット深度", format!("{sample_bits} bit"));
    }
    let seconds = trak.duration as f64 / trak.timescale as f64;
    let (_, bytes) = sample_stats(trak.stbl);
    s.opt(
        "ビットレート",
        (bytes > 0 && seconds > 0.0)
            .then(|| bytes as f64 * 8.0 / seconds)
            .and_then(format_bitrate),
    );
    s.opt("言語", language(trak.language.as_deref()));
    s.opt("名前", trak.name.as_deref());
    s
}

/// 字幕トラックの欄
fn mp4_subtitle_section(title: String, trak: &ParsedTrack<'_>) -> InfoSection {
    let mut s = InfoSection::new(title);
    if let Some((kind, _)) = &trak.stsd_entry {
        let name = match kind {
            b"tx3g" | b"text" => "MPEG-4 Timed Text".to_string(),
            b"wvtt" => "WebVTT".into(),
            b"c608" => "CEA-608".into(),
            b"c708" => "CEA-708".into(),
            b"stpp" => "TTML".into(),
            other => String::from_utf8_lossy(other).trim().to_string(),
        };
        s.push("形式", name);
    }
    s.opt("言語", language(trak.language.as_deref()));
    s.opt("名前", trak.name.as_deref());
    s
}

/// MP4 のヘッダ（ftyp の major brand・moov の中身・ファイルの大きさ）
struct Mp4Header {
    brand: Option<[u8; 4]>,
    moov: Vec<u8>,
    file_len: u64,
}

/// MP4 の moov を読む
fn read_mp4_moov(file: File) -> Result<Mp4Header, String> {
    let len = file
        .metadata()
        .map_err(|e| format!("ファイルを読めません: {e}"))?
        .len();
    let mut r = BufReader::with_capacity(64 * 1024, file);
    let brand = read_major_brand(&mut r);
    let moov = read_moov_bytes(&mut r, len)
        .map_err(|e| format!("動画のヘッダを読めません: {e}"))?
        .ok_or("動画のヘッダ（moov）が見つかりません")?;
    Ok(Mp4Header {
        brand,
        moov,
        file_len: len,
    })
}

/// MP4 / MOV の欄
fn mp4_sections(path: &Path, file: File) -> Result<Vec<InfoSection>, String> {
    let Mp4Header {
        brand,
        moov,
        file_len,
    } = read_mp4_moov(file)?;
    let (movie_timescale, movie_duration) =
        parse_mvhd(&moov).ok_or("動画のヘッダが壊れています")?;
    let raw_traks: Vec<&[u8]> = SliceBoxes::new(&moov)
        .filter(|(k, _)| k == b"trak")
        .map(|(_, body)| body)
        .collect();
    // QuickTime / M4V のチャプターはテキストトラックで入っていて、ほかのトラックの
    // tref → chap から参照される。字幕ではないので一覧から除く
    let chapter_ids: Vec<u32> = raw_traks
        .iter()
        .filter_map(|t| find_box(find_box(t, b"tref")?, b"chap"))
        .flat_map(|chap| {
            chap.chunks_exact(4)
                .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        })
        .collect();
    let traks: Vec<ParsedTrack<'_>> = raw_traks
        .iter()
        .filter_map(|body| parse_trak(body, movie_timescale))
        .collect();
    let seconds = movie_duration.or_else(|| {
        traks
            .iter()
            .map(|t| t.duration as f64 / t.timescale as f64)
            .reduce(f64::max)
    });

    let mut general = InfoSection::new("全般");
    general.push("コンテナ", mp4_container_name(brand));
    general.opt("長さ", seconds.and_then(format_duration));
    general.opt(
        "全体のビットレート",
        seconds
            .filter(|s| *s > 0.0)
            .and_then(|s| format_bitrate(file_len as f64 * 8.0 / s)),
    );
    // タイトルなどのタグ（iTunes 形式の ilst）
    if let Some(tags) = read_tags_quiet(path, false) {
        use lofty::prelude::*;
        use lofty::tag::ItemKey;
        for tag in tags.primary_tag().into_iter().chain(tags.tags()) {
            if general.get("タイトル").is_none() {
                general.opt("タイトル", super::audio::non_empty_str(tag.title()));
            }
            if general.get("エンコーダ").is_none() {
                general.opt(
                    "エンコーダ",
                    tag.get_string(ItemKey::EncoderSoftware)
                        .map(str::trim)
                        .filter(|t| !t.is_empty()),
                );
            }
        }
    }
    if let Some(ms) = mvhd_creation_ms(&moov) {
        general.time("作成日時", ms);
    }
    let mut sections = vec![general];

    let is = |t: &ParsedTrack<'_>, handlers: &[&[u8; 4]]| handlers.contains(&&t.handler);
    let videos: Vec<_> = traks.iter().filter(|t| is(t, &[b"vide"])).collect();
    let audios: Vec<_> = traks.iter().filter(|t| is(t, &[b"soun"])).collect();
    let subs: Vec<_> = traks
        .iter()
        .filter(|t| is(t, &[b"sbtl", b"text", b"subt", b"clcp"]))
        .filter(|t| !chapter_ids.contains(&t.track_id))
        .collect();
    if !chapter_ids.is_empty() {
        sections[0].push("チャプター", "あり");
    }
    for (i, t) in videos.iter().enumerate() {
        sections.push(mp4_video_section(numbered("映像", i, videos.len()), t));
    }
    for (i, t) in audios.iter().enumerate() {
        sections.push(mp4_audio_section(numbered("音声", i, audios.len()), t));
    }
    for (i, t) in subs.iter().enumerate() {
        sections.push(mp4_subtitle_section(numbered("字幕", i, subs.len()), t));
    }
    Ok(sections)
}

/// M4A などの MP4 の音楽ファイルの、最初の音声トラックのコーデック名（「AAC LC」「ALAC」）
pub(crate) fn mp4_first_audio_codec(path: &Path) -> Option<String> {
    let moov = read_mp4_moov(File::open(path).ok()?).ok()?.moov;
    let (timescale, _) = parse_mvhd(&moov)?;
    SliceBoxes::new(&moov)
        .filter(|(k, _)| k == b"trak")
        .filter_map(|(_, body)| parse_trak(body, timescale))
        .filter(|t| &t.handler == b"soun")
        .find_map(|t| {
            let (kind, body) = t.stsd_entry.as_ref()?;
            let header_len = match read_u16(body, 8)? {
                1 => 44,
                2 => 64,
                _ => 28,
            };
            Some(mp4_audio_codec(kind, body.get(header_len..)?))
        })
}

// ---- Matroska / WebM ----

/// Matroska のコーデック ID を名前にする
pub(crate) fn mkv_codec_name(codec_id: &str, private: Option<&[u8]>) -> CodecDetail {
    match codec_id {
        "V_MPEG4/ISO/AVC" => avc_detail(private),
        "V_MPEGH/ISO/HEVC" => hevc_detail(private, "H.265 (HEVC)"),
        "V_AV1" => av1_detail(private),
        "V_VP9" => {
            // CodecPrivate は (ID, 長さ, 値) の並び。1 = profile、2 = level、3 = bit depth
            let mut values = [None; 3];
            let mut p = private.unwrap_or_default();
            while p.len() >= 3 {
                let (id, len) = (p[0], p[1] as usize);
                if (1..=3).contains(&id) && len == 1 {
                    values[id as usize - 1] = Some(p[2]);
                }
                p = p.get(2 + len..).unwrap_or_default();
            }
            vp9_detail(values[0], values[1], values[2])
        }
        "V_VP8" => CodecDetail::named("VP8"),
        "V_THEORA" => CodecDetail::named("Theora"),
        "V_MPEG2" => CodecDetail::named("MPEG-2 Video"),
        "V_MPEG1" => CodecDetail::named("MPEG-1 Video"),
        "V_MJPEG" => CodecDetail::named("Motion JPEG"),
        "V_PRORES" => CodecDetail::named("Apple ProRes"),
        "V_FFV1" => CodecDetail::named("FFV1"),
        id if id.starts_with("V_MPEG4/ISO/") => CodecDetail::named("MPEG-4 Visual"),
        id if id.starts_with("V_REAL/") => CodecDetail::named("RealVideo"),
        id if id.starts_with("A_AAC") => CodecDetail::named(&aac_name(private)),
        "A_OPUS" => CodecDetail::named("Opus"),
        "A_VORBIS" => CodecDetail::named("Vorbis"),
        "A_FLAC" => CodecDetail::named("FLAC"),
        "A_ALAC" => CodecDetail::named("ALAC"),
        "A_AC3" => CodecDetail::named("AC-3"),
        "A_EAC3" => CodecDetail::named("E-AC-3"),
        "A_TRUEHD" => CodecDetail::named("Dolby TrueHD"),
        "A_MPEG/L3" => CodecDetail::named("MP3"),
        "A_MPEG/L2" => CodecDetail::named("MP2"),
        id if id.starts_with("A_DTS") => CodecDetail::named("DTS"),
        id if id.starts_with("A_PCM/") => CodecDetail::named("PCM"),
        "S_TEXT/UTF8" => CodecDetail::named("SRT"),
        "S_TEXT/ASS" | "S_ASS" => CodecDetail::named("ASS"),
        "S_TEXT/SSA" | "S_SSA" => CodecDetail::named("SSA"),
        "S_TEXT/WEBVTT" => CodecDetail::named("WebVTT"),
        "S_HDMV/PGS" => CodecDetail::named("PGS（画像字幕）"),
        "S_VOBSUB" => CodecDetail::named("VobSub（画像字幕）"),
        "S_DVBSUB" => CodecDetail::named("DVB（画像字幕）"),
        other => CodecDetail::named(other),
    }
}

/// Matroska の列挙値を ISO/IEC 23091-2 の番号に戻す（primaries_name / hdr_name で使う）
fn mkv_primaries_code(p: Primaries) -> Option<u16> {
    Some(match p {
        Primaries::Bt709 => 1,
        Primaries::Bt601 => 5,
        Primaries::Smpte170 => 6,
        Primaries::Bt2020 => 9,
        Primaries::SmpteRp432_2 => 11,
        Primaries::SmpteEg432_2 => 12,
        _ => return None,
    })
}

fn mkv_transfer_code(t: TransferCharacteristics) -> Option<u16> {
    Some(match t {
        TransferCharacteristics::Bt2100 => 16,
        TransferCharacteristics::Hlg => 18,
        _ => return None,
    })
}

/// 非圧縮・可逆圧縮の音声か（ビット深度に意味があるもの）
fn is_lossless_mkv_audio(codec_id: &str) -> bool {
    codec_id.starts_with("A_PCM/")
        || matches!(codec_id, "A_FLAC" | "A_ALAC" | "A_TRUEHD" | "A_WAVPACK4")
}

/// 既定・強制のしるし
fn track_flags(default: bool, forced: bool) -> Option<String> {
    let flags: Vec<&str> = [(default, "既定"), (forced, "強制")]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
    (!flags.is_empty()).then(|| flags.join("・"))
}

/// Matroska / WebM の欄
fn mkv_sections(file: File) -> Result<Vec<InfoSection>, String> {
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mkv = MatroskaFile::open(BufReader::with_capacity(64 * 1024, file))
        .map_err(|e| format!("動画のヘッダを読めません: {e}"))?;
    let info = mkv.info();
    let seconds = info
        .duration()
        .map(|d| d * info.timestamp_scale().get() as f64 / 1e9);

    let mut general = InfoSection::new("全般");
    general.push(
        "コンテナ",
        match mkv.ebml_header().doc_type() {
            "webm" => "WebM",
            _ => "Matroska",
        },
    );
    general.opt("長さ", seconds.and_then(format_duration));
    general.opt(
        "全体のビットレート",
        seconds
            .filter(|s| *s > 0.0 && file_len > 0)
            .and_then(|s| format_bitrate(file_len as f64 * 8.0 / s)),
    );
    general.opt("タイトル", info.title());
    general.push("エンコーダ", info.writing_app());
    if info.muxing_app() != info.writing_app() {
        general.push("マルチプレクサ", info.muxing_app());
    }
    if let Some(ns) = info.date_utc() {
        general.time("作成日時", MKV_EPOCH_MS + ns as f64 / 1e6);
    }
    if let Some(chapters) = mkv.chapters() {
        let count: usize = chapters.iter().map(|e| e.chapter_atoms().len()).sum();
        if count > 0 {
            general.push("チャプター", format!("{} 個", group_digits(count as u64)));
        }
    }
    let mut sections = vec![general];

    let of = |kind: TrackType| {
        mkv.tracks()
            .iter()
            .filter(move |t| t.track_type() == kind)
            .collect::<Vec<_>>()
    };
    let videos = of(TrackType::Video);
    let audios = of(TrackType::Audio);
    let subs = of(TrackType::Subtitle);
    for (i, t) in videos.iter().enumerate() {
        let mut s = InfoSection::new(numbered("映像", i, videos.len()));
        let codec = mkv_codec_name(t.codec_id(), t.codec_private());
        let v = t.video();
        let size = v.map_or((0, 0), |v| (v.pixel_width().get(), v.pixel_height().get()));
        // DisplayUnit が画素（既定）か縦横比のときだけ、表示サイズとして使える
        let display = v.and_then(|v| {
            use matroska_demuxer::DisplayUnit;
            if !matches!(
                v.display_unit(),
                None | Some(DisplayUnit::Pixels | DisplayUnit::DisplayAspectRatio)
            ) {
                return None;
            }
            Some((v.display_width()?.get(), v.display_height()?.get()))
        });
        let colour = v.and_then(|v| v.colour());
        let mut codec = codec;
        if codec.bit_depth.is_none() {
            codec.bit_depth = colour
                .and_then(|c| c.bits_per_channel())
                .filter(|b| (1..=16).contains(b))
                .map(|b| b as u8);
        }
        push_video_rows(
            &mut s,
            &codec,
            size,
            display,
            t.default_duration().map(|d| 1e9 / d.get() as f64),
            None,
            colour
                .and_then(|c| c.primaries())
                .and_then(mkv_primaries_code),
            colour
                .and_then(|c| c.transfer_characteristics())
                .and_then(mkv_transfer_code),
            false,
        );
        s.opt("言語", language(t.language()));
        s.opt("名前", t.name());
        sections.push(s);
    }
    for (i, t) in audios.iter().enumerate() {
        let mut s = InfoSection::new(numbered("音声", i, audios.len()));
        s.push(
            "コーデック",
            mkv_codec_name(t.codec_id(), t.codec_private()).name,
        );
        if let Some(a) = t.audio() {
            s.opt("チャンネル", format_channels(a.channels().get()));
            let rate = a
                .output_sampling_frequency()
                .unwrap_or(a.sampling_frequency());
            s.opt("サンプリング周波数", format_sample_rate(rate));
            // 非可逆圧縮の BitDepth はデコード後の形式（ffmpeg は 32 などを書く）なので、
            // 非圧縮・可逆のときだけ出す
            if is_lossless_mkv_audio(t.codec_id()) {
                s.opt("ビット深度", a.bit_depth().map(|b| format!("{b} bit")));
            }
        }
        s.opt("言語", language(t.language()));
        s.opt("名前", t.name());
        if audios.len() > 1 {
            s.opt("フラグ", track_flags(t.flag_default(), t.flag_forced()));
        }
        sections.push(s);
    }
    for (i, t) in subs.iter().enumerate() {
        let mut s = InfoSection::new(numbered("字幕", i, subs.len()));
        s.push("形式", mkv_codec_name(t.codec_id(), t.codec_private()).name);
        s.opt("言語", language(t.language()));
        s.opt("名前", t.name());
        if subs.len() > 1 {
            s.opt("フラグ", track_flags(t.flag_default(), t.flag_forced()));
        }
        sections.push(s);
    }
    Ok(sections)
}
