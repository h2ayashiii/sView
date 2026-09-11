// 設定ウィンドウ。SETTINGS_SECTIONS から UI を組み立て、変更のたびに
// 保存 → 本体ウィンドウへ通知する（適用ボタンは置かない）。
const { invoke } = window.__TAURI__.core;
const { emit } = window.__TAURI__.event;
const settingsWindow = window.__TAURI__.window.getCurrentWindow();

const body = document.getElementById("s-body");
const statusEl = document.getElementById("s-status");

let settings = { ...SETTINGS_DEFAULTS };
const controls = new Map(); // key -> 値を書き戻す関数

function setStatus(text, isError) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", !!isError);
  clearTimeout(setStatus.timer);
  if (!isError) setStatus.timer = setTimeout(() => (statusEl.textContent = ""), 1200);
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
  emit("settings-changed", settings).catch(() => {});
  persist();
}

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
}

document.getElementById("s-close").addEventListener("click", () => settingsWindow.close());
document.getElementById("s-reset").addEventListener("click", () => {
  settings = { ...SETTINGS_DEFAULTS };
  render();
  emit("settings-changed", settings).catch(() => {});
  persist();
});

window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") settingsWindow.close();
});
// 本体と同じく、ブラウザ既定のコンテキストメニューは出さない
window.addEventListener("contextmenu", (e) => e.preventDefault());

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
