// スライダーの塗り分け・吹き出し位置計算

// つまみの幅の分だけ端が内側に寄るので、位置の計算から除く（style.css の --thumb と揃える）
export const RANGE_THUMB_PX = 12;

// シークバー・音量つまみの済んだ側を塗る割合（style.css の --fill）。
// WebView ごとの標準の塗り分けには頼らない（macOS では塗り分けられない）
export function syncRangeFill(input) {
  const min = Number(input.min);
  const max = Number(input.max);
  const ratio = max > min ? (Number(input.value) - min) / (max - min) : 0;
  input.style.setProperty("--fill", String(Math.min(1, Math.max(0, ratio))));
}

// anchor（シークバー / 音量つまみ / 画像の位置のスライダー）のすぐ上、横は clientX に合わせる
// （吹き出しを入れているカードからははみ出さない）
export function showBarTip(tip, text, clientX, anchor) {
  tip.textContent = text;
  tip.hidden = false;
  const card = tip.parentElement.getBoundingClientRect();
  const half = tip.offsetWidth / 2;
  const x = Math.min(card.width - half, Math.max(half, clientX - card.left));
  tip.style.left = `${x}px`;
  tip.style.top = `${anchor.getBoundingClientRect().top - card.top - tip.offsetHeight - 4}px`;
}

export function rangeRatioAt(input, clientX) {
  const r = input.getBoundingClientRect();
  const track = r.width - RANGE_THUMB_PX;
  if (track <= 0) return 0;
  return Math.min(1, Math.max(0, (clientX - r.left - RANGE_THUMB_PX / 2) / track));
}
