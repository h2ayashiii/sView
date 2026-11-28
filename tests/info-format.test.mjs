import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { formatInfoTime, infoToText } from "../src/info-format.js";

describe("info-format", () => {
  it("日時の行は地域の時刻で書き、不正な値は空にする", () => {
    const ms = Date.UTC(2024, 4, 6, 7, 8, 9);
    assert.equal(formatInfoTime(ms, "en-US"), new Date(ms).toLocaleString("en-US"));
    assert.equal(formatInfoTime(Number.NaN), "");
  });

  it("コピー用のテキストは見出しごとに「名前: 値」を並べ、空の欄は飛ばす", () => {
    const ms = Date.UTC(2024, 0, 1);
    const text = infoToText(
      {
        name: "photo.jpg",
        kind: "image",
        sections: [
          {
            title: "ファイル",
            rows: [
              { label: "サイズ", value: "1.00 KB（1,024 バイト）" },
              { label: "更新日時", value: "2024-01-01 00:00:00 UTC", time: ms },
            ],
          },
          { title: "空", rows: [] },
          { title: "画像", rows: [{ label: "解像度", value: "640 × 480" }] },
        ],
      },
      "en-US"
    );
    assert.equal(
      text,
      [
        "photo.jpg",
        "",
        "【ファイル】",
        "サイズ: 1.00 KB（1,024 バイト）",
        `更新日時: ${new Date(ms).toLocaleString("en-US")}`,
        "",
        "【画像】",
        "解像度: 640 × 480",
        "",
      ].join("\n")
    );
  });
});
