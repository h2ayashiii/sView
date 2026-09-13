#!/usr/bin/env node
// 配布バイナリに含まれる第三者クレートの一覧（THIRD-PARTY-NOTICES）を生成する。
// 依存を足したり外したりしたら実行して差し替える。
//
//   node scripts/gen-third-party-notices.mjs
//
// cargo metadata の依存グラフをルートから辿り、配布先である Windows / macOS の
// ビルドに入りうる通常依存だけを集める。build-dependencies と dev-dependencies、
// および Linux / Android など対象外プラットフォーム専用の依存は除く。
//
// 注意: --offline では Android 専用クレート（tauri-plugin-log が引く）を取得できず
// 失敗するため、ネットワークに繋がる状態で実行すること。

import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const outPath = join(root, "THIRD-PARTY-NOTICES");

// ライセンス全文の参照先。ここに無い識別子が出てきたら追記する
const LICENSE_URLS = [
  ["MIT", "https://opensource.org/license/mit"],
  ["Apache-2.0", "https://www.apache.org/licenses/LICENSE-2.0"],
  ["BSD-3-Clause", "https://opensource.org/license/bsd-3-clause"],
  ["MPL-2.0", "https://www.mozilla.org/MPL/2.0/"],
  ["Zlib", "https://opensource.org/license/zlib"],
  ["Unicode-3.0", "https://www.unicode.org/license.txt"],
  ["Unlicense", "https://unlicense.org/"],
  ["0BSD", "https://opensource.org/license/0bsd"],
  ["CC0-1.0", "https://creativecommons.org/publicdomain/zero/1.0/legalcode"],
  ["MIT-0", "https://opensource.org/license/mit-0"],
];

// 依存の target 指定（cfg 式やターゲットトリプル）が Windows / macOS に
// かかりうるかを判定する。判断が付かないものは「含める」側に倒す
const DESKTOP_HINTS = ["windows", "macos", "darwin", "apple", "unix"];
const FOREIGN_ONLY = [
  "linux", "android", "wasm", "emscripten", "wasi", "fuchsia", "redox",
  "netbsd", "freebsd", "openbsd", "dragonfly", "solaris", "illumos", "haiku",
  "hermit", "ios", "tvos", "watchos", "visionos", "vita", "horizon", "nto",
  "aix", "espidf", "xous", "teeos", "sgx", "uefi", "psp",
];

function targetApplies(target) {
  if (!target) return true;
  const t = target.toLowerCase();
  if (DESKTOP_HINTS.some((h) => t.includes(h))) return true;
  if (FOREIGN_ONLY.some((h) => t.includes(h))) return false;
  return true;
}

/** 配布物に入る依存かどうか（通常依存で、対象プラットフォームにかかるもの） */
function isShipped(depKinds) {
  return depKinds.some((k) => !k.kind && targetApplies(k.target));
}

const metadata = JSON.parse(
  execFileSync(
    "cargo",
    ["metadata", "--manifest-path", join(root, "src-tauri/Cargo.toml"), "--format-version", "1"],
    { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 }
  )
);

const packages = new Map(metadata.packages.map((p) => [p.id, p]));
const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
const rootId = metadata.resolve.root ?? metadata.workspace_members[0];

// ルートから幅優先で辿る。ルート自身（sview）は一覧に載せない
const reachable = new Set();
const queue = [rootId];
while (queue.length > 0) {
  const id = queue.shift();
  const node = nodes.get(id);
  if (!node) continue;
  for (const dep of node.deps) {
    if (reachable.has(dep.pkg) || !isShipped(dep.dep_kinds)) continue;
    reachable.add(dep.pkg);
    queue.push(dep.pkg);
  }
}

// ライセンス識別子ごとにまとめる
const groups = new Map();
for (const id of reachable) {
  const pkg = packages.get(id);
  if (!pkg) continue;
  const license = pkg.license ?? "（ライセンス未記載）";
  if (!groups.has(license)) groups.set(license, []);
  groups.get(license).push(pkg);
}

const byName = (a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version);
// 件数の多い順。同数なら識別子の名前順にして、生成のたびに並びがぶれないようにする
const sortedGroups = [...groups.entries()].sort(
  (a, b) => b[1].length - a[1].length || a[0].localeCompare(b[0])
);

const lines = [];
lines.push(
  "sView 第三者ソフトウェアのライセンス表示",
  "=========================================",
  "",
  "sView の配布バイナリには、以下のオープンソースソフトウェアが含まれています。",
  "それぞれの著作権は各プロジェクトの権利者に帰属し、下記のライセンスのもとで",
  "利用しています。sView 本体のライセンス（Commons Clause + MIT）は同梱の",
  "LICENSE を参照してください。",
  "",
  "この一覧は `cargo metadata` の依存グラフから生成しています（生成スクリプト:",
  "scripts/gen-third-party-notices.mjs）。対象は配布先である Windows / macOS の",
  "ビルドに入りうる通常依存のみで、ビルド時にしか使わない依存（build-dependencies）と",
  "テスト用の依存（dev-dependencies）、および Linux / Android など対象外の",
  "プラットフォーム専用の依存は除いています。",
  "",
  "フロントエンドは素の HTML/CSS/JS で、実行時に読み込む第三者ライブラリはありません",
  "（npm の依存は Tauri CLI のみで、配布物には含まれません）。画面描画に使う WebView",
  "（Windows: WebView2 / macOS: WKWebView）は OS が提供するコンポーネントです。",
  "",
  "ライセンス全文は、各ライセンスの正式なテキストを参照してください。"
);
const width = Math.max(...LICENSE_URLS.map(([id]) => id.length));
for (const [id, url] of LICENSE_URLS) lines.push(`  ${id.padEnd(width)}  ${url}`);
lines.push(
  "",
  "「A OR B」と記載されているものは、利用者がいずれかを選択できることを示します。",
  "",
  "MPL-2.0 について",
  "----------------",
  "下記のうち MPL-2.0 のものは、そのファイルを改変した場合に改変部分の",
  "ソース公開を求めるライセンスです。sView はこれらを改変せずそのまま利用して",
  "いるため追加の義務は生じませんが、ソースは各プロジェクトのリポジトリで",
  "入手できます。",
  "",
  "",
  `依存クレート一覧（${reachable.size} 件）`,
  "========================================"
);

for (const [license, pkgs] of sortedGroups) {
  lines.push("", `--- ${license}  (${pkgs.length} 件) ---`, "");
  for (const pkg of pkgs.sort(byName)) {
    lines.push(`  ${pkg.name} ${pkg.version}`);
    if (pkg.repository) lines.push(`      ${pkg.repository}`);
  }
}

writeFileSync(outPath, lines.join("\n") + "\n");
console.log(`THIRD-PARTY-NOTICES: ${reachable.size} 件のクレートを書き出しました`);
