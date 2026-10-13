//! Describes measured bandwidth relative to a file's declared sample rate.
//!
//! A band-limited recording may be native, mastered through a lowpass, or resampled.
//! This module reports the observation only; it cannot reconstruct the source sample rate.

use serde::Serialize;

const HI_RES_THRESHOLD_HZ: u32 = 48_000;
/// A display flag for a substantially narrower measured band, not a provenance threshold.
const MIN_BANDWIDTH_RATIO: f64 = 0.65;

/// Bandwidth measurements; no field claims that the recording was upsampled.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SampleRateAnalysis {
    pub declared_sample_rate_hz: u32,
    pub content_bandwidth_hz: Option<f64>,
    pub bandwidth_ratio: Option<f64>,
    /// True when a measured edge bounds less than 65% of the declared hi-res band.
    pub bandwidth_limited: bool,
}

/// Compares the raw spectral edge with Nyquist, without guessing its cause. Missing or
/// invalid measurements remain absent rather than fabricating full bandwidth.
pub fn analyze_sample_rate(
    declared_sample_rate_hz: u32,
    spectral_cutoff_hz: Option<f64>,
) -> SampleRateAnalysis {
    let nyquist_hz = declared_sample_rate_hz as f64 / 2.0;
    let content_bandwidth_hz =
        spectral_cutoff_hz.filter(|hz| hz.is_finite() && *hz > 0.0 && *hz <= nyquist_hz);
    let bandwidth_ratio = content_bandwidth_hz.map(|hz| hz / nyquist_hz);
    let bandwidth_limited = declared_sample_rate_hz > HI_RES_THRESHOLD_HZ
        && bandwidth_ratio.is_some_and(|ratio| ratio < MIN_BANDWIDTH_RATIO);
    SampleRateAnalysis {
        declared_sample_rate_hz,
        content_bandwidth_hz,
        bandwidth_ratio,
        bandwidth_limited,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measured_edge_is_reported_without_a_source_rate_claim() {
        let result = analyze_sample_rate(96_000, Some(24_800.0));
        assert!(result.bandwidth_limited);
        assert!((result.bandwidth_ratio.unwrap() - 24_800.0 / 48_000.0).abs() < 1e-12);
    }

    #[test]
    fn missing_or_invalid_bandwidth_remains_unmeasured() {
        for edge in [None, Some(f64::NAN), Some(-1.0), Some(100_000.0)] {
            let result = analyze_sample_rate(96_000, edge);
            assert_eq!(result.bandwidth_ratio, None);
            assert!(!result.bandwidth_limited);
        }
    }

    #[test]
    fn cd_rate_or_wide_hi_res_content_is_not_flagged() {
        assert!(!analyze_sample_rate(44_100, Some(10_000.0)).bandwidth_limited);
        assert!(!analyze_sample_rate(96_000, Some(47_000.0)).bandwidth_limited);
    }
}
