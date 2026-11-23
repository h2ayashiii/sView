// 音声トラックの取得・切り替え・ラベル生成
// MKV などに音声が 2 つ以上入っているとき、右クリックメニューの「音声」から切り替える。
// audioTracks は WebView2 では実験的機能で、起動引数（WEBVIEW2_BROWSER_ARGS）で
// 有効にしている。使えない WebView では audioTracks が無いので、「音声」はグレーアウトする。
// macOS の MKV は音声を 1 本だけ詰め直して流すので、一覧は MkvPlayer が持っている

// 音声・字幕のトラックの表示名（番号 + 名前・言語のうちあるもの。どちらも無ければ「音声 n」など）
export function trackLabel(label, language, i, kind) {
  const name =
    label && language && !label.includes(language) ? `${label} (${language})` : label || language;
  return name ? `${i + 1}: ${name}` : `${kind} ${i + 1}`;
}

export function audioTrackLabel(track, i) {
  return trackLabel(track.label, track.language, i, "音声");
}

export function audioTrackList(showingVideo, mkvPlayer, video) {
  if (!showingVideo) return null;
  if (mkvPlayer) return mkvPlayer.audioTracks;
  return video.audioTracks ?? null;
}

// 表示中の音声トラックの番号（分からなければ -1）
export function selectedAudioTrack(tracks) {
  if (!tracks) return -1;
  for (let i = 0; i < tracks.length; i++) if (tracks[i].enabled) return i;
  return -1;
}
