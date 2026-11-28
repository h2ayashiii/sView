use std::env;
use std::fs;
use std::path::Path;

/// `media-formats.json` の指定キー（`"video": [...]` や `"image": { ... }`）から
/// 拡張子のリストを取り出す。build.rs に外部クレートを足さないための最小パーサ。
fn extract_exts(json: &str, key: &str, open: char, close: char) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let start = json
        .find(&needle)
        .unwrap_or_else(|| panic!("media-formats.json に {key} が見つかりません"));
    let rest = &json[start + needle.len()..];
    let open_pos = rest
        .find(open)
        .unwrap_or_else(|| panic!("media-formats.json の {key} に '{open}' が見つかりません"));
    let body_rest = &rest[open_pos + 1..];
    let close_pos = body_rest
        .find(close)
        .unwrap_or_else(|| panic!("media-formats.json の {key} に '{close}' が見つかりません"));
    let body = &body_rest[..close_pos];

    let mut exts = Vec::new();
    if open == '[' {
        // 配列: "mp4", "mkv", ...
        let mut iter = body.split('"').skip(1);
        while let Some(item) = iter.next() {
            exts.push(item.to_string());
            let _ = iter.next();
        }
    } else {
        // オブジェクト: "avif": "image/avif", ...（キーだけを取り出す）
        for line in body.lines() {
            let trimmed = line.trim();
            if let Some(after_quote) = trimmed.strip_prefix('"')
                && let Some(end) = after_quote.find('"')
            {
                exts.push(after_quote[..end].to_string());
            }
        }
    }
    exts.sort();
    exts
}

fn format_const(name: &str, exts: &[String]) -> String {
    let items: Vec<String> = exts.iter().map(|e| format!("{e:?}")).collect();
    format!(
        "#[allow(dead_code)]\npub(crate) const {name}: &[&str] = &[{}];\n",
        items.join(", ")
    )
}

fn generate_media_exts() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR が未設定です");
    let formats_path = Path::new(&manifest_dir).join("../media-formats.json");
    println!("cargo:rerun-if-changed={}", formats_path.display());

    let json = fs::read_to_string(&formats_path)
        .unwrap_or_else(|e| panic!("{} を読めません: {e}", formats_path.display()));

    let image = extract_exts(&json, "image", '{', '}');
    let macos_only_image = extract_exts(&json, "macosImage", '{', '}');
    let mut macos_image = image.clone();
    macos_image.extend(macos_only_image);
    macos_image.sort();
    macos_image.dedup();
    let video = extract_exts(&json, "video", '[', ']');
    let audio = extract_exts(&json, "audio", '[', ']');
    let archive = extract_exts(&json, "archive", '[', ']');

    let mut out = String::from("// media-formats.json から build.rs が自動生成\n");
    out.push_str(&format_const("COMMON_IMAGE_EXTS", &image));
    out.push_str(&format_const("MACOS_IMAGE_EXTS", &macos_image));
    out.push_str(&format_const("VIDEO_EXTS", &video));
    out.push_str(&format_const("AUDIO_EXTS", &audio));
    out.push_str(&format_const("ARCHIVE_EXTS", &archive));

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR が未設定です");
    fs::write(Path::new(&out_dir).join("media_exts.rs"), out)
        .expect("media_exts.rs を書き出せません");
}

fn main() {
    generate_media_exts();
    tauri_build::build()
}
