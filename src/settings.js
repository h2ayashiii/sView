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

// macOS だけ閉じるボタンを信号機ボタンにする（本体の main.js と同じ判定）。
// 非アクティブの間は灰色にする
const winEl = document.getElementById("win");
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
winEl.classList.toggle("mac", IS_MAC);
function syncActive() {
  winEl.classList.toggle("inactive", !document.hasFocus());
}
window.addEventListener("focus", syncActive);
window.addEventListener("blur", syncActive);
syncActive();
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") closeWindow();
});
// 本体と同じく、ブラウザ既定のコンテキストメニューは出さない
window.addEventListener("contextmenu", (e) => e.preventDefault());

const body = document.getElementById("s-body");
const tabBar = document.getElementById("s-tabs");
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
  applyTheme(settings);
  emit("settings-changed", settings).catch(() => {});
  persist();
}

// type: "action" の行から呼ぶ処理。settings-defs.js は本体ウィンドウからも読む
// 純粋なデータなので、実際の処理はこちら側に置く
const ACTIONS = {
  openLogFolder: () => invoke("open_log_folder"),
  openShortcuts: () => invoke("open_shortcuts_window"),
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

// 関連付けの行（画像・動画・音楽のタブに 1 つずつ）の読み直し。
// どれかで関連付けると他の種類の状態も変わりうるので、まとめて読み直す
const assocLoaders = [];

function reloadAssociations() {
  return Promise.all(assocLoaders.map((load) => load().catch(() => {})));
}

// 拡張子の関連付け。状態は OS から読むので settings には入れない。
// item.kind の種類の拡張子だけを並べる
function buildAssociationRow(item) {
  const row = document.createElement("div");
  row.className = "row assoc";

  const label = document.createElement("div");
  label.className = "label";
  label.textContent = item.label;
  const hint = document.createElement("span");
  hint.className = "hint";
  hint.textContent = item.hint ?? "";
  label.appendChild(hint);

  const grid = document.createElement("div");
  grid.className = "assoc-grid";

  const actions = document.createElement("div");
  actions.className = "assoc-actions";
  const selectAll = document.createElement("button");
  selectAll.className = "ghost";
  selectAll.textContent = "すべて選択";
  const selectNone = document.createElement("button");
  selectNone.className = "ghost";
  selectNone.textContent = "すべて解除";
  const apply = document.createElement("button");
  apply.className = "ghost primary";
  apply.textContent = item.buttonLabel ?? "関連付ける";
  actions.append(selectAll, selectNone, apply);

  // 結果の案内文は長いので、フッターではなくこの行の中に出す
  const note = document.createElement("span");
  note.className = "hint assoc-note";

  row.append(label, grid, actions, note);

  const boxes = [];
  // 最後に読んだ OS の状態（他の種類の拡張子も含む）
  let status = null;
  const setAll = (checked) => boxes.forEach((b) => (b.checked = checked));
  selectAll.addEventListener("click", () => setAll(true));
  selectNone.addEventListener("click", () => setAll(false));

  const load = async () => {
    status = await invoke("file_association_status");
    const items = status.items.filter((i) => i.kind === item.kind);
    grid.replaceChildren();
    boxes.length = 0;
    const anyAssociated = items.some((i) => i.associated);
    for (const { ext, associated } of items) {
      const wrap = document.createElement("label");
      wrap.className = "assoc-item";
      const box = document.createElement("input");
      box.type = "checkbox";
      box.value = ext;
      // この種類をまだ何も関連付けていなければ、全部を選んだ状態から始める
      box.checked = anyAssociated ? associated : true;
      const name = document.createElement("span");
      name.textContent = `.${ext}`;
      wrap.append(box, name);
      if (associated) {
        const mark = document.createElement("span");
        mark.className = "assoc-mark";
        mark.textContent = "✓";
        mark.title = "いま sView で開きます";
        wrap.appendChild(mark);
      }
      grid.appendChild(wrap);
      boxes.push(box);
    }
  };
  assocLoaders.push(load);

  apply.addEventListener("click", async () => {
    const exts = boxes.filter((b) => b.checked).map((b) => b.value);
    // Windows は渡した一覧で sView の登録を作り直す（外れた拡張子は登録から消える）ので、
    // 他の種類はいま関連付いているものをそのまま渡して残す。
    // macOS は渡した拡張子を既定にするだけなので、この種類の分だけでよい
    if (status?.platform === "windows") {
      for (const i of status.items) {
        if (i.kind !== item.kind && i.associated) exts.push(i.ext);
      }
    }
    apply.disabled = true;
    note.textContent = "";
    try {
      const message = await invoke("apply_file_associations", { exts });
      note.textContent = message;
      await reloadAssociations();
    } catch (e) {
      setStatus(String(e), true);
    } finally {
      apply.disabled = false;
    }
  });

  load().catch((e) => setStatus(String(e), true));
  return row;
}

// Windows の設定画面で選び終えて戻ってきたら、状態を読み直す
window.addEventListener("focus", () => reloadAssociations());

// タブを切り替える。設定ウィンドウは閉じても隠すだけなので、次に開いたときも同じタブのまま
function selectTab(id) {
  for (const button of tabBar.children) {
    const active = button.dataset.tab === id;
    button.classList.toggle("active", active);
    button.setAttribute("aria-selected", String(active));
  }
  for (const panel of body.children) panel.hidden = panel.dataset.tab !== id;
  body.scrollTop = 0;
}

function build() {
  const panels = new Map();
  for (const tab of SETTINGS_TABS) {
    const button = document.createElement("button");
    button.className = "s-tab";
    button.setAttribute("role", "tab");
    button.dataset.tab = tab.id;
    button.textContent = tab.label;
    button.addEventListener("click", () => selectTab(tab.id));
    tabBar.appendChild(button);

    const panel = document.createElement("div");
    panel.className = "s-panel";
    panel.setAttribute("role", "tabpanel");
    panel.dataset.tab = tab.id;
    body.appendChild(panel);
    panels.set(tab.id, panel);
  }
  for (const section of SETTINGS_SECTIONS) {
    const panel = panels.get(section.tab) ?? panels.get(SETTINGS_TABS[0].id);
    const title = document.createElement("div");
    title.className = "section-title";
    title.textContent = section.label;
    panel.appendChild(title);
    for (const item of section.items) {
      panel.appendChild(item.type === "associations" ? buildAssociationRow(item) : buildRow(item));
    }
  }
  selectTab(SETTINGS_TABS[0].id);
}

function render() {
  for (const [key, apply] of controls) apply(settings[key]);
  applyTheme(settings);
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
  // 再生中の音量は設定ウィンドウに出していないので、既定に戻さずそのまま残す
  const kept = Object.fromEntries(Object.keys(HIDDEN_DEFAULTS).map((k) => [k, settings[k]]));
  settings = { ...SETTINGS_DEFAULTS, ...kept };
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
