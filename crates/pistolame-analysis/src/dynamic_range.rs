//! DR14 (Pleasurize Music Foundation "Dynamic Range" algorithm), the metric audiophiles
//! actually compare against the public loudness-war database (dr.loudness-war.info and
//! successors) — distinct from the simple peak/RMS crest factor already reported in
//! `signal_analysis.rs`, which this module does not replace.
//!
//! Algorithm verified against the reference open-source implementation
//! (`dr14_t.meter`, Simone Riva, GPLv3, follows the official Pleasurize Music Foundation
//! procedure) rather than secondhand forum descriptions — see
//! `dr14tmeter/compute_dr14.py` in that project. One deliberate simplification: the
//! reference adds a `+60` sample fudge to the block size only at exactly 44100 Hz (an
//! implementation quirk of that specific tool, not part of the documented algorithm); we
//! use a plain `3 * sample_rate` block size at every rate, which changes the block
//! duration by 0.136% at 44100 Hz — immaterial to a value that's rounded to an integer.

use serde::Serialize;

use crate::decode::DecodedAudio;

const BLOCK_SECONDS: f64 = 3.0;
const TOP_FRACTION: f64 = 0.2;
/// Sanity bound matching the reference implementation's `max_dynamic(24)`: no real signal
/// can have a dynamic range exceeding the theoretical SNR of 24-bit PCM.
const MAX_PLAUSIBLE_DR_DB: f64 = 24.0 * 20.0 * std::f64::consts::LOG10_2;
/// Reference implementation's `audio_min()`: below this, the "loud" reference blocks are
/// effectively silence and the ratio is meaningless, not a real DR value.
const MIN_RMS_REF_LINEAR: f64 = 1.0 / (1 << 24) as f64;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct DynamicRangeResult {
    /// Whole-file DR, averaged across channels and rounded to the nearest integer — the
    /// "DR12"-style number this community publishes and compares.
    pub dr14: Option<i32>,
    /// Unrounded per-channel value, before the final averaging/rounding above.
    pub per_channel_db: Vec<Option<f64>>,
}

pub fn compute_dr14(decoded: &DecodedAudio) -> DynamicRangeResult {
    let block_samples = (BLOCK_SECONDS * decoded.sample_rate as f64).round() as usize;
    if block_samples == 0 {
        return DynamicRangeResult {
            dr14: None,
            per_channel_db: vec![],
        };
    }

    let per_channel_db: Vec<Option<f64>> = decoded
        .channel_samples
        .iter()
        .map(|samples| channel_dr(samples, block_samples))
        .collect();

    let valid: Vec<f64> = per_channel_db.iter().filter_map(|v| *v).collect();
    let dr14 = if valid.is_empty() {
        None
    } else {
        Some((valid.iter().sum::<f64>() / valid.len() as f64).round() as i32)
    };

    DynamicRangeResult {
        dr14,
        per_channel_db,
    }
}

fn channel_dr(samples: &[f32], block_samples: usize) -> Option<f64> {
    let mut block_rms: Vec<f64> = Vec::new();
    let mut block_peak: Vec<f64> = Vec::new();

    for block in samples.chunks(block_samples) {
        let sum_sq: f64 = block.iter().map(|&s| (s as f64) * (s as f64)).sum();
        // Reference formula includes a factor of 2 — not plain RMS, see module docs.
        block_rms.push((2.0 * sum_sq / block.len() as f64).sqrt());
        block_peak.push(block.iter().map(|&s| (s as f64).abs()).fold(0.0, f64::max));
    }

    let seg_cnt = block_rms.len();
    if seg_cnt == 0 {
        return None;
    }

    block_rms.sort_by(|a, b| a.total_cmp(b));
    block_peak.sort_by(|a, b| a.total_cmp(b));

    // Second-highest peak, not the highest — avoids one transient/glitch sample
    // dominating the reference. Falls back to the only peak available when there's
    // nothing to be "second" to (very short file).
    let peak_2nd = block_peak[seg_cnt.saturating_sub(2).min(seg_cnt - 1)];

    let n_blk = ((seg_cnt as f64 * TOP_FRACTION).floor() as usize)
        .max(1)
        .min(seg_cnt);
    let top = &block_rms[seg_cnt - n_blk..];
    let rms_ref = (top.iter().map(|r| r * r).sum::<f64>() / n_blk as f64).sqrt();

    if rms_ref < MIN_RMS_REF_LINEAR || peak_2nd <= 0.0 {
        return None;
    }

    let dr_db = 20.0 * (peak_2nd / rms_ref).log10();
    if dr_db.abs() > MAX_PLAUSIBLE_DR_DB || !dr_db.is_finite() {
        return None;
    }
    Some(dr_db)
}
