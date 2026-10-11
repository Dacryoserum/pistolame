//! WASAPI exclusive-mode hardware tests (plan B10.5). They need a real device and are ignored
//! by default. Run them with the device plugged in:
//!
//! `cargo test -p pistolame-output --test wasapi_hardware -- --ignored --test-threads=1`
//!
//! `PISTOLAME_TEST_DEVICE` selects the device by a part of its name (for example `K11`); without
//! it the system default device is used. Until the decoder, feeder and verifier exist (T5, T6,
//! T7, T9), the source is a generated test tone and the checks are on the output contract:
//! status, play-out on pause, render source handed back, frame continuity. Level-1 and level-2
//! verification are added to these tests when those tasks land.

#![cfg(windows)]
#![allow(
    clippy::print_stdout,
    reason = "reports skipped formats to the person running the test"
)]

#[path = "../examples/support/test_tone.rs"]
mod test_tone;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use pistolame_core::timing::QUEUED_AUDIO_PLAY_OUT_TIMEOUT;
use pistolame_output::wasapi::WasapiOutput;
use pistolame_output::{
    AudioOutput, ChannelMask, DeviceAccessMode, FormatProbeOutcome, OpenPolicy, OpenStreamRequest,
    OutputDeviceInfo, OutputStream, OutputStreamStatus, QueuedAudioOnClose, RenderSource,
    StreamFormat, ValidBitsAlignment, lossless_device_format_candidates,
};
use test_tone::TestToneRenderSource;

/// Playing time per segment.
const PLAY_DURATION: Duration = Duration::from_secs(3);
/// Released-device time between pause and resume.
const PAUSE_DURATION: Duration = Duration::from_millis(500);
/// Pause/resume cycles per format (plan B10.5).
const PAUSE_RESUME_CYCLES: usize = 2;
/// WASAPI's REFERENCE_TIME unit is 100 ns.
const NANOSECONDS_PER_HUNDRED_NANOSECONDS: u64 = 100;

fn selected_device(output: &mut WasapiOutput) -> OutputDeviceInfo {
    let devices = output.list_devices().expect("device listing works");
    let name_filter = std::env::var("PISTOLAME_TEST_DEVICE")
        .ok()
        .map(|name| name.to_lowercase());
    devices
        .into_iter()
        .find(|device| match &name_filter {
            Some(filter) => device.display_name.to_lowercase().contains(filter),
            None => device.is_system_default,
        })
        .expect("the test device is plugged in")
}

fn stereo_source(sample_rate_hz: u32, bits_per_sample: u8) -> StreamFormat {
    StreamFormat {
        sample_rate_hz,
        bits_per_sample,
        channel_count: 2,
        channel_mask: ChannelMask(0x3),
    }
}

fn is_accepted_by_probe(
    output: &mut WasapiOutput,
    device: &OutputDeviceInfo,
    source: &StreamFormat,
) -> bool {
    let candidates = lossless_device_format_candidates(source, ValidBitsAlignment::MostSignificant);
    output
        .probe_formats(&device.id, &candidates)
        .expect("probing works")
        .iter()
        .any(|result| result.outcome == FormatProbeOutcome::Accepted)
}

fn play_segment(
    output: &mut WasapiOutput,
    device: &OutputDeviceInfo,
    source: StreamFormat,
    render_source: Box<dyn RenderSource>,
) -> Box<dyn RenderSource> {
    let request = OpenStreamRequest {
        device_id: device.id.clone(),
        source_format: source,
        policy: OpenPolicy::ExclusiveOnly,
    };
    let mut stream = output
        .open_stream(request, render_source)
        .unwrap_or_else(|failure| panic!("exclusive open failed: {}", failure.error));
    let negotiated = stream.negotiated();
    assert_eq!(negotiated.access_mode, DeviceAccessMode::Exclusive);
    assert_eq!(negotiated.shared_mode_cause, None);
    assert_eq!(negotiated.device_format, Some(negotiated.render_format));
    assert_eq!(
        negotiated.render_format.sample_rate_hz,
        source.sample_rate_hz
    );
    assert!(negotiated.render_format.valid_bits_per_sample >= source.bits_per_sample);
    assert_eq!(stream.status(), OutputStreamStatus::NotStarted);
    stream.play().expect("play works");
    std::thread::sleep(PLAY_DURATION);
    assert_eq!(stream.status(), OutputStreamStatus::Running);
    let device_period = Duration::from_nanos(
        u64::try_from(negotiated_period_in_hundred_nanoseconds(stream.as_ref()))
            .expect("the device period is positive")
            * NANOSECONDS_PER_HUNDRED_NANOSECONDS,
    );
    let close_started_at = Instant::now();
    let closed = stream.close(QueuedAudioOnClose::PlayOut);
    let close_duration = close_started_at.elapsed();
    assert!(closed.was_queued_audio_fully_played, "play-out on pause");
    // An independent check of the play-out, timed from outside: both ping-pong buffers of
    // audio must have played (at least two periods), and it must end well before the timeout.
    assert!(
        close_duration >= device_period * 2,
        "close(PlayOut) took {close_duration:?}, less than two periods of {device_period:?}"
    );
    assert!(
        close_duration < QUEUED_AUDIO_PLAY_OUT_TIMEOUT,
        "close(PlayOut) took {close_duration:?}"
    );
    closed.render_source
}

fn negotiated_period_in_hundred_nanoseconds(stream: &dyn OutputStream) -> i64 {
    stream.negotiated().device_period_in_hundred_nanoseconds
}

fn play_with_pauses(sample_rate_hz: u32, bits_per_sample: u8) {
    let mut output = WasapiOutput::new();
    let device = selected_device(&mut output);
    let source = stereo_source(sample_rate_hz, bits_per_sample);
    if !is_accepted_by_probe(&mut output, &device, &source) {
        println!(
            "skipped: {} refuses {bits_per_sample}/{sample_rate_hz} in exclusive mode",
            device.display_name
        );
        return;
    }
    let tone = TestToneRenderSource::new(&source);
    let rendered_frames = tone.rendered_frame_counter();
    let mut render_source: Box<dyn RenderSource> = Box::new(tone);
    for _ in 0..=PAUSE_RESUME_CYCLES {
        let frames_before = rendered_frames.load(Ordering::Relaxed);
        render_source = play_segment(&mut output, &device, source, render_source);
        let frames_in_segment = rendered_frames.load(Ordering::Relaxed) - frames_before;
        let expected_minimum_frames =
            u64::from(sample_rate_hz) * PLAY_DURATION.as_millis() as u64 / 1_000;
        assert!(
            frames_in_segment >= expected_minimum_frames,
            "{frames_in_segment} frames rendered in {PLAY_DURATION:?}"
        );
        std::thread::sleep(PAUSE_DURATION);
    }
    assert_eq!(output.diagnostics().mmcss_registration_failures, 0);
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn wasapi_exclusive_cd_stereo_plays_and_pauses() {
    play_with_pauses(44_100, 16);
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn wasapi_exclusive_hires_96_plays_and_pauses() {
    play_with_pauses(96_000, 24);
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn wasapi_exclusive_hires_192_plays_and_pauses() {
    play_with_pauses(192_000, 24);
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn wasapi_exclusive_close_without_play_discards_and_returns_the_source() {
    let mut output = WasapiOutput::new();
    let device = selected_device(&mut output);
    let source = stereo_source(44_100, 16);
    let request = OpenStreamRequest {
        device_id: device.id.clone(),
        source_format: source,
        policy: OpenPolicy::ExclusiveOnly,
    };
    let stream = output
        .open_stream(request, Box::new(TestToneRenderSource::new(&source)))
        .unwrap_or_else(|failure| panic!("exclusive open failed: {}", failure.error));
    let closed = stream.close(QueuedAudioOnClose::Discard);
    assert!(!closed.was_queued_audio_fully_played);
}
