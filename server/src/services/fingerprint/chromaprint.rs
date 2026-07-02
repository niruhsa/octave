//! Chromaprint identification fingerprint (Phase 12E) — optional.
//!
//! Independent of "sounds like": a Chromaprint answers "is this the *same
//! recording*?" (dedup of the same track uploaded twice; AcoustID/MusicBrainz
//! metadata enrichment). Stored in `track_features.chromaprint`. Compiled only
//! with the `chromaprint` cargo feature (pure-Rust `rusty-chromaprint`).

use std::path::Path;

use rusty_chromaprint::{Configuration, Fingerprinter};

use super::decode::decode_mono;

/// Compute a base64-ish compressed Chromaprint for `path`, or `None` if the file
/// can't be decoded (e.g. MP3 in this build). Best-effort — never fatal.
pub fn fingerprint(path: &Path) -> Option<String> {
    let pcm = decode_mono(path).ok()?;
    // `preset_test2` = algorithm id 1 = the `fpcalc`/AcoustID default. Using it
    // (rather than test1) keeps these fingerprints compatible with AcoustID's
    // index, which the Phase-E audio-anchored discography resolution submits to.
    let mut printer = Fingerprinter::new(&Configuration::preset_test2());
    // rusty-chromaprint wants interleaved i16 at a known rate + channel count;
    // we feed our mono f32 as single-channel i16.
    printer.start(pcm.sample_rate, 1).ok()?;
    let samples: Vec<i16> = pcm
        .samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect();
    printer.consume(&samples);
    printer.finish();
    let raw = printer.fingerprint();
    // Hex-encode the raw u32 fingerprint so it round-trips through TEXT.
    Some(raw.iter().map(|w| format!("{w:08x}")).collect::<String>())
}
