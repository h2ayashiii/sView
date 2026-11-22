// 対応拡張子とファイルパス・時間・色の純粋ユーティリティ

export const IMAGE_EXT_FILTER = [
  "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg",
  "png", "svg", "tif", "tiff", "webp",
];

// 動画は OS の WebView が再生できるものだけ（コーデックは同梱しない）。
// 書庫の中の動画は一覧に出さない。MKV は WebView2（Windows）ならそのまま再生でき、
// macOS では Rust 側で fragmented MP4 に詰め直して MSE で再生する（mkv-player.js）
export const VIDEO_EXT_FILTER = ["mp4", "m4v", "webm", "mov", "mkv"];

// 音楽も同じく OS の WebView 任せ。書庫の中の音楽は一覧に出さない
export const AUDIO_EXT_FILTER = ["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus"];

export const ARCHIVE_EXT_FILTER = ["zip", "cbz"];

// blob URL に必要な MIME（拡張子から判定）
export const MIME = {
  avif: "image/avif", bmp: "image/bmp", gif: "image/gif", ico: "image/x-icon",
  jfif: "image/jpeg", jpe: "image/jpeg", jpeg: "image/jpeg", jpg: "image/jpeg",
  png: "image/png", svg: "image/svg+xml", tif: "image/tiff", tiff: "image/tiff",
  webp: "image/webp",
};

export function extOf(name) {
  const m = /\.([^.\\/]+)$/.exec(name);
  return m ? m[1].toLowerCase() : "";
}

export function isVideoPath(name) {
  return VIDEO_EXT_FILTER.includes(extOf(name));
}

export function isAudioPath(name) {
  return AUDIO_EXT_FILTER.includes(extOf(name));
}

export function baseName(path) {
  return path.split(/[\\/]/).pop() || path;
}

// Rust から返る生バイト列の受け取り方は IPC の経路によって変わる
// （カスタムプロトコルなら ArrayBuffer、postMessage 経由なら数値の配列）。
// そのまま Blob に渡すと配列が文字列化されて画像が壊れるので、必ずここで揃える
export function toBytes(data) {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  if (Array.isArray(data)) return Uint8Array.from(data);
  throw new Error("画像データを取得できませんでした");
}

export function formatTime(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const s = Math.floor(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

export function hexToRgba(hex, alpha) {
  const m = /^#?([0-9a-f]{6})$/i.exec(String(hex).trim());
  const n = parseInt(m ? m[1] : "0e0e10", 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}
