#!/usr/bin/env node
// media-formats.json を正（Single Source of Truth）として、
// src/viewer/media-formats.js と src-tauri/tauri.conf.json の fileAssociations を同期する。
//
//   node scripts/sync-media-formats.mjs          # ファイルを更新する
//   node scripts/sync-media-formats.mjs --check  # 差分があれば非 0 で終了（CI 用）

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const checkOnly = process.argv.includes("--check");

const formats = JSON.parse(readFileSync(join(root, "media-formats.json"), "utf8"));
const imageExts = Object.keys(formats.image).sort();
const videoExts = [...formats.video].sort();
const audioExts = [...formats.audio].sort();
const archiveExts = [...formats.archive].sort();

function formatStringArray(items) {
  const inline = `[${items.map((x) => JSON.stringify(x)).join(", ")}]`;
  if (inline.length <= 72) return inline;
  return `[\n${items.map((x) => `  ${JSON.stringify(x)},`).join("\n")}\n]`;
}

function formatMimeObject(map) {
  const entries = Object.keys(map)
    .sort()
    .map((k) => `  ${k}: ${JSON.stringify(map[k])},`);
  return `{\n${entries.join("\n")}\n}`;
}

const generatedJs = `// このファイルは media-formats.json から scripts/sync-media-formats.mjs で自動生成されます。
// 拡張子を追加・変更するときは media-formats.json を編集し、npm run sync:formats を実行してください。

export const IMAGE_EXT_FILTER = ${formatStringArray(imageExts)};

// 動画は OS の WebView が再生できるものだけ（コーデックは同梱しない）。
// 書庫の中の動画は一覧に出さない。MKV は WebView2（Windows）ならそのまま再生でき、
// macOS では Rust 側で fragmented MP4 に詰め直して MSE で再生する（mkv-player.js）
export const VIDEO_EXT_FILTER = ${formatStringArray(videoExts)};

// 音楽も同じく OS の WebView 任せ。書庫の中の音楽は一覧に出さない
export const AUDIO_EXT_FILTER = ${formatStringArray(audioExts)};

export const ARCHIVE_EXT_FILTER = ${formatStringArray(archiveExts)};

// blob URL に必要な MIME（拡張子から判定）
/** @type {Record<string, string>} */
export const MIME = ${formatMimeObject(formats.image)};
`;

const jsPath = join(root, "src/viewer/media-formats.js");
const confPath = join(root, "src-tauri/tauri.conf.json");

const currentConfText = readFileSync(confPath, "utf8");
const conf = JSON.parse(currentConfText);

// 書庫（ARCHIVE_EXTS）は関連付けない。画像・動画・音楽の 3 件だけを同期する
const targetExtsByName = {
  Image: imageExts,
  Video: videoExts,
  Audio: audioExts,
};

let confChanged = false;
for (const assoc of conf.bundle.fileAssociations) {
  const expected = targetExtsByName[assoc.name];
  if (!expected) continue;
  if (JSON.stringify(assoc.ext) !== JSON.stringify(expected)) {
    assoc.ext = expected;
    confChanged = true;
  }
}

let jsChanged = false;
try {
  jsChanged = readFileSync(jsPath, "utf8") !== generatedJs;
} catch {
  jsChanged = true;
}

if (checkOnly) {
  if (jsChanged || confChanged) {
    console.error(
      "media-formats.json と生成ファイルがずれています。`npm run sync:formats` を実行してください。"
    );
    if (jsChanged) console.error("  - src/viewer/media-formats.js");
    if (confChanged) console.error("  - src-tauri/tauri.conf.json");
    process.exit(1);
  }
  console.log("media-formats.json の同期チェック: OK");
  process.exit(0);
}

if (jsChanged) {
  writeFileSync(jsPath, generatedJs);
  console.log("更新しました: src/viewer/media-formats.js");
}
if (confChanged) {
  writeFileSync(confPath, `${JSON.stringify(conf, null, 2)}\n`);
  console.log("更新しました: src-tauri/tauri.conf.json");
}
if (!jsChanged && !confChanged) {
  console.log("変更はありません");
}
