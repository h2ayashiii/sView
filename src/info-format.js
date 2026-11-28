// ファイルの情報ウィンドウ（info.js）の表示用の小さな処理（DOM を使わないのでテストできる）

/**
 * 日時の行（UNIX 時刻・ミリ秒）を利用者の地域の時刻で書く（「2024/05/06 7:08:09」）
 * @param {number} ms
 * @param {string} [locale]
 */
export function formatInfoTime(ms, locale = "ja-JP") {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleString(locale);
}

/**
 * 情報全体をコピー用のテキストにする（見出しは【】で囲み、行は「名前: 値」）
 * @param {import("./types.d.ts").MediaInfo} info
 * @param {string} [locale]
 */
export function infoToText(info, locale = "ja-JP") {
  const lines = [info.name];
  for (const section of info.sections) {
    if (!section.rows.length) continue;
    lines.push("", `【${section.title}】`);
    for (const row of section.rows) {
      const value = row.time != null ? formatInfoTime(row.time, locale) : row.value;
      lines.push(`${row.label}: ${value}`);
    }
  }
  return `${lines.join("\n")}\n`;
}
