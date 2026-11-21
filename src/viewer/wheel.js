// ホイール操作の軸固定・感度しきい値・音量調整

// ホイールで動かす秒数（ボタン・キーの SEEK_STEP_S より細かく）
export const WHEEL_SEEK_S = 3;

// 一続きの操作の軸判定を保つ時間（ミリ秒）
const WHEEL_AXIS_GAP_MS = 150;
// 連続操作とみなす間隔（ミリ秒）
const WHEEL_GESTURE_GAP_MS = 250;

export function createWheelController({ getSensitivity, changeVolumeBy, volumeStep }) {
  // ホイール 1 段の量はデバイス差が大きい（細かい量で何度も届くものもある）ので、
  // しきい値までためてから 1 段変える
  let wheelAccum = 0;

  // 縦横どちらの量を使うか。横（右）は縦の下と同じ向き（次 / 早送り）として扱う。
  // 一続きの操作（イベントが WHEEL_AXIS_GAP_MS 以上途切れるまで。トラックパッドの慣性も含む）の
  // 最初のイベントで量の大きい方に決めて、途中では変えない。トラックパッドの斜めの操作で
  // イベントごとに縦横が入れ替わると、右上・左下のように次と前が 1 回の操作で両方起きるため
  let wheelAxisAt = 0;
  let wheelAxisX = false;

  function wheelDelta(e) {
    if (!e.deltaX && !e.deltaY) return 0;
    const now = performance.now();
    if (now - wheelAxisAt > WHEEL_AXIS_GAP_MS) {
      wheelAxisX = Math.abs(e.deltaX) > Math.abs(e.deltaY);
    }
    wheelAxisAt = now;
    return wheelAxisX ? e.deltaX : e.deltaY;
  }

  // ホイール。下に回すと次の画像（本モードでは見開きごと）/ 早送り。
  // 少し間を空けて回し始めたときは、量によらずすぐ 1 段動かす（1 段の量が小さいマウスでも
  // 1 段で 1 回動くように）。続けて回している間は、しきい値ごとに 1 段動かす。
  // 既定の感度でしきい値は 100（Windows のホイール 1 段の量）
  let wheelLastAt = 0;
  let wheelDir = 0;

  function wheelStep(deltaY, forward, backward) {
    const dir = Math.sign(deltaY);
    if (!dir) return;
    const act = dir > 0 ? forward : backward;
    const now = performance.now();
    const fresh = now - wheelLastAt > WHEEL_GESTURE_GAP_MS || dir !== wheelDir;
    wheelLastAt = now;
    if (fresh) {
      // 次のしきい値は、すぐ動かした 1 段の分を除いてから数える
      wheelDir = dir;
      wheelAccum = 0;
      act();
      return;
    }
    const threshold = 150 - getSensitivity() * 10;
    wheelAccum += deltaY;
    while (Math.abs(wheelAccum) >= threshold) {
      act();
      wheelAccum -= dir * threshold;
    }
  }

  // 音量（右ボタンを押したまま / Ctrl + ホイール）。上に回すと大きくなる
  function wheelVolume(deltaY) {
    const threshold = 220 - getSensitivity() * 18;
    if (Math.sign(deltaY) !== Math.sign(wheelAccum)) wheelAccum = 0;
    wheelAccum += deltaY;
    while (Math.abs(wheelAccum) >= threshold) {
      changeVolumeBy(-Math.sign(wheelAccum) * volumeStep);
      wheelAccum -= Math.sign(wheelAccum) * threshold;
    }
  }

  return {
    wheelDelta,
    wheelStep,
    wheelVolume,
  };
}
