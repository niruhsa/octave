//! PCM decode for the feature extractor (Phase 12).
//!
//! Decodes an audio file to a single mono `f32` channel at its native sample
//! rate via Symphonia's **decoder** (unlike [`crate::services::duration`], which
//! only walks packets for a frame count). The DSP extractor then resamples +
//! frames this.
//!
//! ⚠️ **MP3 is unsupported** here: the project intentionally omits
//! `symphonia-bundle-mp3` (never published past alpha — see `duration.rs`), so
//! MP3 can't be decoded to samples. [`decode_mono`] returns
//! [`DecodeError::UnsupportedCodec`] for MP3 (and any other codec the build
//! can't decode); the analysis pass treats that as *skipped*, not failed, and
//! the track simply has no embedding (radio falls back behaviorally for it).

use std::path::Path;

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Why a decode produced no samples.
#[derive(Debug)]
pub enum DecodeError {
    /// The build can't decode this codec (MP3, or anything not in the enabled
    /// Symphonia feature set). Callers treat this as "skip", not "fail".
    UnsupportedCodec,
    /// The file couldn't be opened / probed / decoded (corrupt, truncated, …).
    Decode(String),
    /// Decoded fine but yielded no usable audio (empty / silent-zero-length).
    Empty,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::UnsupportedCodec => write!(f, "unsupported codec (not decodable)"),
            DecodeError::Decode(e) => write!(f, "decode error: {e}"),
            DecodeError::Empty => write!(f, "no audio frames decoded"),
        }
    }
}

/// Decoded mono audio: interleaved-downmixed `f32` samples + the source rate.
pub struct MonoPcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// File extensions Symphonia can't decode in this build (no MP3 bundle). Caught
/// early so we don't even open the file — and so the error is the clean
/// `UnsupportedCodec` (skip) rather than a probe failure.
const UNSUPPORTED_EXTS: &[&str] = &["mp3", "mp2", "mp1"];

/// Decode `path` to a single mono `f32` channel at the file's native rate.
pub fn decode_mono(path: &Path) -> Result<MonoPcm, DecodeError> {
    if let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        && UNSUPPORTED_EXTS.contains(&ext.as_str())
    {
        return Err(DecodeError::UnsupportedCodec);
    }

    let file = std::fs::File::open(path).map_err(|e| DecodeError::Decode(e.to_string()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| DecodeError::Decode(e.to_string()))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or(DecodeError::UnsupportedCodec)?;
    let track_id = track.id;
    let params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p,
        _ => return Err(DecodeError::UnsupportedCodec),
    };
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|_| DecodeError::UnsupportedCodec)?;

    let mut samples: Vec<f32> = Vec::new();
    let mut sample_rate: u32 = 0;
    let mut scratch: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            // A single bad packet shouldn't doom the whole track — stop the walk
            // and keep whatever we decoded so far.
            Err(_) => break,
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                if sample_rate == 0 {
                    sample_rate = buf.spec().rate();
                }
                downmix_into(&buf, &mut scratch, &mut samples);
            }
            // Recoverable per-packet decode hiccups: skip the packet.
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(DecodeError::Decode(e.to_string())),
        }
    }

    if samples.is_empty() || sample_rate == 0 {
        return Err(DecodeError::Empty);
    }
    Ok(MonoPcm {
        samples,
        sample_rate,
    })
}

/// Copy `buf` to interleaved f32 (via `scratch`), then average channels into a
/// mono sample appended onto `out`.
fn downmix_into(buf: &GenericAudioBufferRef<'_>, scratch: &mut Vec<f32>, out: &mut Vec<f32>) {
    let channels = buf.spec().channels().count().max(1);
    buf.copy_to_vec_interleaved::<f32>(scratch);
    if channels == 1 {
        out.extend_from_slice(scratch);
        return;
    }
    out.reserve(scratch.len() / channels);
    for frame in scratch.chunks_exact(channels) {
        let sum: f32 = frame.iter().copied().sum();
        out.push(sum / channels as f32);
    }
}
