// 設定ウィンドウとショートカット一覧ウィンドウの配色（両方の HTML から読む）。
// 本体ウィンドウと同じく、設定の「背景色」「背景の不透明度」で見た目を決める。
// 文字やバーの色は背景色から作るので、明るい背景を選んでも読めなくならない
function hexToRgb(hex) {
  const m = /^#?([0-9a-f]{6})$/i.exec(String(hex).trim());
  const n = parseInt(m ? m[1] : "0e0e10", 16);
  return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
}

function mix(a, b, t) {
  return {
    r: Math.round(a.r + (b.r - a.r) * t),
    g: Math.round(a.g + (b.g - a.g) * t),
    b: Math.round(a.b + (b.b - a.b) * t),
  };
}

function rgba(c, a) {
  return `rgba(${c.r}, ${c.g}, ${c.b}, ${Math.round(a * 100) / 100})`;
}

// 相対輝度。0.5 未満なら暗い背景とみなして白系の文字を載せる
function isDark(c) {
  return (0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b) / 255 < 0.5;
}

function applyTheme(settings) {
  const base = hexToRgb(settings.backgroundColor);
  const opacity = Number(settings.backgroundOpacity);
  const alpha = Math.min(1, Math.max(0.2, (Number.isFinite(opacity) ? opacity : 97) / 100));
  const dark = isDark(base);
  const ink = dark ? { r: 255, g: 255, b: 255 } : { r: 0, g: 0, b: 0 };
  const text = mix(base, ink, dark ? 0.92 : 0.88);
  // 見出しの帯とウィンドウ上下の枠は、背景より少しだけ濃く・不透明にして
  // 背景（や透けて見えるデスクトップ）に溶け込まないようにする
  const bar = mix(base, ink, dark ? 0.16 : 0.12);
  const chrome = mix(base, ink, dark ? 0.07 : 0.05);
  const arrow = encodeURIComponent(
    `#${[text.r, text.g, text.b].map((v) => v.toString(16).padStart(2, "0")).join("")}`
  );

  const s = document.documentElement.style;
  s.setProperty("--bg", rgba(base, alpha));
  s.setProperty("--solid", rgba(chrome, 1));
  s.setProperty("--chrome", rgba(chrome, Math.min(1, alpha + 0.1)));
  s.setProperty("--bar", rgba(bar, Math.min(1, alpha + 0.22)));
  s.setProperty("--fg", rgba(text, 1));
  s.setProperty("--fg-dim", rgba(text, 0.74));
  s.setProperty("--fg-faint", rgba(text, 0.52));
  s.setProperty("--line", rgba(ink, dark ? 0.12 : 0.18));
  s.setProperty("--hover", rgba(ink, dark ? 0.07 : 0.06));
  s.setProperty("--control", rgba(ink, dark ? 0.1 : 0.08));
  s.setProperty("--control-hover", rgba(ink, dark ? 0.16 : 0.14));
  s.setProperty("--thumb", rgba(mix(base, ink, dark ? 0.8 : 0.62), 1));
  s.setProperty("--scroll", rgba(ink, 0.22));
  s.setProperty("--scroll-hover", rgba(ink, 0.34));
  s.setProperty("--accent", dark ? "#5a82e6" : "#3563d6");
  s.setProperty(
    "--select-arrow",
    `url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 10 6'%3E%3Cpath d='M1 1l4 4 4-4' fill='none' stroke='${arrow}' stroke-width='1.4'/%3E%3C/svg%3E")`
  );
}
