// このファイルは media-formats.json から scripts/sync-media-formats.mjs で自動生成されます。
// 拡張子を追加・変更するときは media-formats.json を編集し、npm run sync:formats を実行してください。

export const IMAGE_EXT_FILTER = [
  "avif",
  "bmp",
  "gif",
  "ico",
  "jfif",
  "jpe",
  "jpeg",
  "jpg",
  "png",
  "svg",
  "tif",
  "tiff",
  "webp",
];

// macOS は WKWebView（Safari 17+）で表示できる HEIC / HEIF を含める
export const MACOS_IMAGE_EXT_FILTER = [
  "avif",
  "bmp",
  "gif",
  "heic",
  "heif",
  "ico",
  "jfif",
  "jpe",
  "jpeg",
  "jpg",
  "png",
  "svg",
  "tif",
  "tiff",
  "webp",
];

// 動画は OS の WebView が再生できるものだけ（コーデックは同梱しない）。
// 書庫の中の動画は一覧に出さない。MKV は WebView2（Windows）ならそのまま再生でき、
// macOS では Rust 側で fragmented MP4 に詰め直して MSE で再生する（mkv-player.js）
export const VIDEO_EXT_FILTER = ["m4v", "mkv", "mov", "mp4", "webm"];

// 音楽も同じく OS の WebView 任せ。書庫の中の音楽は一覧に出さない
export const AUDIO_EXT_FILTER = ["aac", "flac", "m4a", "mp3", "ogg", "opus", "wav"];

export const ARCHIVE_EXT_FILTER = ["cbz", "zip"];

// blob URL に必要な MIME（拡張子から判定）
/** @type {Record<string, string>} */
export const MIME = {
  avif: "image/avif",
  bmp: "image/bmp",
  gif: "image/gif",
  heic: "image/heic",
  heif: "image/heif",
  ico: "image/x-icon",
  jfif: "image/jpeg",
  jpe: "image/jpeg",
  jpeg: "image/jpeg",
  jpg: "image/jpeg",
  png: "image/png",
  svg: "image/svg+xml",
  tif: "image/tiff",
  tiff: "image/tiff",
  webp: "image/webp",
};
