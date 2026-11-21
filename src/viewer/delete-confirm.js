// 削除確認ダイアログ
// OS の確認ダイアログには「今後確認しない」を同居させられないので自前で出す。
// 開いている間はキー・ホイール・右クリックを呼び出し側で止める

import { baseName } from "./media-types.js";

export function createDeleteConfirm({
  confirmEl,
  confirmNameEl,
  confirmSkipEl,
  confirmOkEl,
  confirmCancelEl,
  hideContextMenu,
}) {
  let confirmResolve = null;

  function closeConfirm(answer) {
    if (!confirmResolve) return;
    const done = confirmResolve;
    confirmResolve = null;
    confirmEl.hidden = true;
    done(answer);
  }

  function askDeleteConfirm(path) {
    closeConfirm({ ok: false, skip: false }); // 二重に開かない
    hideContextMenu();
    confirmNameEl.textContent = baseName(path);
    confirmNameEl.title = path;
    confirmSkipEl.checked = false;
    confirmEl.hidden = false;
    confirmOkEl.focus();
    return new Promise((resolve) => (confirmResolve = resolve));
  }

  confirmOkEl.addEventListener("click", () =>
    closeConfirm({ ok: true, skip: confirmSkipEl.checked })
  );
  confirmCancelEl.addEventListener("click", () => closeConfirm({ ok: false, skip: false }));
  // 外側（暗い部分）を押したらキャンセル扱いにする
  confirmEl.addEventListener("mousedown", (e) => {
    if (e.target === confirmEl) closeConfirm({ ok: false, skip: false });
  });

  return {
    askDeleteConfirm,
    closeConfirm,
    isOpen: () => confirmResolve !== null,
  };
}
