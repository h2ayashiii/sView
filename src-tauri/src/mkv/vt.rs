// macOS 専用の VideoToolbox ラッパー。
// 1. VTIsHardwareDecodeSupported(AV1) で、この Mac が AV1 のハードウェアデコード（M3 以降）に
//    対応しているかを調べる。
// 2. 非対応の Mac（M1 / M2 など）向けに、rav1d でデコードしたフレームを VideoToolbox の
//    H.264 ハードウェアエンコーダ（B フレームなし・低遅延）でリアルタイムに圧縮し、
//    fMP4 用の AVCC サンプル列と avcC を返す。

use std::ffi::{c_int, c_void};
use std::ptr::{null, null_mut};
use std::sync::{Mutex, OnceLock};

use super::av1::{
    ColorInfo, DecodedFrame, EncodedFrame, VideoEncoder, build_avcc_from_sps_pps,
    copy_frame_to_nv12, target_h264_bitrate,
};
use super::remux::REMUX_VIDEO_TIMESCALE;

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFDataRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFArrayRef = *const c_void;
type CFIndex = isize;
type OSStatus = i32;
type CVReturn = i32;
type OSType = u32;
type CMVideoCodecType = u32;
type CVPixelBufferRef = *mut c_void;
type CVPixelBufferPoolRef = *mut c_void;
type CVOptionFlags = u64;
type CVAttachmentMode = u32;
type VTCompressionSessionRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type CMFormatDescriptionRef = *const c_void;
type CMBlockBufferRef = *mut c_void;
type VTEncodeInfoFlags = u32;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CF_NUMBER_SINT32_TYPE: CFIndex = 3;
const K_CF_NUMBER_FLOAT64_TYPE: CFIndex = 6;
const K_CV_ATTACHMENT_MODE_SHOULD_PROPAGATE: CVAttachmentMode = 1;
const K_CV_PIXEL_FORMAT_420_VIDEO: OSType = 0x3432_3076; // '420v' (NV12 limited range)
const K_CV_PIXEL_FORMAT_420_FULL: OSType = 0x3432_3066; // '420f' (NV12 full range)
const K_CM_VIDEO_CODEC_TYPE_H264: CMVideoCodecType = 0x6176_6331; // 'avc1'
const K_CM_VIDEO_CODEC_TYPE_AV1: CMVideoCodecType = 0x6176_3031; // 'av01'
const K_CM_TIME_FLAGS_VALID: u32 = 1;
const K_VT_ENCODE_INFO_FRAME_DROPPED: VTEncodeInfoFlags = 1 << 1;

#[repr(C)]
struct OpaqueCallbacks {
    _private: [u8; 0],
}

#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTime {
    fn new(value: i64, timescale: i32) -> Self {
        Self {
            value,
            timescale,
            flags: K_CM_TIME_FLAGS_VALID,
            epoch: 0,
        }
    }

    fn invalid() -> Self {
        Self {
            value: 0,
            timescale: 0,
            flags: 0,
            epoch: 0,
        }
    }
}

type VTCompressionOutputCallback = Option<
    unsafe extern "C" fn(
        output_callback_ref_con: *mut c_void,
        source_frame_ref_con: *mut c_void,
        status: OSStatus,
        info_flags: VTEncodeInfoFlags,
        sample_buffer: CMSampleBufferRef,
    ),
>;

#[allow(non_upper_case_globals)]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: OpaqueCallbacks;
    static kCFTypeDictionaryValueCallBacks: OpaqueCallbacks;

    fn CFRelease(cf: CFTypeRef);
    fn CFStringCreateWithBytes(
        alloc: CFTypeRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external_representation: u8,
    ) -> CFStringRef;
    fn CFNumberCreate(alloc: CFTypeRef, the_type: CFIndex, value_ptr: *const c_void)
    -> CFNumberRef;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        num_values: CFIndex,
        key_callbacks: *const OpaqueCallbacks,
        value_callbacks: *const OpaqueCallbacks,
    ) -> CFDictionaryRef;
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: CFIndex) -> CFTypeRef;
    fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
}

#[allow(non_upper_case_globals)]
#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;
    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;

    static kCVImageBufferColorPrimariesKey: CFStringRef;
    static kCVImageBufferColorPrimaries_ITU_R_709_2: CFStringRef;
    static kCVImageBufferColorPrimaries_EBU_3213: CFStringRef;
    static kCVImageBufferColorPrimaries_SMPTE_C: CFStringRef;
    static kCVImageBufferColorPrimaries_P3_D65: CFStringRef;
    static kCVImageBufferColorPrimaries_ITU_R_2020: CFStringRef;

    static kCVImageBufferTransferFunctionKey: CFStringRef;
    static kCVImageBufferTransferFunction_ITU_R_709_2: CFStringRef;
    static kCVImageBufferTransferFunction_sRGB: CFStringRef;
    static kCVImageBufferTransferFunction_ITU_R_2020: CFStringRef;

    static kCVImageBufferYCbCrMatrixKey: CFStringRef;
    static kCVImageBufferYCbCrMatrix_ITU_R_709_2: CFStringRef;
    static kCVImageBufferYCbCrMatrix_ITU_R_601_4: CFStringRef;
    static kCVImageBufferYCbCrMatrix_ITU_R_2020: CFStringRef;

    fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: CFTypeRef,
        pixel_buffer_pool: CVPixelBufferPoolRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> CVReturn;
    fn CVPixelBufferCreate(
        allocator: CFTypeRef,
        width: usize,
        height: usize,
        pixel_format_type: OSType,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> CVReturn;
    fn CVPixelBufferLockBaseAddress(
        pixel_buffer: CVPixelBufferRef,
        lock_flags: CVOptionFlags,
    ) -> CVReturn;
    fn CVPixelBufferUnlockBaseAddress(
        pixel_buffer: CVPixelBufferRef,
        unlock_flags: CVOptionFlags,
    ) -> CVReturn;
    fn CVPixelBufferGetBaseAddressOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> usize;
    fn CVBufferSetAttachment(
        buffer: CVPixelBufferRef,
        key: CFStringRef,
        value: CFTypeRef,
        attachment_mode: CVAttachmentMode,
    );
}

#[allow(non_upper_case_globals)]
#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    static kCMSampleAttachmentKey_NotSync: CFStringRef;
    static kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms: CFStringRef;

    fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMFormatDescriptionRef;
    fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;
    fn CMSampleBufferGetSampleAttachmentsArray(
        sbuf: CMSampleBufferRef,
        create_if_necessary: u8,
    ) -> CFArrayRef;
    fn CMBlockBufferGetDataLength(buf: CMBlockBufferRef) -> usize;
    fn CMBlockBufferCopyDataBytes(
        src: CMBlockBufferRef,
        offset: usize,
        data_length: usize,
        dst: *mut c_void,
    ) -> OSStatus;
    fn CMFormatDescriptionGetExtension(
        desc: CMFormatDescriptionRef,
        extension_key: CFStringRef,
    ) -> CFTypeRef;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        video_desc: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut c_int,
    ) -> OSStatus;
}

#[allow(non_upper_case_globals)]
#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    static kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder: CFStringRef;
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_AllowOpenGOP: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTProfileLevel_H264_High_AutoLevel: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_ColorPrimaries: CFStringRef;
    static kVTCompressionPropertyKey_TransferFunction: CFStringRef;
    static kVTCompressionPropertyKey_YCbCrMatrix: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;

    fn VTIsHardwareDecodeSupported(codec_type: CMVideoCodecType) -> u8;
    fn VTCompressionSessionCreate(
        allocator: CFTypeRef,
        width: i32,
        height: i32,
        codec_type: CMVideoCodecType,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFTypeRef,
        output_callback: VTCompressionOutputCallback,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;
    fn VTSessionSetProperty(
        session: VTCompressionSessionRef,
        property_key: CFStringRef,
        property_value: CFTypeRef,
    ) -> OSStatus;
    fn VTCompressionSessionPrepareToEncodeFrames(session: VTCompressionSessionRef) -> OSStatus;
    fn VTCompressionSessionGetPixelBufferPool(
        session: VTCompressionSessionRef,
    ) -> CVPixelBufferPoolRef;
    fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        presentation_time_stamp: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut VTEncodeInfoFlags,
    ) -> OSStatus;
    fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_presentation_time_stamp: CMTime,
    ) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);
}

/// この Mac が AV1 のハードウェアデコード（M3 以降など）に対応しているかを返す。
/// 結果はプロセス内で変わらないため初回だけ問い合わせてキャッシュする
pub(crate) fn hw_av1_decode_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED
        .get_or_init(|| unsafe { VTIsHardwareDecodeSupported(K_CM_VIDEO_CODEC_TYPE_AV1) != 0 })
}

/// 解放忘れを防ぐ CF オブジェクトのラッパー
struct CfOwned(CFTypeRef);

impl CfOwned {
    fn from_raw(raw: CFTypeRef) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }

    fn as_ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

fn cf_string(s: &str) -> Option<CfOwned> {
    let raw = unsafe {
        CFStringCreateWithBytes(
            null(),
            s.as_ptr(),
            s.len() as CFIndex,
            K_CF_STRING_ENCODING_UTF8,
            0,
        )
    };
    CfOwned::from_raw(raw)
}

fn cf_number_i32(value: i32) -> Option<CfOwned> {
    let raw = unsafe {
        CFNumberCreate(
            null(),
            K_CF_NUMBER_SINT32_TYPE,
            (&raw const value).cast::<c_void>(),
        )
    };
    CfOwned::from_raw(raw)
}

fn cf_number_f64(value: f64) -> Option<CfOwned> {
    let raw = unsafe {
        CFNumberCreate(
            null(),
            K_CF_NUMBER_FLOAT64_TYPE,
            (&raw const value).cast::<c_void>(),
        )
    };
    CfOwned::from_raw(raw)
}

fn cf_dict(entries: &[(CFTypeRef, CFTypeRef)]) -> Option<CfOwned> {
    let keys: Vec<CFTypeRef> = entries.iter().map(|(k, _)| *k).collect();
    let values: Vec<CFTypeRef> = entries.iter().map(|(_, v)| *v).collect();
    let raw = unsafe {
        CFDictionaryCreate(
            null(),
            keys.as_ptr(),
            values.as_ptr(),
            entries.len() as CFIndex,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    };
    CfOwned::from_raw(raw)
}

/// VideoToolbox の出力コールバックが書き込む共有状態
struct CallbackState {
    frames: Vec<EncodedFrame>,
    avcc: Option<Vec<u8>>,
    error: Option<String>,
    dropped_keyframe: bool,
}

/// フォーマット記述子から fMP4 用の avcC（AVCDecoderConfigurationRecord）を取り出す
unsafe fn extract_avcc(desc: CMFormatDescriptionRef) -> Option<Vec<u8>> {
    if desc.is_null() {
        return None;
    }
    // 1. SampleDescriptionExtensionAtoms["avcC"] から直接取り出す（最も正確）
    let atoms = unsafe {
        CMFormatDescriptionGetExtension(
            desc,
            kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms,
        )
    };
    if !atoms.is_null()
        && let Some(key) = cf_string("avcC")
    {
        let data = unsafe { CFDictionaryGetValue(atoms, key.as_ptr()) };
        if !data.is_null() {
            let len = unsafe { CFDataGetLength(data) };
            let ptr = unsafe { CFDataGetBytePtr(data) };
            if len > 0 && !ptr.is_null() {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                return Some(slice.to_vec());
            }
        }
    }
    // 2. フォールバック: SPS (0) と PPS (1) から組み立てる
    let (mut sps_ptr, mut sps_len) = (null(), 0usize);
    let (mut pps_ptr, mut pps_len) = (null(), 0usize);
    let s0 = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            desc,
            0,
            &raw mut sps_ptr,
            &raw mut sps_len,
            null_mut(),
            null_mut(),
        )
    };
    let s1 = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            desc,
            1,
            &raw mut pps_ptr,
            &raw mut pps_len,
            null_mut(),
            null_mut(),
        )
    };
    if s0 == 0 && s1 == 0 && !sps_ptr.is_null() && !pps_ptr.is_null() && sps_len > 0 && pps_len > 0
    {
        let sps = unsafe { std::slice::from_raw_parts(sps_ptr, sps_len) };
        let pps = unsafe { std::slice::from_raw_parts(pps_ptr, pps_len) };
        return build_avcc_from_sps_pps(sps, pps);
    }
    None
}

unsafe extern "C" fn encode_output_callback(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: CMSampleBufferRef,
) {
    if output_callback_ref_con.is_null() {
        return;
    }
    let state = unsafe { &*(output_callback_ref_con as *const Mutex<CallbackState>) };
    let Ok(mut guard) = state.lock() else {
        return;
    };
    if status != 0 {
        if guard.error.is_none() {
            guard.error = Some(format!(
                "VideoToolbox のエンコードに失敗しました (OSStatus {status})"
            ));
        }
        return;
    }
    if info_flags & K_VT_ENCODE_INFO_FRAME_DROPPED != 0 || sample_buffer.is_null() {
        if guard.frames.is_empty() {
            guard.dropped_keyframe = true;
        }
        return;
    }
    if guard.avcc.is_none() {
        let desc = unsafe { CMSampleBufferGetFormatDescription(sample_buffer) };
        if let Some(avcc) = unsafe { extract_avcc(desc) } {
            guard.avcc = Some(avcc);
        }
    }
    let block = unsafe { CMSampleBufferGetDataBuffer(sample_buffer) };
    if block.is_null() {
        return;
    }
    let len = unsafe { CMBlockBufferGetDataLength(block) };
    if len == 0 {
        return;
    }
    let mut data = vec![0u8; len];
    let copy_status =
        unsafe { CMBlockBufferCopyDataBytes(block, 0, len, data.as_mut_ptr().cast::<c_void>()) };
    if copy_status != 0 {
        if guard.error.is_none() {
            guard.error = Some(format!(
                "エンコード済みデータの取り出しに失敗しました (OSStatus {copy_status})"
            ));
        }
        return;
    }
    // キーフレーム判定（NotSync アタッチメントが無いか false ならキーフレーム）
    let mut keyframe = true;
    let attachments = unsafe { CMSampleBufferGetSampleAttachmentsArray(sample_buffer, 0) };
    if !attachments.is_null() && unsafe { CFArrayGetCount(attachments) } > 0 {
        let dict = unsafe { CFArrayGetValueAtIndex(attachments, 0) };
        if !dict.is_null() {
            let not_sync = unsafe { CFDictionaryGetValue(dict, kCMSampleAttachmentKey_NotSync) };
            if !not_sync.is_null() && not_sync == unsafe { kCFBooleanTrue } {
                keyframe = false;
            }
        }
    }
    // 元の 90 kHz PTS は sourceFrameRefCon に載せてあるため、丸め誤差なく取り戻せる
    let pts = source_frame_ref_con as isize as i64;
    guard.frames.push(EncodedFrame {
        data,
        pts,
        keyframe,
    });
}

/// CICP 値を VideoToolbox / CoreVideo の色空間定数に変換する（SDR H.264 向け）
fn cv_color_keys(
    color: ColorInfo,
) -> (
    Option<CFStringRef>,
    Option<CFStringRef>,
    Option<CFStringRef>,
) {
    unsafe {
        let primaries = match color.primaries {
            1 => Some(kCVImageBufferColorPrimaries_ITU_R_709_2),
            5 => Some(kCVImageBufferColorPrimaries_EBU_3213),
            6 => Some(kCVImageBufferColorPrimaries_SMPTE_C),
            9 => Some(kCVImageBufferColorPrimaries_ITU_R_2020),
            12 => Some(kCVImageBufferColorPrimaries_P3_D65),
            _ => None,
        };
        let transfer = match color.transfer {
            1 | 6 => Some(kCVImageBufferTransferFunction_ITU_R_709_2),
            13 => Some(kCVImageBufferTransferFunction_sRGB),
            14 | 15 => Some(kCVImageBufferTransferFunction_ITU_R_2020),
            _ => None,
        };
        let matrix = match color.matrix {
            1 => Some(kCVImageBufferYCbCrMatrix_ITU_R_709_2),
            5 | 6 => Some(kCVImageBufferYCbCrMatrix_ITU_R_601_4),
            9 => Some(kCVImageBufferYCbCrMatrix_ITU_R_2020),
            _ => None,
        };
        (primaries, transfer, matrix)
    }
}

/// VideoToolbox H.264 ハードウェアエンコーダ（B フレームなし・バースト高速変換向け）
pub(crate) struct VtH264Encoder {
    session: VTCompressionSessionRef,
    callback_state: *mut Mutex<CallbackState>,
    force_keyframe_props: CfOwned,
    width: u32,
    height: u32,
    pixel_format: OSType,
    color: Option<ColorInfo>,
    /// シークで大きく時刻が飛んでも VideoToolbox のレート制御へ滑らかな単調増加時刻を渡すための状態
    /// （本来の 90 kHz PTS は sourceFrameRefCon 経由でそのまま出力へ戻す）
    prev_input_pts: Option<i64>,
    synthetic_vt_pts: i64,
    has_encoded: bool,
    default_frame_ticks: i64,
}

// VTCompressionSessionRef はスレッドセーフな CF オブジェクトであり、
// 呼び出し側（MkvStream）も Mutex で直列化される
unsafe impl Send for VtH264Encoder {}

impl VtH264Encoder {
    pub(crate) fn new(
        width: u32,
        height: u32,
        fps: f64,
        color: Option<ColorInfo>,
    ) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("動画の解像度が不正です".into());
        }
        let full_range = color.is_some_and(|c| c.full_range);
        let pixel_format = if full_range {
            K_CV_PIXEL_FORMAT_420_FULL
        } else {
            K_CV_PIXEL_FORMAT_420_VIDEO
        };

        let enc_spec = unsafe {
            cf_dict(&[(
                kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
                kCFBooleanTrue,
            )])
        }
        .ok_or("エンコーダ設定を作れません")?;

        let cf_fmt = cf_number_i32(pixel_format as i32).ok_or("画素形式を作れません")?;
        let cf_w = cf_number_i32(width as i32).ok_or("幅を作れません")?;
        let cf_h = cf_number_i32(height as i32).ok_or("高さを作れません")?;
        let empty_dict = cf_dict(&[]).ok_or("IOSurface 設定を作れません")?;
        let src_attrs = unsafe {
            cf_dict(&[
                (kCVPixelBufferPixelFormatTypeKey, cf_fmt.as_ptr()),
                (kCVPixelBufferWidthKey, cf_w.as_ptr()),
                (kCVPixelBufferHeightKey, cf_h.as_ptr()),
                (kCVPixelBufferIOSurfacePropertiesKey, empty_dict.as_ptr()),
            ])
        }
        .ok_or("ピクセルバッファ設定を作れません")?;

        let force_keyframe_props =
            unsafe { cf_dict(&[(kVTEncodeFrameOptionKey_ForceKeyFrame, kCFBooleanTrue)]) }
                .ok_or("フレーム設定を作れません")?;

        let state = Box::new(Mutex::new(CallbackState {
            frames: Vec::new(),
            avcc: None,
            error: None,
            dropped_keyframe: false,
        }));
        let state_ptr = Box::into_raw(state);

        let mut session: VTCompressionSessionRef = null_mut();
        let status = unsafe {
            VTCompressionSessionCreate(
                null(),
                width as i32,
                height as i32,
                K_CM_VIDEO_CODEC_TYPE_H264,
                enc_spec.as_ptr(),
                src_attrs.as_ptr(),
                null(),
                Some(encode_output_callback),
                state_ptr.cast::<c_void>(),
                &raw mut session,
            )
        };
        if status != 0 || session.is_null() {
            unsafe { drop(Box::from_raw(state_ptr)) };
            return Err(format!(
                "VideoToolbox エンコーダを初期化できません (OSStatus {status})"
            ));
        }

        let default_frame_ticks = (REMUX_VIDEO_TIMESCALE as f64 / fps.clamp(1.0, 240.0))
            .round()
            .max(1.0) as i64;
        let encoder = Self {
            session,
            callback_state: state_ptr,
            force_keyframe_props,
            width,
            height,
            pixel_format,
            color,
            prev_input_pts: None,
            synthetic_vt_pts: 0,
            has_encoded: false,
            default_frame_ticks,
        };

        unsafe {
            // 断片単位のバースト変換向け設定:
            // - RealTime = false + PrioritizeEncodingSpeedOverQuality = true にすることで、
            //   実時間レートへの省電力スロットリングやフレームドロップを防ぎ、シーク直後も最大速度で圧縮する
            // - AllowFrameReordering = false で B フレームを無効化し、PTS == DTS を保証（音ズレ・遅延防止）
            // - AllowOpenGOP = false で断片境界が独立してデコードできるようにする
            let _ = VTSessionSetProperty(
                encoder.session,
                kVTCompressionPropertyKey_RealTime,
                kCFBooleanFalse,
            );
            let _ = VTSessionSetProperty(
                encoder.session,
                kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                kCFBooleanTrue,
            );
            let _ = VTSessionSetProperty(
                encoder.session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                kCFBooleanFalse,
            );
            let _ = VTSessionSetProperty(
                encoder.session,
                kVTCompressionPropertyKey_AllowOpenGOP,
                kCFBooleanFalse,
            );
            let _ = VTSessionSetProperty(
                encoder.session,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_High_AutoLevel,
            );
            let bitrate = target_h264_bitrate(width, height, fps);
            if let Some(v) = cf_number_i32(bitrate) {
                let _ = VTSessionSetProperty(
                    encoder.session,
                    kVTCompressionPropertyKey_AverageBitRate,
                    v.as_ptr(),
                );
            }
            if let Some(v) = cf_number_f64(2.0) {
                let _ = VTSessionSetProperty(
                    encoder.session,
                    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                    v.as_ptr(),
                );
            }
            if let Some(v) = cf_number_f64(fps.clamp(1.0, 240.0)) {
                let _ = VTSessionSetProperty(
                    encoder.session,
                    kVTCompressionPropertyKey_ExpectedFrameRate,
                    v.as_ptr(),
                );
            }
            if let Some(c) = color {
                let (pri, trc, mtrx) = cv_color_keys(c);
                if let Some(p) = pri {
                    let _ = VTSessionSetProperty(
                        encoder.session,
                        kVTCompressionPropertyKey_ColorPrimaries,
                        p,
                    );
                }
                if let Some(t) = trc {
                    let _ = VTSessionSetProperty(
                        encoder.session,
                        kVTCompressionPropertyKey_TransferFunction,
                        t,
                    );
                }
                if let Some(m) = mtrx {
                    let _ = VTSessionSetProperty(
                        encoder.session,
                        kVTCompressionPropertyKey_YCbCrMatrix,
                        m,
                    );
                }
            }
            let prep = VTCompressionSessionPrepareToEncodeFrames(encoder.session);
            if prep != 0 {
                return Err(format!(
                    "VideoToolbox エンコーダの準備に失敗しました (OSStatus {prep})"
                ));
            }
        }

        Ok(encoder)
    }

    fn alloc_pixel_buffer(&self) -> Result<CVPixelBufferRef, String> {
        let mut pb: CVPixelBufferRef = null_mut();
        let pool = unsafe { VTCompressionSessionGetPixelBufferPool(self.session) };
        let status = if !pool.is_null() {
            unsafe { CVPixelBufferPoolCreatePixelBuffer(null(), pool, &raw mut pb) }
        } else {
            let empty_dict = cf_dict(&[]).ok_or("IOSurface 設定を作れません")?;
            let attrs =
                unsafe { cf_dict(&[(kCVPixelBufferIOSurfacePropertiesKey, empty_dict.as_ptr())]) }
                    .ok_or("ピクセルバッファ設定を作れません")?;
            unsafe {
                CVPixelBufferCreate(
                    null(),
                    self.width as usize,
                    self.height as usize,
                    self.pixel_format,
                    attrs.as_ptr(),
                    &raw mut pb,
                )
            }
        };
        if status != 0 || pb.is_null() {
            return Err(format!(
                "ピクセルバッファを確保できません (CVReturn {status})"
            ));
        }
        Ok(pb)
    }
}

impl VideoEncoder for VtH264Encoder {
    fn encode(&mut self, frame: &DecodedFrame, force_keyframe: bool) -> Result<(), String> {
        let pb = self.alloc_pixel_buffer()?;
        let _pb_guard = CfOwned::from_raw(pb.cast::<c_void>());

        let lock_status = unsafe { CVPixelBufferLockBaseAddress(pb, 0) };
        if lock_status != 0 {
            return Err(format!(
                "ピクセルバッファをロックできません (CVReturn {lock_status})"
            ));
        }
        let copy_res = (|| -> Result<(), String> {
            let y_ptr = unsafe { CVPixelBufferGetBaseAddressOfPlane(pb, 0) };
            let uv_ptr = unsafe { CVPixelBufferGetBaseAddressOfPlane(pb, 1) };
            let y_stride = unsafe { CVPixelBufferGetBytesPerRowOfPlane(pb, 0) };
            let uv_stride = unsafe { CVPixelBufferGetBytesPerRowOfPlane(pb, 1) };
            if y_ptr.is_null() || uv_ptr.is_null() {
                return Err("ピクセルバッファのプレーンを取得できません".into());
            }
            let h = self.height as usize;
            let uv_h = h.div_ceil(2);
            let dst_y = unsafe { std::slice::from_raw_parts_mut(y_ptr, y_stride * h) };
            let dst_uv = unsafe { std::slice::from_raw_parts_mut(uv_ptr, uv_stride * uv_h) };
            copy_frame_to_nv12(
                frame,
                self.width as usize,
                h,
                dst_y,
                y_stride,
                dst_uv,
                uv_stride,
            );
            Ok(())
        })();
        unsafe { CVPixelBufferUnlockBaseAddress(pb, 0) };
        copy_res?;

        if let Some(c) = frame.color.or(self.color) {
            let (pri, trc, mtrx) = cv_color_keys(c);
            unsafe {
                if let Some(p) = pri {
                    CVBufferSetAttachment(
                        pb,
                        kCVImageBufferColorPrimariesKey,
                        p,
                        K_CV_ATTACHMENT_MODE_SHOULD_PROPAGATE,
                    );
                }
                if let Some(t) = trc {
                    CVBufferSetAttachment(
                        pb,
                        kCVImageBufferTransferFunctionKey,
                        t,
                        K_CV_ATTACHMENT_MODE_SHOULD_PROPAGATE,
                    );
                }
                if let Some(m) = mtrx {
                    CVBufferSetAttachment(
                        pb,
                        kCVImageBufferYCbCrMatrixKey,
                        m,
                        K_CV_ATTACHMENT_MODE_SHOULD_PROPAGATE,
                    );
                }
            }
        }

        // 前方・後方どちらのシークでもレート制御が大きく乱れないよう、
        // VideoToolbox へは滑らかな連続時刻を渡し、本来の PTS は sourceFrameRefCon で運ぶ
        if !self.has_encoded {
            self.synthetic_vt_pts = 0;
            self.has_encoded = true;
        } else {
            let delta = match self.prev_input_pts {
                Some(prev) => {
                    let d = frame.pts.saturating_sub(prev);
                    if (1..=REMUX_VIDEO_TIMESCALE as i64).contains(&d) {
                        d
                    } else {
                        self.default_frame_ticks
                    }
                }
                None => self.default_frame_ticks,
            };
            self.synthetic_vt_pts = self.synthetic_vt_pts.saturating_add(delta);
        }
        self.prev_input_pts = Some(frame.pts);

        let retry_kf = {
            let state = unsafe { &*self.callback_state };
            state
                .lock()
                .map(|mut g| std::mem::replace(&mut g.dropped_keyframe, false))
                .unwrap_or(false)
        };
        let pts = CMTime::new(self.synthetic_vt_pts, REMUX_VIDEO_TIMESCALE as i32);
        let dur = CMTime::new(self.default_frame_ticks, REMUX_VIDEO_TIMESCALE as i32);
        let props = if force_keyframe || retry_kf {
            self.force_keyframe_props.as_ptr()
        } else {
            null()
        };
        let status = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                pb,
                pts,
                dur,
                props,
                frame.pts as isize as *mut c_void,
                null_mut(),
            )
        };
        if status != 0 {
            return Err(format!(
                "VideoToolbox のエンコード投入に失敗しました (OSStatus {status})"
            ));
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<Vec<EncodedFrame>, String> {
        let status = unsafe { VTCompressionSessionCompleteFrames(self.session, CMTime::invalid()) };
        if status != 0 {
            return Err(format!(
                "VideoToolbox のエンコード完了待ちに失敗しました (OSStatus {status})"
            ));
        }
        let state = unsafe { &*self.callback_state };
        let mut guard = state
            .lock()
            .map_err(|_| "VideoToolbox の状態を取得できません".to_string())?;
        guard.dropped_keyframe = false;
        if let Some(err) = guard.error.take() {
            return Err(err);
        }
        Ok(std::mem::take(&mut guard.frames))
    }

    fn avcc(&self) -> Option<Vec<u8>> {
        let state = unsafe { &*self.callback_state };
        state.lock().ok().and_then(|g| g.avcc.clone())
    }

    fn on_seek(&mut self) {
        self.prev_input_pts = None;
    }
}

impl Drop for VtH264Encoder {
    fn drop(&mut self) {
        if !self.session.is_null() {
            // Invalidate は保留中のすべてのコールバックが終わってから返るため、
            // その後に callback_state を解放すればダングリングポインタにならない
            unsafe {
                VTCompressionSessionInvalidate(self.session);
                CFRelease(self.session.cast::<c_void>());
            }
            self.session = null_mut();
        }
        if !self.callback_state.is_null() {
            unsafe { drop(Box::from_raw(self.callback_state)) };
            self.callback_state = null_mut();
        }
    }
}
