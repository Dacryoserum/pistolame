//! A sine test tone as a `RenderSource`, shared by the `play_file` example and the WASAPI
//! hardware tests (included with `#[path]`). It stands in for the decoder and feeder until
//! tasks T5, T6 and T9 exist.
//!
//! The tone is computed once, at construction, into a table holding a whole number of cycles;
//! `render` only copies and packs integers, so it honors the real-time contract (no allocation,
//! no lock, no float).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pistolame_output::{
    DeviceSampleFormat, RenderOutcome, RenderSource, RenderStreamDescription, StreamFormat,
};

/// Frequency of the test tone.
const TONE_FREQUENCY_HZ: u32 = 1_000;
/// Peak level of the tone relative to full scale (-20 dBFS), comfortable on headphones.
const TONE_PEAK_RELATIVE_TO_FULL_SCALE: f64 = 0.1;
/// Bytes in the `i32` the packing starts from.
const SAMPLE_WORD_BYTES: usize = 4;

/// Renders a 1 kHz sine at -20 dBFS, the same on every channel, and counts the frames it wrote.
pub struct TestToneRenderSource {
    one_cycle_group_samples: Vec<i32>,
    next_table_index: usize,
    source_bits_per_sample: u8,
    render_format: Option<DeviceSampleFormat>,
    rendered_frame_count: Arc<AtomicU64>,
}

impl TestToneRenderSource {
    /// Builds the tone table for `source_format`'s rate and bit depth.
    pub fn new(source_format: &StreamFormat) -> TestToneRenderSource {
        let sample_rate_hz = source_format.sample_rate_hz.max(1);
        let table_frame_count =
            (sample_rate_hz / greatest_common_divisor(sample_rate_hz, TONE_FREQUENCY_HZ)) as usize;
        let full_scale = f64::from((1u32 << (source_format.bits_per_sample - 1)) - 1);
        let one_cycle_group_samples = (0..table_frame_count)
            .map(|frame_index| {
                let phase =
                    std::f64::consts::TAU * f64::from(TONE_FREQUENCY_HZ) * frame_index as f64
                        / f64::from(sample_rate_hz);
                (phase.sin() * full_scale * TONE_PEAK_RELATIVE_TO_FULL_SCALE).round() as i32
            })
            .collect();
        TestToneRenderSource {
            one_cycle_group_samples,
            next_table_index: 0,
            source_bits_per_sample: source_format.bits_per_sample,
            render_format: None,
            rendered_frame_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Shared counter of tone frames written to device buffers, readable from any thread.
    pub fn rendered_frame_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.rendered_frame_count)
    }
}

impl RenderSource for TestToneRenderSource {
    fn begin_stream(&mut self, description: RenderStreamDescription) {
        self.render_format = Some(description.render_format);
    }

    fn render(&mut self, destination: &mut [u8]) -> RenderOutcome {
        let Some(render_format) = self.render_format else {
            destination.fill(0);
            return RenderOutcome::FilledWithUnderrun;
        };
        let container_bytes = usize::from(render_format.container_bytes);
        let mut frame_count: u64 = 0;
        for frame in destination.chunks_exact_mut(render_format.bytes_per_frame()) {
            let sample = self.one_cycle_group_samples[self.next_table_index];
            self.next_table_index =
                (self.next_table_index + 1) % self.one_cycle_group_samples.len();
            for channel_slot in frame.chunks_exact_mut(container_bytes) {
                pack_sample_most_significant(sample, self.source_bits_per_sample, channel_slot);
            }
            frame_count += 1;
        }
        self.rendered_frame_count
            .fetch_add(frame_count, Ordering::Relaxed);
        RenderOutcome::Filled
    }
}

/// Minimal private packer for the test tone only; replace with
/// `pistolame_core::pcm::pack_samples_into_container` once task T3 is merged.
/// Rule from plan B6: left-justify to 32 bits, keep the `container.len()` most significant
/// bytes, little-endian.
fn pack_sample_most_significant(sample: i32, source_bits_per_sample: u8, container: &mut [u8]) {
    let left_justified = sample << (32 - u32::from(source_bits_per_sample));
    let word_bytes = left_justified.to_le_bytes();
    container.copy_from_slice(&word_bytes[SAMPLE_WORD_BYTES - container.len()..]);
}

fn greatest_common_divisor(mut first: u32, mut second: u32) -> u32 {
    while second != 0 {
        (first, second) = (second, first % second);
    }
    first.max(1)
}
