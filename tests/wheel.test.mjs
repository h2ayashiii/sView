import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createWheelController } from "../src/viewer/wheel.js";

function withFakeNow(fn) {
  const original = performance.now.bind(performance);
  let now = 1000;
  performance.now = () => now;
  try {
    fn((ms) => {
      now += ms;
    });
  } finally {
    performance.now = original;
  }
}

describe("wheel controller", () => {
  it("wheelDelta は一続きの操作の最初のイベントで縦横軸を固定し、間が空くと再判定する", () => {
    withFakeNow((advance) => {
      const wheel = createWheelController({
        getSensitivity: () => 5,
        changeVolumeBy: () => {},
        volumeStep: 0.05,
      });

      assert.equal(wheel.wheelDelta({ deltaX: 0, deltaY: 0 }), 0);
      // 最初は横が大きいので X 軸に固定
      assert.equal(wheel.wheelDelta({ deltaX: 30, deltaY: 10 }), 30);
      advance(50);
      // 150ms 以内なら Y が大きくても X 軸を維持
      assert.equal(wheel.wheelDelta({ deltaX: -5, deltaY: 40 }), -5);
      // 150ms 超空くと軸を再判定
      advance(200);
      assert.equal(wheel.wheelDelta({ deltaX: 5, deltaY: 40 }), 40);
    });
  });

  it("wheelStep は回し始めの 1 段目で即座に動き、連続時は感度しきい値ごとに動く", () => {
    withFakeNow((advance) => {
      const calls = [];
      const wheel = createWheelController({
        getSensitivity: () => 5, // threshold = 150 - 50 = 100
        changeVolumeBy: () => {},
        volumeStep: 0.05,
      });
      const fwd = () => calls.push("fwd");
      const back = () => calls.push("back");

      // 初回は小さい量でもすぐ 1 回動く
      wheel.wheelStep(20, fwd, back);
      assert.deepEqual(calls, ["fwd"]);

      // 250ms 以内の連続操作はしきい値（100）まで貯まる
      advance(50);
      wheel.wheelStep(60, fwd, back);
      assert.deepEqual(calls, ["fwd"]);
      advance(50);
      wheel.wheelStep(50, fwd, back);
      assert.deepEqual(calls, ["fwd", "fwd"]);

      // 向きが変わると即座に 1 回動く
      advance(50);
      wheel.wheelStep(-10, fwd, back);
      assert.deepEqual(calls, ["fwd", "fwd", "back"]);
    });
  });

  it("wheelVolume は上回転で音量を上げ、逆回転で蓄積をリセットする", () => {
    const deltas = [];
    const wheel = createWheelController({
      getSensitivity: () => 5, // threshold = 220 - 90 = 130
      changeVolumeBy: (d) => deltas.push(d),
      volumeStep: 0.05,
    });

    wheel.wheelVolume(-70);
    assert.deepEqual(deltas, []);
    wheel.wheelVolume(-70); // 合計 -140 → +0.05
    assert.deepEqual(deltas, [0.05]);

    // 向きを反転すると蓄積がリセットされる
    wheel.wheelVolume(70);
    assert.deepEqual(deltas, [0.05]);
    wheel.wheelVolume(70); // 合計 +140 → -0.05
    assert.deepEqual(deltas, [0.05, -0.05]);
  });
});
