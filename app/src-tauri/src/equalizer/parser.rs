//! Strict ParametricEQ / Equalizer APO text subset used by EQ profile import.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::model::{
    validate_name, validate_profile, EqualizerBand, EqualizerProfile, EqualizerValidationError,
    FilterKind, EQ_PROFILE_FORMAT_VERSION, MAX_BANDS,
};

pub const MAX_IMPORT_BYTES: usize = 64 * 1024;
const MAX_IMPORT_LINES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseWarning {
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedEqualizerProfile {
    pub profile: EqualizerProfile,
    pub warnings: Vec<ParseWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct EqualizerParseError {
    pub line: Option<usize>,
    pub message: String,
}

impl EqualizerParseError {
    fn global(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }

    fn line(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line),
            message: message.into(),
        }
    }
}

pub fn parse_equalizer_text(
    input: &str,
    proposed_name: &str,
) -> Result<ParsedEqualizerProfile, EqualizerParseError> {
    if input.len() > MAX_IMPORT_BYTES {
        return Err(EqualizerParseError::global(format!(
            "profile exceeds {MAX_IMPORT_BYTES} bytes"
        )));
    }
    let line_count = input.lines().count();
    if line_count > MAX_IMPORT_LINES {
        return Err(EqualizerParseError::global(format!(
            "profile exceeds {MAX_IMPORT_LINES} lines"
        )));
    }
    validate_name(proposed_name, "profile name").map_err(validation_error)?;

    let mut preamp = None;
    let mut bands = Vec::new();
    let mut seen_numbers = HashSet::new();

    for (zero_index, original) in input.lines().enumerate() {
        let line_no = zero_index + 1;
        let line = if zero_index == 0 {
            original.trim_start_matches('\u{feff}').trim()
        } else {
            original.trim()
        };
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        let tokens: Vec<&str> = line.split_whitespace().collect();
        let head = tokens.first().copied().unwrap_or_default();
        if head.trim_end_matches(':').eq_ignore_ascii_case("preamp") {
            if preamp.is_some() {
                return Err(EqualizerParseError::line(line_no, "duplicate Preamp line"));
            }
            if tokens.len() != 3 || !tokens[2].eq_ignore_ascii_case("db") {
                return Err(EqualizerParseError::line(
                    line_no,
                    "expected `Preamp: <number> dB`",
                ));
            }
            preamp = Some(parse_number(tokens[1], line_no, "preamp")?);
            continue;
        }

        if !head.eq_ignore_ascii_case("filter") {
            return Err(EqualizerParseError::line(
                line_no,
                "unsupported directive; version 1 accepts only Preamp and PK Filter lines",
            ));
        }
        if tokens.len() != 12 {
            return Err(EqualizerParseError::line(
                line_no,
                "expected `Filter N: ON|OFF PK Fc <Hz> Hz Gain <dB> dB Q <value>`",
            ));
        }
        let number_text = tokens[1]
            .strip_suffix(':')
            .ok_or_else(|| EqualizerParseError::line(line_no, "filter number must end with ':'"))?;
        let file_number: u32 = number_text.parse().map_err(|_| {
            EqualizerParseError::line(line_no, "filter number must be a positive integer")
        })?;
        if file_number == 0 || !seen_numbers.insert(file_number) {
            return Err(EqualizerParseError::line(
                line_no,
                "filter numbers must be positive and unique",
            ));
        }
        let enabled = if tokens[2].eq_ignore_ascii_case("on") {
            true
        } else if tokens[2].eq_ignore_ascii_case("off") {
            false
        } else {
            return Err(EqualizerParseError::line(
                line_no,
                "filter must be ON or OFF",
            ));
        };
        if !tokens[3].eq_ignore_ascii_case("pk") {
            return Err(EqualizerParseError::line(
                line_no,
                format!("unsupported filter kind '{}'", tokens[3]),
            ));
        }
        expect_token(&tokens, 4, "Fc", line_no)?;
        expect_token(&tokens, 6, "Hz", line_no)?;
        expect_token(&tokens, 7, "Gain", line_no)?;
        expect_token(&tokens, 9, "dB", line_no)?;
        expect_token(&tokens, 10, "Q", line_no)?;
        let frequency_hz = parse_number(tokens[5], line_no, "frequency")?;
        let gain_db = parse_number(tokens[8], line_no, "gain")?;
        let q = parse_number(tokens[11], line_no, "Q")?;
        bands.push(EqualizerBand {
            position: bands.len() as u32 + 1,
            enabled,
            filter_kind: FilterKind::Peaking,
            frequency_hz,
            gain_db,
            q,
        });
        if bands.len() > MAX_BANDS {
            return Err(EqualizerParseError::line(
                line_no,
                format!("profile exceeds {MAX_BANDS} filters"),
            ));
        }
    }

    if bands.is_empty() {
        return Err(EqualizerParseError::global(
            "profile must contain at least one PK filter",
        ));
    }
    let mut profile = EqualizerProfile::new_local(proposed_name.trim(), bands);
    profile.format_version = EQ_PROFILE_FORMAT_VERSION;
    profile.preamp_db = preamp.unwrap_or(0.0);
    validate_profile(&profile).map_err(validation_error)?;

    let warnings = if preamp.is_none() {
        vec![ParseWarning {
            line: None,
            message: "No Preamp line; using 0 dB".to_string(),
        }]
    } else {
        Vec::new()
    };
    Ok(ParsedEqualizerProfile { profile, warnings })
}

pub fn export_equalizer_text(profile: &EqualizerProfile) -> Result<String, EqualizerParseError> {
    validate_profile(profile).map_err(validation_error)?;
    let mut output = format!("Preamp: {} dB\n", format_number(profile.preamp_db));
    for (index, band) in profile.bands.iter().enumerate() {
        let state = if band.enabled { "ON" } else { "OFF" };
        output.push_str(&format!(
            "Filter {}: {state} PK Fc {} Hz Gain {} dB Q {}\n",
            index + 1,
            format_number(band.frequency_hz),
            format_number(band.gain_db),
            format_number(band.q),
        ));
    }
    Ok(output)
}

fn expect_token(
    tokens: &[&str],
    index: usize,
    expected: &str,
    line: usize,
) -> Result<(), EqualizerParseError> {
    if tokens[index].eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(EqualizerParseError::line(
            line,
            format!("expected '{expected}', found '{}'", tokens[index]),
        ))
    }
}

fn parse_number(value: &str, line: usize, field: &str) -> Result<f64, EqualizerParseError> {
    let parsed: f64 = value.parse().map_err(|_| {
        EqualizerParseError::line(line, format!("{field} must be a decimal number"))
    })?;
    if !parsed.is_finite() {
        return Err(EqualizerParseError::line(
            line,
            format!("{field} must be finite"),
        ));
    }
    Ok(parsed)
}

fn format_number(value: f64) -> String {
    // Rust's Display implementation uses a shortest decimal representation
    // that round-trips to the same f64 (Ryu). A fixed decimal precision would
    // silently alter imported measurements on export.
    if value == 0.0 {
        "0".to_string()
    } else {
        value.to_string()
    }
}

fn validation_error(error: EqualizerValidationError) -> EqualizerParseError {
    EqualizerParseError::global(error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRINEAR: &str =
        include_str!("../../../../equalizer_examples/CrinEar Project Monolith Filters.txt");
    const MOONDROP: &str =
        include_str!("../../../../equalizer_examples/Moondrop Robin Filters.txt");
    const SONY: &str = include_str!("../../../../equalizer_examples/Sony WF-1000XM5 Filters.txt");

    #[test]
    fn parses_supplied_profiles_exactly() {
        let cases = [
            (CRINEAR, "CrinEar", -0.7576, 8usize),
            (MOONDROP, "Moondrop", -2.3185, 6usize),
            (SONY, "Sony", -1.7233, 4usize),
        ];
        for (text, name, preamp, count) in cases {
            let parsed = parse_equalizer_text(text, name).unwrap();
            assert_eq!(parsed.profile.preamp_db, preamp);
            assert_eq!(parsed.profile.bands.len(), count);
            assert!(parsed.warnings.is_empty());
        }
        assert_eq!(
            parse_equalizer_text(CRINEAR, "x").unwrap().profile.bands[0].frequency_hz,
            21.0
        );
        assert_eq!(
            parse_equalizer_text(SONY, "x").unwrap().profile.bands[3].q,
            3.8
        );
    }

    #[test]
    fn missing_preamp_warns_and_round_trips_disabled_band() {
        let source = "Filter 9: OFF PK Fc 1000 Hz Gain -2.25 dB Q 3\n";
        let parsed = parse_equalizer_text(source, "One").unwrap();
        assert_eq!(parsed.profile.preamp_db, 0.0);
        assert_eq!(parsed.warnings.len(), 1);
        assert!(!parsed.profile.bands[0].enabled);
        let exported = export_equalizer_text(&parsed.profile).unwrap();
        let reparsed = parse_equalizer_text(&exported, "One").unwrap();
        assert_eq!(reparsed.profile.bands, parsed.profile.bands);
    }

    #[test]
    fn rejects_unsupported_even_when_off() {
        let source = "Preamp: 0 dB\nFilter 1: OFF LS Fc 100 Hz Gain 1 dB Q 1\n";
        let error = parse_equalizer_text(source, "Bad").unwrap_err();
        assert_eq!(error.line, Some(2));
        assert!(error.message.contains("unsupported filter"));
    }

    #[test]
    fn accepts_bom_crlf_comments_and_case_insensitive_tokens() {
        let source =
            "\u{feff}; note\r\npreamp: -1 dB\r\nfilter 2: on pk fc 100 hz gain 2 db q 1\r\n";
        assert_eq!(
            parse_equalizer_text(source, "Fine")
                .unwrap()
                .profile
                .bands
                .len(),
            1
        );
    }

    #[test]
    fn export_preserves_high_precision_values_semantically() {
        let source = "Preamp: -0.123456789012345 dB\nFilter 1: ON PK Fc 1234.567890123 Hz Gain 2.345678901234 dB Q 1.234567890123\n";
        let parsed = parse_equalizer_text(source, "Precise").unwrap();
        let reparsed =
            parse_equalizer_text(&export_equalizer_text(&parsed.profile).unwrap(), "Precise")
                .unwrap();
        assert_eq!(reparsed.profile.preamp_db, parsed.profile.preamp_db);
        assert_eq!(reparsed.profile.bands, parsed.profile.bands);
    }
}
