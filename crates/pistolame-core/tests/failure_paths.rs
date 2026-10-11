//! Plan B10.3 #5: negotiation, fallback and refusal through the engine and a `FakeOutput`;
//! the device lost mid-track; a corrupt file; the remembered device missing at start-up. The
//! engine's reaction to a format boundary is unit-tested on its poll (`engine_thread.rs`).

#![cfg(feature = "test-support")]

mod support;

use pistolame_core::testing::{
    AcceptedFormats, CD_STEREO, CORRUPT, DeviceOccupancy, FakeOutputConfig, fake_device,
    generate_fixture,
};
use pistolame_core::{
    DeviceAccessMode, EngineCommand, EngineEvent, EngineStartOptions, EngineTuning,
    FormatProbeOutcome, OutputDeviceId, OutputFailure, OutputModePreference, PlaybackFailureKind,
    PlaybackState, SharedModeCause, SignalPathAssessment, TransmissionNotVerifiableReason,
    TransmissionStatus,
};
use support::{EVENT_WAIT, EngineRun, start_engine, start_engine_with_options, whole_track_tuning};

const EXCLUSIVE: OutputModePreference = OutputModePreference::Exclusive;

fn tuning() -> EngineTuning {
    whole_track_tuning(&CD_STEREO)
}

/// Opens `CD_STEREO` and returns the assessment of the stream that opened.
fn first_stream(
    config: FakeOutputConfig,
    preference: OutputModePreference,
) -> SignalPathAssessment {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(config, preference, tuning());
    run.controller.hold_after_more_render_calls(1);
    run.open(&fixture.path);
    run.wait_for_stream()
}

/// Opens `CD_STEREO` on a fake that refuses it; returns the run once it is `Stopped`.
fn refused_open(config: FakeOutputConfig) -> EngineRun {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(config, EXCLUSIVE, tuning());
    run.open(&fixture.path);
    run.wait_for_state(PlaybackState::Stopped);
    run
}

#[test]
fn every_refused_exclusive_format_falls_back_to_shared_mode_with_the_full_log() {
    let assessment = first_stream(
        FakeOutputConfig {
            exclusive_formats: AcceptedFormats::Nothing,
            ..FakeOutputConfig::default()
        },
        EXCLUSIVE,
    );
    let negotiated = assessment.negotiated;
    assert_eq!(
        negotiated.access_mode,
        DeviceAccessMode::SharedThroughSystemMixer
    );
    assert_eq!(
        negotiated.shared_mode_cause,
        Some(SharedModeCause::ExclusiveFormatRefused)
    );
    let (shared_attempt, exclusive_attempts) =
        negotiated.negotiation_log.split_last().expect("a log");
    assert!(!exclusive_attempts.is_empty());
    assert!(exclusive_attempts.iter().all(|attempt| {
        attempt.access_mode == DeviceAccessMode::Exclusive
            && attempt.outcome == FormatProbeOutcome::Refused
    }));
    assert_eq!(
        shared_attempt.access_mode,
        DeviceAccessMode::SharedThroughSystemMixer
    );
    assert_eq!(shared_attempt.outcome, FormatProbeOutcome::Accepted);
}

#[test]
fn exclusive_mode_disabled_falls_back_to_shared_mode() {
    let assessment = first_stream(
        FakeOutputConfig {
            is_exclusive_mode_disabled: true,
            ..FakeOutputConfig::default()
        },
        EXCLUSIVE,
    );
    assert_eq!(
        assessment.negotiated.shared_mode_cause,
        Some(SharedModeCause::ExclusiveModeDisabledBySystem)
    );
}

#[test]
fn a_device_busy_in_shared_mode_falls_back_to_shared_mode() {
    let assessment = first_stream(
        FakeOutputConfig {
            occupancy: DeviceOccupancy::BusyInSharedMode,
            ..FakeOutputConfig::default()
        },
        EXCLUSIVE,
    );
    assert_eq!(
        assessment.negotiated.shared_mode_cause,
        Some(SharedModeCause::DeviceBusyInSharedMode)
    );
}

#[test]
fn a_device_held_exclusively_by_another_app_fails_with_no_stream() {
    let run = refused_open(FakeOutputConfig {
        occupancy: DeviceOccupancy::HeldExclusivelyByAnotherApp,
        ..FakeOutputConfig::default()
    });
    assert_eq!(run.failures(), vec![PlaybackFailureKind::DeviceInUse]);
    assert!(run.controller.negotiated_streams().is_empty());
}

#[test]
fn a_device_refusing_both_modes_fails_with_format_refused() {
    let run = refused_open(FakeOutputConfig {
        exclusive_formats: AcceptedFormats::Nothing,
        shared_formats: AcceptedFormats::Nothing,
        ..FakeOutputConfig::default()
    });
    assert_eq!(
        run.failures(),
        vec![PlaybackFailureKind::FormatRefusedByDevice]
    );
    assert!(run.controller.negotiated_streams().is_empty());
}

#[test]
fn the_shared_preference_never_tries_exclusive_mode() {
    let assessment = first_stream(FakeOutputConfig::default(), OutputModePreference::Shared);
    assert_eq!(
        assessment.negotiated.shared_mode_cause,
        Some(SharedModeCause::UserPreference)
    );
    assert!(
        assessment
            .negotiated
            .negotiation_log
            .iter()
            .all(|attempt| { attempt.access_mode == DeviceAccessMode::SharedThroughSystemMixer })
    );
}

#[test]
fn an_absent_preferred_device_selects_the_default_and_says_so() {
    let missing = OutputDeviceId("unplugged-dac".to_owned());
    let mut run = start_engine_with_options(
        FakeOutputConfig::default(),
        EngineStartOptions {
            output_mode_preference: EXCLUSIVE,
            preferred_device_id: Some(missing.clone()),
        },
        tuning(),
    );
    let unavailable = run.wait_for("PreferredDeviceUnavailable", |event| {
        matches!(event, EngineEvent::PreferredDeviceUnavailable { .. })
    });
    assert!(matches!(
        unavailable,
        EngineEvent::PreferredDeviceUnavailable { device_id } if device_id == missing
    ));
    let list = run.wait_for("DeviceListChanged", |event| {
        matches!(event, EngineEvent::DeviceListChanged { .. })
    });
    assert!(matches!(
        list,
        EngineEvent::DeviceListChanged { selected: Some(selected), .. } if selected == fake_device().id
    ));
}

#[test]
fn a_device_lost_mid_track_stops_with_device_unplugged_and_play_restarts_the_track() {
    let fixture = generate_fixture(&CD_STEREO);
    let mut run = start_engine(FakeOutputConfig::default(), EXCLUSIVE, tuning());
    run.controller.hold_after_more_render_calls(20);
    let track_id = run.open(&fixture.path);
    assert!(run.controller.wait_until_held(EVENT_WAIT));
    run.controller.fail_stream(OutputFailure::DeviceInvalidated);
    run.wait_for_state(PlaybackState::Stopped);
    assert_eq!(run.failures(), vec![PlaybackFailureKind::DeviceUnplugged]);
    assert_eq!(
        run.final_status(track_id).transmission,
        TransmissionStatus::NotVerifiable {
            reason: TransmissionNotVerifiableReason::StoppedBeforeTrackEnd
        }
    );
    let device_list_count = run
        .events
        .iter()
        .filter(|event| matches!(event, EngineEvent::DeviceListChanged { .. }))
        .count();
    assert_eq!(
        device_list_count, 2,
        "start-up, then the refresh after the failure"
    );

    // No automatic re-activation: Play restarts the same file as a new track.
    run.controller.release();
    run.send(EngineCommand::Play);
    let restarted = run.wait_for("TrackLoaded", |event| {
        matches!(event, EngineEvent::TrackLoaded { .. })
    });
    assert!(matches!(
        restarted,
        EngineEvent::TrackLoaded { track_id: new_track, .. } if new_track.0 == track_id.0 + 1
    ));
    run.wait_for_state(PlaybackState::Stopped);
}

#[test]
fn a_corrupt_file_fails_with_corrupt_file_after_the_good_part_plays() {
    let fixture = generate_fixture(&CORRUPT);
    let mut run = start_engine(
        FakeOutputConfig::default(),
        EXCLUSIVE,
        whole_track_tuning(&CORRUPT),
    );
    run.open(&fixture.path);
    run.wait_for_state(PlaybackState::Playing);
    let failure = run.wait_for("PlaybackFailed", |event| {
        matches!(event, EngineEvent::PlaybackFailed { .. })
    });
    assert!(matches!(
        failure,
        EngineEvent::PlaybackFailed {
            kind: PlaybackFailureKind::CorruptFile,
            ..
        }
    ));
    run.wait_for_state(PlaybackState::Stopped);
    assert!(
        !run.controller.played_bytes().is_empty(),
        "the good part played"
    );
}

#[test]
fn an_unreadable_file_fails_without_opening_anything() {
    let mut run = start_engine(FakeOutputConfig::default(), EXCLUSIVE, tuning());
    run.send(EngineCommand::OpenFileAndPlay {
        path: std::env::temp_dir().join("pistolame-missing-file.flac"),
    });
    run.wait_for("PlaybackFailed", |event| {
        matches!(
            event,
            EngineEvent::PlaybackFailed {
                kind: PlaybackFailureKind::FileUnreadable,
                ..
            }
        )
    });
    assert!(run.controller.negotiated_streams().is_empty());
}
