import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { applyTheme } from "../src/theme.js";

function withMockDocument(fn) {
  const props = new Map();
  const prev = globalThis.document;
  globalThis.document = {
    documentElement: {
      style: {
        setProperty(name, value) {
          props.set(name, value);
        },
      },
    },
  };
  try {
    fn(props);
  } finally {
    if (prev === undefined) delete globalThis.document;
    else globalThis.document = prev;
  }
}

describe("theme", () => {
  it("暗い背景色には明るい文字色とダーク用アクセントを設定する", () => {
    withMockDocument((props) => {
      applyTheme({ backgroundColor: "#0e0e10", backgroundOpacity: 97 });
      assert.equal(props.get("--bg"), "rgba(14, 14, 16, 0.97)");
      assert.equal(props.get("--accent"), "#5a82e6");
      assert.match(props.get("--fg"), /^rgba\(2\d\d, 2\d\d, 2\d\d, 1\)$/);
      assert.match(props.get("--select-arrow"), /^url\("data:image\/svg\+xml/);
    });
  });

  it("明るい背景色には暗い文字色とライト用アクセントを設定する", () => {
    withMockDocument((props) => {
      applyTheme({ backgroundColor: "#f5f5f5", backgroundOpacity: 100 });
      assert.equal(props.get("--bg"), "rgba(245, 245, 245, 1)");
      assert.equal(props.get("--accent"), "#3563d6");
      assert.match(props.get("--fg"), /^rgba\(29, 29, 29, 1\)$/);
    });
  });

  it("不透明度は 0.2〜1 の範囲にクランプされる", () => {
    withMockDocument((props) => {
      applyTheme({ backgroundColor: "#000000", backgroundOpacity: 5 });
      assert.equal(props.get("--bg"), "rgba(0, 0, 0, 0.2)");
      applyTheme({ backgroundColor: "#000000", backgroundOpacity: 150 });
      assert.equal(props.get("--bg"), "rgba(0, 0, 0, 1)");
    });
  });
});
