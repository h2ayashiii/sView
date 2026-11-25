import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  normalizeSettings,
  SETTINGS_DEFAULTS,
  SETTINGS_SECTIONS,
  SETTINGS_TABS,
} from "../src/settings-defs.js";

describe("settings-defs", () => {
  it("SETTINGS_DEFAULTS には default の無い項目（action / associations）が入らない", () => {
    for (const [key, value] of Object.entries(SETTINGS_DEFAULTS)) {
      assert.notEqual(value, undefined, `${key} が undefined になっています`);
    }
    const actionKeys = SETTINGS_SECTIONS.flatMap((s) =>
      s.items.filter((i) => !("default" in i)).map((i) => i.key)
    );
    assert.ok(actionKeys.length > 0);
    for (const key of actionKeys) {
      assert.equal(key in SETTINGS_DEFAULTS, false, `${key} は既定値に含めない`);
    }
  });

  it("すべてのセクションの tab が SETTINGS_TABS に存在する", () => {
    const tabIds = new Set(SETTINGS_TABS.map((t) => t.id));
    for (const section of SETTINGS_SECTIONS) {
      assert.ok(tabIds.has(section.tab), `未知のタブ: ${section.tab}`);
    }
  });

  it("null / undefined / 空オブジェクトでも既定値で補完される", () => {
    assert.deepEqual(normalizeSettings(null), SETTINGS_DEFAULTS);
    assert.deepEqual(normalizeSettings(undefined), SETTINGS_DEFAULTS);
    assert.deepEqual(normalizeSettings({}), SETTINGS_DEFAULTS);
  });

  it("廃止キー（OBSOLETE_KEYS）は読み捨てられる", () => {
    const result = normalizeSettings({
      showNavButtons: true,
      roundedCorners: false,
      wrapAround: true,
      sideButtons: false,
      windowPerKind: true,
    });
    for (const key of [
      "showNavButtons",
      "roundedCorners",
      "wrapAround",
      "sideButtons",
      "windowPerKind",
    ]) {
      assert.equal(key in result, false, `${key} が残っています`);
    }
  });

  it("古い videoLoop は videoEnd が無いときだけ引き継がれる", () => {
    assert.equal(normalizeSettings({ videoLoop: true }).videoEnd, "repeat");
    assert.equal(normalizeSettings({ videoLoop: false }).videoEnd, "stop");
    assert.equal(normalizeSettings({ videoLoop: true, videoEnd: "next" }).videoEnd, "next");
    assert.equal("videoLoop" in normalizeSettings({ videoLoop: true }), false);
  });

  it("古い videoVolume は audioVolume が無いときだけ引き継がれる", () => {
    assert.equal(normalizeSettings({ videoVolume: 0.4 }).audioVolume, 0.4);
    assert.equal(normalizeSettings({ videoVolume: 0.4, audioVolume: 0.8 }).audioVolume, 0.8);
  });

  it("古い videoMuted は audioMuted が無いときだけ引き継がれる", () => {
    assert.equal(normalizeSettings({ videoMuted: true }).audioMuted, true);
    assert.equal(normalizeSettings({ videoMuted: true, audioMuted: false }).audioMuted, false);
  });

  it("古い windowSizeMode の値（fixed / flexible）を読み替える", () => {
    assert.equal(normalizeSettings({ windowSizeMode: "fixed" }).windowSizeMode, "free");
    assert.equal(normalizeSettings({ windowSizeMode: "flexible" }).windowSizeMode, "image");
  });

  it("LEGACY_VALUES の参照でプロトタイプ継承プロパティを誤変換しない", () => {
    const result = normalizeSettings({ windowSizeMode: "constructor" });
    assert.equal(result.windowSizeMode, "constructor");
  });
});
