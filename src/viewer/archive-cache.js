// 書庫内画像の遅延ロード・LRU blob キャッシュ

import { extOf, MIME, toBytes } from "./media-types.js";

// 書庫は一括展開せず、表示するエントリだけを Rust 側から取り出して
// blob URL 化する。前後の先読み分を含め数枚だけ保持する
// （本モードでは見開きの 2 枚と、前後の見開き 2 枚ずつ）
const BLOB_CACHE_MAX = 6;

export function createArchiveCache({ invoke, convertFileSrc, getArchivePath, getShownEntries }) {
  const blobCache = new Map(); // entry -> blob URL（挿入順 = LRU 順）
  // 取り出し中のエントリ（entry -> blob URL の Promise）。先読み中のページへ移ったときなどに、
  // 同じエントリを 2 回取り出さない（2 回目の blob URL が解放されずに残る）
  const blobLoading = new Map();
  // 書庫を開き直すたびに増やす。取り出している間に書庫が変わったら、その結果は使わない
  // （同じエントリ名の別の書庫の画像を出さない）
  let blobGeneration = 0;

  function clearBlobCache() {
    for (const url of blobCache.values()) URL.revokeObjectURL(url);
    blobCache.clear();
    blobLoading.clear();
    blobGeneration++;
  }

  function trimBlobCache() {
    const shown = getShownEntries();
    for (const [entry, url] of blobCache) {
      if (blobCache.size <= BLOB_CACHE_MAX) break;
      if (shown.includes(entry)) continue; // 表示中のものは残す
      URL.revokeObjectURL(url);
      blobCache.delete(entry);
    }
  }

  function archiveBlobUrl(entry) {
    const cached = blobCache.get(entry);
    if (cached) {
      // LRU 更新
      blobCache.delete(entry);
      blobCache.set(entry, cached);
      return Promise.resolve(cached);
    }
    // 取り出し中なら、その結果を待つ
    const loading = blobLoading.get(entry);
    if (loading) return loading;
    const generation = blobGeneration;
    const promise = invoke("read_archive_image", { archive: getArchivePath(), entry })
      .then((data) => {
        if (generation !== blobGeneration) {
          throw new Error("書庫が切り替わったため読み込みを取りやめました");
        }
        const bytes = toBytes(data);
        const url = URL.createObjectURL(new Blob([bytes], { type: MIME[extOf(entry)] ?? "" }));
        blobCache.set(entry, url);
        trimBlobCache();
        return url;
      })
      .finally(() => {
        if (blobLoading.get(entry) === promise) blobLoading.delete(entry);
      });
    blobLoading.set(entry, promise);
    return promise;
  }

  async function entrySrc(entry) {
    return getArchivePath() ? archiveBlobUrl(entry) : convertFileSrc(entry);
  }

  return {
    clearBlobCache,
    trimBlobCache,
    archiveBlobUrl,
    entrySrc,
  };
}
