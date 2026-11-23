use super::remux::{
    REMUX_VIDEO_TIMESCALE, RemuxAudio, RemuxAudioCodec, RemuxVideo, RemuxVideoCodec,
};

// ---- fragmented MP4 の書き出し ----

pub(crate) struct Sample {
    pub(crate) data: Vec<u8>,
    pub(crate) duration: u32,
    pub(crate) composition_offset: i32,
    pub(crate) keyframe: bool,
}

/// 断片の中のトラック 1 本ぶんのサンプル
pub(crate) struct TrackRun {
    pub(crate) track_id: u32,
    pub(crate) base_decode_time: u64,
    pub(crate) samples: Vec<Sample>,
    pub(crate) has_composition_offsets: bool,
}

pub(crate) fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

pub(crate) fn mp4_full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut full = Vec::with_capacity(body.len() + 4);
    full.extend_from_slice(&((version as u32) << 24 | (flags & 0xFF_FFFF)).to_be_bytes());
    full.extend_from_slice(body);
    mp4_box(kind, &full)
}

/// 単位行列（tkhd / mvhd）
pub(crate) const MP4_MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

pub(crate) fn mp4_matrix() -> Vec<u8> {
    MP4_MATRIX.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// MPEG-4 の記述子（esds の中身）。長さは 1 バイトずつ 7 ビットで書く
pub(crate) fn mp4_descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = body.len();
    let mut bytes = Vec::new();
    let mut rest = len;
    loop {
        bytes.push((rest & 0x7F) as u8);
        rest >>= 7;
        if rest == 0 {
            break;
        }
    }
    for (i, b) in bytes.iter().rev().enumerate() {
        out.push(if i + 1 < bytes.len() { b | 0x80 } else { *b });
    }
    out.extend_from_slice(body);
    out
}

pub(crate) fn mp4_esds(asc: &[u8]) -> Vec<u8> {
    let mut config = vec![0x40, 0x15, 0, 0, 0];
    config.extend_from_slice(&[0; 8]); // maxBitrate / avgBitrate
    config.extend(mp4_descriptor(0x05, asc));
    let mut es = vec![0, 2, 0]; // ES_ID / flags
    es.extend(mp4_descriptor(0x04, &config));
    es.extend(mp4_descriptor(0x06, &[0x02]));
    mp4_full_box(b"esds", 0, 0, &mp4_descriptor(0x03, &es))
}

pub(crate) fn video_sample_entry(video: &RemuxVideo) -> Vec<u8> {
    let mut body = vec![0u8; 6];
    body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    body.extend_from_slice(&[0; 16]);
    body.extend_from_slice(&(video.width as u16).to_be_bytes());
    body.extend_from_slice(&(video.height as u16).to_be_bytes());
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    body.extend_from_slice(&[0; 4]);
    body.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    body.extend_from_slice(&[0; 32]); // compressorname
    body.extend_from_slice(&0x0018u16.to_be_bytes());
    body.extend_from_slice(&(-1i16).to_be_bytes());
    let (kind, config) = match video.codec {
        RemuxVideoCodec::Avc => (b"avc1", b"avcC"),
        // hev1 では再生できない WebKit があるので hvc1 にする
        RemuxVideoCodec::Hevc => (b"hvc1", b"hvcC"),
    };
    body.extend(mp4_box(config, &video.config));
    let (dw, dh) = video.display;
    if (dw, dh) != (video.width, video.height) && dw > 0 && dh > 0 {
        // 画素の縦横比 = 表示の縦横比 / 画素数の縦横比
        let h = dw as u64 * video.height as u64;
        let v = dh as u64 * video.width as u64;
        let g = gcd(h, v).max(1);
        let mut pasp = Vec::new();
        pasp.extend_from_slice(&((h / g).min(u32::MAX as u64) as u32).to_be_bytes());
        pasp.extend_from_slice(&((v / g).min(u32::MAX as u64) as u32).to_be_bytes());
        body.extend(mp4_box(b"pasp", &pasp));
    }
    mp4_box(kind, &body)
}

pub(crate) fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

pub(crate) fn audio_sample_entry(audio: &RemuxAudio) -> Vec<u8> {
    let mut body = vec![0u8; 6];
    body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&audio.channels.to_be_bytes());
    body.extend_from_slice(&16u16.to_be_bytes()); // samplesize
    body.extend_from_slice(&[0; 4]);
    // 16.16 の固定小数点（65535 Hz を超える周波数は書けないので 0 にする。実際の値は中の設定にある）
    let rate = if audio.timescale <= u16::MAX as u32 {
        audio.timescale << 16
    } else {
        0
    };
    body.extend_from_slice(&rate.to_be_bytes());
    match audio.codec {
        RemuxAudioCodec::Aac => {
            body.extend(mp4_esds(&audio.config));
            mp4_box(b"mp4a", &body)
        }
        RemuxAudioCodec::Opus => {
            body.extend(mp4_box(b"dOps", &audio.config));
            mp4_box(b"Opus", &body)
        }
    }
}

pub(crate) fn mp4_trak(
    track_id: u32,
    timescale: u32,
    video: Option<(u32, u32)>,
    entry: Vec<u8>,
) -> Vec<u8> {
    let mut tkhd = vec![0u8; 8]; // creation / modification
    tkhd.extend_from_slice(&track_id.to_be_bytes());
    tkhd.extend_from_slice(&[0; 4]);
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // duration
    tkhd.extend_from_slice(&[0; 8]);
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // layer
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    tkhd.extend_from_slice(&(if video.is_some() { 0u16 } else { 0x0100 }).to_be_bytes());
    tkhd.extend_from_slice(&[0; 2]);
    tkhd.extend(mp4_matrix());
    let (w, h) = video.unwrap_or((0, 0));
    tkhd.extend_from_slice(&(w << 16).to_be_bytes());
    tkhd.extend_from_slice(&(h << 16).to_be_bytes());

    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&timescale.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0x55C4u16.to_be_bytes()); // "und"
    mdhd.extend_from_slice(&[0; 2]);

    let (handler, name, media_header) = if video.is_some() {
        (
            b"vide",
            &b"VideoHandler\0"[..],
            mp4_full_box(b"vmhd", 0, 1, &[0; 8]),
        )
    } else {
        (
            b"soun",
            &b"SoundHandler\0"[..],
            mp4_full_box(b"smhd", 0, 0, &[0; 4]),
        )
    };
    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(handler);
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.extend_from_slice(name);

    let dref = mp4_full_box(
        b"dref",
        0,
        0,
        &[
            1u32.to_be_bytes().to_vec(),
            mp4_full_box(b"url ", 0, 1, &[]),
        ]
        .concat(),
    );
    let stsd = mp4_full_box(
        b"stsd",
        0,
        0,
        &[1u32.to_be_bytes().to_vec(), entry].concat(),
    );
    let stbl = mp4_box(
        b"stbl",
        &[
            stsd,
            mp4_full_box(b"stts", 0, 0, &[0; 4]),
            mp4_full_box(b"stsc", 0, 0, &[0; 4]),
            mp4_full_box(b"stsz", 0, 0, &[0; 8]),
            mp4_full_box(b"stco", 0, 0, &[0; 4]),
        ]
        .concat(),
    );
    let minf = mp4_box(
        b"minf",
        &[media_header, mp4_box(b"dinf", &dref), stbl].concat(),
    );
    let mdia = mp4_box(
        b"mdia",
        &[
            mp4_full_box(b"mdhd", 0, 0, &mdhd),
            mp4_full_box(b"hdlr", 0, 0, &hdlr),
            minf,
        ]
        .concat(),
    );
    mp4_box(
        b"trak",
        &[mp4_full_box(b"tkhd", 0, 3, &tkhd), mdia].concat(),
    )
}

pub(crate) fn build_init_segment(video: &RemuxVideo, audio: Option<&RemuxAudio>) -> Vec<u8> {
    let ftyp = mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso6mp41");
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    // 長さは書かない（フロントエンドが MediaSource.duration に入れる）
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume
    mvhd.extend_from_slice(&[0; 10]);
    mvhd.extend(mp4_matrix());
    mvhd.extend_from_slice(&[0; 24]);
    mvhd.extend_from_slice(&3u32.to_be_bytes()); // next_track_ID
    let mut moov = mp4_full_box(b"mvhd", 0, 0, &mvhd);
    moov.extend(mp4_trak(
        1,
        REMUX_VIDEO_TIMESCALE as u32,
        Some(video.display),
        video_sample_entry(video),
    ));
    let mut mvex = mp4_trex(1);
    if let Some(audio) = audio {
        moov.extend(mp4_trak(
            2,
            audio.timescale,
            None,
            audio_sample_entry(audio),
        ));
        mvex.extend(mp4_trex(2));
    }
    moov.extend(mp4_box(b"mvex", &mvex));
    [ftyp, mp4_box(b"moov", &moov)].concat()
}

pub(crate) fn mp4_trex(track_id: u32) -> Vec<u8> {
    let mut body = track_id.to_be_bytes().to_vec();
    body.extend_from_slice(&1u32.to_be_bytes()); // default_sample_description_index
    body.extend_from_slice(&[0; 12]);
    mp4_full_box(b"trex", 0, 0, &body)
}

/// サンプルの flags（ISO/IEC 14496-12 8.8.3.1）。キーフレームは他に依存しない、
/// それ以外は依存する・同期サンプルではない
pub(crate) fn sample_flags(keyframe: bool) -> u32 {
    if keyframe { 0x0200_0000 } else { 0x0101_0000 }
}

pub(crate) fn mp4_traf(run: &TrackRun, data_offset: u32) -> Vec<u8> {
    // default-base-is-moof: data_offset は moof の頭から数える
    let tfhd = mp4_full_box(b"tfhd", 0, 0x02_0000, &run.track_id.to_be_bytes());
    let tfdt = mp4_full_box(b"tfdt", 1, 0, &run.base_decode_time.to_be_bytes());
    // data-offset, sample-duration, sample-size, sample-flags（, composition-time-offset）
    let mut flags = 0x0001 | 0x0100 | 0x0200 | 0x0400;
    if run.has_composition_offsets {
        flags |= 0x0800;
    }
    let mut body = (run.samples.len() as u32).to_be_bytes().to_vec();
    body.extend_from_slice(&data_offset.to_be_bytes());
    for s in &run.samples {
        body.extend_from_slice(&s.duration.to_be_bytes());
        body.extend_from_slice(&(s.data.len() as u32).to_be_bytes());
        body.extend_from_slice(&sample_flags(s.keyframe).to_be_bytes());
        if run.has_composition_offsets {
            body.extend_from_slice(&s.composition_offset.to_be_bytes());
        }
    }
    // version 1: composition offset を符号付きで書く（並べ替えが見積もりより深いときに負になりうる）
    let trun = mp4_full_box(b"trun", 1, flags, &body);
    mp4_box(b"traf", &[tfhd, tfdt, trun].concat())
}

pub(crate) fn build_fragment(
    sequence: u32,
    video: Option<&TrackRun>,
    audio: Option<&TrackRun>,
) -> Vec<u8> {
    let runs: Vec<&TrackRun> = [video, audio].into_iter().flatten().collect();
    let moof = |offsets: &[u32]| {
        let mut body = mp4_full_box(b"mfhd", 0, 0, &sequence.to_be_bytes());
        for (run, offset) in runs.iter().zip(offsets) {
            body.extend(mp4_traf(run, *offset));
        }
        mp4_box(b"moof", &body)
    };
    // data_offset の値で moof の大きさは変わらないので、一度作って大きさを測る
    let moof_size = moof(&vec![0; runs.len()]).len() as u32;
    let mut offsets = Vec::new();
    let mut offset = moof_size + 8;
    for run in &runs {
        offsets.push(offset);
        offset += run.samples.iter().map(|s| s.data.len() as u32).sum::<u32>();
    }
    let mut out = moof(&offsets);
    let mdat_size = offset - moof_size;
    out.extend_from_slice(&mdat_size.to_be_bytes());
    out.extend_from_slice(b"mdat");
    for run in &runs {
        for s in &run.samples {
            out.extend_from_slice(&s.data);
        }
    }
    out
}
