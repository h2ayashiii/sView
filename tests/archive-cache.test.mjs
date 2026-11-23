import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createArchiveCache } from "../src/viewer/archive-cache.js";

function withMockBlobUrls(fn) {
  const origCreate = URL.createObjectURL;
  const origRevoke = URL.revokeObjectURL;
  let seq = 0;
  const revoked = [];
  URL.createObjectURL = () => `blob:mock-${++seq}`;
  URL.revokeObjectURL = (url) => revoked.push(url);
  return Promise.resolve()
    .then(() => fn(revoked))
    .finally(() => {
      URL.createObjectURL = origCreate;
      URL.revokeObjectURL = origRevoke;
    });
}

describe("archive-cache", () => {
  it("同じエントリの再読み込みはキャッシュから返し、同時読み込みも重複しない", async () => {
    await withMockBlobUrls(async () => {
      let invokes = 0;
      const cache = createArchiveCache({
        invoke: async (_cmd, { entry }) => {
          invokes++;
          return new Uint8Array([entry.length]).buffer;
        },
        convertFileSrc: (p) => `asset://${p}`,
        getArchivePath: () => "/books/sample.cbz",
        getShownEntries: () => ["01.png"],
      });

      // 同時に 2 回呼んでも IPC は 1 回だけ
      const [u1, u2] = await Promise.all([
        cache.archiveBlobUrl("01.png"),
        cache.archiveBlobUrl("01.png"),
      ]);
      assert.equal(u1, u2);
      assert.equal(invokes, 1);

      // キャッシュヒットで IPC は増えない
      const u3 = await cache.archiveBlobUrl("01.png");
      assert.equal(u3, u1);
      assert.equal(invokes, 1);
    });
  });

  it("上限（6 件）を超えると表示中以外の古いエントリから解放する", async () => {
    await withMockBlobUrls(async (revoked) => {
      const cache = createArchiveCache({
        invoke: async () => new Uint8Array([1, 2, 3]).buffer,
        convertFileSrc: (p) => `asset://${p}`,
        getArchivePath: () => "/books/sample.cbz",
        getShownEntries: () => ["01.png"],
      });

      const urls = [];
      for (let i = 1; i <= 7; i++) {
        urls.push(await cache.archiveBlobUrl(`0${i}.png`));
      }
      // 01.png は表示中なので残され、2 番目に古い 02.png が解放される
      assert.deepEqual(revoked, [urls[1]]);

      cache.clearBlobCache();
      assert.equal(revoked.length, 7);
    });
  });

  it("clearBlobCache 後に完了した古い読み込みは破棄される", async () => {
    await withMockBlobUrls(async () => {
      let resolveInvoke;
      const cache = createArchiveCache({
        invoke: () => new Promise((r) => (resolveInvoke = r)),
        convertFileSrc: (p) => `asset://${p}`,
        getArchivePath: () => "/books/a.cbz",
        getShownEntries: () => [],
      });

      const pending = cache.archiveBlobUrl("01.png");
      cache.clearBlobCache();
      resolveInvoke(new Uint8Array([1]).buffer);

      await assert.rejects(pending, /書庫が切り替わった/);
    });
  });

  it("entrySrc は書庫なら blob URL、通常ファイルなら convertFileSrc を使う", async () => {
    await withMockBlobUrls(async () => {
      let archive = null;
      const cache = createArchiveCache({
        invoke: async () => new Uint8Array([1]).buffer,
        convertFileSrc: (p) => `asset://${p}`,
        getArchivePath: () => archive,
        getShownEntries: () => [],
      });

      assert.equal(await cache.entrySrc("/img/a.png"), "asset:///img/a.png");
      archive = "/books/a.zip";
      assert.match(await cache.entrySrc("a.png"), /^blob:mock-/);
    });
  });
});
