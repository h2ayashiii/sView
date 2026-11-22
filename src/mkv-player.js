// MKV を MSE（Media Source Extensions）で再生する（macOS 用）。
// macOS の WKWebView は MKV を再生できないので、Rust 側（lib.rs の mkv_open / mkv_read など）が
// MKV の映像・音声を再エンコードせずに fragmented MP4 へ詰め直し、ここで SourceBuffer に流す。
// デコードは WebView 任せ（コーデックは同梱しない）。対応は映像 H.264 / HEVC、音声 AAC / Opus。
// Windows の WebView2 は MKV をそのまま再生できるので、この経路は使わない。
//
// 読み込みは 1 本のループ（run）で順に行う: シークの要求があればバッファを空にして
// Rust 側を目標の直前のキーフレームへ移し、なければ再生位置の先 MKV_AHEAD_S 秒まで断片を足す。
// Rust 側の読み取りは前へ進むだけなので、要求が重ならないよう同時には 1 つしか出さない

// 再生位置より先に読んでおく長さ（秒）
const MKV_AHEAD_S = 20;
// 再生位置より前に残しておく長さ（秒）。これより前は SourceBuffer.remove() で手放す
const MKV_BEHIND_S = 10;
// 再生位置が映像・音声の始まりより少し手前にあって止まっているとき、飛び越す隙間の上限（秒）
const MKV_GAP_JUMP_S = 1;
// シーク先が読み込み済みの範囲の終わりからこれ以内なら、空にせず続きを読んで追いつく（秒）
const MKV_CONTINUE_S = 3;
// 何も起きなくても読み込みの要否を見直す間隔（ミリ秒）
const MKV_IDLE_MS = 500;

export class MkvPlayer {
  // video: 再生する <video>。path: MKV のパス。
  // options: audio（鳴らす音声の番号。省略で既定）、startTime（始める位置・秒）、
  //   play（再生を始める関数。src を入れた後に呼ぶ）、onError（読めないとき）、onNotice（知らせ）
  constructor(video, path, options = {}) {
    this.video = video;
    this.path = path;
    this.options = options;
    this.closed = false;
    this.id = null;
    this.info = null;
    this.mediaSource = null;
    this.sourceBuffer = null;
    this.eof = false;
    // シークの目標（null = なし）
    this.seekTarget = null;
    // シークでバッファを空にした直後（最初の断片の頭まで再生位置を進めてよい）
    this.afterReset = false;
    this.wakeUp = null;
    this.onEvent = () => this.notify();
    this.onSeeking = () => {
      this.seekTarget = this.video.currentTime;
      this.notify();
    };
    this.onWaiting = () => {
      this.nudgeGap();
      this.notify();
    };
  }

  static isSupported() {
    return typeof MediaSource !== "undefined";
  }

  static mime(videoCodec, audioCodec) {
    const codecs = audioCodec ? `${videoCodec},${audioCodec}` : videoCodec;
    return `video/mp4; codecs="${codecs}"`;
  }

  // 音声トラックの一覧（右クリックメニューの「音声」に出す）。
  // リマックスできない・WebView が再生できない音声は disabled
  get audioTracks() {
    const info = this.info;
    if (!info) return [];
    return info.audioTracks.map((t, i) => ({
      label: t.name ?? "",
      language: t.language ?? "",
      codec: t.codec,
      enabled: info.audio === i,
      disabled: !this.audioPlayable(t),
    }));
  }

  audioPlayable(track) {
    return !!track.codecString &&
      MediaSource.isTypeSupported(MkvPlayer.mime(this.info.videoCodec, track.codecString));
  }

  async start() {
    try {
      await this.open(this.options.audio);
      if (this.closed) return;
      await this.attach();
      if (this.closed) return;
      this.run();
    } catch (e) {
      if (!this.closed) this.options.onError?.(e);
    }
  }

  // Rust 側で MKV を開き、WebView が再生できる組み合わせ（映像・音声）を決める
  async open(audio) {
    const { invoke } = window.__TAURI__.core;
    let info = await invoke("mkv_open", { path: this.path, audio: audio ?? null });
    if (this.closed) return this.close(info.id);
    if (!MediaSource.isTypeSupported(MkvPlayer.mime(info.videoCodec, null))) {
      this.close(info.id);
      throw new Error("この MKV の映像は、この Mac では再生できません");
    }
    if (!MediaSource.isTypeSupported(MkvPlayer.mime(info.videoCodec, info.audioCodec))) {
      // Rust はリマックスできても WebView が鳴らせない音声。鳴らせる別の音声か、音声なしで開き直す
      this.info = info;
      const alt = info.audioTracks.findIndex((t) => this.audioPlayable(t));
      this.close(info.id);
      info = await invoke("mkv_open", { path: this.path, audio: alt });
      if (this.closed) return this.close(info.id);
    }
    this.id = info.id;
    this.info = info;
    // 初めて開くとき（音声を選び直したときは除く）だけ、既定の音声を鳴らせないことを知らせる
    if (audio == null) this.noticeAudio();
  }

  noticeAudio() {
    const tracks = this.info.audioTracks;
    if (!tracks.length) return;
    const wanted = tracks.find((t) => t.default) ?? tracks[0];
    if (this.audioPlayable(wanted)) return;
    this.options.onNotice?.(
      this.info.audio == null
        ? `この MKV の音声（${wanted.codec}）には対応していないため、音声なしで再生します`
        : `この MKV の既定の音声（${wanted.codec}）には対応していないため、別の音声で再生します`
    );
  }

  close(id) {
    if (id == null) return;
    window.__TAURI__.core.invoke("mkv_close", { id }).catch(() => {});
  }

  // MediaSource を <video> につなぎ、初期化セグメントを入れる
  async attach() {
    const { invoke } = window.__TAURI__.core;
    const mediaSource = new MediaSource();
    this.mediaSource = mediaSource;
    const url = URL.createObjectURL(mediaSource);
    const opened = new Promise((resolve) =>
      mediaSource.addEventListener("sourceopen", resolve, { once: true })
    );
    this.video.src = url;
    this.options.play?.();
    await opened;
    URL.revokeObjectURL(url);
    if (this.closed) return;
    const mime = MkvPlayer.mime(this.info.videoCodec, this.info.audioCodec);
    this.sourceBuffer = mediaSource.addSourceBuffer(mime);
    this.sourceBuffer.mode = "segments";
    const init = mkvBytes(await invoke("mkv_init", { id: this.id }));
    if (this.closed) return;
    await this.append(init);
    if (this.closed) return;
    if (this.info.duration > 0) mediaSource.duration = this.info.duration;
    for (const type of ["timeupdate", "play", "playing"]) {
      this.video.addEventListener(type, this.onEvent);
    }
    this.video.addEventListener("seeking", this.onSeeking);
    this.video.addEventListener("waiting", this.onWaiting);
    const start = this.options.startTime ?? 0;
    if (start > 0) this.video.currentTime = start;
  }

  // 読み込みのループ。閉じるまで回る
  async run() {
    const { invoke } = window.__TAURI__.core;
    while (!this.closed) {
      try {
        if (this.seekTarget !== null) {
          await this.seek(this.seekTarget);
          continue;
        }
        if (this.eof || this.bufferedAhead() >= MKV_AHEAD_S) {
          await this.idle();
          continue;
        }
        const data = mkvBytes(await invoke("mkv_read", { id: this.id }));
        if (this.closed) return;
        // 読んでいる間にシークされたら、この断片は捨てる（シーク先から読み直す）
        if (this.seekTarget !== null) continue;
        if (!data.length) {
          this.eof = true;
          this.endOfStream();
          continue;
        }
        await this.appendWithRoom(data);
        this.nudgeGap();
      } catch (e) {
        if (this.closed) return;
        this.closed = true;
        this.options.onError?.(e);
        return;
      }
    }
  }

  async seek(target) {
    this.seekTarget = null;
    const ranges = this.sourceBuffer.buffered;
    if (ranges.length) {
      // 読み込み済みの範囲（先頭側は捨てているので、最後の範囲が今読み足している所）の中か、
      // その少し先なら、そのまま続きを読めば追いつく
      const last = ranges.length - 1;
      if (target >= ranges.start(last) - 0.1 && target <= ranges.end(last) + MKV_CONTINUE_S) {
        return;
      }
    }
    await this.remove(0, Infinity);
    if (this.closed || this.seekTarget !== null) return;
    await window.__TAURI__.core.invoke("mkv_seek", { id: this.id, time: target });
    this.eof = false;
    this.afterReset = true;
  }

  // 再生位置から先に読み込み済みの長さ（秒）
  bufferedAhead() {
    const ranges = this.sourceBuffer.buffered;
    const t = this.video.currentTime;
    for (let i = 0; i < ranges.length; i++) {
      if (t >= ranges.start(i) - 0.1 && t <= ranges.end(i)) return ranges.end(i) - t;
    }
    return 0;
  }

  // 再生位置が読み込み済みの範囲の少し手前で止まっていたら、範囲の頭へ進める
  // （映像の最初のフレームや音声の始まりが 0 秒やシーク先より少し後ろにあると、WebKit は止まったままになる）
  nudgeGap() {
    if (this.closed || !this.sourceBuffer) return;
    const ranges = this.sourceBuffer.buffered;
    const t = this.video.currentTime;
    for (let i = 0; i < ranges.length; i++) {
      if (t >= ranges.start(i) && t < ranges.end(i)) {
        this.afterReset = false;
        return;
      }
      const gap = ranges.start(i) - t;
      if (gap > 0 && (gap <= MKV_GAP_JUMP_S || this.afterReset)) {
        this.afterReset = false;
        this.video.currentTime = ranges.start(i) + 0.001;
        return;
      }
    }
  }

  // 断片を足す。入りきらなければ（QuotaExceededError）再生位置より前を手放してから入れ直す
  async appendWithRoom(data) {
    await this.evictBehind(MKV_BEHIND_S);
    try {
      await this.append(data);
    } catch (e) {
      if (e?.name !== "QuotaExceededError") throw e;
      await this.evictBehind(1);
      await this.append(data);
    }
  }

  async evictBehind(keep) {
    const ranges = this.sourceBuffer.buffered;
    if (!ranges.length) return;
    const end = this.video.currentTime - keep;
    // 少しずつ消すと remove が多くなるので、余分が 5 秒たまってからまとめて消す
    if (end - ranges.start(0) < 5) return;
    await this.remove(0, end);
  }

  append(data) {
    return this.whenUpdated(() => this.sourceBuffer.appendBuffer(data));
  }

  remove(start, end) {
    const ranges = this.sourceBuffer.buffered;
    if (!ranges.length) return Promise.resolve();
    const until = Math.min(end, ranges.end(ranges.length - 1));
    if (until <= start) return Promise.resolve();
    return this.whenUpdated(() => this.sourceBuffer.remove(start, until));
  }

  // SourceBuffer の操作をして、終わる（updateend）まで待つ
  whenUpdated(action) {
    return new Promise((resolve, reject) => {
      const sb = this.sourceBuffer;
      const done = () => {
        sb.removeEventListener("updateend", done);
        sb.removeEventListener("error", failed);
        resolve();
      };
      const failed = () => {
        sb.removeEventListener("updateend", done);
        sb.removeEventListener("error", failed);
        reject(new Error("動画のデータを読み込めませんでした"));
      };
      sb.addEventListener("updateend", done);
      sb.addEventListener("error", failed);
      try {
        action();
      } catch (e) {
        sb.removeEventListener("updateend", done);
        sb.removeEventListener("error", failed);
        reject(e);
      }
    });
  }

  endOfStream() {
    if (this.mediaSource.readyState !== "open" || this.sourceBuffer.updating) return;
    try {
      this.mediaSource.endOfStream();
    } catch {
      // 閉じた直後などは無視する
    }
  }

  // 何か起きる（再生が進む・シークする）か、しばらくたつまで待つ
  idle() {
    return new Promise((resolve) => {
      const timer = setTimeout(resolve, MKV_IDLE_MS);
      this.wakeUp = () => {
        clearTimeout(timer);
        resolve();
      };
    });
  }

  notify() {
    const wake = this.wakeUp;
    this.wakeUp = null;
    wake?.();
  }

  // 読み込みを止めて Rust 側のファイルを閉じる。<video> の src は呼び出し側が外す
  destroy() {
    if (this.closed && this.id == null) return;
    this.closed = true;
    for (const type of ["timeupdate", "play", "playing"]) {
      this.video.removeEventListener(type, this.onEvent);
    }
    this.video.removeEventListener("seeking", this.onSeeking);
    this.video.removeEventListener("waiting", this.onWaiting);
    this.notify();
    this.close(this.id);
    this.id = null;
  }
}

// invoke が返した生のバイト列（ArrayBuffer）を Uint8Array にする
function mkvBytes(data) {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  if (Array.isArray(data)) return Uint8Array.from(data);
  throw new Error("動画のデータを読み込めませんでした");
}
