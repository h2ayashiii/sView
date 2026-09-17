#!/usr/bin/env node
// リリースタグ（v1.2.3）のバージョンをアプリ側へ反映する。
// タグからビルドするときに CI が呼ぶ（手動ビルドでは呼ばれない）。
//
//   node scripts/set-version.mjs v1.2.3
//
// 書き換える先:
//   - src-tauri/tauri.conf.json … アプリが表示するバージョン（PackageInfo）と
//                                 インストーラ / dmg のファイル名
//   - src-tauri/Cargo.toml      … クレートのバージョン（exe のファイル情報）
//   - package.json              … npm 側のメタデータ

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const input = process.argv[2];
if (!input) {
  console.error("使い方: node scripts/set-version.mjs <v1.2.3 | 1.2.3>");
  process.exit(1);
}

// タグ名で渡されることを前提に、先頭の v を落とす
const version = input.replace(/^v/, "");
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error(`バージョンの形式が正しくありません: ${input}（例: v1.2.3）`);
  process.exit(1);
}

/** version の値を差し替えて保存する。既に同じ値なら何もしない
 *  （リポジトリ側が先にそのバージョンになっていても失敗にしない） */
function writeVersion(path, relative, text, pattern) {
  if (!pattern.test(text)) {
    throw new Error(`version が見つかりません: ${relative}`);
  }
  const next = text.replace(pattern, `$1"${version}"`);
  if (next === text) {
    console.log(`${relative}: version = ${version}（変更なし）`);
    return;
  }
  writeFileSync(path, next);
  console.log(`${relative}: version = ${version}`);
}

/** JSON は version の値だけを差し替える（整形や並び順は元のまま残す） */
function patchJson(relative) {
  const path = join(root, relative);
  const text = readFileSync(path, "utf8");
  writeVersion(path, relative, text, /("version"\s*:\s*)"[^"]*"/);
}

/** Cargo.toml は [package] の version 行だけを差し替える（依存の version は触らない） */
function patchCargoToml(relative) {
  const path = join(root, relative);
  const text = readFileSync(path, "utf8");
  const start = text.indexOf("[package]");
  if (start < 0) throw new Error(`[package] が見つかりません: ${relative}`);
  const rest = text.indexOf("\n[", start + 1);
  const end = rest < 0 ? text.length : rest;
  const head = text.slice(0, start);
  const pkg = text.slice(start, end);
  const tail = text.slice(end);
  const pattern = /^(version\s*=\s*)"[^"]*"/m;
  if (!pattern.test(pkg)) {
    throw new Error(`[package] に version がありません: ${relative}`);
  }
  const nextPkg = pkg.replace(pattern, `$1"${version}"`);
  if (nextPkg === pkg) {
    console.log(`${relative}: version = ${version}（変更なし）`);
    return;
  }
  writeFileSync(path, head + nextPkg + tail);
  console.log(`${relative}: version = ${version}`);
}

patchJson("src-tauri/tauri.conf.json");
patchJson("package.json");
patchCargoToml("src-tauri/Cargo.toml");
