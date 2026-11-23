// 対応拡張子とファイルパス・時間・色の純粋ユーティリティ

import {
  ARCHIVE_EXT_FILTER,
  AUDIO_EXT_FILTER,
  IMAGE_EXT_FILTER,
  MIME,
  VIDEO_EXT_FILTER,
} from "./media-formats.js";

export { ARCHIVE_EXT_FILTER, AUDIO_EXT_FILTER, IMAGE_EXT_FILTER, MIME, VIDEO_EXT_FILTER };

/**
 * @param {string} name
 * @returns {string}
 */
export function extOf(name) {
  const m = /\.([^.\\/]+)$/.exec(name);
  return m ? m[1].toLowerCase() : "";
}

/**
 * @param {string} name
 * @returns {boolean}
 */
export function isVideoPath(name) {
  return VIDEO_EXT_FILTER.includes(extOf(name));
}

/**
 * @param {string} name
 * @returns {boolean}
 */
export function isAudioPath(name) {
  return AUDIO_EXT_FILTER.includes(extOf(name));
}

/**
 * @param {string} path
 * @returns {string}
 */
export function baseName(path) {
  return path.split(/[\\/]/).pop() || path;
}

// Rust から返る生バイト列の受け取り方は IPC の経路によって変わる
// （カスタムプロトコルなら ArrayBuffer、postMessage 経由なら数値の配列）。
// そのまま Blob に渡すと配列が文字列化されて画像が壊れるので、必ずここで揃える
/**
 * @param {unknown} data
 * @returns {Uint8Array<ArrayBuffer>}
 */
export function toBytes(data) {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(
      /** @type {ArrayBuffer} */ (data.buffer),
      data.byteOffset,
      data.byteLength
    );
  }
  if (Array.isArray(data)) return Uint8Array.from(data);
  throw new Error("画像データを取得できませんでした");
}

/**
 * @param {number} seconds
 * @returns {string}
 */
export function formatTime(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const s = Math.floor(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/**
 * @param {string} hex
 * @param {number} alpha
 * @returns {string}
 */
export function hexToRgba(hex, alpha) {
  const m = /^#?([0-9a-f]{6})$/i.exec(String(hex).trim());
  const n = parseInt(m ? m[1] : "0e0e10", 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}
