// WebM（audio/webm; codecs="opus"）の初期化セグメントとメディア断片を組み立てる。
// macOS の WKWebView / Safari は MP4（fMP4）コンテナ内の Opus（dOps）に非対応だが、
// WebM コンテナ内の Opus（A_OPUS + OpusHead）には macOS 11.3 以降で対応している。
// そのため Opus 音声のときは MSE の SourceBuffer を映像（video/mp4）と音声（audio/webm）の
// 2 本に分離し、音声だけを最小限の WebM バイトストリームとして流す。

use super::fmp4::TrackRun;

/// タイムスタンプの単位（0.1 ms = 100,000 ns = 10,000 Hz）。
/// 10^9 の約数なので WebKit の CMTime 変換で端数が出ず、Opus の 2.5 ms / 5 ms / 20 ms フレームや
/// 312 サンプル（6.5 ms）の境界もすべて整数ティックで表せる
pub(crate) const WEBM_TIMESTAMP_SCALE_NS: u64 = 100_000;
/// Opus のサンプリング周波数（RFC 6716 / WebM 仕様上常に 48 kHz）
const OPUS_SAMPLE_RATE: u64 = 48_000;
/// 1 つの Cluster 内に収める相対時刻の上限（3.0 秒 = 30,000 ティック）。SimpleBlock の相対時刻は i16（最大 32,767）
const MAX_CLUSTER_SPAN_TICKS: u64 = 30_000;

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

fn push_ebml_element(out: &mut Vec<u8>, id: &[u8], body: &[u8]) {
    out.extend_from_slice(id);
    push_ebml_size(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn push_ebml_uint(out: &mut Vec<u8>, id: &[u8], val: u64) {
    let bytes = val.to_be_bytes();
    let first = bytes
        .iter()
        .position(|&b| b != 0)
        .unwrap_or(bytes.len() - 1);
    push_ebml_element(out, id, &bytes[first..]);
}

fn push_ebml_f64(out: &mut Vec<u8>, id: &[u8], val: f64) {
    push_ebml_element(out, id, &val.to_be_bytes());
}

/// `OpusHead` から先頭スキップ量（48 kHz サンプル数）を読む
pub(crate) fn opus_head_pre_skip(opus_head: &[u8]) -> u16 {
    if opus_head.len() >= 12 && &opus_head[..8] == b"OpusHead" {
        u16::from_le_bytes([opus_head[10], opus_head[11]])
    } else {
        0
    }
}

/// MP4 の `dOps` ボックスの中身（ビッグエンディアン）から WebM 用の `OpusHead`（リトルエンディアン）を作る
pub(crate) fn dops_to_opus_head(dops: &[u8]) -> Option<Vec<u8>> {
    if dops.len() < 11 || dops[0] != 0 {
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
    let mut ebml_body = Vec::with_capacity(32);
    push_ebml_uint(&mut ebml_body, &[0x42, 0x86], 1); // EBMLVersion
    push_ebml_uint(&mut ebml_body, &[0x42, 0xF7], 1); // EBMLReadVersion
    push_ebml_uint(&mut ebml_body, &[0x42, 0xF2], 4); // EBMLMaxIDLength
    push_ebml_uint(&mut ebml_body, &[0x42, 0xF3], 8); // EBMLMaxSizeLength
    push_ebml_element(&mut ebml_body, &[0x42, 0x82], b"webm"); // DocType
    push_ebml_uint(&mut ebml_body, &[0x42, 0x87], 4); // DocTypeVersion
    push_ebml_uint(&mut ebml_body, &[0x42, 0x85], 2); // DocTypeReadVersion

    let mut info_body = Vec::with_capacity(24);
    push_ebml_uint(&mut info_body, &[0x2A, 0xD7, 0xB1], WEBM_TIMESTAMP_SCALE_NS); // TimestampScale
    push_ebml_element(&mut info_body, &[0x4D, 0x80], b"sView"); // MuxingApp
    push_ebml_element(&mut info_body, &[0x57, 0x41], b"sView"); // WritingApp

    let mut audio_body = Vec::with_capacity(16);
    push_ebml_f64(&mut audio_body, &[0xB5], OPUS_SAMPLE_RATE as f64); // SamplingFrequency
    push_ebml_uint(&mut audio_body, &[0x9F], channels.max(1) as u64); // Channels

    let pre_skip = opus_head_pre_skip(opus_head) as u64;
    let codec_delay_ns = (pre_skip * 1_000_000_000 + OPUS_SAMPLE_RATE / 2) / OPUS_SAMPLE_RATE;

    let mut track_entry = Vec::with_capacity(64 + opus_head.len());
    push_ebml_uint(&mut track_entry, &[0xD7], 1); // TrackNumber
    push_ebml_uint(&mut track_entry, &[0x73, 0xC5], 1); // TrackUID
    push_ebml_uint(&mut track_entry, &[0x83], 2); // TrackType = Audio
    push_ebml_element(&mut track_entry, &[0x86], b"A_OPUS"); // CodecID
    push_ebml_element(&mut track_entry, &[0x63, 0xA2], opus_head); // CodecPrivate (OpusHead)
    push_ebml_uint(&mut track_entry, &[0x56, 0xAA], codec_delay_ns); // CodecDelay
    push_ebml_uint(&mut track_entry, &[0x56, 0xBB], 80_000_000); // SeekPreRoll (80 ms)
    push_ebml_element(&mut track_entry, &[0xE1], &audio_body); // Audio

    let mut tracks_body = Vec::with_capacity(track_entry.len() + 4);
    push_ebml_element(&mut tracks_body, &[0xAE], &track_entry);

    let mut out = Vec::with_capacity(160 + opus_head.len());
    push_ebml_element(&mut out, &[0x1A, 0x45, 0xDF, A3_BYTE], &ebml_body);
    // Segment（長さ不明 = 8 バイト VINT の全ビット 1。後続の Cluster を順に append できる）
    out.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]);
    out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    push_ebml_element(&mut out, &[0x15, 0x49, 0xA9, 0x66], &info_body);
    push_ebml_element(&mut out, &[0x16, 0x54, 0xAE, 0x6B], &tracks_body);
    out
}

const A3_BYTE: u8 = 0xA3;

/// 48 kHz 単位の Opus TrackRun から WebM の Cluster 断片を `out` に直接追記する（中間 Vec 確保なし）
pub(crate) fn append_webm_opus_fragment(out: &mut Vec<u8>, run: &TrackRun) {
    if run.samples.is_empty() {
        return;
    }
    let est_bytes: usize = run.samples.iter().map(|s| s.data.len() + 9).sum();
    out.reserve(est_bytes + 32);

    let samples_to_ticks = |samples: u64| -> u64 {
        let ticks_per_sec = 1_000_000_000 / WEBM_TIMESTAMP_SCALE_NS;
        ((samples as u128 * ticks_per_sec as u128 + (OPUS_SAMPLE_RATE as u128 / 2))
            / OPUS_SAMPLE_RATE as u128) as u64
    };
    let start_cluster = |out: &mut Vec<u8>, ts: u64| -> usize {
        out.extend_from_slice(&[0x1F, 0x43, 0xB6, 0x75]); // Cluster ID
        let size_pos = out.len();
        out.extend_from_slice(&[0u8; 8]); // 8 バイト VINT サイズ予約枠
        push_ebml_uint(out, &[0xE7], ts); // Timestamp
        size_pos
    };
    let finish_cluster = |out: &mut Vec<u8>, size_pos: usize| {
        let body_len = (out.len() - (size_pos + 8)) as u64;
        let vint = 0x0100_0000_0000_0000 | (body_len & 0x00FF_FFFF_FFFF_FFFF);
        out[size_pos..size_pos + 8].copy_from_slice(&vint.to_be_bytes());
    };

    let mut cur_samples = run.base_decode_time;
    let mut cluster_ts = samples_to_ticks(cur_samples);
    let mut size_pos = start_cluster(out, cluster_ts);

    for sample in &run.samples {
        let pts = samples_to_ticks(cur_samples);
        cur_samples = cur_samples.saturating_add(sample.duration as u64);
        if pts.saturating_sub(cluster_ts) > MAX_CLUSTER_SPAN_TICKS {
            finish_cluster(out, size_pos);
            cluster_ts = pts;
            size_pos = start_cluster(out, cluster_ts);
        }
        let rel = (pts as i64 - cluster_ts as i64).clamp(i16::MIN as i64, i16::MAX as i64) as i16;
        // SimpleBlock: TrackNumber VINT (1 -> 0x81) + 相対時刻 (i16 BE) + flags (0x80 = keyframe) + payload
        let block_len = 4 + sample.data.len();
        out.push(0xA3);
        push_ebml_size(out, block_len as u64);
        out.push(0x81);
        out.extend_from_slice(&rel.to_be_bytes());
        out.push(0x80);
        out.extend_from_slice(&sample.data);
    }

    finish_cluster(out, size_pos);
}

#[cfg(test)]
pub(crate) fn build_webm_opus_fragment(run: &TrackRun) -> Vec<u8> {
    let mut out = Vec::new();
    append_webm_opus_fragment(&mut out, run);
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
