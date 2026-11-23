import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  audioTrackLabel,
  audioTrackList,
  selectedAudioTrack,
  trackLabel,
} from "../src/viewer/audio-tracks.js";

describe("audio-tracks", () => {
  it("trackLabel は名前と言語の組み合わせに応じて表示名を作る", () => {
    assert.equal(trackLabel("日本語", "jpn", 0, "音声"), "1: 日本語 (jpn)");
    assert.equal(trackLabel("Japanese (jpn)", "jpn", 1, "音声"), "2: Japanese (jpn)");
    assert.equal(trackLabel("Commentary", "", 2, "音声"), "3: Commentary");
    assert.equal(trackLabel("", "eng", 0, "字幕"), "1: eng");
    assert.equal(trackLabel("", "", 3, "字幕"), "字幕 4");
  });

  it("audioTrackLabel は音声トラックの既定種別名を使う", () => {
    assert.equal(audioTrackLabel({ label: "", language: "" }, 0), "音声 1");
    assert.equal(audioTrackLabel({ label: "Stereo", language: "ja" }, 1), "2: Stereo (ja)");
  });

  it("audioTrackList は動画表示中のみトラック一覧を返し、MkvPlayer を優先する", () => {
    const mkvTracks = [{ enabled: true }];
    const videoTracks = [{ enabled: false }];
    assert.equal(
      audioTrackList(false, { audioTracks: mkvTracks }, { audioTracks: videoTracks }),
      null
    );
    assert.equal(
      audioTrackList(true, { audioTracks: mkvTracks }, { audioTracks: videoTracks }),
      mkvTracks
    );
    assert.equal(audioTrackList(true, null, { audioTracks: videoTracks }), videoTracks);
    assert.equal(audioTrackList(true, null, {}), null);
  });

  it("selectedAudioTrack は有効なトラックの番号を返す", () => {
    assert.equal(selectedAudioTrack(null), -1);
    assert.equal(selectedAudioTrack([{ enabled: false }, { enabled: true }]), 1);
    assert.equal(selectedAudioTrack([{ enabled: false }]), -1);
  });
});
