import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { MkvPlayer } from "../src/mkv-player.js";

function makeRanges(pairs) {
  return {
    length: pairs.length,
    start(i) {
      return pairs[i][0];
    },
    end(i) {
      return pairs[i][1];
    },
  };
}

describe("MkvPlayer", () => {
  it("nudgeGap が自ら進めた currentTime の seeking イベントで seekTarget を立てない", () => {
    const video = { currentTime: 50.0 };
    const player = new MkvPlayer(video, "/tmp/sample.mp4");
    player.sourceBuffer = {
      buffered: makeRanges([[50.025, 51.067]]),
    };
    player.afterReset = true;

    player.nudgeGap(false);
    assert.ok(Math.abs(video.currentTime - 50.026) < 1e-6);
    assert.equal(player.afterReset, false);

    // WebKit が currentTime 代入に対して非同期で発火する seeking イベントを模倣
    player.onSeeking();
    assert.equal(player.seekTarget, null);

    // その後にユーザーが別位置へシークした場合は通常どおり seekTarget に記録される
    video.currentTime = 80.0;
    player.onSeeking();
    assert.equal(player.seekTarget, 80.0);
  });

  it("waiting 時に範囲末尾の微小な隙間があれば次の範囲へ飛び越す", () => {
    const video = { currentTime: 51.015 };
    const player = new MkvPlayer(video, "/tmp/sample.mp4");
    player.sourceBuffer = {
      buffered: makeRanges([
        [50.0, 51.04],
        [51.08, 52.1],
      ]),
    };

    // 通常の append 直後（stalled=false）ではジャンプしない
    player.nudgeGap(false);
    assert.equal(video.currentTime, 51.015);

    // waiting 発火時（stalled=true）には次の範囲の先頭へ進める
    player.nudgeGap(true);
    assert.ok(Math.abs(video.currentTime - 51.081) < 1e-6);
    player.onSeeking();
    assert.equal(player.seekTarget, null);
  });

  it("canContinueFromBuffer は変換モードで 1 秒以内の先送りだけ継続扱いにする", () => {
    const video = { currentTime: 10.0 };
    const player = new MkvPlayer(video, "/tmp/sample.mp4");
    player.info = { transcoded: true };
    player.sourceBuffer = {
      buffered: makeRanges([[9.5, 11.0]]),
    };

    assert.equal(player.canContinueFromBuffer(10.5), true);
    assert.equal(player.canContinueFromBuffer(11.8), true);
    assert.equal(player.canContinueFromBuffer(12.5), false);
    assert.equal(player.canContinueFromBuffer(3.0), false);
  });

  it("mkv_read 実行中にバッファ外へシークしたら mkv_cancel_read で中断し整合性を保つ", () => {
    const canceled = [];
    globalThis.window = /** @type {any} */ ({
      __TAURI__: {
        core: {
          invoke(cmd, args) {
            if (cmd === "mkv_cancel_read") canceled.push(args.id);
            return Promise.resolve();
          },
        },
      },
    });
    try {
      const video = { currentTime: 40.0 };
      const player = new MkvPlayer(video, "/tmp/sample.mp4");
      player.id = 7;
      player.info = { transcoded: true };
      player.reading = true;
      player.sourceBuffer = {
        buffered: makeRanges([[0.0, 3.0]]),
      };

      player.onSeeking();
      assert.deepEqual(canceled, [7]);
      assert.equal(player.readCanceled, true);
      // 中断した後は Rust 側のデコード位置が途中になるため、元のバッファ内へ戻っても再シークさせる
      assert.equal(player.canContinueFromBuffer(1.5), false);
    } finally {
      delete globalThis.window;
    }
  });

  it("先頭フレームが 0 秒より少し後ろにある短い動画のループ時に再変換せず先頭へ進める", async () => {
    const video = { currentTime: 0.0 };
    const player = new MkvPlayer(video, "/tmp/sample.mp4");
    player.info = { transcoded: true };
    player.eof = true;
    player.sourceBuffer = {
      buffered: makeRanges([[0.033, 5.0]]),
    };

    assert.equal(player.canContinueFromBuffer(0.0), true);
    await player.seek(0.0);
    assert.ok(Math.abs(video.currentTime - 0.034) < 1e-6);
  });
});
