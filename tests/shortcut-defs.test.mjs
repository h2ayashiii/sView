import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { resolveShortcutAction, SHORTCUT_SECTIONS } from "../src/shortcut-defs.js";

describe("shortcut-defs", () => {
  it("SHORTCUT_SECTIONS の全項目にラベルとキー表記がある", () => {
    assert.ok(SHORTCUT_SECTIONS.length > 0);
    for (const section of SHORTCUT_SECTIONS) {
      assert.ok(section.label);
      assert.ok(section.items.length > 0);
      for (const item of section.items) {
        assert.ok(item.label);
        assert.ok(item.keys || (item.win && item.mac));
      }
    }
  });

  it("設定を開くショートカットは Windows で Ctrl+,、macOS で Cmd+, を認識する", () => {
    const winCtx = { isMac: false, showingMedia: false, showingBook: false };
    const macCtx = { isMac: true, showingMedia: false, showingBook: false };

    assert.deepEqual(resolveShortcutAction({ key: ",", ctrlKey: true }, winCtx), {
      action: "openSettings",
      preventDefault: true,
    });
    assert.equal(resolveShortcutAction({ key: ",", metaKey: true }, winCtx), null);

    assert.deepEqual(resolveShortcutAction({ key: ",", metaKey: true }, macCtx), {
      action: "openSettings",
      preventDefault: true,
    });
    assert.equal(resolveShortcutAction({ key: ",", ctrlKey: true }, macCtx), null);
  });

  it("macOS の Cmd+Backspace は削除、単体 Backspace は前の画像として解決される", () => {
    const macCtx = { isMac: true, showingMedia: false, showingBook: false };
    assert.deepEqual(resolveShortcutAction({ key: "Backspace", metaKey: true }, macCtx), {
      action: "deleteCurrent",
      preventDefault: true,
    });
    assert.deepEqual(resolveShortcutAction({ key: "Backspace" }, macCtx), {
      action: "stepPrev",
      preventDefault: true,
    });
  });

  it("動画・音楽表示中は再生/シーク/音量に割り当て、ページ送りキーは飲み込む", () => {
    const mediaCtx = { isMac: false, showingMedia: true, showingBook: false };
    assert.deepEqual(resolveShortcutAction({ key: " " }, mediaCtx), {
      action: "togglePlay",
      preventDefault: true,
    });
    assert.deepEqual(resolveShortcutAction({ key: " ", repeat: true }, mediaCtx), {
      action: "noop",
      preventDefault: true,
    });
    assert.deepEqual(resolveShortcutAction({ key: "ArrowRight" }, mediaCtx), {
      action: "seekForward",
      preventDefault: true,
    });
    assert.deepEqual(resolveShortcutAction({ key: "ArrowUp" }, mediaCtx), {
      action: "volumeUp",
      preventDefault: true,
    });
    assert.deepEqual(resolveShortcutAction({ key: "PageDown" }, mediaCtx), {
      action: "noop",
      preventDefault: true,
    });
  });

  it("上下キーは本モード中のみ 1 ページ移動として機能する", () => {
    const normalCtx = { isMac: false, showingMedia: false, showingBook: false };
    const bookCtx = { isMac: false, showingMedia: false, showingBook: true };

    assert.equal(resolveShortcutAction({ key: "ArrowUp" }, normalCtx), null);
    assert.deepEqual(resolveShortcutAction({ key: "ArrowUp" }, bookCtx), {
      action: "stepBookPrev",
      preventDefault: true,
    });
    assert.deepEqual(resolveShortcutAction({ key: "ArrowDown" }, bookCtx), {
      action: "stepBookNext",
      preventDefault: true,
    });
  });
});
