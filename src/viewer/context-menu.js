// 右クリックメニューとサブメニューの表示・操作

export function createContextMenu({ ctxmenu, buildItems }) {
  // 開いているサブメニュー（無ければ null）
  let ctxSubmenu = null;

  function hideContextMenu() {
    ctxmenu.hidden = true;
  }

  function closeSubmenu() {
    ctxSubmenu?.remove();
    ctxSubmenu = null;
    for (const el of ctxmenu.querySelectorAll(".ctx-item.open")) el.classList.remove("open");
  }

  // メニューの項目のボタンを作る（サブメニューの中の項目も同じ）
  function menuItemButton(item) {
    const btn = document.createElement("button");
    btn.className = "ctx-item";
    btn.type = "button";
    // checked を持つ項目はチェック（radio なら丸）を左に出す（style.css の .ctx-item::before）
    const checkable = item.checked !== undefined;
    btn.setAttribute(
      "role",
      !checkable ? "menuitem" : item.radio ? "menuitemradio" : "menuitemcheckbox"
    );
    if (checkable) btn.setAttribute("aria-checked", String(!!item.checked));
    btn.disabled = !!item.disabled;
    const label = document.createElement("span");
    label.className = "ctx-label";
    label.textContent = item.label;
    const accel = document.createElement("span");
    accel.className = "ctx-accel";
    // サブメニューを持つ項目は右端に「›」を出す
    accel.textContent = item.submenu ? "\u203a" : item.accel ?? "";
    if (item.submenu) btn.setAttribute("aria-haspopup", "menu");
    btn.append(label, accel);
    if (!item.submenu) {
      btn.addEventListener("click", () => {
        hideContextMenu();
        item.action();
      });
    }
    return btn;
  }

  function appendMenuItems(container, items) {
    for (const item of items) {
      if (item.separator) {
        const hr = document.createElement("div");
        hr.className = "ctx-sep";
        container.appendChild(hr);
      } else {
        container.appendChild(menuItemButton(item));
      }
    }
  }

  // 親の項目の右（入りきらなければ左）にサブメニューを開く
  function openSubmenu(btn, items) {
    closeSubmenu();
    const sub = document.createElement("div");
    sub.className = "ctx-submenu";
    sub.setAttribute("role", "menu");
    appendMenuItems(sub, items);
    ctxmenu.appendChild(sub);
    ctxSubmenu = sub;
    btn.classList.add("open");
    const menuRect = ctxmenu.getBoundingClientRect();
    const btnRect = btn.getBoundingClientRect();
    const subRect = sub.getBoundingClientRect();
    // 親の項目と少し重ねて、斜めにカーソルを動かしても途切れにくくする
    let left = btnRect.right - 4;
    if (left + subRect.width > window.innerWidth - 4) left = btnRect.left - subRect.width + 4;
    left = Math.max(4, left);
    // 上端は親の項目にそろえる（項目の余白の分だけ上げる）
    let top = btnRect.top - 5;
    top = Math.max(4, Math.min(top, window.innerHeight - subRect.height - 4));
    // サブメニューは #ctxmenu の中に置くので、#ctxmenu の内側（枠線の内側）からの位置で入れる
    sub.style.left = `${left - menuRect.left - ctxmenu.clientLeft}px`;
    sub.style.top = `${top - menuRect.top - ctxmenu.clientTop}px`;
  }

  function openContextMenu(x, y) {
    ctxSubmenu = null;
    ctxmenu.textContent = "";
    for (const item of buildItems()) {
      if (item.separator) {
        appendMenuItems(ctxmenu, [item]);
        continue;
      }
      const btn = menuItemButton(item);
      // 項目に合わせたらサブメニューを開き、ほかの項目に移ったら閉じる
      btn.addEventListener("mouseenter", () => {
        if (item.submenu && !item.disabled) {
          if (ctxSubmenu && btn.classList.contains("open")) return;
          openSubmenu(btn, item.submenu);
        } else {
          closeSubmenu();
        }
      });
      if (item.submenu) {
        btn.addEventListener("click", () => {
          if (!item.disabled && !btn.classList.contains("open")) openSubmenu(btn, item.submenu);
        });
      }
      ctxmenu.appendChild(btn);
    }

    // いったん表示してから実寸で画面内に収める
    ctxmenu.style.left = "0px";
    ctxmenu.style.top = "0px";
    ctxmenu.hidden = false;
    const rect = ctxmenu.getBoundingClientRect();
    const left = Math.max(4, Math.min(x, window.innerWidth - rect.width - 4));
    const top = Math.max(4, Math.min(y, window.innerHeight - rect.height - 4));
    ctxmenu.style.left = `${left}px`;
    ctxmenu.style.top = `${top}px`;
  }

  return {
    openContextMenu,
    hideContextMenu,
    closeSubmenu,
  };
}
