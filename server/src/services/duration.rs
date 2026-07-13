//! Audio duration measurement.
//!
//! Tag libraries (lofty / Symphonia's own probe) read duration from file
//! headers, which can be wrong for VBR-encoded files that lack an index
//! (e.g. VBR MP3 without Xing/VBRI header — the encoder estimates from the
//! first frame's bitrate, which can be off by minutes on a long track).
//!
//! This module measures the **actual** duration by reading the track's
//! metadata from Symphonia's `FormatReader` (fast: pre-computed `duration`
//! field) or, as a fallback, counting frames by walking packets to EOF.
//! Returns `None` when the format isn't supported by Symphonia — callers
//! should fall back to the tag-reported duration.

use std::path::Path;
use std::time::Duration as StdDuration;

use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{TimeBase, Timestamp};

/// The maximum number of packets to walk when the format reader doesn't
/// pre-compute the track duration.  A 60-minute 44.1 kHz MP3 has ~3,125
/// frames (1,152 samples/frame), so 200,000 leaves plenty of headroom for
/// high-sample-rate / short-frame-codec files.
const MAX_PACKET_WALK: u64 = 200_000;

/// Measure the actual playable duration of an audio file.
///
/// Uses Symphonia's `Probe` to open a `FormatReader`, then reads the
/// default audio track's metadata.  When the demuxer pre-computes the
/// duration or frame count, the result is instant; otherwise it walks
/// packets to EOF (capped at `MAX_PACKET_WALK`).
pub fn measure_duration(path: &Path) -> Option<StdDuration> {
    // MP3 duration is handled by our own frame-walker for VBR accuracy: even
    // though Symphonia now decodes MP3 (the `mpa` feature is enabled for the
    // fingerprint extractor), its demuxer reports the header-estimated duration
    // for a VBR file without a Xing/VBRI index, which can be off by minutes.
    // Counting frames is correct for CBR and VBR alike.
    if matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("mp3" | "mp2" | "mp1")
    ) {
        return crate::services::mp3::measure_mp3_duration(path);
    }

    let file = std::fs::File::open(path).ok()?;
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
        .ok()?;

    let track = format.default_track(TrackType::Audio)?;

    // Fast path: demuxer pre-computed the duration (most formats).
    // `Duration::get()` exposes the inner u64 (timebase ticks).
    if let (Some(tb), Some(dur)) = (track.time_base, track.duration)
        && let Ok(stamp) = Timestamp::try_from(dur.get())
    {
        return timestamp_to_std(tb, stamp);
    }

    // Fast path: frame count + timebase.
    if let (Some(tb), Some(n_frames)) = (track.time_base, track.num_frames) {
        let secs = n_frames as f64 * tb.numer.get() as f64 / tb.denom.get() as f64;
        return Some(StdDuration::from_secs_f64(secs));
    }

    // Slow path: walk packets to EOF and read the last timestamp.
    // Extract what we need before the mutable borrow on `format`.
    let tb = track.time_base;
    walk_to_end(&mut *format, tb)
}

/// Walk packets through `format` until EOF, returning the timestamp of the
/// last packet converted to seconds via the track's timebase.
fn walk_to_end(format: &mut dyn FormatReader, tb: Option<TimeBase>) -> Option<StdDuration> {
    let tb = tb?;
    let mut last_ts = Timestamp::new(0);
    let mut walked: u64 = 0;

    loop {
        match format.next_packet() {
            Ok(Some(packet)) => {
                last_ts = packet.pts;
                walked += 1;
                if walked >= MAX_PACKET_WALK {
                    break;
                }
            }
            Ok(None) => break, // EOF
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        }
    }

    if walked == 0 {
        return None;
    }

    timestamp_to_std(tb, last_ts)
}

/// Convert a timestamp + timebase to a standard Duration.
fn timestamp_to_std(tb: TimeBase, ts: Timestamp) -> Option<StdDuration> {
    let time = tb.calc_time(ts)?;
    Some(StdDuration::from_secs_f64(time.as_secs_f64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_audio_returns_none() {
        let tmp = std::env::temp_dir().join("duration_test_empty.bin");
        std::fs::write(&tmp, b"not audio").unwrap();
        let d = measure_duration(&tmp);
        let _ = std::fs::remove_file(&tmp);
        assert!(d.is_none());
    }
}
