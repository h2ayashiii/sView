// WebM（audio/webm; codecs="opus"）の初期化セグメントとメディア断片を組み立てる。
// macOS の WKWebView / Safari は MP4（fMP4）コンテナ内の Opus（dOps）に非対応だが、
// WebM コンテナ内の Opus（A_OPUS + OpusHead）には macOS 11.3 以降で対応している。
// そのため Opus 音声のときは MSE の SourceBuffer を映像（video/mp4）と音声（audio/webm）の
// 2 本に分離し、音声だけを最小限の WebM バイトストリームとして流す。

use super::fmp4::TrackRun;

/// タイムスタンプの単位（1 ms = 1,000,000 ns。WebM の標準値）
const WEBM_TIMESTAMP_SCALE_NS: u64 = 1_000_000;
/// Opus のサンプリング周波数（RFC 6716 / WebM 仕様上常に 48 kHz）
const OPUS_SAMPLE_RATE: u64 = 48_000;
/// 1 つの Cluster 内に収める相対時刻の上限（ms）。SimpleBlock の相対時刻は i16（最大 32767 ms）
const MAX_CLUSTER_SPAN_MS: u64 = 30_000;

/// EBML の要素サイズを VINT として書き出す（全ビット 1 の不明サイズは避ける）
fn push_ebml_size(out: &mut Vec<u8>, size: u64) {
    if size <= 0x7E {
        out.push(0x80 | size as u8);
    } else if size <= 0x3FFE {
        let v = 0x4000 | size as u16;
        out.extend_from_slice(&v.to_be_bytes());
    } else if size <= 0x1F_FFFE {
        let v = 0x20_0000 | size as u32;
        out.extend_from_slice(&v.to_be_bytes()[1..]);
    } else if size <= 0x0FFF_FFFE {
        let v = 0x1000_0000 | size as u32;
        out.extend_from_slice(&v.to_be_bytes());
    } else {
        let v = 0x0100_0000_0000_0000 | (size & 0x00FF_FFFF_FFFF_FFFF);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn ebml_element(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(id.len() + 8 + body.len());
    out.extend_from_slice(id);
    push_ebml_size(&mut out, body.len() as u64);
    out.extend_from_slice(body);
    out
}

fn ebml_uint(id: &[u8], val: u64) -> Vec<u8> {
    let bytes = val.to_be_bytes();
    let first = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    ebml_element(id, &bytes[first..])
}

fn ebml_f64(id: &[u8], val: f64) -> Vec<u8> {
    ebml_element(id, &val.to_be_bytes())
}

/// MP4 の `dOps` ボックスの中身（ビッグエンディアン）から WebM 用の `OpusHead`（リトルエンディアン）を作る
pub(crate) fn dops_to_opus_head(dops: &[u8]) -> Option<Vec<u8>> {
    if dops.len() < 11 {
        return None;
    }
    let channels = dops[1];
    if channels == 0 {
        return None;
    }
    let pre_skip = u16::from_be_bytes([dops[2], dops[3]]);
    let input_rate = u32::from_be_bytes([dops[4], dops[5], dops[6], dops[7]]);
    let gain = i16::from_be_bytes([dops[8], dops[9]]);
    let family = dops[10];
    let mut out = Vec::with_capacity(if family == 0 {
        19
    } else {
        21 + channels as usize
    });
    out.extend_from_slice(b"OpusHead");
    out.push(1); // Version = 1
    out.push(channels);
    out.extend_from_slice(&pre_skip.to_le_bytes());
    out.extend_from_slice(&input_rate.to_le_bytes());
    out.extend_from_slice(&gain.to_le_bytes());
    out.push(family);
    if family != 0 {
        let extra = dops.get(11..13 + channels as usize)?;
        out.extend_from_slice(extra);
    }
    Some(out)
}

/// WebM の初期化セグメント（EBML + Segment[不明サイズ] + Info + Tracks[A_OPUS]）を作る
pub(crate) fn build_webm_opus_init(channels: u16, opus_head: &[u8]) -> Vec<u8> {
    let mut ebml_body = Vec::new();
    ebml_body.extend(ebml_uint(&[0x42, 0x86], 1)); // EBMLVersion
    ebml_body.extend(ebml_uint(&[0x42, 0xF7], 1)); // EBMLReadVersion
    ebml_body.extend(ebml_uint(&[0x42, 0xF2], 4)); // EBMLMaxIDLength
    ebml_body.extend(ebml_uint(&[0x42, 0xF3], 8)); // EBMLMaxSizeLength
    ebml_body.extend(ebml_element(&[0x42, 0x82], b"webm")); // DocType
    ebml_body.extend(ebml_uint(&[0x42, 0x87], 4)); // DocTypeVersion
    ebml_body.extend(ebml_uint(&[0x42, 0x85], 2)); // DocTypeReadVersion

    let mut info_body = Vec::new();
    info_body.extend(ebml_uint(&[0x2A, 0xD7, 0xB1], WEBM_TIMESTAMP_SCALE_NS)); // TimestampScale
    info_body.extend(ebml_element(&[0x4D, 0x80], b"sView")); // MuxingApp
    info_body.extend(ebml_element(&[0x57, 0x41], b"sView")); // WritingApp

    let mut audio_body = Vec::new();
    audio_body.extend(ebml_f64(&[0xB5], OPUS_SAMPLE_RATE as f64)); // SamplingFrequency
    audio_body.extend(ebml_uint(&[0x9F], channels.max(1) as u64)); // Channels

    let mut track_body = Vec::new();
    track_body.extend(ebml_uint(&[0xD7], 1)); // TrackNumber
    track_body.extend(ebml_uint(&[0x73, 0xC5], 1)); // TrackUID
    track_body.extend(ebml_uint(&[0x83], 2)); // TrackType = Audio
    track_body.extend(ebml_element(&[0x86], b"A_OPUS")); // CodecID
    track_body.extend(ebml_element(&[0x63, 0xA2], opus_head)); // CodecPrivate (OpusHead)
    // タイムスタンプは remux 側で CodecDelay 減算済みのため 0 にして二重シフトを防ぐ
    track_body.extend(ebml_uint(&[0x56, 0xAA], 0)); // CodecDelay
    track_body.extend(ebml_uint(&[0x56, 0xBB], 80_000_000)); // SeekPreRoll (80 ms)
    track_body.extend(ebml_element(&[0xE1], &audio_body)); // Audio

    let tracks = ebml_element(
        &[0x16, 0x54, 0xAE, 0x6B],
        &ebml_element(&[0xAE], &track_body),
    );

    let mut out = ebml_element(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_body);
    // Segment（長さ不明 = 8 バイト VINT の全ビット 1。後続の Cluster を順に append できる）
    out.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]);
    out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    out.extend(ebml_element(&[0x15, 0x49, 0xA9, 0x66], &info_body));
    out.extend(tracks);
    out
}

/// 48 kHz 単位の Opus TrackRun から WebM の Cluster 断片を作る
pub(crate) fn build_webm_opus_fragment(run: &TrackRun) -> Vec<u8> {
    if run.samples.is_empty() {
        return Vec::new();
    }
    let ticks_to_ms = |ticks: u64| -> u64 {
        ((ticks as u128 * 1000 + (OPUS_SAMPLE_RATE as u128 / 2)) / OPUS_SAMPLE_RATE as u128) as u64
    };
    let mut out = Vec::new();
    let mut cur_ticks = run.base_decode_time;
    let mut cluster_ts_ms = ticks_to_ms(cur_ticks);
    let mut cluster_body = ebml_uint(&[0xE7], cluster_ts_ms);

    for sample in &run.samples {
        let pts_ms = ticks_to_ms(cur_ticks);
        cur_ticks = cur_ticks.saturating_add(sample.duration as u64);
        if pts_ms.saturating_sub(cluster_ts_ms) > MAX_CLUSTER_SPAN_MS {
            out.extend(ebml_element(&[0x1F, 0x43, 0xB6, 0x75], &cluster_body));
            cluster_ts_ms = pts_ms;
            cluster_body = ebml_uint(&[0xE7], cluster_ts_ms);
        }
        let rel =
            (pts_ms as i64 - cluster_ts_ms as i64).clamp(i16::MIN as i64, i16::MAX as i64) as i16;
        // SimpleBlock: TrackNumber VINT (1 -> 0x81) + 相対時刻 (i16 BE) + flags (0x80 = keyframe) + payload
        let block_len = 4 + sample.data.len();
        cluster_body.push(0xA3);
        push_ebml_size(&mut cluster_body, block_len as u64);
        cluster_body.push(0x81);
        cluster_body.extend_from_slice(&rel.to_be_bytes());
        cluster_body.push(0x80);
        cluster_body.extend_from_slice(&sample.data);
    }

    out.extend(ebml_element(&[0x1F, 0x43, 0xB6, 0x75], &cluster_body));
    out
}

/// 分離音声モード用に `[video_len: u32 BE][video][audio]` の 1 バイト列にまとめる。
/// どちらも空（EOF）のときは空ベクタを返す
pub(crate) fn pack_dual_payload(video: &[u8], audio: &[u8]) -> Vec<u8> {
    if video.is_empty() && audio.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(4 + video.len() + audio.len());
    out.extend_from_slice(&(video.len() as u32).to_be_bytes());
    out.extend_from_slice(video);
    out.extend_from_slice(audio);
    out
}

/// `pack_dual_payload` の逆変換（テスト用）
#[cfg(test)]
pub(crate) fn unpack_dual_payload(data: &[u8]) -> Option<(&[u8], &[u8])> {
    if data.len() < 4 {
        return None;
    }
    let vlen = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let rest = &data[4..];
    if vlen > rest.len() {
        return None;
    }
    Some((&rest[..vlen], &rest[vlen..]))
}
