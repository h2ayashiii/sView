// 設定ウィンドウ。SETTINGS_SECTIONS から UI を組み立て、変更のたびに
// 保存 → 本体ウィンドウへ通知する（適用ボタンは置かない）。
// Tauri API が取れなくても、閉じる操作だけは必ず効くようにここから先に用意する。
const tauri = window.__TAURI__ ?? {};
const invoke =
  tauri.core?.invoke ?? (() => Promise.reject(new Error("Tauri API を利用できません")));
const emit = tauri.event?.emit ?? (() => Promise.resolve());
const listen = tauri.event?.listen ?? (() => Promise.resolve());
const settingsWindow = tauri.window?.getCurrentWindow?.() ?? null;

// 閉じても破棄はされず（Rust 側で隠すだけ）、次に開くときは同じウィンドウを使う
function closeWindow() {
  settingsWindow?.close().catch(() => {});
}

document.getElementById("s-close").addEventListener("click", closeWindow);
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeWindow();
});
// 本体と同じく、ブラウザ既定のコンテキストメニューは出さない
window.addEventListener("contextmenu", (e) => e.preventDefault());

const body = document.getElementById("s-body");
const statusEl = document.getElementById("s-status");
const versionEl = document.getElementById("s-version");

// アプリのバージョン（リリース時は CI がタグの値を書き込んでビルドする）。
// 取得できない場合は何も出さない
invoke("app_version")
  .then((version) => (versionEl.textContent = `バージョン ${version}`))
  .catch(() => {});

let settings = { ...SETTINGS_DEFAULTS };
const controls = new Map(); // key -> 値を書き戻す関数

function setStatus(text, isError) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", !!isError);
  clearTimeout(setStatus.timer);
  if (!isError) setStatus.timer = setTimeout(() => (statusEl.textContent = ""), 1200);
}

// ---- 配色 ----
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

function applyTheme() {
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

async function persist() {
  try {
    await invoke("save_settings", { settings });
    setStatus("保存しました");
  } catch (e) {
    setStatus(String(e), true);
  }
}

// 本体ウィンドウは この通知を受けて即座に見た目・挙動を更新する
function update(key, value) {
  settings[key] = value;
  applyTheme();
  emit("settings-changed", settings).catch(() => {});
  persist();
}

// type: "action" の行から呼ぶ処理。settings-defs.js は本体ウィンドウからも読む
// 純粋なデータなので、実際の処理はこちら側に置く
const ACTIONS = {
  openLogFolder: () => invoke("open_log_folder"),
};

function buildRow(item) {
  const row = document.createElement("div");
  row.className = "row";

  const label = document.createElement("label");
  label.className = "label";
  label.textContent = item.label;
  if (item.hint) {
    const hint = document.createElement("span");
    hint.className = "hint";
    hint.textContent = item.hint;
    label.appendChild(hint);
  }

  const control = document.createElement("div");
  control.className = "control";

  if (item.type === "toggle") {
    const wrap = document.createElement("span");
    wrap.className = "switch";
    const input = document.createElement("input");
    input.type = "checkbox";
    const track = document.createElement("span");
    track.className = "track";
    wrap.append(input, track);
    input.addEventListener("change", () => update(item.key, input.checked));
    controls.set(item.key, (v) => (input.checked = !!v));
    control.appendChild(wrap);
    label.htmlFor = input.id = `set-${item.key}`;
  } else if (item.type === "select") {
    const select = document.createElement("select");
    for (const [value, text] of item.options) {
      const opt = document.createElement("option");
      opt.value = value;
      opt.textContent = text;
      select.appendChild(opt);
    }
    select.addEventListener("change", () => update(item.key, select.value));
    controls.set(item.key, (v) => (select.value = v));
    control.appendChild(select);
    label.htmlFor = select.id = `set-${item.key}`;
  } else if (item.type === "range") {
    const input = document.createElement("input");
    input.type = "range";
    input.min = item.min;
    input.max = item.max;
    input.step = item.step;
    const value = document.createElement("span");
    value.className = "value";
    const render = (v) => (value.textContent = `${v}${item.unit ?? ""}`);
    input.addEventListener("input", () => {
      render(input.value);
      update(item.key, Number(input.value));
    });
    controls.set(item.key, (v) => {
      input.value = v;
      render(v);
    });
    control.append(input, value);
    label.htmlFor = input.id = `set-${item.key}`;
  } else if (item.type === "color") {
    const input = document.createElement("input");
    input.type = "color";
    input.addEventListener("input", () => update(item.key, input.value));
    controls.set(item.key, (v) => (input.value = v));
    control.appendChild(input);
    label.htmlFor = input.id = `set-${item.key}`;
  } else if (item.type === "action") {
    // 値を持たない行なので controls には入れない（render() の対象外）
    const button = document.createElement("button");
    button.className = "ghost";
    button.textContent = item.buttonLabel ?? "実行";
    button.addEventListener("click", async () => {
      const run = ACTIONS[item.action];
      if (!run) return;
      button.disabled = true;
      try {
        await run();
      } catch (e) {
        setStatus(String(e), true);
      } finally {
        button.disabled = false;
      }
    });
    control.appendChild(button);
  }

  row.append(label, control);
  return row;
}

function build() {
  for (const section of SETTINGS_SECTIONS) {
    const title = document.createElement("div");
    title.className = "section-title";
    title.textContent = section.label;
    body.appendChild(title);
    for (const item of section.items) body.appendChild(buildRow(item));
  }
}

function render() {
  for (const [key, apply] of controls) apply(settings[key]);
  applyTheme();
}

// 本体ウィンドウ側でも設定は変わる（削除確認の「今後確認しない」）。
// こちらを操作している間は自分の emit が跳ね返ってくるだけなので、
// フォーカスが無いときだけ受け取って表示を合わせる
listen("settings-changed", (event) => {
  if (document.hasFocus()) return;
  settings = normalizeSettings(event.payload);
  render();
});

document.getElementById("s-reset").addEventListener("click", () => {
  settings = { ...SETTINGS_DEFAULTS };
  render();
  emit("settings-changed", settings).catch(() => {});
  persist();
});

build();
invoke("load_settings")
  .then((saved) => {
    settings = normalizeSettings(saved);
    render();
  })
  .catch((e) => {
    render();
    setStatus(String(e), true);
  });
