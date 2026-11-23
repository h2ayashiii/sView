import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  baseName,
  extOf,
  formatTime,
  hexToRgba,
  isAudioPath,
  isVideoPath,
  toBytes,
} from "../src/viewer/media-types.js";

describe("media-types", () => {
  it("extOf はパスから小文字の拡張子を取り出す", () => {
    assert.equal(extOf("photo.JPG"), "jpg");
    assert.equal(extOf("/dir.v1/sub/file.webp"), "webp");
    assert.equal(extOf("C:\\dir.v1\\clip.MKV"), "mkv");
    assert.equal(extOf("README"), "");
    assert.equal(extOf("dir.with.dot/noext"), "");
  });

  it("isVideoPath / isAudioPath が拡張子を大文字小文字問わず判定する", () => {
    assert.equal(isVideoPath("movie.MP4"), true);
    assert.equal(isVideoPath("clip.mkv"), true);
    assert.equal(isVideoPath("song.mp3"), false);
    assert.equal(isAudioPath("track.FLAC"), true);
    assert.equal(isAudioPath("voice.opus"), true);
    assert.equal(isAudioPath("photo.png"), false);
  });

  it("baseName は POSIX / Windows いずれの区切りでもファイル名を返す", () => {
    assert.equal(baseName("/a/b/photo.png"), "photo.png");
    assert.equal(baseName("C:\\Users\\test\\clip.mp4"), "clip.mp4");
    assert.equal(baseName("single.jpg"), "single.jpg");
  });

  it("toBytes は ArrayBuffer / TypedArray / 配列を Uint8Array に揃える", () => {
    const buf = new Uint8Array([1, 2, 3, 4]).buffer;
    assert.deepEqual(toBytes(buf), new Uint8Array([1, 2, 3, 4]));

    const sub = new Uint8Array(buf, 1, 2);
    assert.deepEqual(toBytes(sub), new Uint8Array([2, 3]));

    assert.deepEqual(toBytes([10, 20, 30]), new Uint8Array([10, 20, 30]));
    assert.throws(() => toBytes("invalid"), /画像データを取得できませんでした/);
  });

  it("formatTime は秒数を m:ss または h:mm:ss に整形する", () => {
    assert.equal(formatTime(NaN), "0:00");
    assert.equal(formatTime(-5), "0:00");
    assert.equal(formatTime(Infinity), "0:00");
    assert.equal(formatTime(0), "0:00");
    assert.equal(formatTime(5.9), "0:05");
    assert.equal(formatTime(65), "1:05");
    assert.equal(formatTime(3661), "1:01:01");
  });

  it("hexToRgba は 16 進カラーコードを rgba 文字列に変換し、不正値は既定色へ落とす", () => {
    assert.equal(hexToRgba("#ff8000", 0.5), "rgba(255, 128, 0, 0.5)");
    assert.equal(hexToRgba(" 00aaff ", 1), "rgba(0, 170, 255, 1)");
    assert.equal(hexToRgba("invalid", 0.97), "rgba(14, 14, 16, 0.97)");
  });
});
