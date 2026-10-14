//! WASAPI hardware tests, exclusive and shared mode (plan B10.5). They need a real device and
//! are ignored by default. They play an audible 1 kHz tone at -20 dBFS. Run them with the device
//! plugged in:
//!
//! `cargo test -p pistolame-output --test wasapi_hardware -- --ignored --test-threads=1`
//!
//! `PISTOLAME_TEST_DEVICE` selects the device by a part of its name (for example `K11`); without
//! it the system default device is used. The tone tests check the output contract: status,
//! play-out on pause, render source handed back, frame continuity.
//!
//! The engine tests play a real FLAC file through `EngineHandle` (decoder, feeder, verifier) and
//! assert both verification levels: level 1 `Verified` over the whole track in exclusive mode
//! (`NotApplicableInSharedMode` in shared mode), level 2 `Verified` (or `NoEmbeddedSignature`
//! for a file without MD5). `PISTOLAME_TEST_FLAC` names the file; they are skipped without it.
//! Choose a quiet file: it plays at its own level.

#![cfg(windows)]
#![allow(
    clippy::print_stdout,
    reason = "reports skipped formats and the mix format to the person running the test"
)]

#[path = "../examples/support/test_tone.rs"]
mod test_tone;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use pistolame_core::timing::QUEUED_AUDIO_PLAY_OUT_TIMEOUT;
use pistolame_output::wasapi::WasapiOutput;
use pistolame_output::{
    AudioOutput, ChannelMask, DeviceAccessMode, FormatProbeOutcome, NegotiatedOutput, OpenPolicy,
    OpenStreamRequest, OutputDeviceInfo, OutputStream, OutputStreamStatus, QueuedAudioOnClose,
    RenderSource, SharedModeCause, StreamFormat, ValidBitsAlignment,
    lossless_device_format_candidates,
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
/// A rate for the fallback test that USB Audio 2.0 DACs commonly do not run at natively, so
/// exclusive mode is likely to refuse it while the shared-mode converter accepts it.
const RATE_LIKELY_REFUSED_IN_EXCLUSIVE_MODE_HZ: u32 = 22_050;

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

/// What a segment expects the platform to grant.
#[derive(Clone, Copy, Debug)]
enum ExpectedAccess {
    Exclusive,
    Shared(SharedModeCause),
}

fn assert_negotiated(
    negotiated: &NegotiatedOutput,
    source: &StreamFormat,
    expected: ExpectedAccess,
) {
    assert_eq!(
        negotiated.render_format.sample_rate_hz,
        source.sample_rate_hz
    );
    assert!(negotiated.render_format.valid_bits_per_sample >= source.bits_per_sample);
    match expected {
        ExpectedAccess::Exclusive => {
            assert_eq!(negotiated.access_mode, DeviceAccessMode::Exclusive);
            assert_eq!(negotiated.shared_mode_cause, None);
            assert_eq!(negotiated.device_format, Some(negotiated.render_format));
            assert_eq!(negotiated.system_mix_format, None);
        }
        ExpectedAccess::Shared(cause) => {
            assert_eq!(
                negotiated.access_mode,
                DeviceAccessMode::SharedThroughSystemMixer
            );
            assert_eq!(negotiated.shared_mode_cause, Some(cause));
            assert_eq!(negotiated.device_format, None);
            let mix_format = negotiated
                .system_mix_format
                .expect("shared mode reports the mix format");
            println!("system mix format: {mix_format:?}");
            assert!(mix_format.sample_rate_hz > 0 && mix_format.channel_count > 0);
            let last_attempt = negotiated
                .negotiation_log
                .last()
                .expect("the accepted shared attempt is logged");
            assert_eq!(
                last_attempt.access_mode,
                DeviceAccessMode::SharedThroughSystemMixer
            );
            assert_eq!(last_attempt.outcome, FormatProbeOutcome::Accepted);
        }
    }
}

fn play_segment(
    output: &mut WasapiOutput,
    device: &OutputDeviceInfo,
    source: StreamFormat,
    policy: OpenPolicy,
    expected: ExpectedAccess,
    render_source: Box<dyn RenderSource>,
) -> Box<dyn RenderSource> {
    let request = OpenStreamRequest {
        device_id: device.id.clone(),
        source_format: source,
        policy,
    };
    let mut stream = output
        .open_stream(request, render_source)
        .unwrap_or_else(|failure| panic!("{policy:?} open failed: {}", failure.error));
    assert_negotiated(stream.negotiated(), &source, expected);
    assert_eq!(stream.status(), OutputStreamStatus::NotStarted);
    stream.play().expect("play works");
    std::thread::sleep(PLAY_DURATION);
    assert_eq!(stream.status(), OutputStreamStatus::Running);
    let device_period = Duration::from_nanos(
        u64::try_from(negotiated_period_in_hundred_nanoseconds(stream.as_ref()))
            .expect("the device period is positive")
            * NANOSECONDS_PER_HUNDRED_NANOSECONDS,
    );
    let buffer_duration = Duration::from_secs_f64(
        f64::from(stream.negotiated().buffer_frame_count)
            / f64::from(stream.negotiated().render_format.sample_rate_hz),
    );
    let close_started_at = Instant::now();
    let closed = stream.close(QueuedAudioOnClose::PlayOut);
    let close_duration = close_started_at.elapsed();
    assert!(closed.was_queued_audio_fully_played, "play-out on pause");
    // An independent check of the play-out, timed from outside, and well before the timeout.
    // Exclusive: both ping-pong buffers of audio must have played (at least two periods).
    // Shared: the single buffer was full of audio at the pause, and the play-out waits for it
    // plus one engine period; one period of slack covers the wake-up that was already under
    // way when the close request arrived.
    let minimum_close_duration = match expected {
        ExpectedAccess::Exclusive => device_period * 2,
        ExpectedAccess::Shared(_) => buffer_duration.saturating_sub(device_period),
    };
    assert!(
        close_duration >= minimum_close_duration,
        "close(PlayOut) took {close_duration:?}, less than {minimum_close_duration:?}          (buffer {buffer_duration:?}, period {device_period:?})"
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
    play_cycles(
        &mut output,
        &device,
        source,
        OpenPolicy::ExclusiveOnly,
        ExpectedAccess::Exclusive,
    );
}

/// Plays `PAUSE_RESUME_CYCLES + 1` segments, closing with a play-out between them, and checks
/// that the render source kept rendering across every reopen.
fn play_cycles(
    output: &mut WasapiOutput,
    device: &OutputDeviceInfo,
    source: StreamFormat,
    policy: OpenPolicy,
    expected: ExpectedAccess,
) {
    let sample_rate_hz = source.sample_rate_hz;
    let tone = TestToneRenderSource::new(&source);
    let rendered_frames = tone.rendered_frame_counter();
    let mut render_source: Box<dyn RenderSource> = Box::new(tone);
    for _ in 0..=PAUSE_RESUME_CYCLES {
        let frames_before = rendered_frames.load(Ordering::Relaxed);
        render_source = play_segment(output, device, source, policy, expected, render_source);
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
fn wasapi_shared_cd_stereo_plays_and_pauses() {
    let mut output = WasapiOutput::new();
    let device = selected_device(&mut output);
    play_cycles(
        &mut output,
        &device,
        stereo_source(44_100, 16),
        OpenPolicy::SharedOnly,
        ExpectedAccess::Shared(SharedModeCause::UserPreference),
    );
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn wasapi_shared_hires_96_plays_and_pauses() {
    let mut output = WasapiOutput::new();
    let device = selected_device(&mut output);
    play_cycles(
        &mut output,
        &device,
        stereo_source(96_000, 24),
        OpenPolicy::SharedOnly,
        ExpectedAccess::Shared(SharedModeCause::UserPreference),
    );
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn wasapi_exclusive_then_shared_falls_back_on_a_refused_rate() {
    let mut output = WasapiOutput::new();
    let device = selected_device(&mut output);
    let source = stereo_source(RATE_LIKELY_REFUSED_IN_EXCLUSIVE_MODE_HZ, 16);
    if is_accepted_by_probe(&mut output, &device, &source) {
        println!(
            "skipped: {} accepts {} Hz in exclusive mode, so there is nothing to fall back from",
            device.display_name, source.sample_rate_hz
        );
        return;
    }
    play_cycles(
        &mut output,
        &device,
        source,
        OpenPolicy::ExclusiveThenShared,
        ExpectedAccess::Shared(SharedModeCause::ExclusiveFormatRefused),
    );
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

/// Longest wait for any engine event in the engine tests (the track itself must be shorter).
const ENGINE_EVENT_WAIT: Duration = Duration::from_secs(600);
/// Playing time before the pause in the exclusive engine test.
const ENGINE_PLAY_BEFORE_PAUSE: Duration = Duration::from_secs(2);

/// The engine on WASAPI, with the test device selected, waiting for events.
struct HardwareEngine {
    handle: pistolame_core::EngineHandle,
    latest_status: Option<pistolame_core::VerificationStatus>,
    track_id: Option<pistolame_core::TrackId>,
}

impl HardwareEngine {
    fn start(preference: pistolame_output::OutputModePreference) -> Self {
        let device_id = selected_device(&mut WasapiOutput::new()).id;
        let handle = pistolame_core::EngineHandle::start(
            Box::new(WasapiOutput::new()),
            pistolame_core::DecoderRegistry::with_default_providers(),
            pistolame_core::EngineStartOptions {
                output_mode_preference: preference,
                device_selection: pistolame_core::OutputDeviceSelection::Specific(device_id),
                shared_mode_volume: Default::default(),
            },
        )
        .expect("the engine starts");
        Self {
            handle,
            latest_status: None,
            track_id: None,
        }
    }

    /// Waits for `state`, recording the track and its latest verification status.
    fn wait_for_state(&mut self, expected: pistolame_core::PlaybackState) {
        use pistolame_core::EngineEvent;
        loop {
            let event = self
                .handle
                .receive_event_timeout(ENGINE_EVENT_WAIT)
                .unwrap_or_else(|| panic!("no {expected:?} state"));
            match event {
                EngineEvent::TrackLoaded { track_id, .. } => self.track_id = Some(track_id),
                EngineEvent::VerificationChanged { track_id, status }
                    if Some(track_id) == self.track_id =>
                {
                    self.latest_status = Some(status);
                }
                EngineEvent::PlaybackFailed { kind, detail } => {
                    panic!("playback failed: {kind:?} ({detail})")
                }
                EngineEvent::PlaybackStateChanged { state } if state == expected => return,
                _ => {}
            }
        }
    }
}

/// The file named by `PISTOLAME_TEST_FLAC`, its frame count and its embedded MD5.
fn test_flac() -> Option<(std::path::PathBuf, u64, Option<[u8; 16]>)> {
    let Some(path) = std::env::var_os("PISTOLAME_TEST_FLAC") else {
        println!("skipped: set PISTOLAME_TEST_FLAC to a FLAC file");
        return None;
    };
    let path = std::path::PathBuf::from(path);
    let decoder = pistolame_core::DecoderRegistry::with_default_providers()
        .open(&path)
        .expect("PISTOLAME_TEST_FLAC decodes");
    let info = decoder.stream_info();
    let total_frames = info.total_frames.expect("the file states its length");
    Some((path, total_frames, info.embedded_md5))
}

fn assert_level_two(status: &pistolame_core::VerificationStatus, embedded_md5: Option<[u8; 16]>) {
    use pistolame_core::FileIntegrityStatus;
    match embedded_md5 {
        Some(md5) => assert_eq!(status.file_integrity, FileIntegrityStatus::Verified { md5 }),
        None => assert!(matches!(
            status.file_integrity,
            FileIntegrityStatus::NoEmbeddedSignature {
                computed_md5: Some(_)
            }
        )),
    }
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn engine_exclusive_file_is_verified_on_both_levels_across_a_pause() {
    use pistolame_core::{EngineCommand, PlaybackState, TransmissionStatus};
    let Some((path, total_frames, embedded_md5)) = test_flac() else {
        return;
    };
    let mut engine = HardwareEngine::start(pistolame_output::OutputModePreference::Exclusive);
    engine.handle.send(EngineCommand::OpenFileAndPlay { path });
    engine.wait_for_state(PlaybackState::Playing);
    std::thread::sleep(ENGINE_PLAY_BEFORE_PAUSE);
    engine.handle.send(EngineCommand::Pause);
    engine.wait_for_state(PlaybackState::Paused);
    std::thread::sleep(PAUSE_DURATION);
    engine.handle.send(EngineCommand::Play);
    engine.wait_for_state(PlaybackState::Playing);
    println!(
        "resume delay: {} ms",
        engine.handle.counters().last_resume_delay_milliseconds
    );
    engine.wait_for_state(PlaybackState::Stopped);
    let status = engine.latest_status.clone().expect("a final status");
    println!("final status: {status:?}");
    assert!(matches!(
        status.transmission,
        TransmissionStatus::Verified { first_frame_index: 0, frame_count, .. }
            if frame_count == total_frames
    ));
    assert_level_two(&status, embedded_md5);
    assert_eq!(engine.handle.counters().underrun_events, 0);
    engine.handle.shutdown();
}

#[test]
#[ignore = "needs an audio device; see the module documentation"]
fn engine_shared_file_is_not_applicable_on_level_one_and_verified_on_level_two() {
    use pistolame_core::{EngineCommand, PlaybackState, TransmissionStatus};
    let Some((path, _, embedded_md5)) = test_flac() else {
        return;
    };
    let mut engine = HardwareEngine::start(pistolame_output::OutputModePreference::Shared);
    engine.handle.send(EngineCommand::OpenFileAndPlay { path });
    engine.wait_for_state(PlaybackState::Stopped);
    let status = engine.latest_status.clone().expect("a final status");
    println!("final status: {status:?}");
    assert_eq!(
        status.transmission,
        TransmissionStatus::NotApplicableInSharedMode
    );
    assert_level_two(&status, embedded_md5);
    engine.handle.shutdown();
}
