//! AAC-ELD decoding with the FFmpeg that ships in the Flatpak runtime (LGPL, not bundled).
//!
//! libavcodec is opened with `dlopen` at runtime instead of being linked, so Kase
//! still starts (just without the AirPods mic) wherever FFmpeg or its AAC decoder is
//! missing, and the build needs no FFmpeg headers or bindgen.
//!
//! Without headers, the three structs this touches are read by offset. Only fields that
//! have kept their place since FFmpeg 4 are used (`AVCodecParameters` codec_type, codec_id,
//! extradata, extradata_size; `AVFrame` data, nb_samples, format), and the parameters are
//! read back through FFmpeg after being set, so a layout change is caught and the decoder
//! refuses to open instead of misbehaving. Packets are handed over with
//! `av_packet_from_data`, which needs no struct access at all.

use log::{debug, info};
use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::ptr;
use std::sync::OnceLock;

const AV_CODEC_ID_AAC: c_int = 0x15002;
const AVMEDIA_TYPE_AUDIO: c_int = 1;
const AV_LOG_QUIET: c_int = -8;
const PADDING: usize = 64;
const MAX_UNIT: usize = 1 << 12;
const MAX_SAMPLES: c_int = 8192;

// AVCodecParameters
const PAR_CODEC_TYPE: usize = 0;
const PAR_CODEC_ID: usize = 4;
const PAR_EXTRADATA: usize = 16;
const PAR_EXTRADATA_SIZE: usize = 24;
// AVFrame
const FRAME_DATA0: usize = 0;
const FRAME_NB_SAMPLES: usize = 112;
const FRAME_FORMAT: usize = 116;

// AVSampleFormat
const FMT_S16: c_int = 1;
const FMT_FLT: c_int = 3;
const FMT_S16P: c_int = 6;
const FMT_FLTP: c_int = 8;

/// libavcodec majors this was checked against: FFmpeg 4.x (58) to 8.x (62). 63 (FFmpeg 9)
/// is accepted ahead of time: the few AVFrame/AVCodecParameters fields read here have kept
/// their offsets since 58.
const SONAMES: [&str; 7] = [
    "libavcodec.so.63",
    "libavcodec.so.62",
    "libavcodec.so.61",
    "libavcodec.so.60",
    "libavcodec.so.59",
    "libavcodec.so.58",
    "libavcodec.so",
];
const SUPPORTED_MAJORS: std::ops::RangeInclusive<c_uint> = 58..=63;

type Ptr = *mut c_void;

struct Lib {
    version: c_uint,
    find_decoder: unsafe extern "C" fn(c_int) -> *const c_void,
    alloc_context3: unsafe extern "C" fn(*const c_void) -> Ptr,
    free_context: unsafe extern "C" fn(*mut Ptr),
    open2: unsafe extern "C" fn(Ptr, *const c_void, Ptr) -> c_int,
    parameters_alloc: unsafe extern "C" fn() -> Ptr,
    parameters_free: unsafe extern "C" fn(*mut Ptr),
    parameters_to_context: unsafe extern "C" fn(Ptr, *const c_void) -> c_int,
    parameters_from_context: unsafe extern "C" fn(Ptr, *const c_void) -> c_int,
    packet_alloc: unsafe extern "C" fn() -> Ptr,
    packet_free: unsafe extern "C" fn(*mut Ptr),
    packet_from_data: unsafe extern "C" fn(Ptr, *mut u8, c_int) -> c_int,
    packet_unref: unsafe extern "C" fn(Ptr),
    send_packet: unsafe extern "C" fn(Ptr, *const c_void) -> c_int,
    receive_frame: unsafe extern "C" fn(Ptr, Ptr) -> c_int,
    frame_alloc: unsafe extern "C" fn() -> Ptr,
    frame_free: unsafe extern "C" fn(*mut Ptr),
    malloc: unsafe extern "C" fn(usize) -> Ptr,
    mallocz: unsafe extern "C" fn(usize) -> Ptr,
    free: unsafe extern "C" fn(Ptr),
    opt_set_int: unsafe extern "C" fn(Ptr, *const c_char, i64, c_int) -> c_int,
}

// The function pointers are plain C entry points, callable from any thread.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

fn dl_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated string owned by libc.
    unsafe {
        let e = libc::dlerror();
        if e.is_null() {
            "unknown error".into()
        } else {
            CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    }
}

// Each symbol is cast to the type of the `Lib` field it fills, which states its C signature.
#[allow(clippy::missing_transmute_annotations)]
fn open_lib() -> Result<Lib, String> {
    let mut handle = ptr::null_mut();
    let mut errors = Vec::new();
    for name in SONAMES {
        let c = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
        // SAFETY: a valid C string; the handle is kept for the life of the process.
        handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if !handle.is_null() {
            debug!("[mic] loaded {name}");
            break;
        }
        errors.push(dl_error());
    }
    if handle.is_null() {
        return Err(format!(
            "FFmpeg's libavcodec was not found ({})",
            errors.join("; ")
        ));
    }
    macro_rules! sym {
        ($name:literal) => {{
            // SAFETY: the symbol is looked up in libavcodec and its dependencies (libavutil);
            // its C signature is the one declared in `Lib`.
            let p = unsafe { libc::dlsym(handle, concat!($name, "\0").as_ptr() as *const c_char) };
            if p.is_null() {
                return Err(format!("libavcodec has no {}", $name));
            }
            unsafe { std::mem::transmute::<*mut c_void, _>(p) }
        }};
    }
    let version_fn: unsafe extern "C" fn() -> c_uint = sym!("avcodec_version");
    // SAFETY: avcodec_version takes no arguments and has no preconditions.
    let version = unsafe { version_fn() };
    if !SUPPORTED_MAJORS.contains(&(version >> 16)) {
        let msg = format!(
            "libavcodec {}.{} is not supported by this build (it knows {} to {}); the AirPods \
             mic is unavailable until Kase is updated",
            version >> 16,
            (version >> 8) & 0xFF,
            SUPPORTED_MAJORS.start(),
            SUPPORTED_MAJORS.end()
        );
        log::warn!("[mic] {msg}");
        return Err(msg);
    }
    let set_log_level: unsafe extern "C" fn(c_int) = sym!("av_log_set_level");
    // SAFETY: plain setter. FFmpeg would otherwise print every damaged frame to stderr.
    unsafe { set_log_level(AV_LOG_QUIET) };
    Ok(Lib {
        version,
        find_decoder: sym!("avcodec_find_decoder"),
        alloc_context3: sym!("avcodec_alloc_context3"),
        free_context: sym!("avcodec_free_context"),
        open2: sym!("avcodec_open2"),
        parameters_alloc: sym!("avcodec_parameters_alloc"),
        parameters_free: sym!("avcodec_parameters_free"),
        parameters_to_context: sym!("avcodec_parameters_to_context"),
        parameters_from_context: sym!("avcodec_parameters_from_context"),
        packet_alloc: sym!("av_packet_alloc"),
        packet_free: sym!("av_packet_free"),
        packet_from_data: sym!("av_packet_from_data"),
        packet_unref: sym!("av_packet_unref"),
        send_packet: sym!("avcodec_send_packet"),
        receive_frame: sym!("avcodec_receive_frame"),
        frame_alloc: sym!("av_frame_alloc"),
        frame_free: sym!("av_frame_free"),
        malloc: sym!("av_malloc"),
        mallocz: sym!("av_mallocz"),
        free: sym!("av_free"),
        opt_set_int: sym!("av_opt_set_int"),
    })
}

fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(open_lib).as_ref().map_err(Clone::clone)
}

/// Reads a field of an FFmpeg struct by offset.
///
/// # Safety
/// `base` must point to a live struct at least `off + size_of::<T>()` bytes long.
unsafe fn field<T: Copy>(base: *const c_void, off: usize) -> T {
    unsafe { ptr::read_unaligned((base as *const u8).add(off) as *const T) }
}

/// # Safety
/// As for [`field`].
unsafe fn set_field<T>(base: Ptr, off: usize, value: T) {
    unsafe { ptr::write_unaligned((base as *mut u8).add(off) as *mut T, value) }
}

/// One open decoder. Not shared between threads; moved into the decode thread.
pub struct Decoder {
    lib: &'static Lib,
    ctx: Ptr,
    frame: Ptr,
    packet: Ptr,
}

// Owned exclusively by one thread at a time.
unsafe impl Send for Decoder {}

impl Decoder {
    /// Opens FFmpeg's AAC decoder for the given AudioSpecificConfig.
    /// Opens a decoder for `asc`, whose sample rate is `rate` (FFmpeg's AAC decoder fails
    /// every AAC-ELD frame with "internal bug" unless the context knows the rate).
    pub fn open(asc: &[u8], rate: u32) -> Result<Decoder, String> {
        let lib = lib()?;
        // SAFETY: FFmpeg calls with the documented contracts. Every pointer is checked, and
        // `Decoder` frees whatever was allocated (Drop) on every early return.
        unsafe {
            let codec = (lib.find_decoder)(AV_CODEC_ID_AAC);
            if codec.is_null() {
                return Err("this FFmpeg has no AAC decoder".into());
            }
            let mut d = Decoder {
                lib,
                ctx: (lib.alloc_context3)(codec),
                frame: (lib.frame_alloc)(),
                packet: (lib.packet_alloc)(),
            };
            if d.ctx.is_null() || d.frame.is_null() || d.packet.is_null() {
                return Err("FFmpeg ran out of memory".into());
            }
            d.configure(asc)?;
            if (lib.opt_set_int)(d.ctx, c"ar".as_ptr(), i64::from(rate), 0) < 0 {
                return Err("FFmpeg refused the sample rate".into());
            }
            let rc = (lib.open2)(d.ctx, codec, ptr::null_mut());
            if rc < 0 {
                return Err(format!("FFmpeg could not open the AAC decoder ({rc})"));
            }
            info!(
                "[mic] AAC decoder ready (libavcodec {}.{})",
                lib.version >> 16,
                (lib.version >> 8) & 0xFF
            );
            Ok(d)
        }
    }

    /// Hands the AudioSpecificConfig to the codec context through AVCodecParameters,
    /// then reads it back to confirm the field offsets match this FFmpeg.
    unsafe fn configure(&mut self, asc: &[u8]) -> Result<(), String> {
        let lib = self.lib;
        unsafe {
            let mut par = (lib.parameters_alloc)();
            let mut check = (lib.parameters_alloc)();
            let result = (|| {
                if par.is_null() || check.is_null() {
                    return Err("FFmpeg ran out of memory".to_string());
                }
                let extradata = (lib.mallocz)(asc.len() + PADDING) as *mut u8;
                if extradata.is_null() {
                    return Err("FFmpeg ran out of memory".to_string());
                }
                ptr::copy_nonoverlapping(asc.as_ptr(), extradata, asc.len());
                // Freed with `par` by avcodec_parameters_free.
                set_field(par, PAR_CODEC_TYPE, AVMEDIA_TYPE_AUDIO);
                set_field(par, PAR_CODEC_ID, AV_CODEC_ID_AAC);
                set_field(par, PAR_EXTRADATA, extradata);
                set_field(par, PAR_EXTRADATA_SIZE, asc.len() as c_int);
                if (lib.parameters_to_context)(self.ctx, par) < 0
                    || (lib.parameters_from_context)(check, self.ctx) < 0
                {
                    return Err("FFmpeg refused the stream parameters".to_string());
                }
                let id: c_int = field(check, PAR_CODEC_ID);
                let size: c_int = field(check, PAR_EXTRADATA_SIZE);
                let data: *const u8 = field(check, PAR_EXTRADATA);
                let same = id == AV_CODEC_ID_AAC
                    && size as usize == asc.len()
                    && !data.is_null()
                    && std::slice::from_raw_parts(data, asc.len()) == asc;
                if !same {
                    return Err("this FFmpeg's structures differ from what Kase expects".into());
                }
                Ok(())
            })();
            (lib.parameters_free)(&mut par);
            (lib.parameters_free)(&mut check);
            result
        }
    }

    /// Decodes one access unit and appends its samples (mono, -1.0..=1.0) to `out`.
    /// Returns the number of samples added; an error leaves `out` unchanged.
    pub fn decode(&mut self, unit: &[u8], out: &mut Vec<f32>) -> Result<usize, String> {
        if unit.is_empty() || unit.len() > MAX_UNIT {
            return Err(format!("bad access unit length {}", unit.len()));
        }
        let lib = self.lib;
        // SAFETY: the buffer is av_malloc'd with zeroed padding as av_packet_from_data
        // requires, and owned by the packet afterwards. Frame fields are read only after
        // avcodec_receive_frame succeeded, within the frame's own buffers.
        unsafe {
            let buf = (lib.malloc)(unit.len() + PADDING) as *mut u8;
            if buf.is_null() {
                return Err("FFmpeg ran out of memory".into());
            }
            ptr::copy_nonoverlapping(unit.as_ptr(), buf, unit.len());
            ptr::write_bytes(buf.add(unit.len()), 0, PADDING);
            if (lib.packet_from_data)(self.packet, buf, unit.len() as c_int) < 0 {
                (lib.free)(buf as Ptr);
                return Err("FFmpeg could not wrap the packet".into());
            }
            let sent = (lib.send_packet)(self.ctx, self.packet);
            (lib.packet_unref)(self.packet);
            if sent < 0 {
                return Err(format!("decode error {sent}"));
            }
            let rc = (lib.receive_frame)(self.ctx, self.frame);
            if rc < 0 {
                return Err(format!("decode error {rc}"));
            }
            let n: c_int = field(self.frame, FRAME_NB_SAMPLES);
            let format: c_int = field(self.frame, FRAME_FORMAT);
            let data: *const u8 = field(self.frame, FRAME_DATA0);
            if !(1..=MAX_SAMPLES).contains(&n) || data.is_null() {
                return Err(format!("unexpected frame of {n} samples"));
            }
            let n = n as usize;
            // Mono: the first plane (or the interleaved buffer) holds every sample.
            match format {
                FMT_FLTP | FMT_FLT => {
                    out.extend_from_slice(std::slice::from_raw_parts(data as *const f32, n))
                }
                FMT_S16P | FMT_S16 => out.extend(
                    std::slice::from_raw_parts(data as *const i16, n)
                        .iter()
                        .map(|&s| s as f32 / 32768.0),
                ),
                other => return Err(format!("unexpected sample format {other}")),
            }
            Ok(n)
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: each free function accepts a pointer to a possibly-null pointer.
        unsafe {
            (self.lib.frame_free)(&mut self.frame);
            (self.lib.packet_free)(&mut self.packet);
            (self.lib.free_context)(&mut self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twelve AAC-LC frames (48 kHz mono, a 1 kHz tone), made with FFmpeg's encoder.
    /// AAC-ELD goes through the same decoder; only the AudioSpecificConfig differs.
    const LC_ASC: [u8; 2] = [0x11, 0x88];
    const LC_FRAMES: &[u8] = include_bytes!("testdata/aac_lc_tone.bin");

    /// Forty AAC-ELD frames (48 kHz mono, 512 samples, a 440 Hz tone), made with fdkaac.
    /// The AirPods send 480-sample ELD frames; the decoder setup is the same.
    const ELD_TEST_ASC: [u8; 4] = [0xF8, 0xE6, 0x20, 0x00];
    const ELD_FRAMES: &[u8] = include_bytes!("testdata/aac_eld_tone.bin");

    fn frames() -> Vec<&'static [u8]> {
        split(LC_FRAMES)
    }

    fn split(mut rest: &'static [u8]) -> Vec<&'static [u8]> {
        // Each frame is stored as [len: u16 LE][bytes].
        let mut out = Vec::new();
        while rest.len() >= 2 {
            let n = u16::from_le_bytes([rest[0], rest[1]]) as usize;
            out.push(&rest[2..2 + n]);
            rest = &rest[2 + n..];
        }
        out
    }

    #[test]
    fn decodes_aac_frames_when_ffmpeg_is_installed() {
        let mut d = match Decoder::open(&LC_ASC, 48_000) {
            Ok(d) => d,
            Err(e) => {
                // CI installs FFmpeg; a desktop without it just skips this.
                eprintln!("skipping: {e}");
                return;
            }
        };
        let mut pcm = Vec::new();
        let mut decoded = 0;
        for f in frames() {
            if d.decode(f, &mut pcm).is_ok() {
                decoded += 1;
            }
        }
        assert!(decoded >= 10, "only {decoded} frames decoded");
        assert_eq!(pcm.len(), decoded * 1024);
        let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1 && peak <= 1.0, "peak {peak}");
        // The ELD config of the AirPods stream is accepted too.
        assert!(Decoder::open(&super::super::packets::ELD_ASC, 48_000).is_ok());
    }

    #[test]
    fn decodes_aac_eld_frames_when_ffmpeg_is_installed() {
        // FFmpeg failed every ELD frame ("internal bug") until the context had a rate.
        let Ok(mut d) = Decoder::open(&ELD_TEST_ASC, 48_000) else {
            return;
        };
        let mut pcm = Vec::new();
        let decoded = split(ELD_FRAMES)
            .into_iter()
            .filter(|f| d.decode(f, &mut pcm).is_ok())
            .count();
        assert_eq!(decoded, 40);
        assert_eq!(pcm.len(), 40 * 512);
        let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1 && peak <= 1.0, "peak {peak}");
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        let Ok(mut d) = Decoder::open(&LC_ASC, 48_000) else {
            return;
        };
        let mut pcm = Vec::new();
        assert!(d.decode(&[], &mut pcm).is_err());
        assert!(d.decode(&vec![0u8; MAX_UNIT + 1], &mut pcm).is_err());
        for seed in 0..50u32 {
            let junk: Vec<u8> = (0..64).map(|i| (i * 31 + seed * 7) as u8).collect();
            let _ = d.decode(&junk, &mut pcm);
        }
    }
}
