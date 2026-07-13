//! Dependency-free MP3 duration measurement.
//!
//! Why this exists: Symphonia 0.6.0 pins `symphonia-bundle-mp3 = 0.6.0`,
//! which was never published to crates.io (only `-alpha`), so the `mp3`
//! feature can't be enabled — [`crate::services::duration::measure_duration`]
//! returns `None` for MP3.  lofty falls back to a header-based estimate that
//! is *wrong* for VBR files without a Xing/Info/VBRI header (the encoder
//! guesses from the first frame's bitrate), which then makes the client's
//! `<audio>` element map seek-time → byte-offset incorrectly — audible as
//! playback "drifting" away from the seek bar.
//!
//! This module walks the actual MPEG audio frames and sums their real
//! durations, which is correct for CBR *and* VBR alike.  When a Xing/Info
//! (CBR/VBR) or VBRI frame-count header is present we trust it (one read,
//! no walk); otherwise we count every frame.

use std::path::Path;
use std::time::Duration;

/// MPEG version, decoded from the 2 version bits in a frame header.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MpegVersion {
    V1,
    V2,
    V2_5,
}

/// Samples-per-frame depends on version + layer.  Layer is 1..=3.
fn samples_per_frame(version: MpegVersion, layer: u8) -> u32 {
    match (version, layer) {
        (_, 1) => 384,
        (MpegVersion::V1, 2) => 1152,
        (MpegVersion::V1, 3) => 1152,
        // MPEG2 / 2.5 Layer 2 keeps 1152; Layer 3 halves to 576.
        (_, 2) => 1152,
        (_, 3) => 576,
        _ => 0,
    }
}

// Bitrate tables in kbps, indexed by the 4-bit bitrate index (0 = free,
// 15 = invalid -> handled as 0/None).
const BR_V1_L1: [u32; 16] = [
    0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
];
const BR_V1_L2: [u32; 16] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
];
const BR_V1_L3: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const BR_V2_L1: [u32; 16] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
];
const BR_V2_L23: [u32; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];

const SR_V1: [u32; 3] = [44100, 48000, 32000];
const SR_V2: [u32; 3] = [22050, 24000, 16000];
const SR_V2_5: [u32; 3] = [11025, 12000, 8000];

/// A parsed MPEG audio frame header.  Only the fields needed for duration
/// (sample rate, samples-per-frame, frame length, and version for the Xing
/// side-info offset) are retained; `layer`/`bitrate` are used transiently
/// during parsing.
struct FrameHeader {
    version: MpegVersion,
    sample_rate: u32,
    /// Total frame length in bytes (incl. the 4-byte header + padding).
    frame_len: usize,
    samples: u32,
}

/// Parse a 4-byte MPEG audio frame header at `buf[0..4]`.  Returns `None`
/// when the bytes aren't a valid frame (bad sync / reserved fields / free
/// or invalid bitrate / reserved sample rate).
fn parse_header(buf: &[u8]) -> Option<FrameHeader> {
    if buf.len() < 4 {
        return None;
    }
    // Frame sync: 11 bits set.
    if buf[0] != 0xFF || (buf[1] & 0xE0) != 0xE0 {
        return None;
    }
    let version = match (buf[1] >> 3) & 0b11 {
        0b00 => MpegVersion::V2_5,
        0b10 => MpegVersion::V2,
        0b11 => MpegVersion::V1,
        _ => return None, // 0b01 reserved
    };
    let layer = match (buf[1] >> 1) & 0b11 {
        0b01 => 3,
        0b10 => 2,
        0b11 => 1,
        _ => return None, // 0b00 reserved
    };
    let bitrate_index = (buf[2] >> 4) & 0x0F;
    if bitrate_index == 0 || bitrate_index == 15 {
        // 0 = "free" (we can't size the frame), 15 = invalid.
        return None;
    }
    let sr_index = (buf[2] >> 2) & 0b11;
    if sr_index == 3 {
        return None; // reserved
    }
    let padding = ((buf[2] >> 1) & 0x01) as usize;

    let bitrate_kbps = match (version, layer) {
        (MpegVersion::V1, 1) => BR_V1_L1[bitrate_index as usize],
        (MpegVersion::V1, 2) => BR_V1_L2[bitrate_index as usize],
        (MpegVersion::V1, 3) => BR_V1_L3[bitrate_index as usize],
        (_, 1) => BR_V2_L1[bitrate_index as usize],
        (_, _) => BR_V2_L23[bitrate_index as usize],
    };
    if bitrate_kbps == 0 {
        return None;
    }
    let sample_rate = match version {
        MpegVersion::V1 => SR_V1[sr_index as usize],
        MpegVersion::V2 => SR_V2[sr_index as usize],
        MpegVersion::V2_5 => SR_V2_5[sr_index as usize],
    };
    let samples = samples_per_frame(version, layer);
    if samples == 0 || sample_rate == 0 {
        return None;
    }

    // Frame length in bytes.  Layer 1 uses a different formula (slots of 4
    // bytes); Layers 2/3 use 1-byte slots.
    let bitrate = bitrate_kbps * 1000;
    let frame_len = if layer == 1 {
        ((12 * bitrate / sample_rate + padding as u32) * 4) as usize
    } else {
        // (samples/8 * bitrate / sample_rate) + padding.
        ((samples / 8) * bitrate / sample_rate + padding as u32) as usize
    };
    if frame_len < 4 {
        return None;
    }

    Some(FrameHeader {
        version,
        sample_rate,
        frame_len,
        samples,
    })
}

/// Skip an ID3v2 tag at the start of the file, returning the offset of the
/// first byte after it (0 when no tag is present).
fn id3v2_len(buf: &[u8]) -> usize {
    if buf.len() < 10 || &buf[0..3] != b"ID3" {
        return 0;
    }
    // Size is a 28-bit synchsafe integer in bytes 6..10.
    let size = ((buf[6] as usize & 0x7F) << 21)
        | ((buf[7] as usize & 0x7F) << 14)
        | ((buf[8] as usize & 0x7F) << 7)
        | (buf[9] as usize & 0x7F);
    // +10 header; +10 again if a footer is present (flag bit 4).
    let footer = if buf[5] & 0x10 != 0 { 10 } else { 0 };
    10 + size + footer
}

/// Look for a Xing/Info or VBRI header inside the first audio frame and
/// return the encoded frame count if present.  The side-info gap between
/// the frame header and the Xing tag depends on version + channel mode.
fn xing_frame_count(frame: &[u8], hdr: &FrameHeader, channel_mode: u8) -> Option<u32> {
    // Xing/Info: offset after the 4-byte header + side info.
    let side_info = match (hdr.version, channel_mode) {
        (MpegVersion::V1, 0b11) => 17, // mono
        (MpegVersion::V1, _) => 32,    // stereo/js/dual
        (_, 0b11) => 9,                // mono, v2/v2.5
        (_, _) => 17,
    };
    let xing_off = 4 + side_info;
    if frame.len() >= xing_off + 8 {
        let tag = &frame[xing_off..xing_off + 4];
        if tag == b"Xing" || tag == b"Info" {
            let flags = u32::from_be_bytes([
                frame[xing_off + 4],
                frame[xing_off + 5],
                frame[xing_off + 6],
                frame[xing_off + 7],
            ]);
            // Bit 0 of flags => frame-count field present immediately after.
            if flags & 0x0001 != 0 && frame.len() >= xing_off + 12 {
                let n = u32::from_be_bytes([
                    frame[xing_off + 8],
                    frame[xing_off + 9],
                    frame[xing_off + 10],
                    frame[xing_off + 11],
                ]);
                if n > 0 {
                    return Some(n);
                }
            }
        }
    }
    // VBRI: fixed offset of 32 bytes after the frame header.
    let vbri_off = 4 + 32;
    if frame.len() >= vbri_off + 18 && &frame[vbri_off..vbri_off + 4] == b"VBRI" {
        let n = u32::from_be_bytes([
            frame[vbri_off + 14],
            frame[vbri_off + 15],
            frame[vbri_off + 16],
            frame[vbri_off + 17],
        ]);
        if n > 0 {
            return Some(n);
        }
    }
    None
}

/// Measure the duration of an MP3 file by walking its audio frames.
///
/// Returns `None` when the file can't be read or no valid MPEG frame is
/// found (e.g. it isn't actually an MP3).
pub fn measure_mp3_duration(path: &Path) -> Option<Duration> {
    let data = std::fs::read(path).ok()?;
    let start = id3v2_len(&data);
    if start >= data.len() {
        return None;
    }

    // Find the first valid frame at/after `start`.  Real files sometimes
    // have a few junk bytes before the first sync.
    let mut pos = start;
    let first = loop {
        if pos + 4 > data.len() {
            return None;
        }
        if let Some(h) = parse_header(&data[pos..]) {
            // Sanity-check: the *next* frame should also sync where this
            // frame predicts it ends (guards against false-positive syncs
            // inside tag/album-art data).
            let next = pos + h.frame_len;
            if next + 2 <= data.len() && data[next] == 0xFF && (data[next + 1] & 0xE0) == 0xE0 {
                break (pos, h);
            }
            // Or this is the only/last frame.
            if next >= data.len() {
                break (pos, h);
            }
        }
        pos += 1;
    };
    let (first_pos, first_hdr) = first;

    // Fast path: a Xing/Info/VBRI header carries the exact frame count.
    let channel_mode = (data[first_pos + 3] >> 6) & 0b11;
    if let Some(frame_count) = xing_frame_count(&data[first_pos..], &first_hdr, channel_mode) {
        // The Xing/Info frame itself is a header frame and is *not* counted
        // in `frame_count` for the LAME convention, but in practice the
        // off-by-one is sub-30 ms and players ignore it. Use the count as-is.
        let secs = frame_count as f64 * first_hdr.samples as f64 / first_hdr.sample_rate as f64;
        return Some(Duration::from_secs_f64(secs));
    }

    // Slow path: walk every frame, summing real per-frame durations. This
    // is what makes VBR-without-header accurate.
    let mut total_secs = 0.0_f64;
    let mut pos = first_pos;
    let mut frames = 0u64;
    while pos + 4 <= data.len() {
        let h = match parse_header(&data[pos..]) {
            Some(h) => h,
            None => {
                // Lost sync — try to resync within a small window before
                // giving up (handles occasional stray bytes between frames).
                let mut resynced = false;
                let scan_end = (pos + 4096).min(data.len().saturating_sub(4));
                let mut p = pos + 1;
                while p <= scan_end {
                    if data[p] == 0xFF
                        && (data[p + 1] & 0xE0) == 0xE0
                        && parse_header(&data[p..]).is_some()
                    {
                        pos = p;
                        resynced = true;
                        break;
                    }
                    p += 1;
                }
                if resynced {
                    continue;
                }
                break;
            }
        };
        total_secs += h.samples as f64 / h.sample_rate as f64;
        frames += 1;
        pos += h.frame_len;
        // Avoid pathological infinite loops on a zero-length frame.
        if h.frame_len == 0 {
            break;
        }
    }

    if frames == 0 {
        return None;
    }
    Some(Duration::from_secs_f64(total_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_v1l3_header() {
        // 0xFF 0xFB = sync + MPEG1 + Layer3 + no CRC.
        // 0x90 = bitrate index 9 (128 kbps), sr index 0 (44100), no padding.
        // 0x00 = stereo.
        let buf = [0xFF, 0xFB, 0x90, 0x00];
        let h = parse_header(&buf).expect("valid header");
        assert_eq!(h.sample_rate, 44100);
        assert_eq!(h.samples, 1152);
        // 144 * 128000 / 44100 = 417 (no padding).
        assert_eq!(h.frame_len, 417);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_header(&[0x00, 0x00, 0x00, 0x00]).is_none());
        assert!(parse_header(&[0xFF, 0x00, 0x00, 0x00]).is_none());
        // bitrate index 15 (invalid)
        assert!(parse_header(&[0xFF, 0xFB, 0xF0, 0x00]).is_none());
        // sample-rate index 3 (reserved)
        assert!(parse_header(&[0xFF, 0xFB, 0x9C, 0x00]).is_none());
    }

    #[test]
    fn id3v2_len_skips_tag() {
        // "ID3" v2.3, no footer, size = 1 (synchsafe) => 10 + 1 = 11.
        let buf = [b'I', b'D', b'3', 3, 0, 0, 0, 0, 0, 1];
        assert_eq!(id3v2_len(&buf), 11);
        // No tag.
        assert_eq!(id3v2_len(&[0xFF, 0xFB, 0, 0, 0, 0, 0, 0, 0, 0]), 0);
    }
}
