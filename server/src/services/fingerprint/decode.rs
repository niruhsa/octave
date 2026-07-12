//! PCM decode for the feature extractor (Phase 12).
//!
//! Decodes an audio file to a single mono `f32` channel at its native sample
//! rate via Symphonia's **decoder** (unlike [`crate::services::duration`], which
//! only walks packets for a frame count). The DSP extractor then resamples +
//! frames this.
//!
//! Every codec the build enables decodes here, **including MP3** (the `mpa`
//! Symphonia feature pulls in `symphonia-bundle-mp3`). [`decode_mono`] only
//! returns [`DecodeError::UnsupportedCodec`] for a codec the build genuinely
//! can't decode; the analysis pass treats that as *skipped*, not failed, so the
//! track simply has no embedding (radio falls back behaviorally for it).

use std::path::Path;

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::loudness::{Loudness, LoudnessMeter};

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
    /// Peak sample as a linear amplitude — the max `|sample|` across the
    /// **original** channels (measured before the mono downmix, so it reflects
    /// the true per-channel peak). Drives clip-safe loudness gain. `0.0` for a
    /// silent decode.
    pub peak: f32,
}

/// Decode `path` to a single mono `f32` channel at the file's native rate.
pub fn decode_mono(path: &Path) -> Result<MonoPcm, DecodeError> {
    decode(path, false).map(|(pcm, _)| pcm)
}

/// Decode `path` to mono **and** measure EBU R128 integrated loudness from the
/// original interleaved multichannel signal in the same pass. `Ok((pcm, None))`
/// means the audio decoded but loudness was undefined (silence / below the gate)
/// — the caller stores no loudness but the PCM is still usable for the embedding.
pub fn decode_with_loudness(path: &Path) -> Result<(MonoPcm, Option<Loudness>), DecodeError> {
    decode(path, true)
}

/// Shared decode. `measure_loudness` streams each interleaved buffer into an
/// [`LoudnessMeter`] (before the mono downmix) so a single decode feeds both the
/// DSP embedding and the loudness measurement.
fn decode(path: &Path, measure_loudness: bool) -> Result<(MonoPcm, Option<Loudness>), DecodeError> {
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
    let mut peak: f32 = 0.0;
    // Loudness meter, built lazily on the first buffer (once channels+rate are
    // known). `meter_tried` stops a failed `new()` retrying every buffer.
    let mut meter: Option<LoudnessMeter> = None;
    let mut meter_tried = false;

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
                let channels = buf.spec().channels().count().max(1) as u32;
                // `scratch` holds this buffer's interleaved multichannel samples
                // after `downmix_into`; feed that to the meter (before the
                // downmix) so loudness reflects the true stereo/surround signal.
                downmix_into(&buf, &mut scratch, &mut samples, &mut peak);
                if measure_loudness {
                    if !meter_tried {
                        meter_tried = true;
                        meter = LoudnessMeter::new(channels, buf.spec().rate());
                    }
                    if let Some(m) = meter.as_mut()
                        && m.channels == channels
                    {
                        m.add_interleaved(&scratch);
                    }
                }
            }
            // Recoverable per-packet decode hiccups: skip the packet.
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => return Err(DecodeError::Decode(e.to_string())),
        }
    }

    if samples.is_empty() || sample_rate == 0 {
        return Err(DecodeError::Empty);
    }
    let loudness = meter.and_then(|m| m.finish(peak));
    Ok((
        MonoPcm {
            samples,
            sample_rate,
            peak,
        },
        loudness,
    ))
}

/// Copy `buf` to interleaved f32 (via `scratch`), then average channels into a
/// mono sample appended onto `out`. Also folds the max `|sample|` across the
/// original (pre-downmix) channels into `peak` for clip-safe loudness gain.
fn downmix_into(
    buf: &GenericAudioBufferRef<'_>,
    scratch: &mut Vec<f32>,
    out: &mut Vec<f32>,
    peak: &mut f32,
) {
    let channels = buf.spec().channels().count().max(1);
    buf.copy_to_vec_interleaved::<f32>(scratch);
    for &s in scratch.iter() {
        let a = s.abs();
        if a > *peak {
            *peak = a;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Write a minimal 16-bit PCM WAV so the real Symphonia decode path (not a
    /// fake) is exercised end-to-end.
    fn write_wav(path: &Path, samples: &[i16], rate: u32, channels: u16) {
        let bits = 16u16;
        let block_align = channels * (bits / 8);
        let byte_rate = rate * block_align as u32;
        let data_len = (samples.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes()); // PCM
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&byte_rate.to_le_bytes());
        b.extend_from_slice(&block_align.to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for &s in samples {
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn decode_with_loudness_measures_a_synthetic_wav() {
        let rate = 48_000u32;
        let n = rate * 2; // 2 s
        let pcm_i16: Vec<i16> = (0..n)
            .map(|i| {
                let v = (2.0 * PI * 1000.0 * i as f32 / rate as f32).sin() * 0.5;
                (v * i16::MAX as f32) as i16
            })
            .collect();
        let f = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
        write_wav(f.path(), &pcm_i16, rate, 1);

        let (pcm, loud) = decode_with_loudness(f.path()).expect("decode ok");
        assert_eq!(pcm.sample_rate, rate);
        assert!(!pcm.samples.is_empty());
        // ~0.5 amplitude sine → peak ≈ 0.5.
        assert!(pcm.peak > 0.4 && pcm.peak <= 1.0, "peak {}", pcm.peak);
        let l = loud.expect("loudness measured");
        assert!(
            l.lufs.is_finite() && l.lufs > -30.0 && l.lufs < 0.0,
            "lufs {}",
            l.lufs
        );
    }

    #[test]
    fn decode_mono_without_loudness_returns_none() {
        let rate = 44_100u32;
        let pcm_i16: Vec<i16> = (0..rate)
            .map(|i| ((2.0 * PI * 440.0 * i as f32 / rate as f32).sin() * 0.3 * i16::MAX as f32) as i16)
            .collect();
        let f = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
        write_wav(f.path(), &pcm_i16, rate, 1);
        // The plain `decode_mono` wrapper never measures loudness.
        let pcm = decode_mono(f.path()).expect("decode ok");
        assert_eq!(pcm.sample_rate, rate);
        assert!(pcm.peak > 0.2);
    }
}
