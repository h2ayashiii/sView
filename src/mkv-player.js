// MKV / AV1 動画を MSE（Media Source Extensions）で再生する（macOS 用）。
// - MKV（H.264 / HEVC / VP9、および M3 以降の AV1）は再エンコードせずに fragmented MP4 へ詰め直す。
// - AV1 ハードウェアデコーダが無い Mac（M1 / M2 世代など）では、Rust 側が rav1d でデコードし
//   VideoToolbox（H.264）へリアルタイム変換した fragmented MP4 を流す。
// Windows の WebView2 は MKV も AV1 もそのまま再生できるので、この経路は使わない。
//
// 読み込みは 1 本のループ（run）で順に行う: シークの要求があればバッファを空にして
// Rust 側を目標の直前のキーフレームへ移し、なければ再生位置の先まで断片を足す。
// Rust 側の読み取りは前へ進むだけなので、要求が重ならないよう同時には 1 つしか出さない

// 無変換リマックス時に再生位置より先に読んでおく長さ（秒）
const MKV_AHEAD_S = 20;
// 無変換リマックス時に再生位置より前に残しておく長さ（秒）
const MKV_BEHIND_S = 10;
// AV1 変換時の先読み・保持の長さ（秒）。先読みを短く保ち CPU 負荷・発熱・切り替え時の無駄を抑える
const TRANSCODE_AHEAD_S = 6;
const TRANSCODE_BEHIND_S = 6;
// 再生位置が映像・音声の始まりより少し手前にあって止まっているとき、飛び越す隙間の上限（秒）
const MKV_GAP_JUMP_S = 1;
// シーク先が読み込み済みの範囲の終わりからこれ以内なら、空にせず続きを読んで追いつく（秒）
const MKV_CONTINUE_S = 3;
// 何も起きなくても読み込みの要否を見直す間隔（ミリ秒）
const MKV_IDLE_MS = 500;

export class MkvPlayer {
  // video: 再生する <video>。path: 動画のパス。
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
    this.audioBuffer = null;
    this.eof = false;
    // シークの目標（null = なし）
    this.seekTarget = null;
    // nudgeGap が自ら進めた currentTime の seeking イベントで再シークしない
    this.nudgeTarget = null;
    // シークでバッファを空にした直後（最初の断片の頭まで再生位置を進めてよい）
    this.afterReset = false;
    this.reading = false;
    this.readCanceled = false;
    this.wakeUp = null;
    this.onEvent = () => this.notify();
    this.onSeeking = () => {
      const t = this.video.currentTime;
      if (this.nudgeTarget !== null && Math.abs(t - this.nudgeTarget) < 0.05) {
        this.nudgeTarget = null;
        return;
      }
      this.nudgeTarget = null;
      this.seekTarget = t;
      if (this.reading && this.id != null && !this.canContinueFromBuffer(t)) {
        this.readCanceled = true;
        window.__TAURI__.core.invoke("mkv_cancel_read", { id: this.id }).catch(() => {});
      }
      this.notify();
    };
    this.onWaiting = () => {
      this.nudgeGap(true);
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

  // macOS の WebKit は MP4 内の Opus に非対応のため、Opus のみ別 WebM SourceBuffer で流す
  static audioMime(audioCodec) {
    return audioCodec === "opus" ? 'audio/webm; codecs="opus"' : null;
  }

  aheadSeconds() {
    return this.info?.transcoded ? TRANSCODE_AHEAD_S : MKV_AHEAD_S;
  }

  behindSeconds() {
    return this.info?.transcoded ? TRANSCODE_BEHIND_S : MKV_BEHIND_S;
  }

  mediaLabel() {
    return this.path.toLowerCase().endsWith(".mkv") ? "MKV" : "動画";
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
    if (!track.codecString) return false;
    const sep = MkvPlayer.audioMime(track.codecString);
    return MediaSource.isTypeSupported(
      sep ?? MkvPlayer.mime(this.info.videoCodec, track.codecString)
    );
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

  // Rust 側で動画を開き、WebView が再生できる組み合わせ（映像・音声）を決める
  async open(audio) {
    const { invoke } = window.__TAURI__.core;
    let info = await invoke("mkv_open", { path: this.path, audio: audio ?? null, transcode: null });
    if (this.closed) return this.close(info.id);
    if (!MediaSource.isTypeSupported(MkvPlayer.mime(info.videoCodec, null))) {
      // M3 以降でも WebView が再生できない AV1 プロファイルなら変換モードへフォールバックする
      if (info.canTranscode) {
        this.close(info.id);
        info = await invoke("mkv_open", { path: this.path, audio: audio ?? null, transcode: true });
        if (this.closed) return this.close(info.id);
      }
      if (!MediaSource.isTypeSupported(MkvPlayer.mime(info.videoCodec, null))) {
        this.close(info.id);
        throw new Error(`この ${this.mediaLabel()} の映像は、この Mac では再生できません`);
      }
    }
    const audioMime = info.audioCodec
      ? info.separateAudio
        ? MkvPlayer.audioMime(info.audioCodec)
        : MkvPlayer.mime(info.videoCodec, info.audioCodec)
      : null;
    if (audioMime && !MediaSource.isTypeSupported(audioMime)) {
      // Rust はリマックスできても WebView が鳴らせない音声。鳴らせる別の音声か、音声なしで開き直す
      this.info = info;
      const alt = info.audioTracks.findIndex((t) => this.audioPlayable(t));
      const transcode = info.transcoded ? true : null;
      this.close(info.id);
      info = await invoke("mkv_open", { path: this.path, audio: alt, transcode });
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
    const kind = this.mediaLabel();
    this.options.onNotice?.(
      this.info.audio == null
        ? `この ${kind} の音声（${wanted.codec}）には対応していないため、音声なしで再生します`
        : `この ${kind} の既定の音声（${wanted.codec}）には対応していないため、別の音声で再生します`
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
    const videoMime = this.info.separateAudio
      ? MkvPlayer.mime(this.info.videoCodec, null)
      : MkvPlayer.mime(this.info.videoCodec, this.info.audioCodec);
    this.sourceBuffer = mediaSource.addSourceBuffer(videoMime);
    this.sourceBuffer.mode = "segments";
    if (this.info.separateAudio) {
      const audioMime = MkvPlayer.audioMime(this.info.audioCodec);
      if (audioMime) {
        this.audioBuffer = mediaSource.addSourceBuffer(audioMime);
        this.audioBuffer.mode = "segments";
      }
    }
    const init = mkvBytes(await invoke("mkv_init", { id: this.id }));
    if (this.closed) return;
    if (this.info.separateAudio) {
      const [vInit, aInit] = unpackDual(init);
      if (vInit.length) await this.appendTo(this.sourceBuffer, vInit);
      if (this.closed) return;
      if (aInit.length && this.audioBuffer) await this.appendTo(this.audioBuffer, aInit);
    } else {
      await this.appendTo(this.sourceBuffer, init);
    }
    if (this.closed) return;
    if (this.info.duration > 0) mediaSource.duration = this.info.duration;
    for (const type of ["timeupdate", "play", "playing"]) {
      this.video.addEventListener(type, this.onEvent);
    }
    this.video.addEventListener("seeking", this.onSeeking);
    this.video.addEventListener("waiting", this.onWaiting);
    const start = this.options.startTime ?? 0;
    if (start > 0) {
      this.seekTarget = start;
      this.video.currentTime = start;
    }
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
        if (this.eof || this.bufferedAhead() >= this.aheadSeconds()) {
          await this.idle();
          continue;
        }
        this.reading = true;
        let data;
        try {
          data = mkvBytes(await invoke("mkv_read", { id: this.id }));
        } finally {
          this.reading = false;
        }
        if (this.closed) return;
        if (this.readCanceled) {
          if (this.seekTarget === null) this.seekTarget = this.video.currentTime;
          continue;
        }
        // 読んでいる間にバッファ外へのシークが来ていたら、この断片は捨ててシーク先から読み直す。
        // 続きを読めば追いつく範囲へのシークでは、Rust 側がすでに先へ進んでいるため捨てずに足す
        if (this.seekTarget !== null) {
          if (!this.canContinueFromBuffer(this.seekTarget)) continue;
          this.seekTarget = null;
        }
        if (!data.length) {
          this.eof = true;
          this.endOfStream();
          continue;
        }
        await this.appendWithRoom(data);
        this.nudgeGap(false);
      } catch (e) {
        if (this.closed) return;
        this.closed = true;
        this.options.onError?.(e);
        return;
      }
    }
  }

  // バッファを空にせず続きを読めば追いつけるシーク先かを返す
  canContinueFromBuffer(target) {
    if (this.readCanceled) return false;
    const ranges = this.sourceBuffer?.buffered;
    if (!ranges?.length) return false;
    const last = ranges.length - 1;
    if (target >= ranges.start(last) - 0.1 && target < ranges.end(last)) return true;
    if (this.eof) return false;
    // 変換時は数秒先まで全部エンコードして待つよりシークし直した方が速いので 1 秒以内にする
    const maxAhead = this.info?.transcoded ? 1 : MKV_CONTINUE_S;
    return target >= ranges.start(last) - 0.1 && target <= ranges.end(last) + maxAhead;
  }

  async seek(target) {
    this.seekTarget = null;
    if (this.canContinueFromBuffer(target)) {
      this.nudgeGap(false);
      return;
    }
    await this.remove(0, Infinity);
    if (this.closed || this.seekTarget !== null) return;
    await window.__TAURI__.core.invoke("mkv_seek", { id: this.id, time: target });
    this.readCanceled = false;
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
  nudgeGap(stalled = false) {
    if (this.closed || !this.sourceBuffer) return;
    const ranges = this.sourceBuffer.buffered;
    const t = this.video.currentTime;
    for (let i = 0; i < ranges.length; i++) {
      if (t >= ranges.start(i) && t < ranges.end(i)) {
        this.afterReset = false;
        // 範囲の末尾直前で waiting になり、すぐ後ろに次の範囲があるときは微小な隙間を飛び越す
        if (
          stalled &&
          i + 1 < ranges.length &&
          ranges.end(i) - t < 0.05 &&
          ranges.start(i + 1) - ranges.end(i) <= MKV_GAP_JUMP_S
        ) {
          const next = ranges.start(i + 1) + 0.001;
          this.nudgeTarget = next;
          this.video.currentTime = next;
        }
        return;
      }
      const gap = ranges.start(i) - t;
      if (gap > 0 && (gap <= MKV_GAP_JUMP_S || this.afterReset)) {
        this.afterReset = false;
        const next = ranges.start(i) + 0.001;
        this.nudgeTarget = next;
        this.video.currentTime = next;
        return;
      }
    }
  }

  // 断片を足す。入りきらなければ（QuotaExceededError）再生位置より前を手放してから入れ直す
  async appendWithRoom(data) {
    if (this.info?.separateAudio) {
      const [vData, aData] = unpackDual(data);
      if (vData.length) await this.appendBufferWithRoom(this.sourceBuffer, vData);
      if (this.closed) return;
      if (aData.length && this.audioBuffer) {
        await this.appendBufferWithRoom(this.audioBuffer, aData);
      }
      return;
    }
    await this.appendBufferWithRoom(this.sourceBuffer, data);
  }

  async appendBufferWithRoom(sb, data) {
    await this.evictBehind(sb, this.behindSeconds());
    if (this.closed) return;
    try {
      await this.appendTo(sb, data);
    } catch (e) {
      if (e?.name !== "QuotaExceededError") throw e;
      await this.evictBehind(sb, 1, true);
      if (this.closed) return;
      await this.appendTo(sb, data);
    }
  }

  async evictBehind(sb, keep, force = false) {
    const ranges = sb.buffered;
    if (!ranges.length) return;
    const end = this.video.currentTime - keep;
    // 少しずつ消すと remove が多くなるので、通常時は余分が 5 秒たまってからまとめて消す
    if (!force && end - ranges.start(0) < 5) return;
    await this.removeFrom(sb, 0, end);
  }

  appendTo(sb, data) {
    return this.whenUpdated(sb, () => sb.appendBuffer(data));
  }

  async remove(start, end) {
    await this.removeFrom(this.sourceBuffer, start, end);
    if (this.closed) return;
    if (this.audioBuffer) await this.removeFrom(this.audioBuffer, start, end);
  }

  removeFrom(sb, start, end) {
    const ranges = sb.buffered;
    if (!ranges.length) return Promise.resolve();
    const until = Math.min(end, ranges.end(ranges.length - 1));
    if (until <= start) return Promise.resolve();
    return this.whenUpdated(sb, () => sb.remove(start, until));
  }

  // SourceBuffer の操作をして、終わる（updateend）まで待つ
  /** @returns {Promise<void>} */
  whenUpdated(sb, action) {
    return new Promise((resolve, reject) => {
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
    if (
      this.mediaSource.readyState !== "open" ||
      this.sourceBuffer.updating ||
      this.audioBuffer?.updating
    ) {
      return;
    }
    try {
      this.mediaSource.endOfStream();
    } catch {
      // 閉じた直後などは無視する
    }
  }

  // 何か起きる（再生が進む・シークする）か、しばらくたつまで待つ
  /** @returns {Promise<void>} */
  idle() {
    return new Promise((resolve) => {
      const timer = setTimeout(() => resolve(), MKV_IDLE_MS);
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

// 分離音声パック `[video_len: u32 BE][video][audio]` を映像・音声の Uint8Array に分ける
export function unpackDual(bytes) {
  if (bytes.byteLength < 4) throw new Error("動画のデータを読み込めませんでした");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const vLen = view.getUint32(0, false);
  if (4 + vLen > bytes.byteLength) throw new Error("動画のデータを読み込めませんでした");
  return [bytes.subarray(4, 4 + vLen), bytes.subarray(4 + vLen)];
}

// invoke が返した生のバイト列（ArrayBuffer）を Uint8Array にする
function mkvBytes(data) {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data))
    return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  if (Array.isArray(data)) return Uint8Array.from(data);
  throw new Error("動画のデータを読み込めませんでした");
}
