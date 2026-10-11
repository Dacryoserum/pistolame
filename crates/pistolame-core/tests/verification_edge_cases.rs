//! Plan B10.3 #3: the verification edge cases through the real engine and a `FakeOutput`:
//! missing MD5, seeks, stop, tap overflow, seek to the end, play-out timeouts, pause and resume
//! (with another container, with a fallback to shared mode), mode switches and a corrupt file.
//! Level-1 mismatches are proven at the verifier level with hand-built taps (T7): no test hook
//! enters the feeder or the decode thread.
//!
//! Stepping: the fake holds after a given number of `render` calls, so every command lands at
//! a known point; every wait is an event wait with a timeout.

#![cfg(feature = "test-support")]

mod support;

use std::time::Duration;

use pistolame_core::testing::{
    AcceptedFormats, CD_STEREO, CORRUPT, FakeOutputConfig, FormatChoice, NO_MD5, generate_fixture,
};
use pistolame_core::{
    DeviceAccessMode, DeviceSampleFormat, EngineCommand, EngineTuning,
    FileIntegrityNotVerifiableReason, FileIntegrityStatus, OutputModePreference, PlaybackState,
    SharedModeCause, TransmissionNotVerifiableReason, TransmissionStatus, ValidBitsAlignment,
    lossless_device_format_candidates,
};
use support::{
    EVENT_WAIT, EngineRun, assert_played_exactly, canonical_md5, packed, start_engine,
    whole_track_tuning,
};

const EXCLUSIVE: OutputModePreference = OutputModePreference::Exclusive;
const SHARED: OutputModePreference = OutputModePreference::Shared;
/// Frames per fake `render` call (10 ms at 44.1 kHz).
const BUFFER_FRAMES: u32 = 441;

/// Room for three buffers in the fake device, so a pause always has frames queued.
fn queued_config() -> FakeOutputConfig {
    FakeOutputConfig {
        buffer_frame_count: BUFFER_FRAMES,
        queued_frame_capacity: 3 * BUFFER_FRAMES,
        ..FakeOutputConfig::default()
    }
}

fn hold_after(run: &EngineRun, render_calls: u64) {
    run.controller.hold_after_more_render_calls(render_calls);
}

fn wait_until_held(run: &EngineRun) {
    assert!(
        run.controller.wait_until_held(EVENT_WAIT),
        "the fake never held"
    );
}

fn cd_candidates() -> Vec<DeviceSampleFormat> {
    lossless_device_format_candidates(
        &generate_fixture(&CD_STEREO).format,
        ValidBitsAlignment::MostSignificant,
    )
}

fn not_verifiable(reason: TransmissionNotVerifiableReason) -> TransmissionStatus {
    TransmissionStatus::NotVerifiable { reason }
}

#[test]
fn no_md5_is_verified_on_level_one_and_gives_the_computed_digest() {
    let fixture = generate_fixture(&NO_MD5);
    let mut run = start_engine(queued_config(), EXCLUSIVE, whole_track_tuning(&NO_MD5));
    let track_id = run.play_to_end(&fixture.path);
    let digest = canonical_md5(&fixture.original_interleaved_pcm, 16);
    let status = run.final_status(track_id);
    assert_eq!(
        status.transmission,
        TransmissionStatus::Verified {
            first_frame_index: 0,
            frame_count: NO_MD5.frame_count() as u64,
            md5: digest
        }
    );
    assert_eq!(
        status.file_integrity,
        FileIntegrityStatus::NoEmbeddedSignature {
            computed_md5: Some(digest)
        }
    );
}

#[test]
fn a_seek_mid_track_verifies_from_the_target() {
    let fixture = generate_fixture(&CD_STEREO);
    // A ring shorter than the track: the decoder has not finished a complete pass when the
    // seek arrives, so level 2 cannot conclude (B8).
    let tuning = EngineTuning {
        playback_ring_duration: Duration::from_millis(500),
        start_prefill_duration: Duration::from_millis(200),
        verification_tap_duration: Duration::from_secs(4),
    };
    let mut run = start_engine(queued_config(), EXCLUSIVE, tuning);
    hold_after(&run, 20);
    let track_id = run.open(&fixture.path);
    wait_until_held(&run);
    run.send(EngineCommand::SeekTo {
        position_in_track: Duration::from_secs(1),
    });
    run.wait_for_state(PlaybackState::Opening);
    run.wait_for_state(PlaybackState::Playing);
    run.controller.release();
    run.wait_for_state(PlaybackState::Stopped);
    let target = 44_100usize;
    let status = run.final_status(track_id);
    assert_eq!(
        status.transmission,
        TransmissionStatus::Verified {
            first_frame_index: target as u64,
            frame_count: (CD_STEREO.frame_count() - target) as u64,
            md5: canonical_md5(&fixture.original_interleaved_pcm[target * 2..], 16),
        }
    );
    assert_eq!(
        status.file_integrity,
        FileIntegrityStatus::NotVerifiable {
            reason: FileIntegrityNotVerifiableReason::PlaybackRepositioned
        }
    );
}

#[test]
fn a_seek_back_to_zero_verifies_the_whole_track_on_both_levels() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(queued_config(), EXCLUSIVE, whole_track_tuning(&CD_STEREO));
    hold_after(&run, 30);
    let track_id = run.open(&fixture.path);
    wait_until_held(&run);
    run.send(EngineCommand::SeekTo {
        position_in_track: Duration::ZERO,
    });
    run.wait_for_state(PlaybackState::Opening);
    run.wait_for_state(PlaybackState::Playing);
    run.controller.release();
    run.wait_for_state(PlaybackState::Stopped);
    let status = run.final_status(track_id);
    assert_eq!(
        status.transmission,
        TransmissionStatus::Verified {
            first_frame_index: 0,
            frame_count: CD_STEREO.frame_count() as u64,
            md5: fixture.streaminfo_md5
        }
    );
    assert_eq!(
        status.file_integrity,
        FileIntegrityStatus::Verified {
            md5: fixture.streaminfo_md5
        }
    );
    let counters = run.handle.counters();
    assert_eq!(counters.underrun_events, 0, "a seek is no underrun");
    assert!(counters.stale_frames_discarded > 0);
}

#[test]
fn a_stop_mid_track_is_stopped_before_the_end_on_both_levels() {
    let fixture = generate_fixture(&CD_STEREO);
    // A ring shorter than the track: the decoder cannot reach the end while the fake holds.
    let tuning = EngineTuning {
        playback_ring_duration: Duration::from_millis(500),
        start_prefill_duration: Duration::from_millis(200),
        verification_tap_duration: Duration::from_secs(4),
    };
    let mut run = start_engine(queued_config(), EXCLUSIVE, tuning);
    hold_after(&run, 10);
    let track_id = run.open(&fixture.path);
    wait_until_held(&run);
    run.send(EngineCommand::Stop);
    run.wait_for_state(PlaybackState::Stopped);
    let status = run.final_status(track_id);
    assert_eq!(
        status.transmission,
        not_verifiable(TransmissionNotVerifiableReason::StoppedBeforeTrackEnd)
    );
    assert_eq!(
        status.file_integrity,
        FileIntegrityStatus::NotVerifiable {
            reason: FileIntegrityNotVerifiableReason::StoppedBeforeTrackEnd
        }
    );
}

#[test]
fn a_tiny_tap_overflows_level_one_but_playback_and_level_two_are_exact() {
    let fixture = generate_fixture(&CD_STEREO);
    let tuning = EngineTuning {
        verification_tap_duration: Duration::from_millis(1),
        ..whole_track_tuning(&CD_STEREO)
    };
    let mut run = start_engine(queued_config(), EXCLUSIVE, tuning);
    let track_id = run.play_to_end(&fixture.path);
    let render_format = run.controller.negotiated_streams()[0].render_format;
    assert_played_exactly(
        &run.controller.played_bytes(),
        &packed(&fixture.original_interleaved_pcm, 16, &render_format),
    );
    let status = run.final_status(track_id);
    assert_eq!(
        status.transmission,
        not_verifiable(TransmissionNotVerifiableReason::VerificationTapOverflowed)
    );
    assert_eq!(
        status.file_integrity,
        FileIntegrityStatus::Verified {
            md5: fixture.streaminfo_md5
        }
    );
    assert!(run.handle.counters().verification_tap_overflow_events > 0);
}

#[test]
fn a_seek_to_the_end_is_an_empty_segment_then_stops() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(queued_config(), EXCLUSIVE, whole_track_tuning(&CD_STEREO));
    hold_after(&run, 10);
    let track_id = run.open(&fixture.path);
    wait_until_held(&run);
    // Past the end: clamped to the last frame, the decoder parks there.
    run.send(EngineCommand::SeekTo {
        position_in_track: Duration::from_secs(60),
    });
    run.wait_for_state(PlaybackState::Opening);
    run.wait_for_state(PlaybackState::Playing);
    run.controller.release();
    run.wait_for_state(PlaybackState::Stopped);
    assert_eq!(
        run.final_status(track_id).transmission,
        not_verifiable(TransmissionNotVerifiableReason::EmptySegment)
    );
}

#[test]
fn a_timed_out_play_out_at_the_track_end_revises_level_one() {
    let fixture = generate_fixture(&CD_STEREO);
    let config = FakeOutputConfig {
        play_out_should_time_out: true,
        ..queued_config()
    };
    let mut run = start_engine(config, EXCLUSIVE, whole_track_tuning(&CD_STEREO));
    let track_id = run.play_to_end(&fixture.path);
    assert_eq!(
        run.final_status(track_id).transmission,
        not_verifiable(TransmissionNotVerifiableReason::QueuedAudioLostOnPause)
    );
}

/// Plays `CD_STEREO` with three pauses; `before_resume(pause_index)` runs while paused.
/// Returns the run, the track and the played-byte count at each pause.
fn play_with_three_pauses(
    config: FakeOutputConfig,
    before_resume: impl Fn(&EngineRun, usize),
) -> (EngineRun, pistolame_core::TrackId, Vec<usize>) {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(config, EXCLUSIVE, whole_track_tuning(&CD_STEREO));
    let mut bytes_at_pauses = Vec::new();
    hold_after(&run, 17);
    let track_id = run.open(&fixture.path);
    for (pause_index, render_calls_until_next_pause) in [41u64, 29, 53].into_iter().enumerate() {
        wait_until_held(&run);
        run.send(EngineCommand::Pause);
        run.wait_for_state(PlaybackState::Paused);
        bytes_at_pauses.push(run.controller.played_bytes().len());
        before_resume(&run, pause_index);
        if pause_index == 2 {
            run.controller.release();
        } else {
            hold_after(&run, render_calls_until_next_pause);
        }
        run.send(EngineCommand::Play);
        run.wait_for_state(PlaybackState::Playing);
    }
    run.wait_for_state(PlaybackState::Stopped);
    drop(fixture);
    (run, track_id, bytes_at_pauses)
}

#[test]
fn three_pauses_lose_and_repeat_no_frame_and_stay_verified() {
    let fixture = generate_fixture(&CD_STEREO);
    let (run, track_id, _) = play_with_three_pauses(queued_config(), |_, _| {});
    assert_eq!(
        run.controller.begin_stream_count(),
        4,
        "each resume begins a stream"
    );
    assert!(
        run.controller.dropped_bytes().is_empty(),
        "every pause played out"
    );
    let render_format = run.controller.negotiated_streams()[0].render_format;
    assert_played_exactly(
        &run.controller.played_bytes(),
        &packed(&fixture.original_interleaved_pcm, 16, &render_format),
    );
    assert_eq!(
        run.final_status(track_id).transmission,
        TransmissionStatus::Verified {
            first_frame_index: 0,
            frame_count: CD_STEREO.frame_count() as u64,
            md5: fixture.streaminfo_md5
        }
    );
    assert!(run.handle.counters().last_resume_delay_milliseconds < 60_000);
}

#[test]
fn a_resume_in_another_container_is_still_exact_and_verified() {
    let fixture = generate_fixture(&CD_STEREO);
    let candidates = cd_candidates();
    let first = candidates[0];
    let second = *candidates.last().expect("several candidates");
    assert_ne!(first.container_bytes, second.container_bytes);
    let config = FakeOutputConfig {
        exclusive_formats: AcceptedFormats::Only(vec![FormatChoice::of(&first)]),
        ..queued_config()
    };
    let (run, track_id, bytes_at_pauses) = play_with_three_pauses(config, |run, pause_index| {
        let accepted = if pause_index == 0 { second } else { first };
        run.controller.update_config(|config| {
            config.exclusive_formats = AcceptedFormats::Only(vec![FormatChoice::of(&accepted)]);
        });
    });
    let streams = run.controller.negotiated_streams();
    let formats: Vec<DeviceSampleFormat> =
        streams.iter().map(|stream| stream.render_format).collect();
    assert_eq!(formats, vec![first, second, first, first]);
    // Rebuild the expected bytes stream by stream: each one played its frames in its format.
    let played = run.controller.played_bytes();
    let mut expected = Vec::new();
    let mut next_frame = 0usize;
    let mut stream_start_byte = 0usize;
    for (stream_index, format) in formats.iter().enumerate() {
        let stream_end_byte = bytes_at_pauses.get(stream_index).copied();
        let frame_count = match stream_end_byte {
            Some(end) => (end - stream_start_byte) / format.bytes_per_frame(),
            None => CD_STEREO.frame_count() - next_frame,
        };
        let samples =
            &fixture.original_interleaved_pcm[next_frame * 2..(next_frame + frame_count) * 2];
        expected.extend(packed(samples, 16, format));
        next_frame += frame_count;
        stream_start_byte = stream_end_byte.unwrap_or(0);
    }
    assert_eq!(next_frame, CD_STEREO.frame_count());
    assert_played_exactly(&played, &expected);
    assert_eq!(
        run.final_status(track_id).transmission,
        TransmissionStatus::Verified {
            first_frame_index: 0,
            frame_count: CD_STEREO.frame_count() as u64,
            md5: fixture.streaminfo_md5
        }
    );
}

#[test]
fn a_pause_whose_play_out_times_out_loses_queued_audio() {
    let fixture = generate_fixture(&CD_STEREO);
    let config = FakeOutputConfig {
        play_out_should_time_out: true,
        ..queued_config()
    };
    let mut run = start_engine(config, EXCLUSIVE, whole_track_tuning(&CD_STEREO));
    hold_after(&run, 25);
    let track_id = run.open(&fixture.path);
    wait_until_held(&run);
    run.send(EngineCommand::Pause);
    run.wait_for_state(PlaybackState::Paused);
    run.controller.release();
    run.send(EngineCommand::Play);
    run.wait_for_state(PlaybackState::Stopped);
    assert!(!run.controller.dropped_bytes().is_empty());
    assert_eq!(
        run.final_status(track_id).transmission,
        not_verifiable(TransmissionNotVerifiableReason::QueuedAudioLostOnPause)
    );
}

#[test]
fn a_resume_that_falls_back_to_shared_mode_is_partly_through_the_mixer() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(queued_config(), EXCLUSIVE, whole_track_tuning(&CD_STEREO));
    hold_after(&run, 25);
    let track_id = run.open(&fixture.path);
    wait_until_held(&run);
    run.send(EngineCommand::Pause);
    run.wait_for_state(PlaybackState::Paused);
    run.controller
        .update_config(|config| config.exclusive_formats = AcceptedFormats::Nothing);
    run.controller.release();
    run.send(EngineCommand::Play);
    let assessment = run.wait_for_stream();
    assert_eq!(
        assessment.negotiated.shared_mode_cause,
        Some(SharedModeCause::ExclusiveFormatRefused)
    );
    run.wait_for_state(PlaybackState::Stopped);
    let status = run.final_status(track_id);
    assert_eq!(
        status.transmission,
        not_verifiable(TransmissionNotVerifiableReason::PartlyPlayedThroughSystemMixer)
    );
    assert_eq!(
        status.file_integrity,
        FileIntegrityStatus::Verified {
            md5: fixture.streaminfo_md5
        }
    );
}

/// Starts in `from`, switches to `to` mid-track; the bytes stay exact (both modes get the
/// same first candidate) and level 1 reads "partly through the mixer".
fn switch_mode_mid_track(from: OutputModePreference, to: OutputModePreference) {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(queued_config(), from, whole_track_tuning(&CD_STEREO));
    hold_after(&run, 30);
    let track_id = run.open(&fixture.path);
    run.wait_for_stream();
    wait_until_held(&run);
    run.send(EngineCommand::SetOutputModePreference { preference: to });
    let assessment = run.wait_for_stream();
    let expected_mode = match to {
        OutputModePreference::Exclusive => DeviceAccessMode::Exclusive,
        OutputModePreference::Shared => DeviceAccessMode::SharedThroughSystemMixer,
    };
    assert_eq!(assessment.negotiated.access_mode, expected_mode);
    run.controller.release();
    run.wait_for_state(PlaybackState::Stopped);
    let render_format = run.controller.negotiated_streams()[0].render_format;
    assert_played_exactly(
        &run.controller.played_bytes(),
        &packed(&fixture.original_interleaved_pcm, 16, &render_format),
    );
    assert_eq!(
        run.final_status(track_id).transmission,
        not_verifiable(TransmissionNotVerifiableReason::PartlyPlayedThroughSystemMixer)
    );
}

#[test]
fn switching_exclusive_to_shared_mid_track_is_partly_through_the_mixer() {
    switch_mode_mid_track(EXCLUSIVE, SHARED);
}

#[test]
fn switching_shared_to_exclusive_mid_track_is_partly_through_the_mixer() {
    switch_mode_mid_track(SHARED, EXCLUSIVE);
}

#[test]
fn shared_from_start_to_end_is_not_applicable_also_after_a_stop() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(queued_config(), SHARED, whole_track_tuning(&CD_STEREO));
    let track_id = run.play_to_end(&fixture.path);
    assert_eq!(
        run.final_status(track_id).transmission,
        TransmissionStatus::NotApplicableInSharedMode
    );

    hold_after(&run, 10);
    let stopped_track_id = run.open(&fixture.path);
    wait_until_held(&run);
    run.send(EngineCommand::Stop);
    run.wait_for_state(PlaybackState::Stopped);
    assert_eq!(
        run.final_status(stopped_track_id).transmission,
        TransmissionStatus::NotApplicableInSharedMode
    );
}

#[test]
fn a_corrupt_file_verifies_what_was_decoded_and_flags_level_two() {
    let fixture = generate_fixture(&CORRUPT);
    let mut run = start_engine(queued_config(), EXCLUSIVE, whole_track_tuning(&CORRUPT));
    let track_id = run.play_to_end(&fixture.path);
    let status = run.final_status(track_id);
    let TransmissionStatus::Verified {
        first_frame_index,
        frame_count,
        ..
    } = status.transmission
    else {
        panic!("level 1 should be verified over the decoded part: {status:?}");
    };
    assert_eq!(first_frame_index, 0);
    assert!(frame_count > 0 && frame_count < CORRUPT.frame_count() as u64);
    assert_eq!(
        status.file_integrity,
        FileIntegrityStatus::NotVerifiable {
            reason: FileIntegrityNotVerifiableReason::DecodeErrorInTrack
        }
    );
}
