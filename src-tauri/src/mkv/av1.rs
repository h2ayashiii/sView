// AV1 動画のメタデータ解析・ソフトウェアデコード（rav1d）・リアルタイム H.264 変換。
// macOS で AV1 ハードウェアデコーダを持たない環境（M1 / M2 など）向けに、
// rav1d でデコードした YUV フレームを NV12 に変換して VideoToolbox へ流し、
// B フレームなしの H.264（avc1）として fMP4 に詰め直す。
//
// - Windows バイナリには rav1d を含めない（WebView2 が標準で AV1 を再生できる）
// - macOS（M3 以降）ではハードウェアデコード可能な AV1 は再エンコードせずそのまま再生する
// - Linux のテスト環境（cfg(test)）でも rav1d デコードと変換パイプラインを検証できる

/// 映像の色空間（ISO/IEC 23091-2 / CICP 値。fMP4 の colr ボックスと VideoToolbox に渡す）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorInfo {
    pub(crate) primaries: u16,
    pub(crate) transfer: u16,
    pub(crate) matrix: u16,
    pub(crate) full_range: bool,
}

impl ColorInfo {
    /// 8bit H.264（SDR）出力の colr ボックスにそのまま書いて問題ない値へ整える。
    /// PQ (16) / HLG (18) のような HDR 伝達特性を 8bit H.264 に残すと色が崩れるため外す
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    pub(crate) fn for_sdr_h264(self) -> Option<Self> {
        if self.primaries == 2 && self.transfer == 2 && self.matrix == 2 && !self.full_range {
            return None;
        }
        let transfer = match self.transfer {
            16 | 18 => 1, // HDR (PQ / HLG) は 8bit H.264 のため BT.709 に落とす
            other => other,
        };
        let primaries = if matches!(self.transfer, 16 | 18) && self.primaries == 9 {
            1
        } else {
            self.primaries
        };
        let matrix = if matches!(self.transfer, 16 | 18) && self.matrix == 9 {
            1
        } else {
            self.matrix
        };
        Some(Self {
            primaries,
            transfer,
            matrix,
            full_range: self.full_range,
        })
    }
}

/// このマシンが AV1 のハードウェアデコードに対応しているか（macOS の M3 以降のみ true）
pub(crate) fn hw_av1_decode_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        super::vt::hw_av1_decode_supported()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// このビルドが AV1 のソフトウェア変換（rav1d + ハードウェアエンコード）に対応しているか
pub(crate) fn can_transcode_av1() -> bool {
    cfg!(target_os = "macos")
}

/// av1C（AV1 Codec Configuration Record）から MP4 の codecs 文字列（`av01.P.LLT.DD`）を作る
pub(crate) fn av1_codec_string(av1c: &[u8]) -> Option<String> {
    if av1c.len() < 4 || av1c[0] != 0x81 {
        return None;
    }
    let profile = (av1c[1] >> 5) & 0x07;
    if profile > 2 {
        return None;
    }
    let level = av1c[1] & 0x1F;
    let tier = if av1c[2] & 0x80 != 0 { 'H' } else { 'M' };
    let high_bitdepth = (av1c[2] & 0x40) != 0;
    let twelve_bit = (av1c[2] & 0x20) != 0;
    let bit_depth = match (high_bitdepth, twelve_bit) {
        (false, _) => 8,
        (true, false) => 10,
        (true, true) => 12,
    };
    Some(format!("av01.{profile}.{level:02}{tier}.{bit_depth:02}"))
}

/// H.264 の SPS / PPS から ISO/IEC 14496-15 の avcC を組み立てる
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn build_avcc_from_sps_pps(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() < 4
        || pps.is_empty()
        || sps.len() > u16::MAX as usize
        || pps.len() > u16::MAX as usize
    {
        return None;
    }
    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    out.push(1); // configurationVersion
    out.push(sps[1]); // AVCProfileIndication
    out.push(sps[2]); // profile_compatibility
    out.push(sps[3]); // AVCLevelIndication
    out.push(0xFF); // lengthSizeMinusOne = 3 (4 バイト長プレフィックス)
    out.push(0xE1); // numOfSequenceParameterSets = 1
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // numOfPictureParameterSets = 1
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    Some(out)
}

/// AV1 の LEB128 可変長整数を読む。戻り値は (値, 消費バイト数)
fn read_leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &b) in data.iter().take(8).enumerate() {
        value |= ((b & 0x7F) as u64) << (i * 7);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// ビット単位の簡易リーダー（AV1 Sequence Header OBU の先頭解析用）
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bits(&mut self, n: usize) -> Option<u32> {
        if n > 32 || self.bit_pos + n > self.data.len() * 8 {
            return None;
        }
        let mut out = 0u32;
        for _ in 0..n {
            let byte = self.data[self.bit_pos / 8];
            let shift = 7 - (self.bit_pos % 8);
            out = (out << 1) | ((byte >> shift) & 1) as u32;
            self.bit_pos += 1;
        }
        Some(out)
    }

    fn read_bit(&mut self) -> Option<bool> {
        Some(self.read_bits(1)? != 0)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        if self.bit_pos + n > self.data.len() * 8 {
            return None;
        }
        self.bit_pos += n;
        Some(())
    }

    fn read_uvlc(&mut self) -> Option<u32> {
        let mut leading = 0usize;
        while !self.read_bit()? {
            leading += 1;
            if leading >= 32 {
                return Some(u32::MAX);
            }
        }
        if leading == 0 {
            return Some(0);
        }
        let val = self.read_bits(leading)?;
        Some(val + ((1u32 << leading) - 1))
    }
}

/// AV1 ビットストリーム（OBU 列）から Sequence Header OBU を探して 4 バイトの av1C を作る。
/// CodecPrivate（av1C）が省かれている MKV / WebM でも av1C を補えるようにする
pub(crate) fn av1c_from_obu_stream(data: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0;
    while pos < data.len() {
        let header = data[pos];
        pos += 1;
        if header & 0x80 != 0 {
            return None; // obu_forbidden_bit
        }
        let obu_type = (header >> 3) & 0x0F;
        let has_ext = (header & 0x04) != 0;
        let has_size = (header & 0x02) != 0;
        if has_ext {
            if pos >= data.len() {
                return None;
            }
            pos += 1;
        }
        let payload = if has_size {
            let (sz, used) = read_leb128(&data[pos..])?;
            pos += used;
            let end = pos.checked_add(sz as usize)?;
            let slice = data.get(pos..end)?;
            pos = end;
            slice
        } else {
            let slice = &data[pos..];
            pos = data.len();
            slice
        };
        if obu_type == 1 {
            return parse_av1_seq_hdr_to_av1c(payload);
        }
    }
    None
}

/// Sequence Header OBU のペイロードから 4 バイトの av1C を組み立てる（AV1 仕様 §5.5.1）
fn parse_av1_seq_hdr_to_av1c(payload: &[u8]) -> Option<Vec<u8>> {
    let mut br = BitReader::new(payload);
    let seq_profile = br.read_bits(3)? as u8;
    let _still_picture = br.read_bit()?;
    let reduced_still = br.read_bit()?;
    let (seq_level_idx_0, seq_tier_0);
    if reduced_still {
        seq_level_idx_0 = br.read_bits(5)? as u8;
        seq_tier_0 = 0u8;
    } else {
        let timing_info_present = br.read_bit()?;
        let mut decoder_model_info_present = false;
        let mut buffer_delay_length = 0usize;
        if timing_info_present {
            br.skip(32)?; // num_units_in_display_tick
            br.skip(32)?; // time_scale
            if br.read_bit()? {
                br.read_uvlc()?; // num_ticks_per_picture_minus_1
            }
            decoder_model_info_present = br.read_bit()?;
            if decoder_model_info_present {
                buffer_delay_length = br.read_bits(5)? as usize + 1;
                br.skip(32)?; // num_units_in_decoding_tick
                br.skip(10)?; // buffer_removal_time / frame_presentation_time lengths
            }
        }
        let initial_display_delay_present = br.read_bit()?;
        let op_cnt = br.read_bits(5)? as usize + 1;
        let mut level0 = 0u8;
        let mut tier0 = 0u8;
        for i in 0..op_cnt {
            br.skip(12)?; // operating_point_idc
            let level = br.read_bits(5)? as u8;
            let tier = if level > 7 { br.read_bits(1)? as u8 } else { 0 };
            if i == 0 {
                level0 = level;
                tier0 = tier;
            }
            if decoder_model_info_present && br.read_bit()? {
                br.skip(buffer_delay_length * 2 + 1)?;
            }
            if initial_display_delay_present && br.read_bit()? {
                br.skip(4)?;
            }
        }
        seq_level_idx_0 = level0;
        seq_tier_0 = tier0;
    }
    let frame_width_bits = br.read_bits(4)? as usize + 1;
    let frame_height_bits = br.read_bits(4)? as usize + 1;
    br.skip(frame_width_bits + frame_height_bits)?;
    if !reduced_still {
        let frame_id_present = br.read_bit()?;
        if frame_id_present {
            br.skip(4 + 3)?;
        }
    }
    br.skip(3)?; // use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter
    if !reduced_still {
        br.skip(4)?; // interintra, masked_compound, warped_motion, dual_filter
        let order_hint = br.read_bit()?;
        if order_hint {
            br.skip(2)?; // jnt_comp, ref_frame_mvs
        }
        let screen_content = if br.read_bit()? { 2 } else { br.read_bits(1)? };
        if screen_content > 0 && !br.read_bit()? {
            br.skip(1)?;
        }
        if order_hint {
            br.skip(3)?;
        }
    }
    br.skip(3)?; // enable_superres, enable_cdef, enable_restoration

    // color_config()
    let high_bitdepth = br.read_bit()?;
    let twelve_bit = if seq_profile == 2 && high_bitdepth {
        br.read_bit()?
    } else {
        false
    };
    let monochrome = if seq_profile == 1 {
        false
    } else {
        br.read_bit()?
    };
    let color_desc_present = br.read_bit()?;
    let (pri, trc, mtrx) = if color_desc_present {
        (br.read_bits(8)?, br.read_bits(8)?, br.read_bits(8)?)
    } else {
        (2, 2, 2)
    };
    let (subsampling_x, subsampling_y, chroma_pos) = if monochrome {
        let _color_range = br.read_bit()?;
        (true, true, 0u8)
    } else if pri == 1 && trc == 13 && mtrx == 0 {
        (false, false, 0u8)
    } else {
        let _color_range = br.read_bit()?;
        let (sx, sy) = match seq_profile {
            0 => (true, true),
            1 => (false, false),
            _ => {
                if twelve_bit {
                    let sx = br.read_bit()?;
                    let sy = if sx { br.read_bit()? } else { false };
                    (sx, sy)
                } else {
                    (true, false)
                }
            }
        };
        let pos = if sx && sy { br.read_bits(2)? as u8 } else { 0 };
        (sx, sy, pos)
    };

    let b1 = (seq_profile << 5) | (seq_level_idx_0 & 0x1F);
    let b2 = ((seq_tier_0 & 1) << 7)
        | ((high_bitdepth as u8) << 6)
        | ((twelve_bit as u8) << 5)
        | ((monochrome as u8) << 4)
        | ((subsampling_x as u8) << 3)
        | ((subsampling_y as u8) << 2)
        | (chroma_pos & 0x03);
    Some(vec![0x81, b1, b2, 0x00])
}

/// 解像度とフレームレートから VideoToolbox H.264 リアルタイム変換の目標ビットレート（bps）を決める。
/// 一時的なローカル再生向けのため、画質劣化を抑えつつ転送量が増え過ぎない値にする
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn target_h264_bitrate(width: u32, height: u32, fps: f64) -> i32 {
    let pixels = (width as f64) * (height as f64);
    let fps = fps.clamp(15.0, 120.0);
    // 1080p 30fps（約 62M px/s）で ~12 Mbps、4K 60fps で ~45 Mbps になる係数
    let bps = pixels * fps * 0.19;
    bps.clamp(2_000_000.0, 60_000_000.0) as i32
}

/// デコード済み映像フレームの画素配置
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PixelLayout {
    I400,
    I420,
    I422,
    I444,
}

/// プレーン 1 枚への安全な参照（ストライドが負でも行番号から正しくスライスできる）
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) struct PlaneView<'a> {
    ptr: *const u8,
    stride: isize,
    row_bytes: usize,
    height: usize,
    _marker: std::marker::PhantomData<&'a [u8]>,
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
impl<'a> PlaneView<'a> {
    /// スライスから正ストライドの PlaneView を作る（テスト用にも使う）
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_slice(
        data: &'a [u8],
        stride: usize,
        row_bytes: usize,
        height: usize,
    ) -> Self {
        assert!(height == 0 || (height - 1) * stride + row_bytes <= data.len());
        Self {
            ptr: data.as_ptr(),
            stride: stride as isize,
            row_bytes,
            height,
            _marker: std::marker::PhantomData,
        }
    }

    /// 生ポインタ（rav1d の Dav1dPicture.data など）から PlaneView を作る
    #[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
    unsafe fn from_raw(ptr: *const u8, stride: isize, row_bytes: usize, height: usize) -> Self {
        Self {
            ptr,
            stride,
            row_bytes,
            height,
            _marker: std::marker::PhantomData,
        }
    }

    #[inline]
    pub(crate) fn row(&self, y: usize) -> &'a [u8] {
        debug_assert!(y < self.height);
        unsafe {
            let p = self.ptr.offset(y as isize * self.stride);
            std::slice::from_raw_parts(p, self.row_bytes)
        }
    }
}

/// rav1d 等がデコードした 1 フレーム（Y / U / V プレーン + 90 kHz PTS）
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) struct DecodedFrame<'a> {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) bpc: u8,
    pub(crate) layout: PixelLayout,
    pub(crate) y: PlaneView<'a>,
    pub(crate) u: Option<PlaneView<'a>>,
    pub(crate) v: Option<PlaneView<'a>>,
    pub(crate) pts: i64,
    pub(crate) color: Option<ColorInfo>,
}

/// VideoToolbox（またはテスト用モック）がエンコードした 1 フレーム
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct EncodedFrame {
    pub(crate) data: Vec<u8>,
    pub(crate) pts: i64,
    pub(crate) keyframe: bool,
}

/// H.264 エンコーダの抽象（macOS 本番は VtH264Encoder、Linux テストはモックを差し込む）
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) trait VideoEncoder: Send {
    fn encode(&mut self, frame: &DecodedFrame<'_>, force_keyframe: bool) -> Result<(), String>;
    fn flush(&mut self) -> Result<Vec<EncodedFrame>, String>;
    fn avcc(&self) -> Option<Vec<u8>>;
}

/// プレーナ YUV（8/10/12 bit、I420 / I422 / I444 / I400）を NV12（Y + UV インターリーブ）へ直接書き込む。
/// 8bit I420 を最速パスとして行単位コピー＆単純ループで処理し、中間バッファを確保しない
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn copy_frame_to_nv12(
    frame: &DecodedFrame<'_>,
    dst_w: usize,
    dst_h: usize,
    dst_y: &mut [u8],
    dst_y_stride: usize,
    dst_uv: &mut [u8],
    dst_uv_stride: usize,
) {
    let copy_w = frame.width.min(dst_w);
    let copy_h = frame.height.min(dst_h);
    let pad_y = if frame.color.is_some_and(|c| c.full_range) {
        0u8
    } else {
        16u8
    };

    // Y プレーン
    if frame.bpc <= 8 {
        for row in 0..copy_h {
            let src = frame.y.row(row);
            let dst = &mut dst_y[row * dst_y_stride..row * dst_y_stride + dst_w];
            dst[..copy_w].copy_from_slice(&src[..copy_w]);
            if copy_w < dst_w {
                dst[copy_w..].fill(pad_y);
            }
        }
    } else {
        let shift = (frame.bpc - 8) as u32;
        let half = 1u32 << (shift - 1);
        for row in 0..copy_h {
            let src = frame.y.row(row);
            let dst = &mut dst_y[row * dst_y_stride..row * dst_y_stride + dst_w];
            for (x, out) in dst[..copy_w].iter_mut().enumerate() {
                let raw = u16::from_le_bytes([src[x * 2], src[x * 2 + 1]]) as u32;
                *out = ((raw + half) >> shift).min(255) as u8;
            }
            if copy_w < dst_w {
                dst[copy_w..].fill(pad_y);
            }
        }
    }
    for row in copy_h..dst_h {
        dst_y[row * dst_y_stride..row * dst_y_stride + dst_w].fill(pad_y);
    }

    // UV プレーン（NV12: 4:2:0 で U, V を交互に並べる）
    let dst_uv_h = dst_h.div_ceil(2);
    let dst_uv_w = dst_w.div_ceil(2);
    let copy_uv_h = copy_h.div_ceil(2);
    let copy_uv_w = copy_w.div_ceil(2);

    let (Some(u_plane), Some(v_plane)) = (&frame.u, &frame.v) else {
        // I400（グレースケール）: 色差を無彩色（128）で埋める
        for row in 0..dst_uv_h {
            dst_uv[row * dst_uv_stride..row * dst_uv_stride + dst_uv_w * 2].fill(128);
        }
        return;
    };

    let (step_x, step_y) = match frame.layout {
        PixelLayout::I420 => (1usize, 1usize),
        PixelLayout::I422 => (1usize, 2usize),
        PixelLayout::I444 => (2usize, 2usize),
        PixelLayout::I400 => unreachable!(),
    };
    let max_src_uv_y = u_plane.height.saturating_sub(1);

    if frame.bpc <= 8 {
        if step_x == 1 && step_y == 1 {
            // 最も多い 8bit I420 の高速パス
            for row in 0..copy_uv_h {
                let sy = row.min(max_src_uv_y);
                let u_row = u_plane.row(sy);
                let v_row = v_plane.row(sy);
                let dst = &mut dst_uv[row * dst_uv_stride..row * dst_uv_stride + dst_uv_w * 2];
                let w = copy_uv_w.min(u_row.len()).min(v_row.len());
                for x in 0..w {
                    dst[x * 2] = u_row[x];
                    dst[x * 2 + 1] = v_row[x];
                }
                if w < dst_uv_w {
                    dst[w * 2..].fill(128);
                }
            }
        } else {
            for row in 0..copy_uv_h {
                let sy = (row * step_y).min(max_src_uv_y);
                let u_row = u_plane.row(sy);
                let v_row = v_plane.row(sy);
                let dst = &mut dst_uv[row * dst_uv_stride..row * dst_uv_stride + dst_uv_w * 2];
                let max_x = u_row.len().min(v_row.len()).saturating_sub(1);
                for x in 0..copy_uv_w {
                    let sx = (x * step_x).min(max_x);
                    dst[x * 2] = u_row[sx];
                    dst[x * 2 + 1] = v_row[sx];
                }
                if copy_uv_w < dst_uv_w {
                    dst[copy_uv_w * 2..].fill(128);
                }
            }
        }
    } else {
        let shift = (frame.bpc - 8) as u32;
        let half = 1u32 << (shift - 1);
        for row in 0..copy_uv_h {
            let sy = (row * step_y).min(max_src_uv_y);
            let u_row = u_plane.row(sy);
            let v_row = v_plane.row(sy);
            let dst = &mut dst_uv[row * dst_uv_stride..row * dst_uv_stride + dst_uv_w * 2];
            let max_x = (u_row.len().min(v_row.len()) / 2).saturating_sub(1);
            for x in 0..copy_uv_w {
                let sx = (x * step_x).min(max_x) * 2;
                let u_val = u16::from_le_bytes([u_row[sx], u_row[sx + 1]]) as u32;
                let v_val = u16::from_le_bytes([v_row[sx], v_row[sx + 1]]) as u32;
                dst[x * 2] = ((u_val + half) >> shift).min(255) as u8;
                dst[x * 2 + 1] = ((v_val + half) >> shift).min(255) as u8;
            }
            if copy_uv_w < dst_uv_w {
                dst[copy_uv_w * 2..].fill(128);
            }
        }
    }
    for row in copy_uv_h..dst_uv_h {
        dst_uv[row * dst_uv_stride..row * dst_uv_stride + dst_uv_w * 2].fill(128);
    }
}

// ---- rav1d ソフトウェアデコーダ & リアルタイム変換器（macOS 本番 + Linux テスト） ----

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
mod decoder {
    use std::ffi::c_int;
    use std::mem::MaybeUninit;
    use std::ptr::NonNull;

    use rav1d::Dav1dResult;
    use rav1d::include::dav1d::data::Dav1dData;
    use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dLogger, Dav1dSettings};
    use rav1d::include::dav1d::headers::{
        DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
        DAV1D_PIXEL_LAYOUT_I444,
    };
    use rav1d::include::dav1d::picture::Dav1dPicture;
    use rav1d::src::lib::{
        dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_flush,
        dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data,
    };

    use super::{ColorInfo, DecodedFrame, EncodedFrame, PixelLayout, PlaneView, VideoEncoder};

    /// 異常な解像度の壊れたファイルで OOM しないための上限（8K × 8K）
    const MAX_FRAME_PIXELS: u32 = 8192 * 8192;

    fn is_eagain(res: Dav1dResult) -> bool {
        res.0 < 0
            && std::io::Error::from_raw_os_error(-res.0).kind() == std::io::ErrorKind::WouldBlock
    }

    struct PicGuard(Dav1dPicture);

    impl Drop for PicGuard {
        fn drop(&mut self) {
            unsafe { dav1d_picture_unref(NonNull::new(&raw mut self.0)) };
        }
    }

    /// rav1d デコーダの安全な RAII ラッパー
    pub(crate) struct Av1Decoder {
        ctx: Option<Dav1dContext>,
    }

    // Dav1dContext（RawArc<Rav1dContext>）はスレッド間で移動可能で、
    // 呼び出し側（MkvStream）が Mutex で排他制御する
    unsafe impl Send for Av1Decoder {}

    impl Av1Decoder {
        pub(crate) fn new() -> Result<Self, String> {
            let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
            unsafe {
                dav1d_default_settings(NonNull::new_unchecked(settings.as_mut_ptr()));
            }
            let mut settings = unsafe { settings.assume_init() };
            // 負荷とメモリを抑えるためスレッド数は 2〜4、フレーム遅延は 2 に制限する
            // （M1 / M2 の性能コア 4 本で 1080p60〜4K を十分余裕をもって復号しつつ
            //   システム全体やバッテリーへの負担を軽く保つ）
            let threads =
                std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 4)) as c_int;
            settings.n_threads = threads;
            settings.max_frame_delay = 2;
            settings.frame_size_limit = MAX_FRAME_PIXELS;
            settings.apply_grain = 1;
            // 標準エラーへログを出さない
            settings.logger = unsafe { Dav1dLogger::new(None, None) };

            let mut ctx: Option<Dav1dContext> = None;
            let res =
                unsafe { dav1d_open(NonNull::new(&raw mut ctx), NonNull::new(&raw mut settings)) };
            if res.0 != 0 || ctx.is_none() {
                return Err(format!("AV1 デコーダを初期化できません (error {})", res.0));
            }
            Ok(Self { ctx })
        }

        /// AV1 パケット 1 つ（1 Temporal Unit）を投入し、出力可能なフレームをすべてコールバックへ渡す
        pub(crate) fn send_packet(
            &mut self,
            packet: &[u8],
            pts: i64,
            on_frame: &mut impl FnMut(&DecodedFrame<'_>) -> Result<(), String>,
        ) -> Result<(), String> {
            if packet.is_empty() {
                return Ok(());
            }
            let ctx = self.ctx.ok_or("AV1 デコーダは閉じています")?;
            let mut data = Dav1dData::default();
            let ptr = unsafe { dav1d_data_create(NonNull::new(&raw mut data), packet.len()) };
            if ptr.is_null() {
                return Err("AV1 パケット用のメモリを確保できません".into());
            }
            unsafe {
                std::ptr::copy_nonoverlapping(packet.as_ptr(), ptr, packet.len());
            }
            // 元の 90 kHz PTS をそのまま乗せ、出力ピクチャで取り戻す（VFR でも音ズレしない）
            data.m.timestamp = pts;

            loop {
                let res = unsafe { dav1d_send_data(Some(ctx), NonNull::new(&raw mut data)) };
                if res.0 == 0 {
                    break;
                }
                if is_eagain(res) {
                    // 内部キューが満杯なので 1 枚取り出してから再送する
                    self.pull_one(on_frame)?;
                    continue;
                }
                unsafe { dav1d_data_unref(NonNull::new(&raw mut data)) };
                // 壊れたパケットが 1 つ混ざっていても全体が即死しないよう読み飛ばす
                return Ok(());
            }
            // 通常時は 1 パケット投入ごとに 1 回だけ取り出す。
            // 連続して呼ぶと rav1d が drain モードに入りフレーム並列度が落ちるため
            let _ = self.pull_one(on_frame)?;
            Ok(())
        }

        /// デコード済みフレームを最大 1 枚取り出す。取り出せたら true、EAGAIN なら false
        fn pull_one(
            &mut self,
            on_frame: &mut impl FnMut(&DecodedFrame<'_>) -> Result<(), String>,
        ) -> Result<bool, String> {
            let ctx = self.ctx.ok_or("AV1 デコーダは閉じています")?;
            let mut guard = PicGuard(Dav1dPicture::default());
            let res = unsafe { dav1d_get_picture(Some(ctx), NonNull::new(&raw mut guard.0)) };
            if res.0 == 0 {
                Self::dispatch_picture(&guard.0, on_frame)?;
                return Ok(true);
            }
            if is_eagain(res) {
                return Ok(false);
            }
            // 単一フレームの復号エラーは読み飛ばして次へ進める
            Ok(false)
        }

        /// EOF 時にデコーダ内部の遅延フレームをすべて吐き出す
        pub(crate) fn drain(
            &mut self,
            on_frame: &mut impl FnMut(&DecodedFrame<'_>) -> Result<(), String>,
        ) -> Result<(), String> {
            // rav1d は直前の呼び出しが send_data のとき 1 回目の get_picture で drain モードに入り、
            // 2 回目以降で遅延フレームを返し始めるため、EAGAIN が 2 回連続するまで回す
            let mut consecutive_eagain = 0u8;
            while consecutive_eagain < 2 {
                if self.pull_one(on_frame)? {
                    consecutive_eagain = 0;
                } else {
                    consecutive_eagain += 1;
                }
            }
            Ok(())
        }

        /// シーク時にデコーダの内部状態をリセットする（スレッドを作り直さず軽量に復帰する）
        pub(crate) fn flush(&mut self) {
            if let Some(ctx) = self.ctx {
                unsafe { dav1d_flush(ctx) };
            }
        }

        fn dispatch_picture(
            pic: &Dav1dPicture,
            on_frame: &mut impl FnMut(&DecodedFrame<'_>) -> Result<(), String>,
        ) -> Result<(), String> {
            let width = pic.p.w.max(0) as usize;
            let height = pic.p.h.max(0) as usize;
            if width == 0 || height == 0 {
                return Ok(());
            }
            let bpc = pic.p.bpc.clamp(8, 12) as u8;
            let bytes_per_sample = if bpc > 8 { 2 } else { 1 };
            let layout = match pic.p.layout {
                DAV1D_PIXEL_LAYOUT_I400 => PixelLayout::I400,
                DAV1D_PIXEL_LAYOUT_I420 => PixelLayout::I420,
                DAV1D_PIXEL_LAYOUT_I422 => PixelLayout::I422,
                DAV1D_PIXEL_LAYOUT_I444 => PixelLayout::I444,
                _ => return Ok(()),
            };
            let Some(y_ptr) = pic.data[0] else {
                return Ok(());
            };
            let y = unsafe {
                PlaneView::from_raw(
                    y_ptr.as_ptr().cast::<u8>(),
                    pic.stride[0],
                    width * bytes_per_sample,
                    height,
                )
            };
            let (u, v) = if layout == PixelLayout::I400 {
                (None, None)
            } else {
                let (uv_w, uv_h) = match layout {
                    PixelLayout::I420 => (width.div_ceil(2), height.div_ceil(2)),
                    PixelLayout::I422 => (width.div_ceil(2), height),
                    PixelLayout::I444 => (width, height),
                    PixelLayout::I400 => unreachable!(),
                };
                let (Some(u_ptr), Some(v_ptr)) = (pic.data[1], pic.data[2]) else {
                    return Ok(());
                };
                unsafe {
                    (
                        Some(PlaneView::from_raw(
                            u_ptr.as_ptr().cast::<u8>(),
                            pic.stride[1],
                            uv_w * bytes_per_sample,
                            uv_h,
                        )),
                        Some(PlaneView::from_raw(
                            v_ptr.as_ptr().cast::<u8>(),
                            pic.stride[1],
                            uv_w * bytes_per_sample,
                            uv_h,
                        )),
                    )
                }
            };
            let color = pic.seq_hdr.map(|seq_ptr| {
                let seq = unsafe { seq_ptr.as_ref() };
                ColorInfo {
                    primaries: seq.pri as u16,
                    transfer: seq.trc as u16,
                    matrix: seq.mtrx as u16,
                    full_range: seq.color_range != 0,
                }
            });
            let frame = DecodedFrame {
                width,
                height,
                bpc,
                layout,
                y,
                u,
                v,
                pts: pic.m.timestamp,
                color,
            };
            on_frame(&frame)
        }
    }

    impl Drop for Av1Decoder {
        fn drop(&mut self) {
            if self.ctx.is_some() {
                unsafe { dav1d_close(NonNull::new(&raw mut self.ctx)) };
            }
        }
    }

    pub(crate) type EncoderFactory = Box<
        dyn Fn(u32, u32, f64, Option<ColorInfo>) -> Result<Box<dyn VideoEncoder>, String> + Send,
    >;

    /// AV1（rav1d）→ H.264（VideoToolbox / テスト用エンコーダ）のリアルタイム変換器。
    /// 最初のデコード済みフレームで実際の解像度と色空間を確定してからエンコーダを作る
    pub(crate) struct Av1Transcoder {
        decoder: Av1Decoder,
        encoder: Option<Box<dyn VideoEncoder>>,
        factory: EncoderFactory,
        fps: f64,
        encoded_size: Option<(u32, u32)>,
        encoded_color: Option<ColorInfo>,
        avcc: Option<Vec<u8>>,
        last_encoded_pts: Option<i64>,
        force_next_keyframe: bool,
    }

    impl Av1Transcoder {
        #[cfg(target_os = "macos")]
        pub(crate) fn new_videotoolbox(fps: f64) -> Result<Self, String> {
            Self::with_factory(
                fps,
                Box::new(|w, h, fps, color| {
                    Ok(Box::new(super::super::vt::VtH264Encoder::new(
                        w, h, fps, color,
                    )?))
                }),
            )
        }

        pub(crate) fn with_factory(fps: f64, factory: EncoderFactory) -> Result<Self, String> {
            Ok(Self {
                decoder: Av1Decoder::new()?,
                encoder: None,
                factory,
                fps: if fps.is_finite() && fps > 0.0 {
                    fps
                } else {
                    30.0
                },
                encoded_size: None,
                encoded_color: None,
                avcc: None,
                last_encoded_pts: None,
                force_next_keyframe: true,
            })
        }

        /// 次の断片の最初の出力フレームを必ずキーフレーム（IDR）にする
        pub(crate) fn mark_next_keyframe(&mut self) {
            self.force_next_keyframe = true;
        }

        /// シーク時にデコーダをリセットする（エンコーダセッションは維持し avcC を変えない）
        pub(crate) fn reset_for_seek(&mut self) {
            self.decoder.flush();
            if let Some(enc) = self.encoder.as_mut() {
                let _ = enc.flush();
            }
            self.last_encoded_pts = None;
            self.force_next_keyframe = true;
        }

        /// AV1 パケットを復号し、`skip_before_pts` 以降のフレームだけをエンコーダへ渡す。
        /// シーク時にキーフレームから目標時刻までの不要なエンコードを省くのに使う。
        /// 戻り値は今回エンコーダへ投入したフレーム数と、直近の投入 PTS
        pub(crate) fn push_packet(
            &mut self,
            packet: &[u8],
            pts: i64,
            skip_before_pts: Option<i64>,
        ) -> Result<(usize, Option<i64>), String> {
            let mut encoded_count = 0usize;
            let mut last_pts = None;
            let factory = &self.factory;
            let fps = self.fps;
            let encoder = &mut self.encoder;
            let encoded_size = &mut self.encoded_size;
            let encoded_color = &mut self.encoded_color;
            let last_encoded_pts = &mut self.last_encoded_pts;
            let force_next_keyframe = &mut self.force_next_keyframe;

            self.decoder.send_packet(packet, pts, &mut |frame| {
                if let Some(min_pts) = skip_before_pts
                    && frame.pts < min_pts
                {
                    return Ok(());
                }
                Self::encode_decoded(
                    frame,
                    factory,
                    fps,
                    encoder,
                    encoded_size,
                    encoded_color,
                    last_encoded_pts,
                    force_next_keyframe,
                )?;
                encoded_count += 1;
                last_pts = *last_encoded_pts;
                Ok(())
            })?;
            Ok((encoded_count, last_pts))
        }

        /// EOF 時にデコーダに残ったフレームをすべてエンコーダへ流す
        pub(crate) fn drain_decoder(
            &mut self,
            skip_before_pts: Option<i64>,
        ) -> Result<(usize, Option<i64>), String> {
            let mut encoded_count = 0usize;
            let mut last_pts = None;
            let factory = &self.factory;
            let fps = self.fps;
            let encoder = &mut self.encoder;
            let encoded_size = &mut self.encoded_size;
            let encoded_color = &mut self.encoded_color;
            let last_encoded_pts = &mut self.last_encoded_pts;
            let force_next_keyframe = &mut self.force_next_keyframe;

            self.decoder.drain(&mut |frame| {
                if let Some(min_pts) = skip_before_pts
                    && frame.pts < min_pts
                {
                    return Ok(());
                }
                Self::encode_decoded(
                    frame,
                    factory,
                    fps,
                    encoder,
                    encoded_size,
                    encoded_color,
                    last_encoded_pts,
                    force_next_keyframe,
                )?;
                encoded_count += 1;
                last_pts = *last_encoded_pts;
                Ok(())
            })?;
            Ok((encoded_count, last_pts))
        }

        #[allow(clippy::too_many_arguments)]
        fn encode_decoded(
            frame: &DecodedFrame<'_>,
            factory: &EncoderFactory,
            fps: f64,
            encoder: &mut Option<Box<dyn VideoEncoder>>,
            encoded_size: &mut Option<(u32, u32)>,
            encoded_color: &mut Option<ColorInfo>,
            last_encoded_pts: &mut Option<i64>,
            force_next_keyframe: &mut bool,
        ) -> Result<(), String> {
            if encoder.is_none() {
                let w = frame.width as u32;
                let h = frame.height as u32;
                let sdr_color = frame.color.and_then(ColorInfo::for_sdr_h264);
                *encoder = Some(factory(w, h, fps, sdr_color)?);
                *encoded_size = Some((w, h));
                *encoded_color = sdr_color;
            }
            // B フレームなしの fMP4 では PTS が厳密に単調増加である必要がある
            let pts = match *last_encoded_pts {
                Some(prev) if frame.pts <= prev => prev + 1,
                _ => frame.pts.max(0),
            };
            let adjusted = DecodedFrame {
                width: frame.width,
                height: frame.height,
                bpc: frame.bpc,
                layout: frame.layout,
                y: PlaneView {
                    ptr: frame.y.ptr,
                    stride: frame.y.stride,
                    row_bytes: frame.y.row_bytes,
                    height: frame.y.height,
                    _marker: std::marker::PhantomData,
                },
                u: frame.u.as_ref().map(|p| PlaneView {
                    ptr: p.ptr,
                    stride: p.stride,
                    row_bytes: p.row_bytes,
                    height: p.height,
                    _marker: std::marker::PhantomData,
                }),
                v: frame.v.as_ref().map(|p| PlaneView {
                    ptr: p.ptr,
                    stride: p.stride,
                    row_bytes: p.row_bytes,
                    height: p.height,
                    _marker: std::marker::PhantomData,
                }),
                pts,
                color: frame.color.and_then(ColorInfo::for_sdr_h264),
            };
            let force_kf = *force_next_keyframe;
            *force_next_keyframe = false;
            *last_encoded_pts = Some(pts);
            encoder.as_mut().unwrap().encode(&adjusted, force_kf)
        }

        /// エンコーダで現在完了しているフレームを回収する
        pub(crate) fn flush_encoder(&mut self) -> Result<Vec<EncodedFrame>, String> {
            let Some(enc) = self.encoder.as_mut() else {
                return Ok(Vec::new());
            };
            let frames = enc.flush()?;
            if self.avcc.is_none() {
                self.avcc = enc.avcc();
            }
            Ok(frames)
        }

        pub(crate) fn avcc(&self) -> Option<&[u8]> {
            self.avcc.as_deref()
        }

        pub(crate) fn encoded_size(&self) -> Option<(u32, u32)> {
            self.encoded_size
        }

        pub(crate) fn encoded_color(&self) -> Option<ColorInfo> {
            self.encoded_color
        }
    }
}

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
pub(crate) use decoder::{Av1Transcoder, EncoderFactory};
