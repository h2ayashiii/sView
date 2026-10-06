// sView - minimal frameless image viewer
// withGlobalTauri: true のため window.__TAURI__ からAPIを利用（バンドラ不要）
const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen, emit } = window.__TAURI__.event;
const dialog = window.__TAURI__.dialog;
const appWindow = window.__TAURI__.window.getCurrentWindow();

const app = document.getElementById("app");
const stage = document.getElementById("stage");
const img = document.getElementById("image");
const video = document.getElementById("video");
const videobar = document.getElementById("videobar");
const vbPlay = document.getElementById("vb-play");
const vbMute = document.getElementById("vb-mute");
const vbBack = document.getElementById("vb-back");
const vbFwd = document.getElementById("vb-fwd");
const vbPrev = document.getElementById("vb-prev");
const vbNext = document.getElementById("vb-next");
const vbSeek = document.getElementById("vb-seek");
const vbVolume = document.getElementById("vb-volume");
const vbTime = document.getElementById("vb-time");
const vbDuration = document.getElementById("vb-duration");
const vbTip = document.getElementById("vb-tip");
const audioBox = document.getElementById("audio");
const artwork = document.getElementById("artwork");
const audioArt = document.getElementById("audio-art");
const audioPanel = document.getElementById("audio-panel");
const audioTitle = document.getElementById("audio-title");
const audioSub = document.getElementById("audio-sub");
const audioProgressFill = document.getElementById("audio-progress-fill");
const placeholder = document.getElementById("placeholder");
const errorBox = document.getElementById("error");
const filenameEl = document.getElementById("filename");
const counterEl = document.getElementById("counter");
const chromeEl = document.getElementById("chrome");
const navPrev = document.getElementById("nav-prev");
const navNext = document.getElementById("nav-next");
const ctxmenu = document.getElementById("ctxmenu");
const btnMax = document.getElementById("btn-max");
const confirmEl = document.getElementById("confirm");
const confirmNameEl = document.getElementById("confirm-name");
const confirmSkipEl = document.getElementById("confirm-skip");
const confirmOkEl = document.getElementById("confirm-ok");

// 現在の設定（settings-defs.js の既定値で開始し、読み込み後に上書きされる）
let settings = { ...SETTINGS_DEFAULTS };

// macOS だけ閉じるボタンを左上に置く（OS の慣習に合わせる）
const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
app.classList.toggle("mac", IS_MAC);

const IMAGE_EXT_FILTER = [
  "avif", "bmp", "gif", "ico", "jfif", "jpe", "jpeg", "jpg",
  "png", "svg", "tif", "tiff", "webp",
];
// 動画は OS の WebView が再生できるものだけ（コーデックは同梱しない）。
// 書庫の中の動画は一覧に出さない
const VIDEO_EXT_FILTER = ["mp4", "m4v", "webm", "mov"];
// 音楽も同じく OS の WebView 任せ。書庫の中の音楽は一覧に出さない
const AUDIO_EXT_FILTER = ["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus"];
const ARCHIVE_EXT_FILTER = ["zip", "cbz"];

// blob URL に必要な MIME（拡張子から判定）
const MIME = {
  avif: "image/avif", bmp: "image/bmp", gif: "image/gif", ico: "image/x-icon",
  jfif: "image/jpeg", jpe: "image/jpeg", jpeg: "image/jpeg", jpg: "image/jpeg",
  png: "image/png", svg: "image/svg+xml", tif: "image/tiff", tiff: "image/tiff",
  webp: "image/webp",
};

// フォルダの場合は画像のフルパス、書庫の場合は書庫内のエントリ名（自然順ソート済み）
let images = [];
let index = -1;
// 書庫を開いている場合はそのフルパス。フォルダの場合は null
let archivePath = null;
// フォルダを開いている場合はそのフルパス（監視対象）。書庫の場合は null
let folderPath = null;
// 一覧に並べている種類（"image" / "video" / "audio"。決まっていなければ null）。
// 単体のファイルを開いたらその種類、フォルダ・書庫なら先頭のファイルの種類で、
// 読み直しても変えない
let listKind = null;
// 一覧を読んだ深さ（単体のファイルから開いたら 1 = 直下だけ、フォルダ・書庫は 3）。
// 読み直しと監視も同じ深さで行う
let listDepth = 1;
// 表示名から取り除く先頭部分（書庫では全エントリの共通フォルダ、
// フォルダをサブフォルダごと読んだときは開いたフォルダのパス）
let entryPrefix = "";
// 表示要求の世代。非同期読み込みの結果が古い場合は捨てる
let showToken = 0;
// 表示中のものが動画なら true（#image の代わりに #video を使う）
let showingVideo = false;
// 表示中のものが音楽なら true（#video で鳴らし、#audio にアートワークを出す）
let showingAudio = false;
// 動画か音楽（どちらも #video で再生し、再生操作のカードを出す）
let showingMedia = false;

// ---- 書庫内画像の遅延ロード ----
// 書庫は一括展開せず、表示するエントリだけを Rust 側から取り出して
// blob URL 化する。前後の先読み分を含め数枚だけ保持する
const BLOB_CACHE_MAX = 5;
const blobCache = new Map(); // entry -> blob URL（挿入順 = LRU 順）

function extOf(name) {
  const m = /\.([^.\\/]+)$/.exec(name);
  return m ? m[1].toLowerCase() : "";
}

function isVideoPath(name) {
  return VIDEO_EXT_FILTER.includes(extOf(name));
}

function isAudioPath(name) {
  return AUDIO_EXT_FILTER.includes(extOf(name));
}

function clearBlobCache() {
  for (const url of blobCache.values()) URL.revokeObjectURL(url);
  blobCache.clear();
}

function trimBlobCache() {
  while (blobCache.size > BLOB_CACHE_MAX) {
    const [oldest, url] = blobCache.entries().next().value;
    if (oldest === images[index]) break; // 表示中のものは残す
    URL.revokeObjectURL(url);
    blobCache.delete(oldest);
  }
}

// Rust から返る生バイト列の受け取り方は IPC の経路によって変わる
// （カスタムプロトコルなら ArrayBuffer、postMessage 経由なら数値の配列）。
// そのまま Blob に渡すと配列が文字列化されて画像が壊れるので、必ずここで揃える
function toBytes(data) {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) {
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  }
  if (Array.isArray(data)) return Uint8Array.from(data);
  throw new Error("画像データを取得できませんでした");
}

async function archiveBlobUrl(entry) {
  const cached = blobCache.get(entry);
  if (cached) {
    // LRU 更新
    blobCache.delete(entry);
    blobCache.set(entry, cached);
    return cached;
  }
  const bytes = toBytes(await invoke("read_archive_image", { archive: archivePath, entry }));
  const url = URL.createObjectURL(new Blob([bytes], { type: MIME[extOf(entry)] ?? "" }));
  blobCache.set(entry, url);
  trimBlobCache();
  return url;
}

// ---- zoom / pan state ----
// mode "fit": ウィンドウにフィット（このときステージはウィンドウドラッグ領域になる）
// mode "zoom": 拡大縮小・パン可能
let mode = "fit";
let scale = 1;
let tx = 0;
let ty = 0;

function setFitMode() {
  mode = "fit";
  img.className = "fit";
  video.className = "fit";
  artwork.className = "fit";
  img.style.transform = "";
  img.style.position = "";
  unfreezeImageSize();
  // 動画・音楽の上はクリックで再生 / 一時停止にするので、ウィンドウのドラッグ領域にしない
  // （押したまま動かしたときは自分で startDragging() を呼ぶ）
  if (showingMedia) stage.removeAttribute("data-tauri-drag-region");
  else stage.setAttribute("data-tauri-drag-region", "");
  stage.classList.remove("panning");
}

// ---- ウィンドウのサイズ変更中は画像を据え置く ----
// ドラッグの間じゅう画像を拡大縮小し直すと、そのたびに描き直しになって重く、
// 絵がちらついて見える（角を引っ張って縦横が同時に変わるときに目立つ）。
// 端・角のどこを引っ張っても、マウスを離してウィンドウの大きさが決まるまでは
// 画像・動画・アートワークの大きさを変えず、離してから合わせ直す
// （ウィンドウサイズの設定によらない）
let frozenSize = false;
// 画像を切り替えて自分でウィンドウを合わせ直したあと、手で変えたのではないと
// みなす時間。この間は据え置かず、ウィンドウと一緒に画像も合わせる
const SELF_RESIZE_MS = 400;
let selfResizedAt = 0;

function freezeImageSize() {
  if (frozenSize || mode !== "fit" || !mediaSize()) return;
  const el = mediaEl();
  const rect = el.getBoundingClientRect();
  if (!rect.width || !rect.height) return;
  el.style.width = `${rect.width}px`;
  el.style.height = `${rect.height}px`;
  el.classList.add("frozen");
  frozenSize = true;
}

function unfreezeImageSize() {
  frozenSize = false;
  for (const el of [img, video, artwork]) {
    el.style.width = "";
    el.style.height = "";
    el.classList.remove("frozen");
  }
  layoutArtwork();
}

// アートワークの箱を、実際に描かれる絵の大きさにする。
// object-fit のままだと箱が枠いっぱいに広がり、角の丸めが絵ではなく箱に掛かってしまう
function layoutArtwork() {
  if (!showingAudio || frozenSize) return;
  const { naturalWidth: w, naturalHeight: h } = artwork;
  const pad = getComputedStyle(audioArt);
  const boxW = audioArt.clientWidth - parseFloat(pad.paddingLeft) - parseFloat(pad.paddingRight);
  const boxH = audioArt.clientHeight - parseFloat(pad.paddingTop) - parseFloat(pad.paddingBottom);
  if (!w || !h || !artwork.complete || boxW <= 0 || boxH <= 0) {
    artwork.style.width = "";
    artwork.style.height = "";
    return;
  }
  const s = Math.min(boxW / w, boxH / h);
  artwork.style.width = `${w * s}px`;
  artwork.style.height = `${h * s}px`;
}

// 動画・音楽は拡大縮小しない（常にウィンドウに合わせて表示する）
function enterZoomMode() {
  if (mode === "zoom" || showingMedia || !img.src || !img.naturalWidth) return;
  // 据え置き中なら先に戻す（据え置きの大きさから倍率を出すと二重にかかる）
  unfreezeImageSize();
  // フィット表示の <img> は箱がウィンドウ全体で、絵は object-fit: contain で中に収まっている。
  // 箱ではなく実際に描かれている絵の大きさと位置から倍率を出す
  const rect = img.getBoundingClientRect();
  scale = Math.min(rect.width / img.naturalWidth, rect.height / img.naturalHeight);
  tx = rect.left + (rect.width - img.naturalWidth * scale) / 2;
  ty = rect.top + (rect.height - img.naturalHeight * scale) / 2;
  mode = "zoom";
  img.className = "";
  img.style.position = "absolute";
  img.style.left = "0";
  img.style.top = "0";
  stage.removeAttribute("data-tauri-drag-region");
  stage.classList.add("panning");
  applyTransform();
}

function applyTransform() {
  img.style.transform = `translate(${tx}px, ${ty}px) scale(${scale})`;
}

function zoomAt(cx, cy, factor) {
  enterZoomMode();
  if (mode !== "zoom") return;
  const next = Math.min(30, Math.max(0.03, scale * factor));
  const f = next / scale;
  tx = cx - (cx - tx) * f;
  ty = cy - (cy - ty) * f;
  scale = next;
  applyTransform();
}

function zoomTo(target) {
  enterZoomMode();
  if (mode !== "zoom") return;
  const cx = window.innerWidth / 2;
  const cy = window.innerHeight / 2;
  const f = target / scale;
  tx = cx - (cx - tx) * f;
  ty = cy - (cy - ty) * f;
  scale = target;
  applyTransform();
}

// ---- display ----
// エラーと案内（端に到達したなど）を同じ場所に出す。kind で色だけ変える
function showToast(message, kind) {
  errorBox.textContent = String(message);
  errorBox.classList.toggle("notice", kind === "notice");
  errorBox.hidden = false;
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => (errorBox.hidden = true), kind === "notice" ? 1500 : 4000);
}

function showError(message) {
  showToast(message, "error");
}

function baseName(path) {
  return path.split(/[\\/]/).pop();
}

function updateChrome() {
  if (index < 0 || !images.length) {
    app.classList.add("no-image");
    filenameEl.textContent = "";
    counterEl.textContent = "";
    appWindow.setTitle("sView").catch(() => {});
    return;
  }
  app.classList.remove("no-image");
  // 書庫内やサブフォルダまで読んだフォルダは同名ファイルが別フォルダに並びうるので、
  // 共通フォルダ（開いたフォルダ）を除いた相対パスで表示する
  // （単一フォルダの書庫なら結果的にファイル名だけになる）
  const name = entryPrefix && images[index].startsWith(entryPrefix)
    ? images[index].slice(entryPrefix.length)
    : baseName(images[index]);
  filenameEl.textContent = archivePath ? `${baseName(archivePath)} / ${name}` : name;
  filenameEl.title = archivePath ? `${archivePath} :: ${images[index]}` : images[index];
  counterEl.textContent = `${index + 1} / ${images.length}`;
  navPrev.disabled = index === 0;
  navNext.disabled = index === images.length - 1;
  // 動画・音楽の前後移動は再生操作カードのボタンだけ（端で折り返す設定なら端でも押せる）
  vbPrev.disabled = !settings.wrapAround && index === 0;
  vbNext.disabled = !settings.wrapAround && index === images.length - 1;
  appWindow.setTitle(`${baseName(images[index])} - sView`).catch(() => {});
}

function preloadNeighbors() {
  if (!settings.preload || images.length < 2) return;
  for (const off of [1, -1]) {
    // 端で折り返さないので、範囲外は先読みしない
    const i = index + off;
    if (i < 0 || i >= images.length) continue;
    // 動画・音楽は大きいので先読みしない
    if (!archivePath && (isVideoPath(images[i]) || isAudioPath(images[i]))) continue;
    if (archivePath) {
      // 先読みも 1 件ずつ。失敗しても表示には影響させない
      archiveBlobUrl(images[i]).catch(() => {});
    } else {
      new Image().src = convertFileSrc(images[i]);
    }
  }
}

// 表示中の要素（画像か動画。音楽ならアートワーク）
function mediaEl() {
  if (showingAudio) return artwork;
  return showingVideo ? video : img;
}

// 表示中の画像・動画の実寸。まだ分からなければ null。
// 音楽はアートワークの実寸に加えて、周りの余白と曲名の帯の大きさ（px で固定。
// ウィンドウの大きさによらない）を extraWidth / extraHeight で返す。
// 「メディアに合わせる」では Rust 側がこの分を除いた残りをアートワークの縦横比に合わせる
// （固定の分を今のウィンドウ幅で比率に直して縦横比に混ぜると、直前のウィンドウの形に
// 引きずられ、同じ曲でも開き直すたびに大きさが少しずつ変わってしまう）
function mediaSize() {
  if (showingAudio) {
    const { naturalWidth: w, naturalHeight: h } = artwork;
    if (!w || !h || !artwork.complete) return null;
    const pad = getComputedStyle(audioArt);
    return {
      width: w,
      height: h,
      extraWidth: parseFloat(pad.paddingLeft) + parseFloat(pad.paddingRight),
      extraHeight: parseFloat(pad.paddingTop) + audioPanel.offsetHeight,
    };
  }
  const [width, height] = showingVideo
    ? [video.videoWidth, video.videoHeight]
    : [img.naturalWidth, img.naturalHeight];
  return width && height ? { width, height } : null;
}

// 読み込みが終わってからでないと画像の実寸が分からないので、
// ウィンドウサイズ合わせと起動時倍率はここでまとめて行う
function onImageReady(run) {
  if (img.complete && img.naturalWidth) run();
  else img.addEventListener("load", run, { once: true });
}

function onArtworkReady(run) {
  if (artwork.complete && artwork.naturalWidth) run();
  else artwork.addEventListener("load", run, { once: true });
}

// 動画は再生前に大きさ（メタデータ）だけ先に分かる
function onVideoReady(run) {
  if (video.readyState >= HTMLMediaElement.HAVE_METADATA) run();
  else video.addEventListener("loadedmetadata", run, { once: true });
}

function fitsToImage() {
  return settings.windowSizeMode === "image";
}

// 表示中のファイルの種類（何も開いていなければ null）
function shownKind() {
  if (index < 0) return null;
  return showingVideo ? "video" : showingAudio ? "audio" : "image";
}

// 表示する種類を Rust 側へ知らせる。設定「ファイルの種類ごとにウィンドウを保持する」が
// オンなら、種類が変わったところでその種類の大きさ・位置へ戻る。
// 縦横比合わせ（fitWindowToImage）はその後に行うので、終わるのを待てるよう覚えておく
let windowKindSync = Promise.resolve();

function syncWindowKind() {
  const kind = shownKind();
  if (!kind) return;
  // 種類ごとの大きさへ戻すのは自分で変えたもの（据え置かずに一緒に合わせる）
  selfResizedAt = Date.now();
  windowKindSync = invoke("set_window_kind", {
    kind,
    perKind: !!settings.windowPerKind,
  })
    .catch(() => {})
    .then(() => (selfResizedAt = Date.now()));
}

// 「メディアに合わせる」のとき、ウィンドウの縦横比を表示中の画像に固定する。
// 以後は端や角をドラッグしている最中も Rust 側が縦横比を保つ。
// 「自由に変更」や画像を開いていないときは解除する
function syncAspectLock() {
  const size = fitsToImage() && index >= 0 ? mediaSize() : null;
  return invoke("set_aspect_lock", {
    ratio: size ? size.width / size.height : null,
    extraWidth: size?.extraWidth ?? 0,
    extraHeight: size?.extraHeight ?? 0,
  }).catch(() => {});
}

// 「メディアに合わせる」のとき、余白が出ないようウィンドウを画像の縦横比に合わせる。
// 大きさ（広さ）は Rust 側が覚えていて、手でサイズを変えたときだけ変わる
async function fitWindowToImage() {
  await windowKindSync;
  await syncAspectLock();
  if (!fitsToImage()) return;
  const size = mediaSize();
  if (!size) return;
  // 自分で変えている間の印。サイズ変更の通知は invoke の前後どちらでも届きうる
  selfResizedAt = Date.now();
  try {
    await invoke("fit_window_to_image", size);
  } catch (e) {
    showError(e);
  }
  selfResizedAt = Date.now();
}

// 設定「画像を開いたときの表示」が等倍なら 100% で表示する
function applyStartupZoom() {
  if (settings.startupZoom === "actual") zoomTo(1);
}

async function show() {
  if (index < 0 || index >= images.length) return;
  const token = ++showToken;
  // 前の動画・音楽は必ず止めて手放す（再生を続けさせない・ファイルを掴んだままにしない）
  unloadVideo();
  // 曲の終わりから次の曲へ進んだときは、設定にかかわらず続けて再生する
  const continuing = continuePlayback;
  continuePlayback = false;
  // 書庫の中の動画・音楽は一覧に出てこないので、常にファイルとして開ける
  showingVideo = !archivePath && isVideoPath(images[index]);
  showingAudio = !archivePath && isAudioPath(images[index]);
  showingMedia = showingVideo || showingAudio;
  app.classList.toggle("video", showingVideo);
  app.classList.toggle("audio", showingAudio);
  video.hidden = !showingVideo;
  audioBox.hidden = !showingAudio;
  img.hidden = showingMedia;
  syncWindowKind();
  setFitMode();
  placeholder.hidden = true;
  updateChrome();
  if (showingMedia) {
    img.removeAttribute("src");
    const autoplay = continuing || (showingAudio ? settings.audioAutoplay : settings.videoAutoplay);
    loadVideo(convertFileSrc(images[index]), autoplay);
    if (showingAudio) {
      loadAudioInfo(images[index], token);
    } else {
      setArtwork(null);
      onVideoReady(() => {
        if (token === showToken) fitWindowToImage();
      });
    }
    preloadNeighbors();
    return;
  }
  setArtwork(null);
  try {
    const src = archivePath
      ? await archiveBlobUrl(images[index])
      : convertFileSrc(images[index]);
    if (token !== showToken) return; // 既に別の画像へ移動している
    img.src = src;
    onImageReady(async () => {
      if (token !== showToken) return;
      await fitWindowToImage();
      if (token !== showToken) return;
      applyStartupZoom();
    });
  } catch (e) {
    if (token === showToken) showError(e);
    return;
  }
  preloadNeighbors();
}

img.addEventListener("error", () => {
  if (img.src) showError(`読み込みに失敗しました: ${baseName(images[index] ?? "")}`);
});

// ---- 動画 ----
// 再生は OS の WebView 任せ（Windows: WebView2 / macOS: WKWebView）。
// 再生できる形式は OS ごとに違い、sView からは増やせない
const SEEK_STEP_S = 5;
// ↑ ↓ で変える音量の幅（0〜1）
const VOLUME_STEP = 0.05;
// ↑ ↓ を押し続けている間は保存をまとめる（押すたびに settings.json を書かない）
const VOLUME_SAVE_DELAY_MS = 500;
let volumeSaveTimer = null;
// シークバーをつかんでいる間は、再生位置でつまみを動かさない
let seekDragging = false;
// 自動再生の制限で音を消したことは 1 回だけ知らせる（動画ごとに出すとうるさい）
let autoplayMuteNoticed = false;

function loadVideo(src, autoplay) {
  video.loop = mediaLoop();
  setVolume(mediaVolume());
  video.src = src;
  syncVideoBar();
  if (autoplay) playVideo();
}

// 設定「動画が終わったら」「曲が終わったら」（"next" / "repeat" / "stop"）
function mediaEnd() {
  return showingAudio ? settings.audioEnd : settings.videoEnd;
}

// 繰り返し再生するか（「同じ動画 / 曲を繰り返す」のとき）
function mediaLoop() {
  return mediaEnd() === "repeat";
}

// src を外して読み込み直すと、WebView はファイルを手放す
// （Windows では再生中のファイルをゴミ箱へ送れないことがあるため）
function unloadVideo() {
  if (!video.getAttribute("src")) return;
  video.pause();
  video.removeAttribute("src");
  video.load();
  seekDragging = false;
}

// 音楽は動画より音が大きいことが多いので、同じ音量つまみの位置でも実際の音量を半分にする
// （つまみ・↑ ↓ キー・保存する値は 0〜100%。100% のとき素材の音量の半分で鳴る）
const AUDIO_VOLUME_GAIN = 0.5;

function volumeGain() {
  return showingAudio ? AUDIO_VOLUME_GAIN : 1;
}

// つまみ上の音量（0〜1）と、<video> 要素に入れる実際の音量の変換
function getVolume() {
  return video.volume / volumeGain();
}

function setVolume(v) {
  video.volume = Math.min(1, Math.max(0, v)) * volumeGain();
}

// 音量は動画と音楽で別々に保存する（0〜100%）
function volumeKey() {
  return showingAudio ? "audioVolume" : "videoVolume";
}

function mediaVolume() {
  const v = Number(settings[volumeKey()]);
  return Number.isFinite(v) ? Math.min(1, Math.max(0, v / 100)) : 1;
}

function playVideo() {
  video.play().catch((e) => {
    // WebView の自動再生の制限で、音ありの再生だけ止められることがある。
    // 消音なら許されるので、音を消して再生し直す
    if (e?.name !== "NotAllowedError" || video.muted) return;
    video.muted = true;
    syncVideoBar();
    video.play().catch(() => {});
    if (autoplayMuteNoticed) return;
    autoplayMuteNoticed = true;
    showToast("自動再生のため音を消しました（M で戻せます）", "notice");
  });
}

function togglePlay() {
  if (!showingMedia) return;
  if (video.paused || video.ended) playVideo();
  else video.pause();
}

// 消音は保存しない（起動したときは常に音が出る。起動している間は次のファイルにも持ち越す）
function toggleMute() {
  if (!showingMedia) return;
  video.muted = !video.muted;
}

function seekBy(seconds) {
  if (!showingMedia || !Number.isFinite(video.duration)) return;
  video.currentTime = Math.min(video.duration, Math.max(0, video.currentTime + seconds));
  // どこまで動いたかが見えるよう、再生操作を出す
  showChrome();
}

function changeVolumeBy(delta) {
  if (!showingMedia) return;
  const volume = Math.min(1, Math.max(0, Math.round((getVolume() + delta) * 100) / 100));
  setVolume(volume);
  settings[volumeKey()] = Math.round(volume * 100);
  // 音量を上げたら消音も解く（再生バーの音量つまみと同じ扱い）
  if (delta > 0 && video.muted) video.muted = false;
  clearTimeout(volumeSaveTimer);
  volumeSaveTimer = setTimeout(saveSettings, VOLUME_SAVE_DELAY_MS);
  // 音量つまみで結果が見えるよう、再生操作を出す
  showChrome();
}

function formatTime(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const s = Math.floor(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

function syncVideoBar() {
  const paused = video.paused || video.ended;
  videobar.classList.toggle("paused", paused);
  vbPlay.title = paused ? "再生 (Space)" : "一時停止 (Space)";
  vbPlay.setAttribute("aria-label", paused ? "再生" : "一時停止");
  const muted = video.muted || video.volume === 0;
  videobar.classList.toggle("muted", muted);
  vbMute.title = muted ? "消音を解除 (M)" : "消音 (M)";
  vbMute.setAttribute("aria-label", muted ? "消音を解除" : "消音");
  vbVolume.value = String(Math.round(getVolume() * 100));
  syncRangeFill(vbVolume);
  const duration = video.duration;
  vbDuration.textContent = formatTime(duration);
  vbTime.textContent = formatTime(video.currentTime);
  const ratio = duration > 0 && Number.isFinite(duration) ? video.currentTime / duration : 0;
  if (!seekDragging) vbSeek.value = String(Math.round(ratio * 1000));
  syncRangeFill(vbSeek);
  audioProgressFill.style.width = `${ratio * 100}%`;
}

// シークバー・音量つまみの済んだ側を塗る割合（style.css の --fill）。
// WebView ごとの標準の塗り分けには頼らない（macOS では塗り分けられない）
function syncRangeFill(input) {
  const min = Number(input.min), max = Number(input.max);
  const ratio = max > min ? (Number(input.value) - min) / (max - min) : 0;
  input.style.setProperty("--fill", String(Math.min(1, Math.max(0, ratio))));
}

for (const input of [vbSeek, vbVolume]) {
  input.addEventListener("input", () => syncRangeFill(input));
}

for (const type of ["play", "pause", "ended", "timeupdate", "durationchange", "volumechange", "emptied"]) {
  video.addEventListener(type, syncVideoBar);
}

video.addEventListener("error", () => {
  if (!showingMedia || !video.getAttribute("src")) return;
  const name = baseName(images[index] ?? "");
  const kind = showingAudio ? "音楽ファイル" : "動画";
  // 3: デコードできない / 4: 形式に対応していない。どちらも OS 側の再生機能の問題
  const code = video.error?.code;
  showError(
    code === 3 || code === 4
      ? `この${kind}は再生できません（OS が対応していない形式です）: ${name}`
      : `${kind}を読み込めませんでした: ${name}`
  );
});

// ---- 音楽 ----
// 再生は動画と同じ #video で行い（映像は出さない）、代わりにアートワークと曲名を出す。
// アートワークはタグに埋め込まれたもの → 同じフォルダの cover.jpg など → 既定の絵の順
const DEFAULT_ARTWORK = "audio-artwork.svg";
// 曲の終わりから次の曲へ進むときだけ true（次の show() で再生を始める）
let continuePlayback = false;
// 表示中のアートワークの blob URL（差し替えるときに解放する）
let artworkUrl = null;

function setArtwork(url) {
  if (artworkUrl) URL.revokeObjectURL(artworkUrl);
  artworkUrl = url && url.startsWith("blob:") ? url : null;
  if (url) artwork.src = url;
  else artwork.removeAttribute("src");
}

artwork.addEventListener("load", layoutArtwork);
new ResizeObserver(layoutArtwork).observe(audioArt);

// 埋め込みのアートワークが壊れていて表示できないときは既定の絵に替える
artwork.addEventListener("error", () => {
  if (showingAudio && artwork.getAttribute("src") !== DEFAULT_ARTWORK) setArtwork(DEFAULT_ARTWORK);
});

async function loadAudioInfo(path, token) {
  // タグが読めるまではファイル名を出しておく
  audioTitle.textContent = baseName(path);
  audioSub.textContent = "";
  const [info, bytes] = await Promise.all([
    invoke("audio_info", { path }).catch(() => null),
    invoke("audio_artwork", { path }).then(toBytes).catch(() => null),
  ]);
  if (token !== showToken) return; // 既に別のファイルへ移動している
  audioTitle.textContent = info?.title || baseName(path);
  audioSub.textContent = [info?.artist, info?.album].filter(Boolean).join(" — ");
  setArtwork(bytes?.length ? URL.createObjectURL(new Blob([bytes])) : DEFAULT_ARTWORK);
  onArtworkReady(() => {
    if (token === showToken) fitWindowToImage();
  });
}

// 次に再生するファイルの位置。一覧は同じ種類だけなので通常は隣のファイルだが、
// 念のため表示中と別の種類は飛ばす。無ければ -1
// （最後まで来たら、設定「端で最初 / 最後へ折り返す」のときだけ先頭から探す）
function nextMediaIndex() {
  const sameKind = showingAudio ? isAudioPath : isVideoPath;
  for (let off = 1; off < images.length; off++) {
    let i = index + off;
    if (i >= images.length) {
      if (!settings.wrapAround) return -1;
      i -= images.length;
    }
    if (sameKind(images[i])) return i;
  }
  return -1;
}

// 設定「動画が終わったら」「曲が終わったら」が「次の〜へ進む」なら、次のファイルを続けて再生する
video.addEventListener("ended", () => {
  if (!showingMedia || mediaEnd() !== "next") return;
  const next = nextMediaIndex();
  if (next < 0) return;
  index = next;
  continuePlayback = true;
  show();
});

// ボタンは押したらフォーカスを外す（Space が「ボタンを押す」にならないように）
vbPlay.addEventListener("click", () => {
  togglePlay();
  vbPlay.blur();
});
vbMute.addEventListener("click", () => {
  toggleMute();
  vbMute.blur();
});
vbPrev.addEventListener("click", () => {
  step(-1);
  vbPrev.blur();
});
vbNext.addEventListener("click", () => {
  step(1);
  vbNext.blur();
});
vbBack.addEventListener("click", () => {
  seekBy(-SEEK_STEP_S);
  vbBack.blur();
});
vbFwd.addEventListener("click", () => {
  seekBy(SEEK_STEP_S);
  vbFwd.blur();
});

vbSeek.addEventListener("pointerdown", () => (seekDragging = true));
vbSeek.addEventListener("input", () => {
  if (!Number.isFinite(video.duration)) return;
  video.currentTime = (Number(vbSeek.value) / 1000) * video.duration;
  vbTime.textContent = formatTime(video.currentTime);
});
vbSeek.addEventListener("change", () => {
  seekDragging = false;
  vbSeek.blur();
  // バーの外で離したときは吹き出しも消す
  if (!vbSeek.matches(":hover")) hideVideoTip();
});

// ---- シークバー・音量つまみに乗せたときの吹き出し ----
// シークバーはカーソル位置の時刻、音量つまみは今の音量を出す。
// つまみの幅の分だけ端が内側に寄るので、位置の計算から除く（style.css の --thumb と揃える）
const RANGE_THUMB_PX = 12;
let volumeHover = false;

// anchor（シークバー / 音量つまみ）のすぐ上、横は clientX に合わせる（カードからははみ出さない）
function showVideoTip(text, clientX, anchor) {
  vbTip.textContent = text;
  vbTip.hidden = false;
  const card = videobar.getBoundingClientRect();
  const half = vbTip.offsetWidth / 2;
  const x = Math.min(card.width - half, Math.max(half, clientX - card.left));
  vbTip.style.left = `${x}px`;
  vbTip.style.top = `${anchor.getBoundingClientRect().top - card.top - vbTip.offsetHeight - 4}px`;
}

function hideVideoTip() {
  vbTip.hidden = true;
}

function rangeRatioAt(input, clientX) {
  const r = input.getBoundingClientRect();
  const track = r.width - RANGE_THUMB_PX;
  if (track <= 0) return 0;
  return Math.min(1, Math.max(0, (clientX - r.left - RANGE_THUMB_PX / 2) / track));
}

function showVolumeTip() {
  const r = vbVolume.getBoundingClientRect();
  if (!r.width) return;
  const x = r.left + RANGE_THUMB_PX / 2 + getVolume() * (r.width - RANGE_THUMB_PX);
  const percent = Math.round(getVolume() * 100);
  showVideoTip(video.muted ? `消音中（音量 ${percent}%）` : `音量 ${percent}%`, x, vbVolume);
}

vbSeek.addEventListener("pointermove", (e) => {
  if (!Number.isFinite(video.duration) || !(video.duration > 0)) return;
  showVideoTip(formatTime(rangeRatioAt(vbSeek, e.clientX) * video.duration), e.clientX, vbSeek);
});
vbSeek.addEventListener("pointerleave", () => {
  if (!seekDragging) hideVideoTip();
});
vbVolume.addEventListener("pointerenter", () => {
  volumeHover = true;
  showVolumeTip();
});
vbVolume.addEventListener("pointerleave", () => {
  volumeHover = false;
  hideVideoTip();
});
// つまみを動かしたとき・↑ ↓ や M で変えたときも、乗せている間は表示を追従させる
video.addEventListener("volumechange", () => {
  if (volumeHover) showVolumeTip();
});

vbVolume.addEventListener("input", () => {
  setVolume(Number(vbVolume.value) / 100);
  // 音量を上げたら消音も解く（上げても聞こえないのは分かりにくい）
  if (video.volume > 0 && video.muted) video.muted = false;
});
// 保存はつまみを離したときだけ（ドラッグ中に何度も書き込まない）
vbVolume.addEventListener("change", () => {
  settings[volumeKey()] = Number(vbVolume.value);
  saveSettings();
  vbVolume.blur();
});

// ---- open / navigate ----
// opts.kind / opts.depth: 読み直しで、最初に開いたときと同じ種類・深さで並べる
// opts.current: 一覧にあればそのファイルを表示する
// opts.preferredIndex: current が無いときの表示位置（一覧の長さに収める）
async function openPath(path, opts = {}) {
  try {
    const res = await invoke("list_images", {
      path,
      kind: opts.kind ?? null,
      depth: opts.depth ?? null,
    });
    if (res.archive !== archivePath) clearBlobCache();
    archivePath = res.archive ?? null;
    folderPath = res.dir ?? null;
    listKind = res.kind ?? null;
    listDepth = res.depth ?? 1;
    entryPrefix = res.prefix ?? "";
    images = res.images;
    const at = opts.current != null ? images.indexOf(opts.current) : -1;
    const preferred = opts.preferredIndex ?? -1;
    if (at >= 0) index = at;
    else if (preferred >= 0 && images.length) index = Math.min(preferred, images.length - 1);
    else index = res.index;
    syncWatcher();
    await show();
  } catch (e) {
    showError(e);
  }
}

// 表示できる画像が無くなったときに、起動直後と同じ「ドロップ待ち」の姿へ戻す
function clearView() {
  showToken++;
  images = [];
  index = -1;
  unloadVideo();
  showingVideo = false;
  showingAudio = false;
  showingMedia = false;
  app.classList.remove("video", "audio");
  video.hidden = true;
  audioBox.hidden = true;
  setArtwork(null);
  // src の無い <img> を見せたままにすると、フィット表示の箱（ウィンドウいっぱい）に
  // 読み込み失敗の枠が出て、案内が横へ押し出される
  img.hidden = true;
  img.removeAttribute("src");
  setFitMode();
  placeholder.hidden = false;
  updateChrome();
  syncAspectLock();
}

function step(delta) {
  if (images.length === 0) return;
  let next = index + delta;
  if (next < 0 || next >= images.length) {
    if (!settings.wrapAround) {
      // 折り返さない設定のときは、そこが端であることだけ知らせる
      showToast(next < 0 ? "最初の画像です" : "最後の画像です", "notice");
      return;
    }
    next = (next + images.length) % images.length;
  }
  index = next;
  show();
}

async function rescan() {
  if (index < 0 || !images.length) return;
  if (archivePath) {
    // 書庫の中身が差し替わっている可能性があるのでキャッシュを捨てて開き直す
    clearBlobCache();
    await openPath(archivePath, { preferredIndex: index });
  } else if (folderPath) {
    // 単体のファイルから開いたかフォルダを開いたかで並べ方が違うので、
    // 最初に開いたときと同じ種類・深さで読み直す
    await openPath(folderPath, {
      kind: listKind,
      depth: listDepth,
      current: images[index],
      preferredIndex: index,
    });
  }
}

// ---- フォルダの自動追従 ----
// 監視そのものは Rust 側（OS のネイティブ通知）が行い、ここには「変わった」という
// 合図だけが届く。変化が無い間は何も走らないので、開いたまま放置しても負荷はかからない。
//
// 合図はファイル 1 個のコピーでも複数回届くうえ、届いた時点ではまだ
// 書き込み途中のことがある。最後の合図から少し待ってから 1 回だけ読み直す
const RESCAN_DELAY_MS = 800;
let rescanTimer = null;

let watchFailed = false;

// フォルダを開いている間だけ監視する。書庫のときと設定でオフのときは外す
function syncWatcher() {
  const target = !archivePath && settings.watchFolder ? folderPath : null;
  invoke("watch_folder", { path: target, depth: listDepth }).catch((e) => {
    // 監視できなくてもビューア自体は使えるので、知らせるのは 1 回だけにする
    // （R キーでいつでも読み直せる）
    if (watchFailed) return;
    watchFailed = true;
    showError(`${e}（R キーで読み直せます）`);
  });
}

function scheduleRefresh() {
  clearTimeout(rescanTimer);
  rescanTimer = setTimeout(() => {
    rescanTimer = null;
    refreshFolder();
  }, RESCAN_DELAY_MS);
}

// 一覧だけを作り直す。表示中の画像は（消えていない限り）そのまま見続けられる
async function refreshFolder() {
  if (archivePath || !folderPath) return;
  const dir = folderPath;
  const depth = listDepth;
  let res;
  try {
    res = await invoke("list_images", { path: dir, kind: listKind, depth });
  } catch {
    // フォルダごと消えた・読めなくなった場合。今の表示はそのまま残す
    return;
  }
  // 読んでいる間に別のものを開いていたら、その結果は捨てる
  // （同じフォルダでも、ファイルから開き直して深さが変わったときは捨てる）
  if (archivePath || folderPath !== dir || listDepth !== depth) return;

  const current = index >= 0 ? images[index] : null;
  const added = res.images.length - images.length;
  images = res.images;
  // 空のフォルダを開いていたときは、ここで初めて種類が決まる
  listKind = res.kind ?? listKind;

  const at = current ? images.indexOf(current) : -1;
  if (at >= 0) {
    // 表示中の画像は動かさない（前に画像が増えても位置を追いかける）
    index = at;
    updateChrome();
    preloadNeighbors();
  } else if (!images.length) {
    clearView();
  } else {
    // 表示中の画像が外から消された（または 0 枚のフォルダに画像が増えた）。
    // 同じ位置＝次の画像へ移る
    index = Math.min(Math.max(index, 0), images.length - 1);
    await show();
  }
  if (added > 0) showToast(`ファイルが ${added} 件増えました`, "notice");
}

listen("folder-changed", scheduleRefresh);

// ---- 削除 ----
// 完全削除はせず OS のゴミ箱へ送る。書庫の中身はファイルとして存在しないので対象外
function canDelete() {
  return !archivePath && index >= 0 && index < images.length;
}

let confirmResolve = null;

function closeConfirm(answer) {
  if (!confirmResolve) return;
  const done = confirmResolve;
  confirmResolve = null;
  confirmEl.hidden = true;
  done(answer);
}

// OS の確認ダイアログには「今後確認しない」を同居させられないので自前で出す。
// 開いている間はキー・ホイール・右クリックをこちらで止める
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
document
  .getElementById("confirm-cancel")
  .addEventListener("click", () => closeConfirm({ ok: false, skip: false }));
// 外側（暗い部分）を押したらキャンセル扱いにする
confirmEl.addEventListener("mousedown", (e) => {
  if (e.target === confirmEl) closeConfirm({ ok: false, skip: false });
});

async function deleteCurrent() {
  if (!canDelete()) {
    if (archivePath) showToast("圧縮フォルダの中の画像は削除できません", "notice");
    return;
  }
  const path = images[index];
  if (settings.confirmDelete) {
    const answer = await askDeleteConfirm(path);
    if (!answer.ok) return;
    if (answer.skip) {
      settings.confirmDelete = false;
      saveSettings();
    }
    // 確認している間に別の画像へ移っていたら、そのときの 1 枚を消さない
    if (images[index] !== path) return;
  }
  // 再生中の動画・音楽はファイルを手放してから送る
  const wasVideo = showingMedia;
  unloadVideo();
  try {
    await invoke("delete_image", { path });
  } catch (e) {
    showError(e);
    // 送れなかったら、止めた動画を表示し直す
    if (wasVideo && images[index] === path) show();
    return;
  }
  // 監視の合図を待たずにその場で一覧から外す
  // （ネットワークドライブなど監視が効かない場所でも確実に反映する）
  images.splice(index, 1);
  if (!images.length) {
    clearView();
  } else {
    if (index >= images.length) index = images.length - 1;
    await show();
  }
  showToast("ゴミ箱へ移動しました", "notice");
}

async function openDialog() {
  try {
    const selected = await dialog.open({
      multiple: false,
      filters: [
        {
          name: "画像・動画・音楽・圧縮フォルダ",
          extensions: [
            ...IMAGE_EXT_FILTER, ...VIDEO_EXT_FILTER, ...AUDIO_EXT_FILTER, ...ARCHIVE_EXT_FILTER,
          ],
        },
        { name: "画像", extensions: IMAGE_EXT_FILTER },
        { name: "動画", extensions: VIDEO_EXT_FILTER },
        { name: "音楽", extensions: AUDIO_EXT_FILTER },
        { name: "圧縮フォルダ (zip / cbz)", extensions: ARCHIVE_EXT_FILTER },
      ],
    });
    if (typeof selected === "string") await openPath(selected);
  } catch (e) {
    showError(e);
  }
}

async function openFolderDialog() {
  try {
    const selected = await dialog.open({ directory: true, multiple: false });
    if (typeof selected === "string") await openPath(selected);
  } catch (e) {
    showError(e);
  }
}

function toggleFullscreen() {
  selfResizedAt = Date.now();
  appWindow.isFullscreen()
    .then((fs) => appWindow.setFullscreen(!fs))
    .catch(() => {});
}

// ---- overlay chrome の自動表示・非表示 ----
// マウスを動かしている間だけ出す。同じ場所に置いたままなら
// CHROME_IDLE_MS 後に消して、マウスを乗せていないときと同じ「画像だけ」の表示に戻す
const CHROME_IDLE_MS = 3000;
let chromeTimer = null;

function hideChrome() {
  clearTimeout(chromeTimer);
  chromeTimer = null;
  // ボタンをクリックするとフォーカスが残り、:focus-within で出たままになるので外す
  if (chromeEl.contains(document.activeElement)) document.activeElement.blur();
  app.classList.remove("chrome-visible");
  hideVideoTip();
}

function showChrome() {
  app.classList.add("chrome-visible");
  clearTimeout(chromeTimer);
  chromeTimer = setTimeout(hideChrome, CHROME_IDLE_MS);
}

window.addEventListener("mousemove", showChrome);
window.addEventListener("mousedown", showChrome);
// ウィンドウの外へ出たら待たずに消す。WebKit は document への mouseleave を
// 出さないので、行き先のない mouseout（relatedTarget が null）でも拾う
document.addEventListener("mouseleave", hideChrome);
document.addEventListener("mouseout", (e) => {
  if (!e.relatedTarget) hideChrome();
});
window.addEventListener("blur", hideChrome);

// macOS の WKWebView はウィンドウがアクティブなときしかカーソルを追わないので、
// 非アクティブ中の移動や外へ出たことは Rust 側（mac_pointer）から知らせてもらう
if (IS_MAC) {
  listen("pointer-inside", (e) => (e.payload ? showChrome() : hideChrome()));
  listen("pointer-moved", showChrome);
}

// 非アクティブの間はウィンドウ操作ボタンを灰色にする（macOS の見た目）
function syncActive() {
  app.classList.toggle("inactive", !document.hasFocus());
}
window.addEventListener("focus", syncActive);
window.addEventListener("blur", syncActive);
syncActive();

// ---- input: keyboard ----
// 動画・音楽を表示している間だけのキー。処理したら true を返す。
// ← → は早送り・巻き戻し、↑ ↓ は音量。前後のファイルへはキーでは移らず、
// 再生操作カードの ⏮ ⏭ ボタンだけで移る
function handleVideoKey(e) {
  switch (e.key) {
    case " ":
    case "k":
    case "K":
      if (!e.repeat) togglePlay();
      break;
    case "m":
    case "M":
      if (!e.repeat) toggleMute();
      break;
    case "j":
    case "J":
      seekBy(-SEEK_STEP_S);
      break;
    case "l":
    case "L":
      seekBy(SEEK_STEP_S);
      break;
    case "ArrowLeft":
      seekBy(-SEEK_STEP_S);
      break;
    case "ArrowRight":
      seekBy(SEEK_STEP_S);
      break;
    // 画像の前後移動に使うキーは、動画・音楽では何もしない（誤ってファイルが変わらないように）
    case "PageUp":
    case "PageDown":
    case "Backspace":
    case "Home":
    case "End":
      break;
    case "ArrowUp":
      changeVolumeBy(VOLUME_STEP);
      break;
    case "ArrowDown":
      changeVolumeBy(-VOLUME_STEP);
      break;
    default:
      return false;
  }
  e.preventDefault();
  return true;
}

window.addEventListener("keydown", (e) => {
  // 確認ウィンドウが開いている間は、そちらの操作だけを受け付ける。
  // Enter はフォーカスのあるボタンを押す既定の動きに任せる（誤って「移動」を
  // 選ばせないため、こちらでは横取りしない）
  if (!confirmEl.hidden) {
    if (e.key === "Escape") {
      e.preventDefault();
      closeConfirm({ ok: false, skip: false });
    }
    return;
  }
  // 設定を開くショートカット（macOS: Command + , / その他: Ctrl + ,）
  if (e.key === "," && (IS_MAC ? e.metaKey : e.ctrlKey) && !e.altKey) {
    e.preventDefault();
    openSettings();
    return;
  }
  // macOS で「ゴミ箱に入れる」は Command + Delete。この Delete は Backspace として
  // 届くので、修飾キーで弾く前にここで拾う（単体の Backspace は前の画像のまま）
  if (IS_MAC && e.key === "Backspace" && e.metaKey && !e.ctrlKey && !e.altKey) {
    e.preventDefault();
    deleteCurrent();
    return;
  }
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  if (showingMedia && handleVideoKey(e)) return;
  switch (e.key) {
    // ↑ ↓ は今後の機能のために空けておく（前後移動には使わない）
    case "ArrowRight":
    case "PageDown":
    case " ":
      e.preventDefault();
      step(1);
      break;
    case "ArrowLeft":
    case "PageUp":
    case "Backspace":
      e.preventDefault();
      step(-1);
      break;
    case "Home":
      if (images.length) { index = 0; show(); }
      break;
    case "End":
      if (images.length) { index = images.length - 1; show(); }
      break;
    case "+":
    case "=":
      zoomAt(window.innerWidth / 2, window.innerHeight / 2, 1.25);
      break;
    case "-":
      zoomAt(window.innerWidth / 2, window.innerHeight / 2, 1 / 1.25);
      break;
    case "0":
      setFitMode();
      break;
    case "1":
      zoomTo(1);
      break;
    case "o":
    case "O":
      openDialog();
      break;
    case "d":
    case "D":
      openFolderDialog();
      break;
    case "r":
    case "R":
      rescan();
      break;
    case "f":
    case "F":
      toggleFullscreen();
      break;
    case "Delete":
      e.preventDefault();
      deleteCurrent();
      break;
    case "Escape":
      // メニューが開いているときは、まずそれを閉じる
      if (!ctxmenu.hidden) hideContextMenu();
      else appWindow.close().catch(() => {});
      break;
  }
});

// ---- input: mouse back/forward buttons ----
// Windows: XBUTTON1/2 -> button 3/4。WebView2 の既定の履歴ナビゲーションは
// auxclick の preventDefault で抑止する。
// macOS: マウスにより挙動が異なるが、side button は同じく button 3/4 の
// mouseup として届く（届かないユーティリティ常駐マウスはキー操作で代替）。
window.addEventListener("mouseup", (e) => {
  if (!settings.sideButtons || !confirmEl.hidden) return;
  // 動画・音楽では前後のファイルではなく 5 秒戻る / 進む
  if (e.button === 3) {
    e.preventDefault();
    if (showingMedia) seekBy(-SEEK_STEP_S);
    else step(-1);
  } else if (e.button === 4) {
    e.preventDefault();
    if (showingMedia) seekBy(SEEK_STEP_S);
    else step(1);
  }
});
window.addEventListener("auxclick", (e) => {
  if (e.button === 3 || e.button === 4) e.preventDefault();
});

// ---- input: wheel（画像は拡大縮小、動画・音楽は音量） ----
window.addEventListener(
  "wheel",
  (e) => {
    if (!images.length || !confirmEl.hidden) return;
    e.preventDefault();
    if (showingMedia) {
      wheelVolume(e.deltaY);
      return;
    }
    const factor = Math.exp(-e.deltaY * 0.0004 * settings.wheelSensitivity);
    zoomAt(e.clientX, e.clientY, factor);
  },
  { passive: false }
);

// ---- input: pan (zoom mode only) ----
let panning = null;
stage.addEventListener("mousedown", (e) => {
  if (mode !== "zoom" || e.button !== 0 || !confirmEl.hidden) return;
  panning = { x: e.clientX, y: e.clientY };
});
window.addEventListener("mousemove", (e) => {
  if (!panning) return;
  tx += e.clientX - panning.x;
  ty += e.clientY - panning.y;
  panning = { x: e.clientX, y: e.clientY };
  applyTransform();
});
window.addEventListener("mouseup", () => (panning = null));

// ---- input: 動画・音楽の上の押下（すぐ離すと再生 / 一時停止、押したまま動かすとウィンドウ移動） ----
// 動画・音楽の表示中の #stage はドラッグ領域にしない（setFitMode）。付けたままだと押した瞬間に
// OS のウィンドウ移動が始まり、離したことが WebView に届かずクリックと区別できないため。
// 代わりに一定以上動いたところで自分で startDragging() を呼ぶ
const VIDEO_DRAG_THRESHOLD_PX = 4;
const VIDEO_CLICK_MS = 350;
// 開いていたメニューを閉じるための押下では切り替えない
let pressClosedMenu = false;
let videoPress = null;
stage.addEventListener("mousedown", (e) => {
  videoPress = null;
  // ダブルクリックの 2 回目は数えない（2 回切り替わって元に戻らないように）
  if (!showingMedia || e.button !== 0 || e.detail > 1 || pressClosedMenu || !confirmEl.hidden) return;
  videoPress = { x: e.clientX, y: e.clientY, t: performance.now() };
});
window.addEventListener("mousemove", (e) => {
  if (!videoPress) return;
  if (!(e.buttons & 1)) {
    videoPress = null;
    return;
  }
  if (Math.hypot(e.clientX - videoPress.x, e.clientY - videoPress.y) < VIDEO_DRAG_THRESHOLD_PX) return;
  videoPress = null;
  appWindow.startDragging().catch(() => {});
});
window.addEventListener("mouseup", (e) => {
  if (e.button !== 0 || !videoPress) return;
  const quick = performance.now() - videoPress.t <= VIDEO_CLICK_MS;
  videoPress = null;
  // 長く押して動かさずに離したときは何もしない
  if (quick && showingMedia) togglePlay();
});

// ---- misc UI ----
placeholder.addEventListener("click", openDialog);
navPrev.addEventListener("click", () => step(-1));
navNext.addEventListener("click", () => step(1));
// ---- ウィンドウ操作（最小化 / 最大化 / 閉じる） ----
// 挙動は OS 任せ。最小化はタスクバー・Dock へ、最大化は作業領域いっぱいに広げ、
// もう一度押すと元の大きさに戻る
// 最大化中は画面の隅と食い違うので角は丸めない
function applyCorners() {
  const round = !app.classList.contains("maximized");
  app.style.borderRadius = round ? "8px" : "0";
}

function syncMaximized() {
  appWindow
    .isMaximized()
    .then((maximized) => {
      app.classList.toggle("maximized", maximized);
      applyCorners();
      const label = maximized ? "元のサイズに戻す" : "最大化";
      btnMax.title = label;
      btnMax.setAttribute("aria-label", label);
    })
    .catch(() => {});
}

document.getElementById("btn-min").addEventListener("click", () => {
  appWindow.minimize().catch(() => {});
});
btnMax.addEventListener("click", () => {
  selfResizedAt = Date.now();
  appWindow.toggleMaximize().then(syncMaximized).catch(() => {});
});
document.getElementById("btn-close").addEventListener("click", () => appWindow.close());
// サイズ変更が落ち着いたときの後始末。ドラッグ中は何度も届くので 1 回だけ行う。
// タイトルバーのダブルクリックや OS 側の操作でも最大化の状態は変わるので、
// 見た目（アイコン）を合わせ直し、「メディアに合わせる」では縦横比の基準を
// 今の大きさに取り直す（ドラッグ中、OS は掴んだときの大きさを基準に
// 動かし続けるので、終わったところで実際の大きさに戻しておく）。
// 縦横比を保つこと自体は Rust 側の仕事で、ここでは何もしない
const RESIZE_SETTLE_MS = 200;
let resizeTimer = null;
// 通知が来るたびに増やす。マウスの状態を問い合わせている間に次の通知が来たら、
// 古い問い合わせの結果では何もしない
let resizeSeq = 0;

async function onResizeSettled() {
  const seq = resizeSeq;
  // 通知が途切れても、ボタンを押したままなら引っ張っている途中で止まっているだけ。
  // 離すまで据え置いたまま待つ（途中で合わせ直すと、動かし直したときにちらつく）
  if (frozenSize && (await invoke("mouse_button_down").catch(() => false))) {
    if (seq === resizeSeq) resizeTimer = setTimeout(onResizeSettled, RESIZE_SETTLE_MS);
    return;
  }
  if (seq !== resizeSeq) return;
  unfreezeImageSize();
  syncMaximized();
  syncAspectLock();
}

appWindow
  .onResized(() => {
    // 手でサイズを変えている間は、画像を据え置く
    // （画像の切り替えや最大化など、自分で変えたときは一緒に合わせる）
    if (Date.now() - selfResizedAt > SELF_RESIZE_MS) freezeImageSize();
    resizeSeq++;
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(onResizeSettled, RESIZE_SETTLE_MS);
  })
  .catch(() => {});
syncMaximized();

// Tauri はドラッグ領域のダブルクリックを最大化に割り当てる。
// OS の慣習どおりタイトルバーだけ通し、画像やステータスバーの上では止める
window.addEventListener(
  "mousedown",
  (e) => {
    if (e.detail < 2) return;
    const region = e.target.closest?.("[data-tauri-drag-region]");
    if (region && !region.closest("#titlebar")) e.stopPropagation();
  },
  true
);
window.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  if (confirmEl.hidden) openContextMenu(e.clientX, e.clientY);
});

// ---- drag & drop ----
listen("tauri://drag-enter", () => app.classList.add("dropping"));
listen("tauri://drag-leave", () => app.classList.remove("dropping"));
listen("tauri://drag-drop", (event) => {
  app.classList.remove("dropping");
  const paths = event.payload?.paths ?? [];
  if (paths.length) openPath(paths[0]);
});

// ---- startup ----
// macOS の Dock / Finder からの "Opened" イベント（起動後）
listen("open-file", (event) => openPath(event.payload));

// CLI 引数 / 関連付け起動（Windows）、または起動前に届いた macOS の Opened。
// 設定を読み終えてから開く（種類ごとのウィンドウや自動再生を設定どおりにするため）
function openStartupFile() {
  invoke("get_startup_file")
    .then((path) => {
      if (path) openPath(path);
    })
    .catch(() => {});
}

// ---- settings ----
// 設定ウィンドウからの変更は "settings-changed" で届き、その場で反映する
function hexToRgba(hex, alpha) {
  const m = /^#?([0-9a-f]{6})$/i.exec(String(hex).trim());
  const n = parseInt(m ? m[1] : "0e0e10", 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

function applySettings() {
  app.style.background = hexToRgba(settings.backgroundColor, settings.backgroundOpacity / 100);
  applyCorners();
  app.classList.toggle("hide-filename", !settings.showFilename);
  img.style.imageRendering = settings.imageRendering;
  video.loop = mediaLoop();
  setVolume(mediaVolume());
  appWindow.setAlwaysOnTop(!!settings.alwaysOnTop).catch(() => {});
  syncWindowKind();
  syncWatcher();
}

// 本体ウィンドウ側から設定を書き換える（削除確認の「今後確認しない」）。
// 設定ウィンドウが開いていればそちらの表示も合わせる
async function saveSettings() {
  try {
    await invoke("save_settings", { settings });
    await emit("settings-changed", settings);
  } catch (e) {
    showError(e);
  }
}

async function openSettings() {
  hideContextMenu();
  try {
    await invoke("open_settings_window");
  } catch (e) {
    showError(e);
  }
}

// ---- wheel volume ----
// ホイール 1 段の量はデバイス差が大きい（トラックパッドは細かく何度も届く）ので、
// しきい値までためてから音量を 1 段（VOLUME_STEP）変える。上に回すと大きくなる
let wheelAccum = 0;
function wheelVolume(deltaY) {
  const threshold = 220 - settings.wheelSensitivity * 18;
  if (Math.sign(deltaY) !== Math.sign(wheelAccum)) wheelAccum = 0;
  wheelAccum += deltaY;
  while (Math.abs(wheelAccum) >= threshold) {
    changeVolumeBy(-Math.sign(wheelAccum) * VOLUME_STEP);
    wheelAccum -= Math.sign(wheelAccum) * threshold;
  }
}

// ---- context menu ----
// 設定を開くショートカットの表示（キー処理側と揃える）
const SETTINGS_ACCEL = IS_MAC ? "\u2318," : "Ctrl+,";

const REVEAL_LABEL = IS_MAC
  ? "Finder で表示"
  : /Win/.test(navigator.platform || navigator.userAgent)
    ? "エクスプローラーで表示"
    : "ファイルマネージャーで表示";

// 表示中のファイル。書庫内の画像の場合は書庫そのものを指す
function currentFilePath() {
  if (archivePath) return archivePath;
  return index >= 0 && index < images.length ? images[index] : null;
}

async function revealCurrent() {
  const path = currentFilePath();
  if (!path) return;
  try {
    await invoke("reveal_in_file_manager", { path });
  } catch (e) {
    showError(e);
  }
}

function hideContextMenu() {
  ctxmenu.hidden = true;
}

function buildContextMenu() {
  const hasFile = currentFilePath() !== null;
  return [
    { label: REVEAL_LABEL, disabled: !hasFile, action: revealCurrent },
    {
      // 「…」は確認ウィンドウが出るときだけ添える
      label: settings.confirmDelete ? "ゴミ箱へ移動…" : "ゴミ箱へ移動",
      accel: "Del",
      disabled: !canDelete(),
      action: deleteCurrent,
    },
    { separator: true },
    { label: "ファイルを開く…", accel: "O", action: openDialog },
    { label: "フォルダを開く…", accel: "D", action: openFolderDialog },
    { label: "再読み込み", accel: "R", disabled: !hasFile, action: rescan },
    { separator: true },
    { label: "全画面表示", accel: "F", action: toggleFullscreen },
    { separator: true },
    { label: "設定…", accel: SETTINGS_ACCEL, action: openSettings },
    { label: "終了", accel: "Esc", action: () => appWindow.close().catch(() => {}) },
  ];
}

function openContextMenu(x, y) {
  ctxmenu.textContent = "";
  for (const item of buildContextMenu()) {
    if (item.separator) {
      const hr = document.createElement("div");
      hr.className = "ctx-sep";
      ctxmenu.appendChild(hr);
      continue;
    }
    const btn = document.createElement("button");
    btn.className = "ctx-item";
    btn.type = "button";
    btn.setAttribute("role", "menuitem");
    btn.disabled = !!item.disabled;
    const label = document.createElement("span");
    label.textContent = item.label;
    const accel = document.createElement("span");
    accel.className = "ctx-accel";
    accel.textContent = item.accel ?? "";
    btn.append(label, accel);
    btn.addEventListener("click", () => {
      hideContextMenu();
      item.action();
    });
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

window.addEventListener("mousedown", (e) => {
  pressClosedMenu = !ctxmenu.hidden && !ctxmenu.contains(e.target);
  if (pressClosedMenu) hideContextMenu();
}, true);
window.addEventListener("blur", hideContextMenu);
window.addEventListener("resize", hideContextMenu);

listen("settings-changed", (event) => {
  const previousMode = settings.windowSizeMode;
  settings = normalizeSettings(event.payload);
  applySettings();
  // 「メディアに合わせる」に切り替えた直後は、今の大きさを基準に縦横比だけ合わせる
  if (settings.windowSizeMode !== previousMode) fitWindowToImage();
});

invoke("load_settings")
  .then((saved) => {
    settings = normalizeSettings(saved);
    applySettings();
  })
  .catch(() => applySettings())
  .finally(openStartupFile);
