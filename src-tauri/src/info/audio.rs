// 音楽の情報: 音声の形式（lofty が読むヘッダ）とタグ・アートワーク

use std::path::Path;

use lofty::file::{FileType, TaggedFile};

use super::InfoSection;
use super::format::{format_bitrate, format_channels, format_duration, format_sample_rate};
use super::video::mp4_first_audio_codec;
use crate::media::has_ext;

/// lofty でファイルを読む（読めなくても警告を出さない。動画の MP4 などにも使うため）
pub(crate) fn read_tags_quiet(path: &Path, properties: bool) -> Option<TaggedFile> {
    use lofty::config::ParseOptions;
    use lofty::probe::Probe;
    Probe::open(path)
        .ok()?
        .guess_file_type()
        .ok()?
        .options(ParseOptions::new().read_properties(properties))
        .read()
        .ok()
}

/// 前後の空白を除き、空なら None
pub(crate) fn non_empty_str(text: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// lofty の形式名（MP4 はトラックのコーデックを見て AAC / ALAC を分ける）
fn codec_name(file_type: FileType, path: &Path) -> Option<String> {
    Some(match file_type {
        FileType::Aac => "AAC (ADTS)".into(),
        FileType::Aiff => "PCM (AIFF)".into(),
        FileType::Ape => "Monkey's Audio".into(),
        FileType::Flac => "FLAC".into(),
        FileType::Mpeg if has_ext(path, &["mp3"]) => "MP3".into(),
        FileType::Mpeg => "MPEG Audio".into(),
        FileType::Mp4 => mp4_first_audio_codec(path).unwrap_or_else(|| "MPEG-4 Audio".into()),
        FileType::Mpc => "Musepack".into(),
        FileType::Opus => "Opus".into(),
        FileType::Vorbis => "Vorbis".into(),
        FileType::Speex => "Speex".into(),
        FileType::Wav => "PCM (WAV)".into(),
        FileType::WavPack => "WavPack".into(),
        _ => return None,
    })
}

/// 音楽ファイルの欄（「音声」と、タグがあれば「タグ」）
pub(crate) fn audio_sections(path: &Path) -> Result<Vec<InfoSection>, String> {
    use lofty::prelude::*;
    use lofty::tag::ItemKey;

    let file = read_tags_quiet(path, true).ok_or("音声の形式を読めません")?;
    let props = file.properties();
    let mut audio = InfoSection::new("音声");
    audio.opt("コーデック", codec_name(file.file_type(), path));
    audio.opt("長さ", format_duration(props.duration().as_secs_f64()));
    let bitrate = props.audio_bitrate().or(props.overall_bitrate());
    audio.opt(
        "ビットレート",
        bitrate.and_then(|kbps| format_bitrate(kbps as f64 * 1000.0)),
    );
    audio.opt(
        "サンプリング周波数",
        props
            .sample_rate()
            .and_then(|r| format_sample_rate(r as f64)),
    );
    audio.opt("ビット深度", props.bit_depth().map(|b| format!("{b} bit")));
    audio.opt(
        "チャンネル",
        props.channels().and_then(|c| format_channels(c as u64)),
    );
    let mut sections = vec![audio];

    // 主となるタグを先に見て、足りない項目はほかのタグで補う
    let mut tags = InfoSection::new("タグ");
    let mut pictures = Vec::new();
    for tag in file.primary_tag().into_iter().chain(file.tags()) {
        let text = |key: ItemKey| {
            tag.get_string(key)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        };
        let fill = |s: &mut InfoSection, label: &str, value: Option<String>| {
            if s.get(label).is_none() {
                s.opt(label, value);
            }
        };
        fill(&mut tags, "曲名", non_empty_str(tag.title()));
        fill(&mut tags, "アーティスト", non_empty_str(tag.artist()));
        fill(&mut tags, "アルバム", non_empty_str(tag.album()));
        fill(
            &mut tags,
            "アルバムアーティスト",
            text(ItemKey::AlbumArtist),
        );
        fill(&mut tags, "作曲者", text(ItemKey::Composer));
        fill(&mut tags, "ジャンル", non_empty_str(tag.genre()));
        fill(
            &mut tags,
            "日付",
            tag.date()
                .map(|d| d.to_string())
                .or_else(|| text(ItemKey::Year)),
        );
        let numbered = |n: Option<u32>, total: Option<u32>| match (n, total) {
            (Some(n), Some(t)) if t > 0 => Some(format!("{n} / {t}")),
            (Some(n), _) => Some(n.to_string()),
            _ => None,
        };
        fill(
            &mut tags,
            "トラック",
            numbered(tag.track(), tag.track_total()),
        );
        fill(
            &mut tags,
            "ディスク",
            numbered(tag.disk(), tag.disk_total()),
        );
        fill(&mut tags, "コメント", non_empty_str(tag.comment()));
        fill(&mut tags, "エンコーダ", text(ItemKey::EncoderSoftware));
        if pictures.is_empty() {
            pictures = tag.pictures().iter().map(|p| p.data()).collect();
        }
    }
    // 埋め込みアートワーク（1 枚目の形式と大きさ）
    if let Some(first) = pictures.first() {
        let mut text = String::new();
        if let Ok(kind) = imagesize::image_type(first) {
            let name = match kind {
                imagesize::ImageType::Jpeg => "JPEG",
                imagesize::ImageType::Png => "PNG",
                imagesize::ImageType::Webp => "WebP",
                imagesize::ImageType::Gif => "GIF",
                imagesize::ImageType::Bmp => "BMP",
                _ => "",
            };
            text.push_str(name);
        }
        if let Ok(size) = imagesize::blob_size(first) {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!("{} × {}", size.width, size.height));
        }
        if pictures.len() > 1 {
            text.push_str(&format!("（ほか {} 枚）", pictures.len() - 1));
        }
        tags.push(
            "アートワーク",
            if text.is_empty() {
                "あり".into()
            } else {
                text
            },
        );
    }
    if !tags.rows.is_empty() {
        sections.push(tags);
    }
    Ok(sections)
}
