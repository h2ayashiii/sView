// 字幕の読み込み・表示制御
// WebView は MKV の中の字幕を出さない（macOS の MKV は字幕を詰め直さない）ので、Rust 側（video_subtitles）で
// テキスト字幕（SRT / ASS）を取り出し、自前の TextTrack に VTTCue として入れて表示する。
// addTextTrack で足したトラックは <video> から外せないので、1 本だけ作って使い回し、
// 字幕を切り替えるときは中身（cue）を入れ替える

import { trackLabel } from "./audio-tracks.js";

// 字幕を出す行（下から数える。-1 が最下行なので -2 で 1 行分上に離す）
const SUBTITLE_LINE = -2;

export function createSubtitleController({ video, invoke, isCurrentToken }) {
  let subtitleTextTrack = null;
  // 表示中の動画の字幕トラック（Rust の SubtitleTrack の配列）
  let subtitleTracks = [];
  // 表示している字幕の番号（-1 = オフ）
  let subtitleIndex = -1;
  // 字幕を読み込んでいる最中か（右クリックメニューで「読み込み中」と出す）
  let subtitleLoading = false;

  function ensureSubtitleTextTrack() {
    if (!subtitleTextTrack) subtitleTextTrack = video.addTextTrack("subtitles", "sView");
    return subtitleTextTrack;
  }

  // 字幕 n を表示する（-1 で消す）。選んだものは保存しない
  function selectSubtitle(n) {
    subtitleIndex = n >= 0 && n < subtitleTracks.length ? n : -1;
    if (!subtitleTextTrack && subtitleIndex < 0) return;
    const track = ensureSubtitleTextTrack();
    // disabled のあいだは cues が null なので、いったん hidden にしてから空にする
    track.mode = "hidden";
    for (const cue of Array.from(track.cues ?? [])) track.removeCue(cue);
    if (subtitleIndex < 0) {
      track.mode = "disabled";
      return;
    }
    for (const { start, end, text } of subtitleTracks[subtitleIndex].cues) {
      if (end <= start) continue;
      const cue = new VTTCue(start, end, text);
      // 既定（line: "auto"）では動画の下端に張り付くので、1 行分だけ上に離す
      // （負の値は下から数えた行。-1 が最下行）
      cue.line = SUBTITLE_LINE;
      track.addCue(cue);
    }
    track.mode = "showing";
  }

  function clearSubtitles() {
    subtitleTracks = [];
    subtitleLoading = false;
    selectSubtitle(-1);
  }

  // 動画の字幕を読み込む。開いたときは常にオフ（動画で既定とされている字幕があっても出さない）
  async function loadSubtitles(path, token) {
    clearSubtitles();
    subtitleLoading = true;
    let tracks = [];
    try {
      tracks = await invoke("video_subtitles", { path });
    } catch (e) {
      // 字幕が読めなくても動画は再生できるので、知らせずに字幕なしで続ける
      console.warn("字幕を読み込めませんでした", e);
    }
    if (!isCurrentToken(token)) return; // 既に別のファイルへ移動している
    subtitleLoading = false;
    subtitleTracks = tracks.filter((t) => t.cues.length > 0);
  }

  // 右クリックメニューの「字幕」。合わせると「オフ」と字幕トラックの一覧を出す
  function subtitleMenuItem() {
    if (subtitleLoading) return { label: "字幕（読み込み中）", disabled: true, submenu: [] };
    const items = subtitleTracks.map((t, i) => ({
      label: trackLabel(t.name, t.language, i, "字幕"),
      radio: true,
      checked: subtitleIndex === i,
      action: () => selectSubtitle(i),
    }));
    return {
      label: "字幕",
      disabled: items.length === 0,
      submenu: [
        {
          label: "オフ",
          radio: true,
          checked: subtitleIndex < 0,
          action: () => selectSubtitle(-1),
        },
        ...items,
      ],
    };
  }

  return {
    selectSubtitle,
    clearSubtitles,
    loadSubtitles,
    subtitleMenuItem,
  };
}
