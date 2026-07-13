//! Feature extraction (Phase 12) — decode → fixed-length similarity embedding.
//!
//! [`FeatureExtractor`] is a trait so the *quality* layer can be swapped without
//! touching the analysis pipeline. The shipped [`DspExtractor`] computes a
//! classic DSP feature vector (MFCC + chroma + spectral + rhythm) — pure Rust,
//! no model file, no GPU. A learned ONNX encoder is an optional drop-in upgrade
//! (the `onnx` feature; see [`super::onnx`]); bumping the `model_version`
//! auto-re-analyzes existing rows.

use std::f32::consts::PI;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

use crate::error::{AppError, Result};

use super::decode::{DecodeError, MonoPcm, decode_mono};

/// Decode + embed an audio file into a fixed-length, unit-normalized vector
/// whose cosine distance approximates perceptual similarity.
#[async_trait]
pub trait FeatureExtractor: Send + Sync {
    /// Stable id stored as `model_version`; bumping it re-analyzes everything.
    fn model_version(&self) -> &str;
    /// Embedding length (every row this extractor writes has this `dims`).
    fn dims(&self) -> usize;
    /// Decode `path` and return a unit-normalized embedding. Returns
    /// [`AppError::InvalidArgument`] for a codec this build can't decode — the
    /// pass treats that as "skip", not "fail". (MP3, FLAC, AAC/ALAC, OGG/Vorbis,
    /// WAV/AIFF all decode.)
    async fn extract(&self, path: &Path) -> Result<Vec<f32>>;

    /// Embed from an already-decoded native-rate mono PCM buffer, if this
    /// extractor supports it — lets the analysis pass decode **once** and share
    /// that buffer with the loudness meter (Phase 16). `None` (the default) means
    /// the extractor needs its own decode (e.g. ONNX, which feeds the model a
    /// specific rate/layout), so the pass falls back to [`Self::extract`].
    async fn embed_from_pcm(&self, _pcm: MonoPcm) -> Option<Result<Vec<f32>>> {
        None
    }
}

// ---------------------------------------------------------------------------
// DSP extractor (model_version = "dsp-v1")
// ---------------------------------------------------------------------------

/// Sample rate every file is resampled to before framing (a fixed rate makes
/// the frequency→feature mapping consistent across files).
const TARGET_RATE: u32 = 22_050;
/// Analysis window size (≈93 ms at 22.05 kHz) and hop (50 % overlap).
const FRAME: usize = 2048;
const HOP: usize = 1024;
const N_MELS: usize = 26;
const N_MFCC: usize = 13;
const N_CHROMA: usize = 12;

/// Concatenated embedding length — see the layout in [`aggregate`].
/// MFCC(13·2) + chroma(12·2) + spectral(3·2) + rms(2) + tempo(1) = 59.
const DSP_DIMS: usize = N_MFCC * 2 + N_CHROMA * 2 + 3 * 2 + 2 + 1;

/// The shipped phase-1 extractor: a hand-rolled DSP feature vector.
#[derive(Clone, Default)]
pub struct DspExtractor;

impl DspExtractor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FeatureExtractor for DspExtractor {
    fn model_version(&self) -> &str {
        "dsp-v1"
    }
    fn dims(&self) -> usize {
        DSP_DIMS
    }
    async fn extract(&self, path: &Path) -> Result<Vec<f32>> {
        let path: PathBuf = path.to_path_buf();
        // Decode + DSP is CPU-bound — keep it off the async runtime.
        tokio::task::spawn_blocking(move || extract_blocking(&path))
            .await
            .map_err(|e| AppError::Internal(format!("extract task join: {e}")))?
    }

    async fn embed_from_pcm(&self, pcm: MonoPcm) -> Option<Result<Vec<f32>>> {
        // Reuse the pass's already-decoded PCM — DSP-only, no decode. Still
        // CPU-bound (resample + FFT), so keep it off the async runtime.
        let res = tokio::task::spawn_blocking(move || embed_mono(&pcm))
            .await
            .unwrap_or_else(|e| Err(AppError::Internal(format!("embed task join: {e}"))));
        Some(res)
    }
}

/// Synchronous decode + feature pipeline (run on a blocking thread).
fn extract_blocking(path: &Path) -> Result<Vec<f32>> {
    let pcm = decode_mono(path).map_err(|e| match e {
        // Map "can't decode this codec" to InvalidArgument so the pass can tell
        // a skip apart from a real failure.
        DecodeError::UnsupportedCodec => AppError::InvalidArgument(format!("unanalyzable: {}", e)),
        other => AppError::Internal(format!("decode {}: {other}", path.display())),
    })?;
    embed_mono(&pcm)
}

/// The DSP feature pipeline over already-decoded mono PCM (resample → frame →
/// aggregate → L2-normalize). Split out of [`extract_blocking`] so the pass can
/// feed it the same PCM the loudness meter measured (see `embed_from_pcm`).
pub(super) fn embed_mono(pcm: &MonoPcm) -> Result<Vec<f32>> {
    let mono = resample(&pcm.samples, pcm.sample_rate, TARGET_RATE);
    if mono.len() < FRAME {
        return Err(AppError::InvalidArgument(
            "track too short to analyze".into(),
        ));
    }

    let frames = frame_features(&mono);
    if frames.is_empty() {
        return Err(AppError::InvalidArgument("no analyzable frames".into()));
    }
    let mut embedding = aggregate(&frames);
    l2_normalize(&mut embedding);
    debug_assert_eq!(embedding.len(), DSP_DIMS);
    Ok(embedding)
}

/// Per-frame feature row (in the order they're aggregated).
struct FrameRow {
    mfcc: [f32; N_MFCC],
    chroma: [f32; N_CHROMA],
    centroid: f32,
    rolloff: f32,
    flatness: f32,
    rms: f32,
    /// Spectral flux vs. the previous frame (onset strength; drives tempo).
    flux: f32,
}

/// Linear-interpolation resample `input` from `from` Hz to `to` Hz. Cheap and
/// deterministic; quality is plenty for similarity features.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let idx = src.floor() as usize;
        let frac = (src - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Walk `mono` frame by frame, producing one [`FrameRow`] per hop.
fn frame_features(mono: &[f32]) -> Vec<FrameRow> {
    let window = hann(FRAME);
    let mel = MelFilterbank::new(TARGET_RATE, FRAME, N_MELS);
    let dct = dct_matrix(N_MELS, N_MFCC);
    let chroma_map = chroma_bins(TARGET_RATE, FRAME);

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FRAME);

    let n_bins = FRAME / 2 + 1;
    let mut rows = Vec::new();
    let mut prev_mag: Option<Vec<f32>> = None;

    let mut pos = 0;
    while pos + FRAME <= mono.len() {
        let frame = &mono[pos..pos + FRAME];
        pos += HOP;

        // RMS from the raw (un-windowed) frame.
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();

        // Windowed FFT → magnitude spectrum.
        let mut buf: Vec<Complex<f32>> = frame
            .iter()
            .zip(&window)
            .map(|(s, w)| Complex::new(s * w, 0.0))
            .collect();
        fft.process(&mut buf);
        let mag: Vec<f32> = buf[..n_bins].iter().map(|c| c.norm()).collect();

        // Mel → log → DCT → MFCC.
        let mel_energies = mel.apply(&mag);
        let log_mel: Vec<f32> = mel_energies.iter().map(|e| (e + 1e-10).ln()).collect();
        let mut mfcc = [0.0f32; N_MFCC];
        for (k, m) in mfcc.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (j, &lm) in log_mel.iter().enumerate() {
                acc += lm * dct[k * N_MELS + j];
            }
            *m = acc;
        }

        // Chroma (12 pitch classes).
        let mut chroma = [0.0f32; N_CHROMA];
        for (bin, &m) in mag.iter().enumerate() {
            if let Some(pc) = chroma_map[bin] {
                chroma[pc] += m;
            }
        }
        normalize_in_place(&mut chroma);

        // Spectral shape.
        let centroid = spectral_centroid(&mag);
        let rolloff = spectral_rolloff(&mag, 0.85);
        let flatness = spectral_flatness(&mag);

        // Spectral flux (positive change vs. previous frame).
        let flux = match &prev_mag {
            Some(prev) => mag
                .iter()
                .zip(prev)
                .map(|(c, p)| (c - p).max(0.0))
                .sum::<f32>(),
            None => 0.0,
        };
        prev_mag = Some(mag);

        rows.push(FrameRow {
            mfcc,
            chroma,
            centroid,
            rolloff,
            flatness,
            rms,
            flux,
        });
    }
    rows
}

/// Aggregate per-frame rows into the final embedding: mean+variance of each
/// feature, plus a single tempo estimate from the onset (flux) envelope.
fn aggregate(rows: &[FrameRow]) -> Vec<f32> {
    let n = rows.len() as f32;
    let mut out = Vec::with_capacity(DSP_DIMS);

    // Helper: push mean+variance of a per-frame scalar.
    let push_stats = |vals: &[f32], out: &mut Vec<f32>| {
        let mean = vals.iter().sum::<f32>() / n;
        let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
        out.push(mean);
        out.push(var);
    };

    // MFCC mean+var per coefficient.
    for k in 0..N_MFCC {
        let col: Vec<f32> = rows.iter().map(|r| r.mfcc[k]).collect();
        push_stats(&col, &mut out);
    }
    // Chroma mean+var per pitch class.
    for k in 0..N_CHROMA {
        let col: Vec<f32> = rows.iter().map(|r| r.chroma[k]).collect();
        push_stats(&col, &mut out);
    }
    // Spectral shape mean+var.
    push_stats(
        &rows.iter().map(|r| r.centroid).collect::<Vec<_>>(),
        &mut out,
    );
    push_stats(
        &rows.iter().map(|r| r.rolloff).collect::<Vec<_>>(),
        &mut out,
    );
    push_stats(
        &rows.iter().map(|r| r.flatness).collect::<Vec<_>>(),
        &mut out,
    );
    // RMS energy mean+var.
    push_stats(&rows.iter().map(|r| r.rms).collect::<Vec<_>>(), &mut out);
    // Tempo (single value, normalized).
    let flux: Vec<f32> = rows.iter().map(|r| r.flux).collect();
    out.push(estimate_tempo_norm(&flux));

    out
}

/// Estimate tempo from the onset envelope by autocorrelation, normalized to a
/// 0..1 feature (BPM / 250). Returns 0 when no clear period is found.
fn estimate_tempo_norm(flux: &[f32]) -> f32 {
    if flux.len() < 16 {
        return 0.0;
    }
    let fps = TARGET_RATE as f32 / HOP as f32;
    // Plausible tempo band: 50–200 BPM → lag range in frames.
    let min_lag = (fps * 60.0 / 200.0).floor().max(1.0) as usize;
    let max_lag = (fps * 60.0 / 50.0).ceil().min(flux.len() as f32 / 2.0) as usize;
    if max_lag <= min_lag {
        return 0.0;
    }
    // Mean-remove for a cleaner autocorrelation.
    let mean = flux.iter().sum::<f32>() / flux.len() as f32;
    let centered: Vec<f32> = flux.iter().map(|f| f - mean).collect();

    let mut best_lag = 0usize;
    let mut best = f32::MIN;
    for lag in min_lag..=max_lag {
        let mut acc = 0.0;
        for i in lag..centered.len() {
            acc += centered[i] * centered[i - lag];
        }
        if acc > best {
            best = acc;
            best_lag = lag;
        }
    }
    if best_lag == 0 || best <= 0.0 {
        return 0.0;
    }
    let bpm = 60.0 * fps / best_lag as f32;
    (bpm / 250.0).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// DSP primitives
// ---------------------------------------------------------------------------

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / (n as f32 - 1.0)).cos())
        .collect()
}

fn spectral_centroid(mag: &[f32]) -> f32 {
    let total: f32 = mag.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    let weighted: f32 = mag.iter().enumerate().map(|(i, m)| i as f32 * m).sum();
    weighted / total / mag.len() as f32 // normalized 0..~1 (bin fraction)
}

fn spectral_rolloff(mag: &[f32], pct: f32) -> f32 {
    let total: f32 = mag.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    let threshold = total * pct;
    let mut cum = 0.0;
    for (i, m) in mag.iter().enumerate() {
        cum += m;
        if cum >= threshold {
            return i as f32 / mag.len() as f32;
        }
    }
    1.0
}

fn spectral_flatness(mag: &[f32]) -> f32 {
    let power: Vec<f32> = mag.iter().map(|m| m * m + 1e-10).collect();
    let n = power.len() as f32;
    let log_mean = power.iter().map(|p| p.ln()).sum::<f32>() / n;
    let geo = log_mean.exp();
    let arith = power.iter().sum::<f32>() / n;
    if arith <= 0.0 {
        0.0
    } else {
        (geo / arith).clamp(0.0, 1.0)
    }
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

fn normalize_in_place(v: &mut [f32]) {
    let sum: f32 = v.iter().sum();
    if sum > 0.0 {
        for x in v.iter_mut() {
            *x /= sum;
        }
    }
}

/// A bank of `n_mels` triangular filters over the FFT magnitude bins.
struct MelFilterbank {
    /// Row-major `n_mels × n_bins` weights.
    weights: Vec<f32>,
    n_bins: usize,
}

impl MelFilterbank {
    fn new(rate: u32, frame: usize, n_mels: usize) -> Self {
        let n_bins = frame / 2 + 1;
        let f_max = rate as f32 / 2.0;
        let mel_max = hz_to_mel(f_max);
        // n_mels+2 mel points → n_mels triangular filters.
        let points: Vec<f32> = (0..n_mels + 2)
            .map(|i| mel_to_hz(mel_max * i as f32 / (n_mels + 1) as f32))
            .collect();
        let bin_hz = rate as f32 / frame as f32;
        let mut weights = vec![0.0f32; n_mels * n_bins];
        for m in 0..n_mels {
            let (lo, ctr, hi) = (points[m], points[m + 1], points[m + 2]);
            for (bin, w) in weights[m * n_bins..(m + 1) * n_bins].iter_mut().enumerate() {
                let f = bin as f32 * bin_hz;
                *w = if f >= lo && f <= ctr {
                    if ctr > lo { (f - lo) / (ctr - lo) } else { 0.0 }
                } else if f > ctr && f <= hi {
                    if hi > ctr { (hi - f) / (hi - ctr) } else { 0.0 }
                } else {
                    0.0
                };
            }
        }
        Self { weights, n_bins }
    }

    fn apply(&self, mag: &[f32]) -> Vec<f32> {
        let n_mels = self.weights.len() / self.n_bins;
        (0..n_mels)
            .map(|m| {
                self.weights[m * self.n_bins..(m + 1) * self.n_bins]
                    .iter()
                    .zip(mag)
                    .map(|(w, x)| w * x)
                    .sum()
            })
            .collect()
    }
}

fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// DCT-II matrix (`n_out × n_in`, row-major) for MFCC computation.
fn dct_matrix(n_in: usize, n_out: usize) -> Vec<f32> {
    let mut m = vec![0.0f32; n_out * n_in];
    let scale = (2.0 / n_in as f32).sqrt();
    for k in 0..n_out {
        for j in 0..n_in {
            m[k * n_in + j] = scale * (PI / n_in as f32 * (j as f32 + 0.5) * k as f32).cos();
        }
    }
    m
}

/// Map each FFT bin to a pitch class (0..12), or `None` for sub-audible bins.
fn chroma_bins(rate: u32, frame: usize) -> Vec<Option<usize>> {
    let n_bins = frame / 2 + 1;
    let bin_hz = rate as f32 / frame as f32;
    (0..n_bins)
        .map(|bin| {
            let f = bin as f32 * bin_hz;
            if f < 20.0 {
                return None;
            }
            // MIDI pitch from frequency; pitch class = pitch mod 12.
            let pitch = 69.0 + 12.0 * (f / 440.0).log2();
            if pitch.is_finite() && pitch >= 0.0 {
                Some((pitch.round() as i64).rem_euclid(12) as usize)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsp_dims_constant_matches_layout() {
        assert_eq!(DSP_DIMS, 59);
        assert_eq!(DspExtractor::new().dims(), 59);
    }

    #[test]
    fn resample_identity_when_rates_equal() {
        let x = vec![0.1, 0.2, 0.3];
        assert_eq!(resample(&x, 44100, 44100), x);
    }

    #[test]
    fn resample_halves_length_when_downsampling_2x() {
        let x: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let y = resample(&x, 44100, 22050);
        assert!((y.len() as i64 - 50).abs() <= 1);
    }

    #[test]
    fn hann_window_endpoints_are_zero() {
        let w = hann(8);
        assert!(w[0].abs() < 1e-6);
        assert!(w[7].abs() < 1e-6);
    }

    #[test]
    fn mel_round_trip_is_monotonic() {
        assert!(hz_to_mel(1000.0) > hz_to_mel(100.0));
        assert!((mel_to_hz(hz_to_mel(440.0)) - 440.0).abs() < 1.0);
    }

    #[test]
    fn chroma_maps_a440_to_pitch_class_9() {
        let map = chroma_bins(TARGET_RATE, FRAME);
        let bin_hz = TARGET_RATE as f32 / FRAME as f32;
        let bin = (440.0 / bin_hz).round() as usize;
        assert_eq!(map[bin], Some(9)); // A = pitch class 9
    }

    /// A synthetic sine produces a deterministic, unit-norm embedding of the
    /// right length — and the same input twice gives the identical vector.
    #[test]
    fn synthetic_tone_embedding_is_deterministic_and_unit_norm() {
        let rate = TARGET_RATE;
        let samples: Vec<f32> = (0..rate * 2)
            .map(|i| (2.0 * PI * 220.0 * i as f32 / rate as f32).sin() * 0.5)
            .collect();
        let rows = frame_features(&samples);
        assert!(!rows.is_empty());
        let mut a = aggregate(&rows);
        l2_normalize(&mut a);
        assert_eq!(a.len(), DSP_DIMS);
        let norm = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);

        let rows2 = frame_features(&samples);
        let mut b = aggregate(&rows2);
        l2_normalize(&mut b);
        assert_eq!(a, b);
    }
}
