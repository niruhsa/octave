//! End-to-end fingerprinting test (Phase 12): exercise the *real* decode →
//! DSP-extract → cosine path on synthetic WAV files (PCM, which Symphonia
//! decodes in this build — no committed binary fixtures, no MP3).
//!
//! Asserts (§12 of the plan):
//!   * the extractor produces a non-empty, unit-norm embedding of the right
//!     `dims` for every supported sample, and
//!   * a "sane nearest neighbor for an obvious pair" — two near-identical tones
//!     are more similar than a clearly-different tone.

use std::f32::consts::PI;
use std::io::Write;
use std::path::PathBuf;

use server::services::fingerprint::{cosine_similarity, DspExtractor, FeatureExtractor};

/// Write a mono 16-bit PCM WAV of `samples` at `rate` Hz to a temp path.
fn write_wav(name: &str, rate: u32, samples: &[f32]) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    let mut f = std::fs::File::create(&path).unwrap();

    let data_len = (samples.len() * 2) as u32;
    let byte_rate = rate * 2;
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    // fmt chunk
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    f.write_all(&1u16.to_le_bytes()).unwrap(); // mono
    f.write_all(&rate.to_le_bytes()).unwrap();
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap(); // block align
    f.write_all(&16u16.to_le_bytes()).unwrap(); // bits/sample
    // data chunk
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        f.write_all(&v.to_le_bytes()).unwrap();
    }
    f.flush().unwrap();
    path
}

/// A 2-second tone at `freq` Hz with optional 2nd-harmonic content (`bright`).
fn tone(rate: u32, freq: f32, bright: f32, amp: f32) -> Vec<f32> {
    (0..rate * 2)
        .map(|i| {
            let t = i as f32 / rate as f32;
            let fundamental = (2.0 * PI * freq * t).sin();
            let harmonic = (2.0 * PI * freq * 2.0 * t).sin() * bright;
            (fundamental + harmonic) * amp
        })
        .collect()
}

#[tokio::test]
async fn dsp_extracts_unit_norm_embeddings_and_ranks_an_obvious_pair() {
    let rate = 22_050;
    let ext = DspExtractor::new();
    let dims = ext.dims();

    // Two near-identical low tones (220 Hz, slightly different amplitude/timbre)
    // and one clearly-different bright high tone (660 Hz + strong harmonic).
    let a = write_wav("fp_int_a.wav", rate, &tone(rate, 220.0, 0.05, 0.5));
    let a2 = write_wav("fp_int_a2.wav", rate, &tone(rate, 220.0, 0.08, 0.42));
    let b = write_wav("fp_int_b.wav", rate, &tone(rate, 660.0, 0.6, 0.5));

    let ea = ext.extract(&a).await.expect("extract a");
    let ea2 = ext.extract(&a2).await.expect("extract a2");
    let eb = ext.extract(&b).await.expect("extract b");

    for e in [&ea, &ea2, &eb] {
        assert_eq!(e.len(), dims, "embedding length matches dims()");
        let norm = e.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "embedding is unit-norm (got {norm})");
    }

    // The obvious pair (220 vs 220) must be more similar than 220 vs 660.
    let near = cosine_similarity(&ea, &ea2);
    let far = cosine_similarity(&ea, &eb);
    assert!(
        near > far,
        "near pair {near} should beat far pair {far}"
    );

    for p in [a, a2, b] {
        let _ = std::fs::remove_file(p);
    }
}

#[tokio::test]
async fn mp3_extension_is_reported_unanalyzable() {
    // The DSP extractor must cleanly refuse MP3 (no symphonia MP3 bundle) with
    // an InvalidArgument so the pass classifies it as "skip", not "fail".
    let path = std::env::temp_dir().join("fp_int_fake.mp3");
    std::fs::write(&path, b"not really an mp3").unwrap();
    let err = DspExtractor::new().extract(&path).await.unwrap_err();
    let _ = std::fs::remove_file(&path);
    assert!(
        matches!(err, server::error::AppError::InvalidArgument(_)),
        "MP3 should be InvalidArgument (skip), got {err:?}"
    );
}
