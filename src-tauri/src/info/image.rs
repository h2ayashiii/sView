// 画像の情報: 形式・解像度（ヘッダだけを読む）と Exif（撮影機材・撮影条件・撮影日時・位置）

use std::io::{BufRead, Seek, SeekFrom};
use std::path::Path;

use exif::{In, Tag, Value};

use super::InfoSection;
use super::format::{format_size, trim_float};
use crate::media::has_ext;

/// 画像の形式名（中身から判定する。分からなければ拡張子）
fn image_format(kind: Option<imagesize::ImageType>, path: &Path) -> Option<String> {
    use imagesize::ImageType;
    let name = match kind {
        Some(ImageType::Jpeg) => "JPEG",
        Some(ImageType::Png) => "PNG",
        Some(ImageType::Gif) => "GIF",
        Some(ImageType::Webp) => "WebP",
        Some(ImageType::Bmp) => "BMP",
        Some(ImageType::Ico) => "ICO",
        Some(ImageType::Tiff) => "TIFF",
        Some(ImageType::Heif(imagesize::Compression::Av1)) => "AVIF",
        Some(ImageType::Heif(_)) => "HEIF",
        _ if has_ext(path, &["svg"]) => "SVG",
        _ => return super::ext_upper(path),
    };
    Some(name.into())
}

/// PNG の IHDR から色の深さと色の種類を読む（「8 bit / RGBA」）
fn png_color<R: BufRead + Seek>(r: &mut R) -> Option<String> {
    let mut head = [0u8; 26];
    r.seek(SeekFrom::Start(0)).ok()?;
    r.read_exact(&mut head).ok()?;
    if &head[12..16] != b"IHDR" {
        return None;
    }
    let color = match head[25] {
        0 => "グレースケール",
        2 => "RGB",
        3 => "インデックスカラー",
        4 => "グレースケール + アルファ",
        6 => "RGBA",
        _ => return None,
    };
    Some(format!("{} bit / {color}", head[24]))
}

/// 画像の欄（「画像」と、Exif があれば「撮影情報」）
pub(crate) fn image_sections<R: BufRead + Seek>(r: &mut R, path: &Path) -> Vec<InfoSection> {
    let mut section = InfoSection::new("画像");
    let mut head = [0u8; 64];
    let read = r
        .seek(SeekFrom::Start(0))
        .and_then(|_| r.read(&mut head))
        .unwrap_or(0);
    let kind = imagesize::image_type(&head[..read]).ok();
    section.opt("形式", image_format(kind, path));
    if r.seek(SeekFrom::Start(0)).is_ok()
        && let Ok(size) = imagesize::reader_size(&mut *r)
        && let Some(text) = format_size(size.width as u64, size.height as u64)
    {
        let mp = size.width as f64 * size.height as f64 / 1_000_000.0;
        let text = if mp >= 0.1 {
            format!("{text}（{} MP）", trim_float(mp, 1))
        } else {
            text
        };
        section.push("解像度", text);
    }
    if matches!(kind, Some(imagesize::ImageType::Png)) {
        section.opt("色", png_color(r));
    }

    let mut sections = vec![section];
    // SVG / ICO / BMP / GIF には Exif が入らない
    let exif = r.seek(SeekFrom::Start(0)).ok().and_then(|_| {
        exif::Reader::new()
            .continue_on_error(true)
            .read_from_container(r)
            .or_else(|e| e.distill_partial_result(|_| {}))
            .ok()
    });
    if let Some(exif) = exif {
        let camera = exif_section(&exif);
        if !camera.rows.is_empty() {
            sections.push(camera);
        }
    }
    sections
}

/// Exif の文字列（前後の空白と NUL を除く）
fn exif_text(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    let Value::Ascii(parts) = &field.value else {
        return None;
    };
    let text = parts
        .iter()
        .map(|p| {
            String::from_utf8_lossy(p)
                .trim_matches(['\0', ' '])
                .to_string()
        })
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

fn exif_uint(exif: &exif::Exif, tag: Tag) -> Option<u32> {
    exif.get_field(tag, In::PRIMARY)?.value.get_uint(0)
}

/// Exif の数値（有理数・整数どちらでも）
fn exif_f64(exif: &exif::Exif, tag: Tag) -> Option<f64> {
    let value = &exif.get_field(tag, In::PRIMARY)?.value;
    let v = match value {
        Value::Rational(v) => v.first().filter(|r| r.denom != 0)?.to_f64(),
        Value::SRational(v) => v.first().filter(|r| r.denom != 0)?.to_f64(),
        _ => value.get_uint(0)? as f64,
    };
    v.is_finite().then_some(v)
}

/// 露出時間（「1/250 秒」「2.5 秒」）
pub(crate) fn format_exposure(seconds: f64) -> Option<String> {
    if seconds <= 0.0 {
        return None;
    }
    Some(if seconds < 1.0 {
        format!("1/{} 秒", trim_float(1.0 / seconds, 1))
    } else {
        format!("{} 秒", trim_float(seconds, 1))
    })
}

/// Exif の日時（「2024:01:02 03:04:05」）を「2024-01-02 03:04:05」にする
pub(crate) fn format_exif_datetime(text: &str, offset: Option<&str>) -> Option<String> {
    let text = text.trim();
    if text.len() < 10 || text.starts_with("0000") {
        return None;
    }
    let (date, time) = text.split_at(10);
    let mut out = format!("{}{}", date.replace(':', "-"), time);
    if let Some(offset) = offset.map(str::trim).filter(|o| !o.is_empty()) {
        out.push_str(&format!(" (UTC{offset})"));
    }
    Some(out)
}

/// 画像の向き（Exif Orientation）
fn orientation_name(v: u32) -> Option<&'static str> {
    Some(match v {
        1 => "標準",
        2 => "左右反転",
        3 => "180° 回転",
        4 => "上下反転",
        5 => "左右反転 + 反時計回りに 90° 回転",
        6 => "時計回りに 90° 回転",
        7 => "左右反転 + 時計回りに 90° 回転",
        8 => "反時計回りに 90° 回転",
        _ => return None,
    })
}

fn exposure_program_name(v: u32) -> Option<&'static str> {
    Some(match v {
        1 => "マニュアル",
        2 => "プログラム",
        3 => "絞り優先",
        4 => "シャッター優先",
        5 => "クリエイティブ",
        6 => "アクション",
        7 => "ポートレート",
        8 => "風景",
        _ => return None,
    })
}

/// GPS の緯度・経度（度分秒の 3 つの有理数と、N / S / E / W）を 10 進の度にする
fn gps_degrees(exif: &exif::Exif, tag: Tag, ref_tag: Tag) -> Option<f64> {
    let Value::Rational(parts) = &exif.get_field(tag, In::PRIMARY)?.value else {
        return None;
    };
    if parts.len() < 3 || parts.iter().take(3).any(|p| p.denom == 0) {
        return None;
    }
    let deg = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
    let negative = matches!(exif_text(exif, ref_tag).as_deref(), Some("S" | "W"));
    Some(if negative { -deg } else { deg })
}

/// 「撮影情報」の欄
fn exif_section(exif: &exif::Exif) -> InfoSection {
    let mut s = InfoSection::new("撮影情報");
    s.opt("メーカー", exif_text(exif, Tag::Make));
    s.opt("機種", exif_text(exif, Tag::Model));
    s.opt("レンズ", exif_text(exif, Tag::LensModel));
    let taken = exif_text(exif, Tag::DateTimeOriginal).or_else(|| exif_text(exif, Tag::DateTime));
    let offset = exif_text(exif, Tag::OffsetTimeOriginal);
    s.opt(
        "撮影日時",
        taken.and_then(|t| format_exif_datetime(&t, offset.as_deref())),
    );
    s.opt(
        "露出時間",
        exif_f64(exif, Tag::ExposureTime).and_then(format_exposure),
    );
    s.opt(
        "F 値",
        exif_f64(exif, Tag::FNumber)
            .filter(|f| *f > 0.0)
            .map(|f| format!("f/{}", trim_float(f, 1))),
    );
    s.opt(
        "ISO 感度",
        exif_uint(exif, Tag::PhotographicSensitivity)
            .filter(|v| *v > 0)
            .map(|v| v.to_string()),
    );
    if let Some(focal) = exif_f64(exif, Tag::FocalLength).filter(|f| *f > 0.0) {
        let mut text = format!("{} mm", trim_float(focal, 1));
        if let Some(full) = exif_uint(exif, Tag::FocalLengthIn35mmFilm).filter(|v| *v > 0)
            && (full as f64 - focal).abs() >= 1.0
        {
            text.push_str(&format!("（35mm 換算 {full} mm）"));
        }
        s.push("焦点距離", text);
    }
    s.opt(
        "露出補正",
        exif_f64(exif, Tag::ExposureBiasValue).map(|v| {
            if v.abs() < 0.005 {
                "0 EV".to_string()
            } else {
                format!("{}{} EV", if v > 0.0 { "+" } else { "" }, trim_float(v, 2))
            }
        }),
    );
    s.opt(
        "露出プログラム",
        exif_uint(exif, Tag::ExposureProgram).and_then(exposure_program_name),
    );
    s.opt(
        "フラッシュ",
        exif_uint(exif, Tag::Flash).map(|v| if v & 1 != 0 { "発光" } else { "非発光" }),
    );
    s.opt(
        "ホワイトバランス",
        exif_uint(exif, Tag::WhiteBalance).and_then(|v| match v {
            0 => Some("オート"),
            1 => Some("マニュアル"),
            _ => None,
        }),
    );
    s.opt(
        "向き",
        exif_uint(exif, Tag::Orientation).and_then(orientation_name),
    );
    if let (Some(lat), Some(lon)) = (
        gps_degrees(exif, Tag::GPSLatitude, Tag::GPSLatitudeRef),
        gps_degrees(exif, Tag::GPSLongitude, Tag::GPSLongitudeRef),
    ) {
        s.push("位置", format!("{lat:.6}, {lon:.6}"));
    }
    if let Some(alt) = exif_f64(exif, Tag::GPSAltitude) {
        let below = exif_uint(exif, Tag::GPSAltitudeRef) == Some(1);
        s.push(
            "高度",
            format!("{} m", trim_float(if below { -alt } else { alt }, 1)),
        );
    }
    s.opt("ソフトウェア", exif_text(exif, Tag::Software));
    s.opt("説明", exif_text(exif, Tag::ImageDescription));
    s.opt("作者", exif_text(exif, Tag::Artist));
    s.opt("著作権", exif_text(exif, Tag::Copyright));
    s
}
