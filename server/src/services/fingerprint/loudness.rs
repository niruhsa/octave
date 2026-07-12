//! EBU R128 / ITU-R BS.1770 integrated-loudness measurement (Phase 16).
//!
//! The fingerprint pass already decodes every track to PCM for the similarity
//! embedding; this measures each track's **integrated loudness** (LUFS) from
//! that same decode, so the client can apply a compensating gain in the player
//! (ReplayGain-style) and every track plays at a consistent perceived loudness.
//!
//! The measurement is fed the decoder's **interleaved multichannel** buffers
//! (see [`super::decode`]), not the mono downmix — a downmix would read 3–6 dB
//! off depending on stereo width, since BS.1770 sums per-channel loudness. Wraps
//! [`ebur128`] (a pure-Rust port of libebur128 — K-weighting + gating, no C dep).

use ebur128::{EbuR128, Mode};

/// A track's measured loudness.
#[derive(Debug, Clone, Copy)]
pub struct Loudness {
    /// Integrated loudness in LUFS (EBU R128).
    pub lufs: f32,
    /// Peak sample as a linear amplitude (0..1+, max `|sample|` across the
    /// original channels), for clip-safe gain.
    pub peak: f32,
}

/// Streaming integrated-loudness meter, fed the decoder's interleaved
/// multichannel PCM buffer-by-buffer. Any backend error makes the final
/// measurement `None` (skipped, never fatal — the track just gets no loudness).
pub struct LoudnessMeter {
    ebu: EbuR128,
    /// Channel count the meter was built with; buffers with a different layout
    /// are skipped (channel count never changes within a normal file).
    pub channels: u32,
    ok: bool,
}

impl LoudnessMeter {
    /// Build a meter for `channels`-channel audio at `rate` Hz. `None` if the
    /// backend rejects the layout, so measurement is skipped rather than fatal.
    pub fn new(channels: u32, rate: u32) -> Option<Self> {
        let ebu = EbuR128::new(channels, rate, Mode::I).ok()?;
        Some(Self {
            ebu,
            channels,
            ok: true,
        })
    }

    /// Feed one interleaved buffer (`channels`-interleaved f32 samples).
    pub fn add_interleaved(&mut self, frames: &[f32]) {
        if !self.ok || frames.is_empty() {
            return;
        }
        if self.ebu.add_frames_f32(frames).is_err() {
            self.ok = false; // give up quietly; the measurement resolves to None
        }
    }

    /// Finish: the integrated loudness plus the caller-supplied linear `peak`.
    /// `None` when measurement failed or the loudness is undefined (silence /
    /// everything below the absolute gate → `ebur128` yields a non-finite or
    /// effectively-silent value).
    pub fn finish(self, peak: f32) -> Option<Loudness> {
        if !self.ok {
            return None;
        }
        let lufs = self.ebu.loudness_global().ok()?;
        if !lufs.is_finite() || lufs < -70.0 {
            return None;
        }
        Some(Loudness {
            lufs: lufs as f32,
            peak,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn sine(freq: f32, rate: u32, secs: f32, amp: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f32 / rate as f32).sin() * amp)
            .collect()
    }

    /// Measure a mono buffer, feeding it in decode-sized chunks like the real
    /// decode loop.
    fn measure_mono(samples: &[f32], rate: u32, peak: f32) -> Option<Loudness> {
        let mut m = LoudnessMeter::new(1, rate)?;
        for chunk in samples.chunks(4096) {
            m.add_interleaved(chunk);
        }
        m.finish(peak)
    }

    #[test]
    fn steady_tone_yields_finite_loudness() {
        let s = sine(1000.0, 48_000, 2.0, 0.5);
        let l = measure_mono(&s, 48_000, 0.5).expect("some loudness");
        assert!(l.lufs.is_finite());
        assert!(
            l.lufs > -30.0 && l.lufs < 0.0,
            "lufs {} out of plausible range",
            l.lufs
        );
        assert!((l.peak - 0.5).abs() < 1e-6);
    }

    #[test]
    fn louder_tone_reads_higher_lufs() {
        // amp 0.5 vs 0.125 = a 4× amplitude ratio = 12 dB of power.
        let loud = measure_mono(&sine(1000.0, 48_000, 2.0, 0.5), 48_000, 0.5).unwrap();
        let quiet = measure_mono(&sine(1000.0, 48_000, 2.0, 0.125), 48_000, 0.125).unwrap();
        assert!(
            loud.lufs > quiet.lufs + 6.0,
            "expected a large loudness gap, got {} vs {}",
            loud.lufs,
            quiet.lufs
        );
    }

    #[test]
    fn silence_has_no_loudness() {
        let s = vec![0.0f32; 48_000 * 2];
        assert!(measure_mono(&s, 48_000, 0.0).is_none());
    }
}
